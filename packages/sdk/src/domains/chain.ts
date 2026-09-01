/**
 * The chain domain: JSON-RPC to a public EVM network, from the client directly.
 *
 * Every other domain in this package talks to a Migo server through {@link Rpc}. This one talks to
 * Avalanche's public C-Chain RPC and deliberately skips the Migo server entirely — §184: the server
 * is never a blockchain proxy, never holds a nonce, and never sees a transaction, because the chain
 * is public and a proxy would only add a trusted party the network does not need. The RPC URL comes
 * from a {@link Network} constant the user picked by name, never as free input — a self-supplied
 * RPC is the classic way a wallet gets shown a fake chain (spec #44).
 *
 * # What this class does and does not decide
 *
 * The read side (balance, nonce, gas, fees) and the write side (broadcast) are here; *signing* is
 * not. `broadcast` takes a {@link SignedTx} from `@migo/crypto` — the private key never enters this
 * file, and the only bytes this class hands to the network are ones the user already confirmed
 * against a full transaction display (spec #40).
 *
 * # The session rule (spec #44)
 *
 * The first RPC of a client's life is `eth_chainId`, and its answer must equal the configured
 * network's chain id before anything else is asked — a mismatch is the chain-confusion case, and
 * the honest response is to refuse, not to pick one of the two ids. `broadcast` re-verifies: it is
 * the one moment bytes that carry value leave, and an endpoint that answers differently mid-session
 * must not get them.
 *
 * # The two confirmations that are never the same state (spec #41)
 *
 * `eth_sendRawTransaction` returning a hash means the RPC *accepted* the transaction, not that the
 * blockchain confirmed it. `broadcast` therefore reports acceptance and nothing more, and the only
 * road to `CONFIRMED` is {@link track}: `eth_getTransactionReceipt` answering `status: 1`. The
 * tracker polls with exponential backoff and a deadline; a receipt with `status: 0` is `REVERTED`,
 * a transaction that vanishes from the mempool before a block is `DROPPED`, and a deadline that
 * runs out is `EXPIRED` — reported as an unresolved *ending*, never as success.
 */

import { account } from '@migo/crypto';

import { SdkError } from '../errors.js';
import type { FetchLike } from '../rest.js';

export const AVALANCHE_MAINNET: account.Network = account.AVALANCHE_MAINNET;
export const FUJI_TESTNET: account.Network = account.FUJI_TESTNET;
/** The network type a {@link ChainClient} speaks, re-stated so this module is its own import root. */
export type Network = account.Network;

/**
 * The transaction states of spec #41. The first seven are the live path
 * (`DRAFT → PREPARED → AWAITING_CONFIRMATION → SIGNED → BROADCAST → PENDING → CONFIRMED`); the
 * last six are failure endings. `DRAFT` through `SIGNED` are driven by the send flow's UI (a form,
 * a built transaction, a confirmation screen, a signature) and only appear here as names so every
 * client labels them identically; `BROADCAST` onward are what the chain itself decides.
 */
export type TxState =
  | 'DRAFT'
  | 'PREPARED'
  | 'AWAITING_CONFIRMATION'
  | 'SIGNED'
  | 'BROADCAST'
  | 'PENDING'
  | 'CONFIRMED'
  | 'REJECTED'
  | 'FAILED'
  | 'REVERTED'
  | 'DROPPED'
  | 'REPLACED'
  | 'EXPIRED';

/** The states the tracker can end in; everything else is progress or the caller's own doing. */
export type TrackedOutcome = 'CONFIRMED' | 'REVERTED' | 'DROPPED' | 'EXPIRED';

/** The chain refused or failed the broadcast — `FAILED`, distinct from every tracked outcome. */
export class ChainError extends SdkError {
  /** The JSON-RPC error code the endpoint answered with, when there was one. */
  readonly code: number | undefined;

  constructor(message: string, code?: number) {
    super(message);
    this.name = 'ChainError';
    this.code = code;
  }
}

/** What a broadcast needs to know about a transaction to price its gas. */
export interface GasSubject {
  /** The sender, when known — the node's balance and nonce checks are per-account. */
  readonly from?: Uint8Array;
  /** The recipient, 20 bytes. */
  readonly to: Uint8Array;
  /** The amount, wei. */
  readonly value: bigint;
  /** Call data — empty for a native transfer. */
  readonly data: Uint8Array;
}

/** A fee ceiling pair for an EIP-1559 transaction, both in wei per gas. */
export interface FeeEstimate {
  /** The priority fee ceiling, from `eth_maxPriorityFeePerGas`. */
  readonly maxPriorityFeePerGas: bigint;
  /**
   * The total fee ceiling: the observed gas price plus the priority fee. A ceiling, not a price —
   * EIP-1559 refunds the difference between this and what the block actually cost.
   */
  readonly maxFeePerGas: bigint;
}

/** Options for {@link ChainClient.track}. */
export interface TrackOptions {
  /**
   * How long to keep polling before declaring `EXPIRED`. Default two minutes — enough for several
   * C-Chain blocks (about two seconds each) plus a congested mempool, not so long that a stuck
   * send keeps a spinner alive all day.
   */
  readonly timeoutMs?: number;
  /** The first poll interval; each subsequent one grows by half, capped at `maxIntervalMs`. */
  readonly initialIntervalMs?: number;
  /** The poll interval ceiling. Default 15 seconds. */
  readonly maxIntervalMs?: number;
  /**
   * How many consecutive `null` transaction lookups to tolerate before declaring `DROPPED`. A
   * transaction can sit unindexed for a poll or two right after broadcast; it cannot sit there
   * forever. Default 6.
   */
  readonly missingTolerance?: number;
  /**
   * Called on every state the tracker passes through (`PENDING` on first sight, then the ending),
   * so a UI can show progress without owning the poll loop.
   */
  readonly onState?: (state: TrackedOutcome | 'PENDING', txHash: string) => void;
}

/** The result of tracking a transaction to an ending. */
export interface TrackResult {
  /** The ending the tracker reached. */
  readonly outcome: TrackedOutcome;
  /** The block that included the transaction, when it got into one. */
  readonly blockNumber: number | undefined;
  /** The gas the transaction actually used, when it got into a block. */
  readonly gasUsed: bigint | undefined;
  /** The tx hash, echoed for callers that track several at once. */
  readonly txHash: string;
}

/**
 * A JSON-RPC 2.0 conversation with one pinned EVM network.
 *
 * One instance per network per client. Not part of {@link MigoClient}: the chain conversation is
 * orthogonal to the Migo session (it needs no login and no trust), so it is constructed directly
 * and lives or dies with the wallet surface that uses it.
 */
export class ChainClient {
  readonly #network: account.Network;
  readonly #fetch: FetchLike;
  readonly #url: string;
  #nextId = 1;
  #chainVerified = false;

  constructor(options: { network: account.Network; fetch?: FetchLike }) {
    this.#network = options.network;
    this.#fetch = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
    this.#url = options.network.rpcUrl;
  }

  /** The network this client speaks, with its chain id and pinned RPC. */
  get network(): account.Network {
    return this.#network;
  }

  /**
   * The session rule: asks `eth_chainId` and refuses to continue unless it matches. Called
   * automatically before every operation (once per client, and again at every broadcast); public so
   * a wallet surface can open the session explicitly and fail before rendering anything.
   *
   * @throws the crypto package's `AccountError` (`ChainMismatch`) naming both ids.
   */
  async verifyChain(): Promise<void> {
    const observed = await this.#rpc<string>('eth_chainId', []);
    const parsed = Number.parseInt(observed, 16);
    if (!Number.isSafeInteger(parsed)) {
      throw new ChainError(`eth_chainId answered a non-integer: ${observed}`);
    }
    // checkChainId throws on mismatch — the caller's remedy is a different network, never a
    // transaction built against the mismatched one.
    account.checkChainId(this.#network, parsed);
    this.#chainVerified = true;
  }

  /**
   * The balance of an address, in wei, as of the latest block. Explicitly a pull: §184 forbids
   * silent polling, so callers refresh when the user asks.
   */
  async getBalance(address: Uint8Array): Promise<bigint> {
    await this.#ensureSession();
    // JSON-RPC quantities are hex strings, whatever their magnitude.
    const balance = await this.#rpc<string>('eth_getBalance', [addressHex(address), 'latest']);
    return quantityWei(balance, 'balance');
  }

  /**
   * The account's next nonce, from `eth_getTransactionCount` with `'pending'` — the count includes
   * the account's in-flight transactions, so two sends composed in a row get distinct nonces rather
   * than a second broadcast that quietly replaces the first.
   */
  async getNonce(address: Uint8Array): Promise<number> {
    await this.#ensureSession();
    const nonce = await this.#rpc<string>('eth_getTransactionCount', [
      addressHex(address),
      'pending',
    ]);
    return this.#quantity(nonce, 'nonce');
  }

  /**
   * The gas a transaction needs, from `eth_estimateGas`. The estimate is for the current block; a
   * caller that shows it to a user should add nothing to it silently — the ceiling the user
   * confirms is the one signed.
   */
  async estimateGas(subject: GasSubject): Promise<number> {
    await this.#ensureSession();
    const gas = await this.#rpc<string>('eth_estimateGas', [
      {
        ...(subject.from !== undefined ? { from: addressHex(subject.from) } : {}),
        to: addressHex(subject.to),
        value: `0x${subject.value.toString(16)}`,
        data: `0x${hexOf(subject.data)}`,
      },
    ]);
    return this.#quantity(gas, 'gas estimate');
  }

  /**
   * The EIP-1559 fee ceilings for the current block: the priority fee the endpoint recommends and
   * a total ceiling above it. Both are *ceilings* — the chain charges what the block costs and
   * refunds the rest.
   */
  async getFees(): Promise<FeeEstimate> {
    await this.#ensureSession();
    const [priority, gasPrice] = await Promise.all([
      this.#rpc<string>('eth_maxPriorityFeePerGas', []).then((value) =>
        quantityWei(value, 'priority fee'),
      ),
      this.#rpc<string>('eth_gasPrice', []).then((value) => quantityWei(value, 'gas price')),
    ]);
    return {
      maxPriorityFeePerGas: priority,
      maxFeePerGas: gasPrice + priority,
    };
  }

  /**
   * Broadcasts a signed transaction and reports *acceptance* — never confirmation. An RPC that
   * answers a hash other than `Keccak-256(raw)` is refused: the hash is the only handle the user
   * will track this transaction by, and a substituted one means the tracker would follow someone
   * else's transaction to its ending.
   *
   * @throws {ChainError} if the endpoint refuses the transaction (`FAILED` in spec #41's terms).
   */
  async broadcast(signed: account.SignedTx): Promise<string> {
    // The session rule, again, at the one moment value-carrying bytes leave.
    await this.verifyChain();
    const rawHex = `0x${hexOf(signed.raw())}`;
    const answered = await this.#rpc<string>('eth_sendRawTransaction', [rawHex]);
    if (answered !== signed.txHashHex()) {
      throw new ChainError(
        `eth_sendRawTransaction answered a foreign hash: ${answered} (expected ${signed.txHashHex()})`,
      );
    }
    return answered;
  }

  /**
   * Follows a broadcast transaction to an honest ending: `CONFIRMED` only via
   * `eth_getTransactionReceipt` answering `status: 1`, `REVERTED` on `status: 0`, `DROPPED` when
   * the transaction is gone from the mempool without a block, `EXPIRED` when the deadline runs
   * out. "The RPC accepted it" is a state this method never returns.
   *
   * The poll interval starts at `initialIntervalMs` (default 2 seconds) and grows by half each
   * round up to `maxIntervalMs`, because a transaction that has waited a minute is not going to
   * confirm in the next two seconds and polling like it will is noise.
   */
  async track(txHash: string, options: TrackOptions = {}): Promise<TrackResult> {
    const timeoutMs = options.timeoutMs ?? 120_000;
    const maxIntervalMs = options.maxIntervalMs ?? 15_000;
    const missingTolerance = options.missingTolerance ?? 6;
    let interval = options.initialIntervalMs ?? 2_000;

    const deadline = Date.now() + timeoutMs;
    let missing = 0;
    let seen = false;
    for (;;) {
      const receipt = await this.#getReceipt(txHash);
      if (receipt !== undefined) {
        const confirmed = receipt.status === 1n;
        const outcome: TrackedOutcome = confirmed ? 'CONFIRMED' : 'REVERTED';
        options.onState?.(outcome, txHash);
        return {
          outcome,
          blockNumber: receipt.blockNumber,
          gasUsed: receipt.gasUsed,
          txHash,
        };
      }
      // No receipt. The transaction may simply not be in a block yet — or it may be gone: look
      // for it in the mempool and count consecutive absences.
      const pending = await this.#getTransaction(txHash);
      if (pending !== undefined) {
        missing = 0;
        if (!seen) {
          seen = true;
          options.onState?.('PENDING', txHash);
        }
      } else {
        missing += 1;
        // A transaction the mempool never indexed (right after broadcast) and one that appeared
        // then vanished are both gone as far as this client can tell. `REPLACED` — a same-nonce
        // sibling confirming instead — is indistinguishable without an indexer, so a vanished
        // transaction reports `DROPPED` and the Activity list lets a refresh correct it.
        if (seen || missing >= missingTolerance) {
          options.onState?.('DROPPED', txHash);
          return { outcome: 'DROPPED', blockNumber: undefined, gasUsed: undefined, txHash };
        }
      }
      if (Date.now() + interval >= deadline) {
        options.onState?.('EXPIRED', txHash);
        return { outcome: 'EXPIRED', blockNumber: undefined, gasUsed: undefined, txHash };
      }
      await sleep(interval);
      interval = Math.min(Math.floor((interval * 3) / 2), maxIntervalMs);
    }
  }

  // --- plumbing --------------------------------------------------------------

  /** The session rule on first use: no RPC leaves this class before the chain id has been checked. */
  async #ensureSession(): Promise<void> {
    if (!this.#chainVerified) {
      await this.verifyChain();
    }
  }

  /** One JSON-RPC request/response over the pinned URL. */
  async #rpc<T>(method: string, params: unknown[]): Promise<T> {
    const response = await this.#fetch(this.#url, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: this.#nextId++, method, params }),
    });
    if (!response.ok) {
      throw new ChainError(`${method}: HTTP ${response.status}`, undefined);
    }
    const body = (await response.json()) as {
      result?: unknown;
      error?: { code?: unknown; message?: unknown };
    };
    if (body.error !== undefined) {
      const code = typeof body.error.code === 'number' ? body.error.code : undefined;
      const message = typeof body.error.message === 'string' ? body.error.message : method;
      throw new ChainError(`${method}: ${message}`, code);
    }
    return body.result as T;
  }

  /** The receipt of a mined transaction, or `undefined` when there is not one yet. */
  async #getReceipt(
    txHash: string,
  ): Promise<{ status: bigint; blockNumber: number; gasUsed: bigint } | undefined> {
    await this.#ensureSession();
    const receipt = await this.#rpc<{
      status?: string;
      blockNumber?: string;
      gasUsed?: string;
    } | null>('eth_getTransactionReceipt', [txHash]);
    if (receipt === null || receipt === undefined) {
      return undefined;
    }
    const status = this.#quantity(receipt.status ?? '0x0', 'receipt status');
    return {
      status: BigInt(status),
      blockNumber: this.#quantity(receipt.blockNumber ?? '0x0', 'receipt block'),
      gasUsed: BigInt(this.#quantity(receipt.gasUsed ?? '0x0', 'receipt gas used')),
    };
  }

  /** The mempool-or-block entry for a transaction, or `undefined` when the chain has none. */
  async #getTransaction(txHash: string): Promise<true | undefined> {
    await this.#ensureSession();
    const tx = await this.#rpc<unknown>('eth_getTransactionByHash', [txHash]);
    return tx === null || tx === undefined ? undefined : true;
  }

  /** A JSON-RPC quantity string ("0x…") as a number, refusing a non-integer or a past-2^53 one. */
  #quantity(value: string, what: string): number {
    const parsed = Number.parseInt(value, 16);
    if (!Number.isSafeInteger(parsed)) {
      throw new ChainError(`${what} is not a small integer quantity: ${value}`);
    }
    return parsed;
  }
}

/** A JSON-RPC quantity as wei — a bigint, because balances and fees live far above 2^53. */
function quantityWei(value: string, what: string): bigint {
  if (!/^0x[0-9a-fA-F]*$/.test(value)) {
    throw new ChainError(`${what} is not a quantity: ${value}`);
  }
  return BigInt(value === '0x' ? '0x0' : value);
}

/** 20 bytes as the `0x`-prefixed lowercase hex every RPC method takes. */
function addressHex(address: Uint8Array): string {
  return `0x${hexOf(address)}`;
}

function hexOf(bytes: Uint8Array): string {
  let out = '';
  for (const byte of bytes) {
    out += byte.toString(16).padStart(2, '0');
  }
  return out;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}
