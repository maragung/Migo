/**
 * The transaction domain: EIP-1559 on Avalanche C-Chain, signed on the device.
 *
 * # What opens here and what does not
 *
 * `evm.ts` shipped the wallet as an address and nothing more; this module is the half it
 * deliberately deferred: a transaction can now be *built* and *signed*, entirely locally, for
 * broadcast on Avalanche C-Chain. What does not change is the boundary: the private key still never
 * leaves the {@link EvmWallet} object, the only output that crosses it is a signed raw transaction,
 * and the server remains unaware that blockchain exists. The RPC conversation — nonce, gas estimate,
 * broadcast, receipt — belongs to the clients, because it is a conversation with a public network
 * and needs no trust from this one.
 *
 * # One transaction type, from one standard
 *
 * EIP-1559 (type `0x02`) is the only type built here. Avalanche has supported dynamic fees since
 * Cortina, so a legacy fallback would be a second code path with its own bugs and no user it serves.
 * The fields are the EIP's nine: `chainId`, `nonce`, `maxPriorityFeePerGas`, `maxFeePerGas`,
 * `gasLimit`, `to`, `value`, `data`, and an access list this build always writes empty.
 *
 * The serialized form is `0x02 || RLP([the nine fields, yParity, r, s])` — a FLAT list of twelve
 * items, unlike legacy transactions where the signature wraps an already-encoded body. The signing
 * hash is `Keccak-256(0x02 || RLP(fields))`, and the signature is ECDSA-secp256k1 with low-s
 * normalization, exactly as every EVM chain verifies them — which is the point: a transaction this
 * module signs is not a "Migo-shaped" transaction, it is the transaction any Ethereum tool would
 * accept, over a key any BIP-44 wallet would derive.
 *
 * # RLP, in the open
 *
 * RLP is implemented here rather than imported because it is small (four length-prefix rules and
 * one recursion), it is the exact place a subtle bug becomes a transaction that pays a different
 * amount than the one the user confirmed, and the decoder must be strict anyway: recovery parses
 * bytes that came from a network, so a non-minimal length prefix or a trailing byte after a valid
 * item is rejected rather than tolerated. The encoder is pinned by the RLP specification's own
 * examples and the decoder by a real Avalanche C-Chain transaction, both in the conformance vectors
 * (`account-tx.json`).
 *
 * # Chain identity is checked, not assumed
 *
 * A transaction is bound to its chain by the `chainId` inside the signed body — that is the whole
 * replay protection. {@link AVALANCHE_MAINNET} and {@link FUJI_TESTNET} carry the two chains this
 * build speaks with their pinned RPC URLs, and {@link checkChainId} exists so an RPC-observed
 * `eth_chainId` can be verified against the configured network *before* anything is built: a
 * mismatch must close the session, not produce a transaction for the wrong chain. The RPC URL is a
 * documented constant, not a configuration knob, because the user picks a network and never a URL —
 * a self-supplied RPC is the classic way a wallet gets shown a fake chain.
 *
 * # EIP-712
 *
 * Typed-data signing is the other half a wallet eventually owes: {@link Eip712Domain} and
 * {@link eip712HashStruct} implement the EIP-712 hash construction over a small, explicit value
 * model — the caller supplies the type hash, the values arrive already typed, and nothing parses a
 * type string at runtime (a parser here would be a large attack surface with no product need behind
 * it yet). The one rule the EIP buries in a subclause — and the one the conformance vectors pin
 * against the EIP's own worked example — is that a struct's *type hash* covers not just its own
 * declaration but every struct type it references, appended sorted by name; {@link eip712EncodeType}
 * builds that string so no port has to remember it.
 */

import { bytesToHex, concatBytes } from '@noble/ciphers/utils.js';
import { secp256k1 } from '@noble/curves/secp256k1.js';
import { keccak_256 } from '@noble/hashes/sha3.js';

import { AccountError } from './errors.js';
import { eip55 } from './evm.js';
import type { EvmWallet } from './evm.js';

/**
 * The secp256k1 group order, for low-s normalization. A signature with `s > n/2` is valid ECDSA but
 * invalid on every EVM chain, which rejects malleable signatures — so normalization is part of
 * signing, not a caller concern.
 */
const SECP256K1_N = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n;

/** The transaction type byte for EIP-1559 — the first byte of both the signing hash input and the raw transaction. */
const EIP1559_TYPE = 0x02;

/** The 20-byte Ethereum address length. */
const ADDRESS_LEN = 20;

// --- networks -----------------------------------------------------------------

/**
 * An EVM network this build can name: a chain id and a pinned RPC.
 *
 * The RPC URL is a documented constant, not a configuration knob, because the user picks a network
 * and never a URL — a self-supplied RPC is the classic way a wallet gets shown a fake chain.
 */
export interface Network {
  /** Human name for UI surfaces. */
  readonly name: string;
  /** The EIP-155 chain id, signed inside every transaction. */
  readonly chainId: number;
  /** The pinned public RPC endpoint. */
  readonly rpcUrl: string;
}

/** Avalanche C-Chain mainnet: chain id 43114. */
export const AVALANCHE_MAINNET: Network = {
  name: 'Avalanche C-Chain',
  chainId: 43114,
  rpcUrl: 'https://api.avax.network/ext/bc/C/rpc',
};

/** Avalanche Fuji testnet: chain id 43113. The verification network — never mainnet. */
export const FUJI_TESTNET: Network = {
  name: 'Avalanche Fuji',
  chainId: 43113,
  rpcUrl: 'https://api.avax-test.network/ext/bc/C/rpc',
};

/**
 * Verifies an RPC-observed chain id against `network`. Called with the answer to `eth_chainId`
 * before a transaction is built; a mismatch is the chain-confusion case and must close the session.
 *
 * @throws {AccountError} `ChainMismatch` naming both ids.
 */
export function checkChainId(network: Network, observed: number): void {
  if (observed !== network.chainId) {
    throw AccountError.chainMismatch(network.chainId, observed);
  }
}

// --- RLP ----------------------------------------------------------------------

/**
 * One RLP item: a byte string or a list of items.
 *
 * RLP has exactly these two shapes; integers are byte strings an encoder produced from a big-endian
 * minimal representation, and the round trip is the caller's to interpret. The danger in RLP is
 * never the tree shape, it is the length-prefix rules, which live in one place below.
 */
export type RlpItem = Uint8Array | RlpItem[];

/**
 * Encodes a payload length prefix. `offset` is 0x80 for strings, 0xC0 for lists; the short form is
 * `offset + len` for payloads up to 55 bytes, the long form is `offset + 55 + (bytes of length)`
 * followed by the length itself in minimal big-endian.
 */
function encodeLength(out: number[], length: number, offset: number): void {
  if (length <= 55) {
    out.push(offset + length);
    return;
  }
  const bytes: number[] = [];
  let value = length;
  while (value > 0) {
    bytes.push(value & 0xff);
    value = Math.floor(value / 256);
  }
  out.push(offset + 55 + bytes.length);
  for (let i = bytes.length - 1; i >= 0; i -= 1) {
    out.push(bytes[i]!);
  }
}

/**
 * The RLP encoding of one item. Canonical: a one-byte string below 0x80 is the byte itself, zero
 * encodes as the empty string, and every length is minimal.
 */
export function rlpEncode(item: RlpItem): Uint8Array {
  const out: number[] = [];
  encodeInto(item, out);
  return Uint8Array.from(out);
}

function encodeInto(item: RlpItem, out: number[]): void {
  if (item instanceof Uint8Array) {
    // Canonical form: a one-byte string below 0x80 is the byte itself — the rule the decoder
    // enforces and the encoder must honor, or their round trip is not the identity.
    if (item.length === 1 && item[0]! < 0x80) {
      out.push(item[0]!);
      return;
    }
    encodeLength(out, item.length, 0x80);
    out.push(...item);
    return;
  }
  const payload: number[] = [];
  for (const child of item) {
    encodeInto(child, payload);
  }
  encodeLength(out, payload.length, 0xc0);
  out.push(...payload);
}

/**
 * Decodes exactly one RLP item from `bytes`, rejecting trailing data.
 *
 * Strictness is the point: this parses raw transactions that arrived over a network, where a
 * tolerant decoder is a differential oracle at best and a memory-exhaustion primitive at worst.
 * Lengths must be minimal (a one-byte payload inside a long-form prefix is rejected) and the input
 * must be consumed exactly.
 *
 * @throws {AccountError} `MalformedRlp` for any non-canonical or truncated input.
 */
export function rlpDecode(bytes: Uint8Array): RlpItem {
  const { item, consumed } = decodeItem(bytes, 0);
  if (consumed !== bytes.length) {
    throw AccountError.malformedRlp('trailing bytes after a complete item');
  }
  return item;
}

/** Returns one decoded item and the offset just past it. */
function decodeItem(data: Uint8Array, offset: number): { item: RlpItem; consumed: number } {
  if (offset >= data.length) {
    throw AccountError.malformedRlp('input ends where an item was expected');
  }
  const first = data[offset]!;
  if (first < 0x80) {
    return { item: data.subarray(offset, offset + 1), consumed: offset + 1 };
  }
  let length: number;
  let start: number;
  if (first <= 0xb7) {
    length = first - 0x80;
    start = offset + 1;
  } else if (first <= 0xbf) {
    ({ length, start } = decodeLength(data, offset + 1, first - 0xb7));
  } else if (first <= 0xf7) {
    length = first - 0xc0;
    start = offset + 1;
  } else {
    ({ length, start } = decodeLength(data, offset + 1, first - 0xf7));
  }
  const end = start + length;
  if (end > data.length) {
    throw AccountError.malformedRlp('input ends inside an item');
  }
  if (first <= 0xbf) {
    // A single byte below 0x80 inside a string prefix was never written by a canonical encoder.
    if (length === 1 && data[start]! < 0x80) {
      throw AccountError.malformedRlp('single byte below 0x80 must encode as itself');
    }
    return { item: data.slice(start, end), consumed: end };
  }
  const items: RlpItem[] = [];
  let at = start;
  while (at < end) {
    const child = decodeItem(data, at);
    items.push(child.item);
    at = child.consumed;
  }
  return { item: items, consumed: end };
}

/** Reads a long-form length, refusing a leading zero byte and a short payload in long form. */
function decodeLength(
  data: Uint8Array,
  at: number,
  lengthOfLength: number,
): { length: number; start: number } {
  if (at + lengthOfLength > data.length) {
    throw AccountError.malformedRlp('input ends inside a length prefix');
  }
  const bytes = data.subarray(at, at + lengthOfLength);
  if (bytes[0]! === 0) {
    throw AccountError.malformedRlp('length has a leading zero byte');
  }
  let length = 0;
  for (const byte of bytes) {
    length = length * 256 + byte;
  }
  if (length <= 55) {
    throw AccountError.malformedRlp('length written in long form for a short-form payload');
  }
  return { length, start: at + lengthOfLength };
}

/**
 * The minimal big-endian byte encoding of an integer; zero is the empty string — the one integer
 * rule most hand-rolled encoders get wrong.
 */
export function rlpUint(value: bigint): Uint8Array {
  if (value === 0n) {
    return new Uint8Array(0);
  }
  if (value < 0n) {
    // RLP encodes magnitudes; a negative transaction field is a caller bug, not input data.
    throw AccountError.malformedRlp('negative integer');
  }
  const hex = value.toString(16);
  const padded = hex.length % 2 === 0 ? hex : `0${hex}`;
  const bytes = new Uint8Array(padded.length / 2);
  for (let i = 0; i < bytes.length; i += 1) {
    bytes[i] = Number.parseInt(padded.slice(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

/**
 * Reads an RLP byte string as the integer an encoder wrote with {@link rlpUint}: the empty string
 * is zero, a zero-leading multi-byte form and a lone `0x00` are non-minimal and refused.
 *
 * @throws {AccountError} `MalformedRlp` for a non-minimal integer encoding.
 */
export function rlpAsUint(item: RlpItem): bigint {
  if (!(item instanceof Uint8Array)) {
    throw AccountError.malformedRlp('integer is a list');
  }
  if (item.length === 0) {
    return 0n;
  }
  // 0 encodes as an empty string; 0x00 as its own byte is written only for the string "\x00",
  // never for the integer zero.
  if (item.length > 1 && item[0]! === 0) {
    throw AccountError.malformedRlp('integer has a non-minimal (zero-leading) encoding');
  }
  if (item.length === 1 && item[0]! === 0) {
    throw AccountError.malformedRlp('integer zero must encode as the empty string');
  }
  let value = 0n;
  for (const byte of item) {
    value = (value << 8n) | BigInt(byte);
  }
  return value;
}

// --- the transaction ----------------------------------------------------------

/**
 * An EIP-1559 transaction body: everything the user confirmed, exactly as it will be signed.
 *
 * `data` is empty for a native AVAX transfer — the whole token-transfer semantics of a native send
 * live in `value`, and a non-empty `data` makes the transaction a contract call, which the send UI
 * does not offer. The access list is always empty in this build and encoded as such; the field
 * exists in the encoding because the chain expects nine items.
 *
 * Integer fields beyond `Number`'s exact range — `value` in wei and the two fee ceilings — are
 * `bigint` by construction; a `number` at a call site is a silent-precision bug the type prevents.
 */
export class Eip1559Tx {
  readonly chainId: number;
  readonly nonce: number;
  readonly maxPriorityFeePerGas: bigint;
  readonly maxFeePerGas: bigint;
  readonly gasLimit: number;
  readonly to: Uint8Array;
  readonly value: bigint;
  readonly data: Uint8Array;

  constructor(fields: {
    /** The chain this transaction is bound to by signature (EIP-155 replay protection). */
    chainId: number;
    /** The account's next nonce, from `eth_getTransactionCount` — never guessed. */
    nonce: number;
    /** The priority fee ceiling, wei per gas. */
    maxPriorityFeePerGas: bigint;
    /** The total fee ceiling, wei per gas. */
    maxFeePerGas: bigint;
    /** The gas limit, from `eth_estimateGas` plus headroom. */
    gasLimit: number;
    /** The recipient, 20 bytes. */
    to: Uint8Array;
    /** The amount, wei. AVAX has 18 decimals. */
    value: bigint;
    /** Call data — empty for a native transfer. */
    data: Uint8Array;
  }) {
    this.chainId = fields.chainId;
    this.nonce = fields.nonce;
    this.maxPriorityFeePerGas = fields.maxPriorityFeePerGas;
    this.maxFeePerGas = fields.maxFeePerGas;
    this.gasLimit = fields.gasLimit;
    this.to = fields.to;
    this.value = fields.value;
    this.data = fields.data;
  }

  /** The nine signed fields as RLP items (the raw transaction reuses them and appends the signature). */
  #bodyItems(): RlpItem[] {
    return [
      rlpUint(BigInt(this.chainId)),
      rlpUint(BigInt(this.nonce)),
      rlpUint(this.maxPriorityFeePerGas),
      rlpUint(this.maxFeePerGas),
      rlpUint(BigInt(this.gasLimit)),
      this.to,
      rlpUint(this.value),
      this.data,
      [], // access list, always empty in this build
    ];
  }

  /** The nine signed fields as an RLP list, with the empty access list. */
  bodyRlp(): Uint8Array {
    return rlpEncode(this.#bodyItems());
  }

  /**
   * The hash signed: `Keccak-256(0x02 || RLP(fields))`. This — not the raw transaction, not the
   * receipt — is what the user's confirmation and the signature must agree on, which is why it is
   * a named method and not a private detail.
   */
  signingHash(): Uint8Array {
    return keccak256(concatBytes(Uint8Array.of(EIP1559_TYPE), this.bodyRlp()));
  }

  /**
   * Signs this transaction with `wallet`, returning the raw transaction ready for
   * `eth_sendRawTransaction`.
   *
   * The signature is recoverable-signature based so the y-parity comes from the signature itself,
   * and low-s normalization flips the parity with it — the two must move together or the recovery
   * lands on the wrong point.
   */
  sign(wallet: EvmWallet): SignedTx {
    const digest = this.signingHash();
    // 'recovered' format: [recovery id, r (32), s (32)], with an RFC 6979 nonce — deterministic
    // within this port, and valid regardless: any low-s signature is the same transaction to the
    // chain, which is why the vectors pin no signature bytes.
    const recovered = secp256k1.sign(digest, wallet.privateKeyBytes(), {
      prehash: false,
      format: 'recovered',
    });
    let parity = recovered[0]! & 1; // the x-overflow bit (recid 2/3) never occurs for r < n
    const r = recovered.slice(1, 33);
    let s: Uint8Array = recovered.slice(33, 65);

    // Low-s, re-stated rather than trusted to a library option: if s is above n/2 the EVM rejects
    // the signature as malleable, and replacing s with n - s mirrors the recovered point, so the
    // parity bit flips with it.
    const sValue = bytesToBigInt(s);
    if (sValue > SECP256K1_N / 2n) {
      s = bigIntTo32Bytes(SECP256K1_N - sValue);
      parity ^= 1;
    }

    // The serialized form is `0x02 || RLP([the nine fields, yParity, r, s])` — a *flat* list of
    // twelve items, unlike legacy transactions where the signature wraps an already-encoded body.
    const envelope = rlpEncode([...this.#bodyItems(), rlpUint(BigInt(parity)), r, s]);
    const raw = concatBytes(Uint8Array.of(EIP1559_TYPE), envelope);
    const txHash = keccak256(raw);
    return new SignedTx(raw, txHash);
  }
}

/**
 * A signed transaction: the raw bytes and the hash the chain will know it by. The hash is
 * `Keccak-256(raw)` — it depends on the signature, so it is computed once here rather than
 * re-derived by every consumer.
 */
export class SignedTx {
  readonly #raw: Uint8Array;
  readonly #txHash: Uint8Array;

  constructor(raw: Uint8Array, txHash: Uint8Array) {
    this.#raw = raw;
    this.#txHash = txHash;
  }

  /** The raw transaction, ready for `eth_sendRawTransaction`. */
  raw(): Uint8Array {
    return this.#raw.slice();
  }

  /** `Keccak-256(raw)` — the hash to track the transaction by. */
  txHash(): Uint8Array {
    return this.#txHash.slice();
  }

  /** The hash as lowercase hex with a `0x` prefix, the form every RPC method takes. */
  txHashHex(): string {
    return `0x${bytesToHex(this.#txHash)}`;
  }
}

/**
 * Recovers the sender's 20-byte address from a raw type-2 transaction: the mirror of
 * {@link Eip1559Tx.sign} and the proof the ports' signing paths are held to. It exercises every
 * layer a conformance port needs — strict RLP decode, body re-encode, digest, signature validity,
 * parity handling — without needing ECDSA to be deterministic anywhere (the Rust, TypeScript, and
 * Kotlin stacks each use their own nonces by design, and any valid low-s signature is the same
 * transaction to the chain).
 *
 * Strictness: the envelope must be the nine fields, `r` and `s` exactly 32 bytes, `s` at most n/2,
 * the parity bit 0 or 1, and the chain id non-zero (a type-2 transaction without replay protection
 * is not one).
 *
 * @throws {AccountError} `NotATransaction` for a non-type-2 envelope, `MalformedRlp` for structural
 * problems, and `BadSignature` when the signature is not a valid recovery over the body's own
 * digest.
 */
export function recoverSender(raw: Uint8Array): Uint8Array {
  // The type byte is outside the RLP envelope, and the envelope itself is a *flat* list of twelve
  // items: the nine body fields, then yParity, r, s — unlike a legacy transaction, which nests an
  // encoded body inside the signature wrapper.
  if (raw.length === 0 || raw[0]! !== EIP1559_TYPE) {
    throw AccountError.notATransaction();
  }
  let envelope: RlpItem;
  try {
    envelope = rlpDecode(raw.subarray(1));
  } catch (error) {
    // Structural problems inside a transaction envelope are the same refusal as a non-list: no
    // distinction is offered to a caller that did not build these bytes.
    if (error instanceof AccountError && error.kind === 'MalformedRlp') {
      throw AccountError.notATransaction();
    }
    throw error;
  }
  if (!Array.isArray(envelope) || envelope.length !== 12) {
    throw AccountError.notATransaction();
  }
  const body = envelope.slice(0, 9);

  // A type-2 envelope whose chain id is zero, oversized, or non-minimally encoded is not one a
  // canonical signer produced — the same refusal as a non-list, with no detail offered.
  let chainId: bigint;
  try {
    chainId = rlpAsUint(body[0]!);
  } catch (error) {
    if (error instanceof AccountError && error.kind === 'MalformedRlp') {
      throw AccountError.notATransaction();
    }
    throw error;
  }
  if (chainId === 0n || chainId > 0xffffffffffffffffn) {
    throw AccountError.notATransaction();
  }

  // Re-encode the body exactly as it arrived and hash it — recovery must run over the bytes the
  // signature was made over, byte for byte, so a non-canonical field encoding inside `raw` is
  // preserved rather than normalized away here (the decoder's minimality rules already refused the
  // ambiguous forms).
  const digest = keccak256(concatBytes(Uint8Array.of(EIP1559_TYPE), rlpEncode(body)));

  const parity = rlpAsUint(envelope[9]!);
  if (parity > 1) {
    throw AccountError.badSignature();
  }
  const rBytes = bodyString(envelope[10]!);
  const sBytes = bodyString(envelope[11]!);
  if (rBytes.length !== 32 || sBytes.length !== 32) {
    throw AccountError.badSignature();
  }
  if (bytesToBigInt(sBytes) > SECP256K1_N / 2n) {
    // High-s: not a signature any EVM chain accepts, and not one this module produces — treating
    // it as recoverable would certify bytes the chain will reject.
    throw AccountError.badSignature();
  }

  // recoverPublicKey takes the recovered format [recid, r, s] and returns the compressed point;
  // the address is Keccak-256 over the uncompressed coordinates.
  const compressed = secp256k1.recoverPublicKey(
    concatBytes(Uint8Array.of(Number(parity)), rBytes, sBytes),
    digest,
    { prehash: false },
  );
  const point = secp256k1.Point.fromHex(bytesToHex(compressed));
  const uncompressed = point.toBytes(false);
  const addressHash = keccak256(uncompressed.subarray(1));
  return addressHash.slice(12);
}

/** A body field that must be a byte string, not a list. */
function bodyString(item: RlpItem): Uint8Array {
  if (!(item instanceof Uint8Array)) {
    throw AccountError.notATransaction();
  }
  return item;
}

// --- EIP-712 ------------------------------------------------------------------

/**
 * Builds an EIP-712 `encodeType` string: the primary struct's declaration, followed by the
 * declaration of every struct type it references — transitively — sorted by name.
 *
 * The appendix is the part of EIP-712 every hand-rolled implementation gets wrong (the Rust
 * reference did, first): a struct that references other structs does not hash to
 * `keccak("Name(type member,…)")` alone; the referenced declarations ride along. The sort is by
 * struct name; because a declaration begins with its name followed by `(` (0x28, below every
 * character that can continue a name), sorting the declaration strings themselves is equivalent.
 *
 * Each referenced declaration must itself already carry *its* references — the caller closes the
 * transitive set; this function only sorts and appends what it is given.
 */
export function eip712EncodeType(primary: string, referenced: readonly string[]): string {
  return primary + [...referenced].sort().join('');
}

/**
 * `Keccak-256(encodeType)` for the primary struct being signed — the message half's counterpart of
 * {@link Eip712Domain.typeHash}, which does the same one computation for the domain.
 */
export function eip712TypeHash(primary: string, referenced: readonly string[]): Uint8Array {
  return keccak256(new TextEncoder().encode(eip712EncodeType(primary, referenced)));
}

/**
 * A typed value in the EIP-712 model this module hashes.
 *
 * Deliberately narrow: the types the account surface actually signs today, with 256-bit integers
 * carried as `bigint` (JavaScript's native form; the encoding is 32-byte big-endian). Structs
 * compose by hash: a struct field's contribution to its parent's encoding is the child's own
 * `hashStruct` output — computed by the caller with the child's type hash (built with
 * {@link eip712EncodeType} if the child itself references structs) and supplied here as a
 * `bytes32` value.
 */
export type Eip712Value =
  | { readonly type: 'address'; readonly value: Uint8Array }
  | { readonly type: 'bytes32'; readonly value: Uint8Array }
  | { readonly type: 'uint256'; readonly value: bigint }
  | { readonly type: 'string'; readonly value: string }
  | { readonly type: 'bytes'; readonly value: Uint8Array }
  | { readonly type: 'array'; readonly values: readonly Eip712Value[] };

/**
 * The 32-byte abi encoding of one typed value inside a hashStruct: fixed types are padded/hashed
 * per EIP-712's `encodeData`, dynamic types by hashing their contents.
 */
export function eip712EncodeValue(value: Eip712Value): Uint8Array {
  switch (value.type) {
    case 'address': {
      if (value.value.length !== ADDRESS_LEN) {
        throw AccountError.badLength('eip712 address', ADDRESS_LEN, value.value.length);
      }
      const out = new Uint8Array(32);
      out.set(value.value, 12); // left-padded to 32 bytes
      return out;
    }
    case 'bytes32':
      if (value.value.length !== 32) {
        throw AccountError.badLength('eip712 bytes32', 32, value.value.length);
      }
      return value.value.slice();
    case 'uint256': {
      if (value.value < 0n) {
        throw AccountError.badLength('eip712 uint256', 32, -1);
      }
      return bigIntTo32Bytes(value.value);
    }
    case 'string':
      return keccak256(new TextEncoder().encode(value.value));
    case 'bytes':
      return keccak256(value.value);
    case 'array': {
      let concatenated = new Uint8Array(0);
      for (const item of value.values) {
        concatenated = concatBytes(concatenated, eip712EncodeValue(item));
      }
      return keccak256(concatenated);
    }
  }
}

/**
 * The EIP-712 domain of a signing request. Field presence matters: the domain separator's type
 * hash is built from exactly the fields that are set, in the EIP's fixed order (name, version,
 * chainId, verifyingContract, salt), because a separator computed over different fields than the
 * dApp displayed is the primary EIP-712 phishing shape.
 */
export class Eip712Domain {
  readonly name: string | undefined;
  readonly version: string | undefined;
  readonly chainId: number | undefined;
  readonly verifyingContract: Uint8Array | undefined;
  readonly salt: Uint8Array | undefined;

  constructor(fields: {
    name?: string | undefined;
    version?: string | undefined;
    chainId?: number | undefined;
    verifyingContract?: Uint8Array | undefined;
    salt?: Uint8Array | undefined;
  }) {
    this.name = fields.name;
    this.version = fields.version;
    this.chainId = fields.chainId;
    this.verifyingContract = fields.verifyingContract;
    this.salt = fields.salt;
  }

  /** `Keccak-256("EIP712Domain(" + joined types + ")")` over exactly the present fields. */
  typeHash(): Uint8Array {
    const types: string[] = [];
    if (this.name !== undefined) {
      types.push('string name');
    }
    if (this.version !== undefined) {
      types.push('string version');
    }
    if (this.chainId !== undefined) {
      types.push('uint256 chainId');
    }
    if (this.verifyingContract !== undefined) {
      types.push('address verifyingContract');
    }
    if (this.salt !== undefined) {
      types.push('bytes32 salt');
    }
    return keccak256(new TextEncoder().encode(`EIP712Domain(${types.join(',')})`));
  }

  /** The domain separator: `Keccak-256(typeHash || encodeData(domain values))`. */
  separator(): Uint8Array {
    const parts: Uint8Array[] = [this.typeHash()];
    if (this.name !== undefined) {
      parts.push(eip712EncodeValue({ type: 'string', value: this.name }));
    }
    if (this.version !== undefined) {
      parts.push(eip712EncodeValue({ type: 'string', value: this.version }));
    }
    if (this.chainId !== undefined) {
      parts.push(eip712EncodeValue({ type: 'uint256', value: BigInt(this.chainId) }));
    }
    if (this.verifyingContract !== undefined) {
      parts.push(eip712EncodeValue({ type: 'address', value: this.verifyingContract }));
    }
    if (this.salt !== undefined) {
      parts.push(eip712EncodeValue({ type: 'bytes32', value: this.salt }));
    }
    return keccak256(concatBytes(...parts));
  }
}

/**
 * `hashStruct`: `Keccak-256(typeHash || encodeData(values))`, the message half of the EIP-712
 * digest.
 */
export function eip712HashStruct(typeHash: Uint8Array, values: readonly Eip712Value[]): Uint8Array {
  const parts: Uint8Array[] = [typeHash];
  for (const value of values) {
    parts.push(eip712EncodeValue(value));
  }
  return keccak256(concatBytes(...parts));
}

/**
 * The final digest a wallet signs: `Keccak-256(0x1901 || domainSeparator || hashStruct)`.
 */
export function eip712Digest(domainSeparator: Uint8Array, structHash: Uint8Array): Uint8Array {
  return keccak256(concatBytes(Uint8Array.of(0x19, 0x01), domainSeparator, structHash));
}

// --- address input ------------------------------------------------------------

/**
 * Parses an address string for the send flow: `0x` optional, exactly 40 hex characters.
 * All-lowercase and all-uppercase are accepted as unchecked; mixed case is accepted only when its
 * EIP-55 checksum matches — a typo in a checksummed recipient is the last line of defense before
 * funds move, and it must fail here rather than on the chain.
 *
 * @throws {AccountError} `BadAddress` for anything that is not 40 hex characters,
 * `AddressChecksumFailed` for a mixed-case string whose EIP-55 checksum does not match.
 */
export function parseAddress(text: string): Uint8Array {
  const stripped = text.startsWith('0x') ? text.slice(2) : text;
  if (stripped.length !== 40 || !/^[0-9a-fA-F]*$/.test(stripped)) {
    throw AccountError.badAddress();
  }
  const hasLower = /[a-z]/.test(stripped);
  const hasUpper = /[A-Z]/.test(stripped);
  if (hasLower && hasUpper) {
    const bytes = hexToBytes(stripped);
    if (eip55(bytes).slice(2) !== stripped) {
      throw AccountError.addressChecksumFailed();
    }
    return bytes;
  }
  return hexToBytes(stripped);
}

// --- small helpers ------------------------------------------------------------

/** Keccak-256, the one hash the EVM world agrees on. */
function keccak256(data: Uint8Array): Uint8Array {
  return keccak_256(data);
}

/** Hex decode for a known-even-length, known-hex string. */
function hexToBytes(text: string): Uint8Array {
  const out = new Uint8Array(text.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(text.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function bytesToBigInt(bytes: Uint8Array): bigint {
  let value = 0n;
  for (const byte of bytes) {
    value = (value << 8n) | BigInt(byte);
  }
  return value;
}

/** A `bigint` as exactly 32 big-endian bytes — the encodeData form of every 256-bit integer. */
function bigIntTo32Bytes(value: bigint): Uint8Array {
  const out = new Uint8Array(32);
  let at = 32;
  let rest = value;
  while (rest > 0n && at > 0) {
    at -= 1;
    out[at] = Number(rest & 0xffn);
    rest >>= 8n;
  }
  if (rest > 0n) {
    throw AccountError.badLength('eip712 uint256', 32, 33);
  }
  return out;
}
