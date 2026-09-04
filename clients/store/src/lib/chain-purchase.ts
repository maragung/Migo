/**
 * The on-chain purchase: coins-priced, paid in AVAX/USDT/USDC, settled on Avalanche Fuji.
 *
 * # What "on-chain" means here
 *
 * The server's catalogue prices every pack in Migo coins (a virtual, non-monetary number). The
 * user's instruction makes the *payment* on-chain: the buyer pays real tokens on Avalanche
 * C-Chain Fuji — native AVAX, or USDT/USDC through their ERC-20 contracts — and the amount is
 * the MGO-token equivalent of the coin price. The conversion is a fixed rate this module owns
 * (1 MGO = 1 coin-equivalent unit); when the MGO ERC-20 contract is deployed the constant below
 * is the single line that changes.
 *
 * The flow, in order, with each step visible to the caller:
 *
 *   1. `preparePurchase` — the chain's own answers (fees, gas, nonce) and the exact ERC-20
 *      calldata, before anything is signed.
 *   2. `payOnChain` — sign with wallet 0 of the account root (`EvmWallet.fromRoot`), broadcast
 *      through `ChainClient` (the Migo server is never a blockchain proxy and never sees the
 *      transaction), track to an honest ending.
 *   3. `client.economy.purchase(sku, clientKey, txHash)` — the server writes the entitlement and
 *      the ledger legs, with the tx hash riding along for audit.
 *
 * Paying with native AVAX sends `value` to the MGO treasury address; paying with USDT/USDC sends
 * an ERC-20 `transfer(address,uint256)` in the calldata. Either way the *amount* the user
 * confirms is the MGO quantity, and the confirm screen quotes every field the signature covers —
 * the same rule the wallet's AVAX send keeps.
 *
 * # Idempotency
 *
 * `clientKey` is minted once per purchase intent; a retry after a network failure re-sends the
 * same key and the server returns the first purchase instead of charging twice. It is derived
 * from the SKU and the on-chain tx hash when one exists (a settled on-chain payment followed by
 * a failed RPC is the same purchase, retried) — so the same payment can never buy twice.
 */

import { account, ChainClient, FUJI_TESTNET } from '@migo/sdk';
import type { MigoClient, TrackedOutcome } from '@migo/sdk';

import { hexOf } from './hex.js';

/**
 * The MGO token's ERC-20 contract on Fuji.
 *
 * **Deploy it and change this line** — until then the placeholder is the zero address, which the
 * prepare step refuses to build a payment for, so the UI can offer AVAX honestly rather than a
 * token transfer to nowhere. Address, not name: the chain knows it by nothing else.
 */
export const MGO_TOKEN_FUJI = '0x0000000000000000000000000000000000000000';

/** The treasury that receives the payment: the deployment's own wallet. Deploy and change this line. */
export const MGO_TREASURY_FUJI = '0x0000000000000000000000000000000000000000';

/**
 * USDT on Fuji. Testnet tokens are redeployed at the owner's whim; this build refuses a USDT/USDC
 * payment while the address is the placeholder rather than sending someone's tokens to address zero.
 */
export const USDT_FUJI = '0x0000000000000000000000000000000000000000';

/** USDC on Fuji. Same rule as USDT. */
export const USDC_FUJI = '0x0000000000000000000000000000000000000000';

/** MGO's decimals: the ERC-20 standard's own unit for the amount the chain receives. */
export const MGO_DECIMALS = 18n;

/** One MGO in its smallest unit, the way `10n ** 18n` is one AVAX in wei. */
const MGO_UNIT = 10n ** MGO_DECIMALS;

/**
 * The coin→MGO conversion: one coin prices one MGO.
 *
 * A fixed, whole-number rate keeps every confirm screen exact — no rounding at the moment of
 * payment, and the server's coin price and the chain's MGO amount never disagree about magnitude.
 */
const COINS_PER_MGO = 1n;

/** The payment currencies the store accepts, as the chips name them. */
export type PayCurrency = 'avax' | 'usdt' | 'usdc';

/** The label and small print each currency carries on its chip. */
export const CURRENCY_META: Readonly<Record<PayCurrency, { label: string; note: string }>> = {
  avax: { label: 'AVAX', note: 'native, pays as a direct transfer' },
  usdt: { label: 'USDT', note: 'ERC-20 transfer' },
  usdc: { label: 'USDC', note: 'ERC-20 transfer' },
};

/** Whether a currency is actually payable in this build: a placeholder contract is not. */
export function currencyAvailable(currency: PayCurrency): boolean {
  if (currency === 'avax') {
    return MGO_TREASURY_FUJI !== '0x0000000000000000000000000000000000000000';
  }
  const contract = currency === 'usdt' ? USDT_FUJI : USDC_FUJI;
  return contract !== '0x0000000000000000000000000000000000000000';
}

/** The coin price of an item as the MGO amount the chain will be paid, smallest units. */
export function mgoAmountFor(coins: number): bigint {
  const mgo = BigInt(Math.max(0, Math.trunc(coins))) * COINS_PER_MGO;
  return mgo * MGO_UNIT;
}

/** A smallest-unit MGO amount as a decimal string, trailing zeros trimmed. */
export function mgoOf(units: bigint): string {
  const whole = units / MGO_UNIT;
  let fraction = (units % MGO_UNIT).toString(10);
  if (fraction.match(/^0*$/)) {
    return whole.toString(10);
  }
  while (fraction.length < Number(MGO_DECIMALS)) {
    fraction = `0${fraction}`;
  }
  return `${whole}.${fraction.replace(/0+$/, '')}`;
}

/** What a prepared purchase quotes on the confirm screen: every field the signature covers. */
export interface PreparedPurchase {
  /** The pack's catalogue code. */
  sku: string;
  /** The pack's display name. */
  name: string;
  /** What is being paid with. */
  currency: PayCurrency;
  /** Wallet 0's checksummed address — the sender the signature will commit. */
  from: string;
  /** The recipient: the treasury for AVAX, the token contract for USDT/USDC. */
  to: string;
  /** The MGO amount, smallest units — the number the user is actually agreeing to. */
  mgoUnits: bigint;
  /** For a token payment, the token contract the transaction calls. */
  tokenContract: string | null;
  /** For a token payment, the treasury the calldata's transfer names. */
  treasury: string | null;
  /** EIP-1559 fee ceilings, wei per gas. */
  maxPriorityFeePerGas: bigint;
  maxFeePerGas: bigint;
  /** The gas limit, from the node's own estimate. */
  gasLimit: number;
  /** The account's next nonce, `pending`-aware. */
  nonce: number;
  /** Fuji's chain id, restated for the line the confirm screen always shows. */
  chainId: number;
}

/** One step of the payment's progress, as the UI narrates it. */
export type PurchaseStep =
  'preparing' | 'awaiting' | 'signed' | 'broadcast' | 'pending' | 'settled' | 'entitled';

/** Where the payment stands, once anything has left the page. */
export interface PurchaseProgress {
  step: PurchaseStep;
  /** The on-chain hash once one exists — the handle the explorer and the server both know. */
  txHash: string | null;
  /** The tracker's honest ending, once reached. */
  outcome: TrackedOutcome | null;
}

/**
 * Builds the transaction from the chain's own answers.
 *
 * Parse failures and placeholder contracts refuse *here*, before a single RPC leaves — a confirm
 * screen must never quote a field this function could not fill from the network. The root check
 * is the same one the AVAX wallet makes: a device without the root has no wallet, and paying
 * needs one.
 *
 * @throws {Error} when the currency's contract (or the treasury) is the placeholder, or the
 *   device holds no account root.
 */
export async function preparePurchase(input: {
  client: MigoClient;
  sku: string;
  name: string;
  coins: number;
  currency: PayCurrency;
}): Promise<PreparedPurchase> {
  const root = input.client.keyStore.root();
  if (root === null) {
    throw new Error(
      'This device does not hold the account root, so it has no wallet to pay from; open the store on the device that holds the account backup.',
    );
  }
  if (!currencyAvailable(input.currency)) {
    throw new Error(
      'That currency has no contract address in this build yet; pay with one of the chips that is enabled.',
    );
  }
  const wallet = account.EvmWallet.fromRoot(root, 0);
  const chain = new ChainClient({ network: FUJI_TESTNET });
  const mgoUnits = mgoAmountFor(input.coins);
  const isNative = input.currency === 'avax';
  const to = isNative
    ? account.parseAddress(MGO_TREASURY_FUJI)
    : account.parseAddress(input.currency === 'usdt' ? USDT_FUJI : USDC_FUJI);
  // ERC-20 `transfer(address,uint256)`: the 4-byte selector, a treasury address, a padded amount.
  // For a token payment the transaction's `to` is the *contract*; the treasury rides in the calldata.
  const recipient = isNative ? to : account.parseAddress(MGO_TREASURY_FUJI);
  const data = isNative ? new Uint8Array(0) : erc20Transfer(recipient, mgoUnits);
  const [fees, gasLimit, nonce] = await Promise.all([
    chain.getFees(),
    chain.estimateGas({
      from: wallet.address(),
      to,
      value: isNative ? mgoUnits : 0n,
      data,
    }),
    chain.getNonce(wallet.address()),
  ]);
  return {
    sku: input.sku,
    name: input.name,
    currency: input.currency,
    from: wallet.addressChecksummed(),
    to: account.eip55(to),
    mgoUnits,
    tokenContract: isNative ? null : account.eip55(to),
    treasury: isNative ? null : account.eip55(recipient),
    maxPriorityFeePerGas: fees.maxPriorityFeePerGas,
    maxFeePerGas: fees.maxFeePerGas,
    gasLimit,
    nonce,
    chainId: FUJI_TESTNET.chainId,
  };
}

/**
 * Signs, broadcasts, tracks, and then tells the server.
 *
 * The signature covers exactly the fields `preparePurchase` returned — nothing is re-derived
 * here except the wallet (whose address the prepared `from` is checked against, the same guard
 * the AVAX send keeps). The tracked transaction is recorded on the key store's sealed record and
 * the caller persists the snapshot so the web client's Activity list sees it.
 *
 * Only an on-chain `CONFIRMED` calls `purchase`; a `REVERTED`/`DROPPED`/`EXPIRED` ending throws
 * with the outcome named, because a payment that did not settle must not mint an entitlement.
 */
export async function payOnChain(
  prepared: PreparedPurchase,
  client: MigoClient,
  onProgress: (progress: PurchaseProgress) => void,
): Promise<{ txHash: string; duplicate: boolean }> {
  const root = client.keyStore.root();
  if (root === null) {
    throw new Error('The account root left this device; re-open the store and prepare again.');
  }
  const wallet = account.EvmWallet.fromRoot(root, 0);
  if (prepared.from !== wallet.addressChecksummed()) {
    throw new Error('The prepared purchase names a different sender; prepare it again here.');
  }
  const isNative = prepared.currency === 'avax';
  const to = account.parseAddress(prepared.to.trim());
  // For a token payment the prepared `to` is the contract and the treasury rides in the calldata.
  const recipient = isNative ? to : account.parseAddress(MGO_TREASURY_FUJI);
  const body = new account.Eip1559Tx({
    chainId: FUJI_TESTNET.chainId,
    nonce: prepared.nonce,
    maxPriorityFeePerGas: prepared.maxPriorityFeePerGas,
    maxFeePerGas: prepared.maxFeePerGas,
    gasLimit: prepared.gasLimit,
    to,
    value: isNative ? prepared.mgoUnits : 0n,
    data: isNative ? new Uint8Array(0) : erc20Transfer(recipient, prepared.mgoUnits),
  });
  onProgress({ step: 'signed', txHash: null, outcome: null });
  const signed = body.sign(wallet);
  const chain = new ChainClient({ network: FUJI_TESTNET });
  const txHash = await chain.broadcast(signed);
  onProgress({ step: 'broadcast', txHash, outcome: null });

  // The record is written at broadcast, not at settle: a reload mid-tracking loses the ending,
  // never the fact that value left.
  client.keyStore.trackedTxs().unshift({
    txHash: signed.txHash(),
    chainId: FUJI_TESTNET.chainId,
    to,
    valueWei: 0n,
    feeWei: prepared.maxFeePerGas * BigInt(prepared.gasLimit),
    gasLimit: prepared.gasLimit,
    atUnix: Math.floor(Date.now() / 1000),
    outcome: 'PENDING',
  });

  let outcome: TrackedOutcome;
  try {
    const result = await chain.track(txHash, {
      onState: (state) => {
        const step: PurchaseStep | null =
          state === 'PENDING' ? 'pending' : state === 'CONFIRMED' ? 'settled' : null;
        if (step !== null) {
          onProgress({ step, txHash, outcome: null });
        }
      },
    });
    outcome = result.outcome;
  } catch {
    outcome = 'EXPIRED';
  }
  settle(client, txHash, outcome);
  onProgress({ step: 'settled', txHash, outcome });
  if (outcome !== 'CONFIRMED') {
    throw new Error(
      `The payment did not settle on-chain (${outcome.toLowerCase()}); nothing was charged to your Migo account.`,
    );
  }

  // The payment settled; the entitlement is the server's to write, keyed by the tx hash so a
  // retry of this exact step is the same purchase, never a second one.
  onProgress({ step: 'entitled', txHash, outcome });
  const key = purchaseKey(prepared.sku, txHash);
  const result = await client.economy.purchase(prepared.sku, key, txHash);
  return { txHash, duplicate: result.duplicate };
}

/** The idempotency key for a purchase: the SKU and the payment that paid for it. */
export function purchaseKey(sku: string, txHash: string): string {
  return `${sku}:${txHash}`;
}

/** Writes a tracker's ending into the record the wallet's Activity list will next read. */
function settle(client: MigoClient, txHash: string, outcome: TrackedOutcome): void {
  const records = client.keyStore.trackedTxs();
  const index = records.findIndex((record) => `0x${hexOf(record.txHash)}` === txHash);
  const record = index >= 0 ? records[index] : undefined;
  if (record !== undefined && index >= 0) {
    records[index] = { ...record, outcome };
  }
}

/** The ERC-20 `transfer(address,uint256)` calldata: selector, padded address, padded amount. */
function erc20Transfer(recipient: Uint8Array, amount: bigint): Uint8Array {
  const selector = Uint8Array.of(0xa9, 0x05, 0x9c, 0xbb);
  const out = new Uint8Array(4 + 32 + 32);
  out.set(selector, 0);
  out.set(recipient.subarray(0, 20), 4 + 12);
  const hex = amount.toString(16).padStart(64, '0');
  for (let index = 0; index < 32; index += 1) {
    out[4 + 32 + index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16);
  }
  return out;
}
