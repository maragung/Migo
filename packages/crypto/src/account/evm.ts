/**
 * The EVM wallet domain: BIP-32 over the domain seed, BIP-44 coin type 60.
 *
 * # Standards, not a Migo format
 *
 * The wallet is the one domain where an established hierarchical standard exists, so this module
 * implements that standard rather than a Migo-shaped cousin of it (spec #4): the `MIGO/EVM/V1`
 * domain seed becomes a BIP-32 master seed, accounts are the BIP-44 path `m/44'/60'/0'/0/i`, the
 * curve is secp256k1, and the address is the last 20 bytes of Keccak-256 over the 64-byte
 * uncompressed public key, checksummed per EIP-55 for display. A wallet recovered from a container
 * is therefore not just "the same address Migo shows" — it is the address any standards-conformant
 * Ethereum tool derives from the same seed.
 *
 * The private key never leaves the device. The server receives the address and metadata; that is
 * all it is ever offered, and all it can do with an address is display it — this release has no
 * RPC, no balances, no broadcasting, and the UI must not imply otherwise (§182).
 *
 * # What is delegated
 *
 * BIP-32's CKDpriv is delegated to `@scure/bip32` (`HDKey`), which is the canonical audited
 * implementation and is pinned by the official BIP-32 test vectors; secp256k1 to `@noble/curves`;
 * and Keccak-256 to `@noble/hashes` — Ethereum's pre-standard padding (`keccak_256`), *not*
 * SHA3-256, because SHA3 here would produce plausible-looking addresses no chain agrees with. The
 * independent Python generator behind the conformance vectors reproduces every address, so a
 * divergence in any of these fails a byte comparison, not just an internal cross-check.
 */

import { bytesToHex } from '@noble/ciphers/utils.js';
import { secp256k1 } from '@noble/curves/secp256k1.js';
import { keccak_256 } from '@noble/hashes/sha3.js';
import { HDKey } from '@scure/bip32';

import { AccountError } from './errors.js';
import { DOMAIN_EVM, type MigoRoot } from './root.js';

/** BIP-44 coin type for Ethereum and the EVM family. */
export const EIP155_COIN_TYPE = 60;
/**
 * The account path this build derives, minus the trailing index: the code appends `/{index}`, and
 * a BIP-44 tool prints the full `m/44'/60'/0'/0/i` for the result.
 */
export const EVM_BIP44_PATH = "m/44'/60'/0'/0";

/** The 20-byte Ethereum address length. */
const ADDRESS_LEN = 20;
/**
 * The largest non-hardened child index. BIP-32 reserves indices at or above 2^31 for hardened
 * derivation, and a wallet account index is always a non-hardened child, so an index in that range
 * is refused as {@link AccountError.invalidDerivation} rather than silently deriving a
 * different-semantics (hardened) key. The Rust reference takes a `u32` and would treat the high
 * bit as the hardened flag; this port narrows the accepted range to the values a wallet index can
 * actually take, which is the safer direction and covers every value the app or the vectors use.
 */
const MAX_INDEX = 0x7fff_ffff;

const ENCODER = new TextEncoder();

/**
 * A derived EVM wallet: one BIP-44 account of the root's EVM domain.
 *
 * The private key lives in a `#private` field; the address is computed once at construction and
 * stored, because it is the public identity of the wallet and callers display it far more often
 * than they derive it. {@link toString} renders the checksummed address so a log line is useful
 * without being dangerous, and never the key.
 */
export class EvmWallet {
  readonly #privateKey: Uint8Array;
  readonly #chainCode: Uint8Array;
  readonly #address: Uint8Array;

  private constructor(privateKey: Uint8Array, chainCode: Uint8Array, address: Uint8Array) {
    this.#privateKey = privateKey;
    this.#chainCode = chainCode;
    this.#address = address;
  }

  /**
   * Derives wallet `index` of the root's EVM domain.
   *
   * @throws {AccountError} `InvalidDerivation` for an index outside `0..=0x7fffffff`, or in the
   * BIP-32-assigned probability-2^-127 case of an invalid intermediate scalar.
   */
  static fromRoot(root: MigoRoot, index: number): EvmWallet {
    return EvmWallet.derive(root.domainSeed(DOMAIN_EVM), index);
  }

  /**
   * Derives wallet `index` from an explicit EVM domain seed — the form the conformance vectors and
   * a container restore use. The domain seed becomes the BIP-32 master seed
   * (`I = HMAC-SHA512("Bitcoin seed", seed)`), and the path `m/44'/60'/0'/0/index` is walked level
   * by level exactly as BIP-44 prescribes for coin type 60.
   *
   * @throws {AccountError} As {@link EvmWallet.fromRoot}.
   */
  static derive(domainSeed: Uint8Array, index: number): EvmWallet {
    if (!Number.isInteger(index) || index < 0 || index > MAX_INDEX) {
      throw AccountError.invalidDerivation();
    }
    let privateKey: Uint8Array | null;
    let chainCode: Uint8Array | null;
    try {
      const node = HDKey.fromMasterSeed(domainSeed).derive(`${EVM_BIP44_PATH}/${index}`);
      privateKey = node.privateKey;
      chainCode = node.chainCode;
    } catch {
      // Any failure in the walk — an out-of-range seed length, or the vanishingly rare invalid
      // scalar BIP-32 assigns probability 2^-127 — is one refusal: this seed is not one this
      // construction should be applied to.
      throw AccountError.invalidDerivation();
    }
    if (
      privateKey === null ||
      privateKey.length !== 32 ||
      chainCode === null ||
      chainCode.length !== 32
    ) {
      throw AccountError.invalidDerivation();
    }
    return new EvmWallet(privateKey.slice(), chainCode.slice(), addressOf(privateKey));
  }

  /** The 20-byte address. */
  address(): Uint8Array {
    return this.#address.slice();
  }

  /**
   * The EIP-55 checksummed address, the only form that should ever be shown to a user — a mistyped
   * checksummed address is rejected by every tool that receives it, which is exactly the property
   * display wants.
   */
  addressChecksummed(): string {
    return eip55(this.#address);
  }

  /** The BIP-32 chain code after the full path, for container metadata. */
  chainCode(): Uint8Array {
    return this.#chainCode.slice();
  }

  /**
   * The private key bytes, for signing inside the device's secure environment. The only accessor
   * that exposes secret material, and it exists because whatever consumes this wallet next —
   * transaction signing, EIP-712 — is a local operation by definition.
   */
  privateKeyBytes(): Uint8Array {
    return this.#privateKey.slice();
  }

  /** `EvmWallet(0x…)` with the checksummed address. Never the key. */
  toString(): string {
    return `EvmWallet(${eip55(this.#address)})`;
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return this.toString();
  }
}

/**
 * The 20-byte Ethereum address of a secret key: the last 20 bytes of Keccak-256 over the 64-byte
 * public key (X || Y, without the `0x04` prefix — including it is the classic way to derive a
 * valid-looking wrong address).
 */
function addressOf(privateKey: Uint8Array): Uint8Array {
  const uncompressed = secp256k1.getPublicKey(privateKey, false);
  const digest = keccak_256(uncompressed.subarray(1));
  return digest.slice(12);
}

/**
 * Renders a 20-byte address in EIP-55 form: lowercase hex, then each letter uppercased where the
 * corresponding nibble of Keccak-256 of that lowercase hex string is ≥ 8.
 *
 * @throws {AccountError} `BadLength` if the address is not 20 bytes.
 */
export function eip55(address: Uint8Array): string {
  if (address.length !== ADDRESS_LEN) {
    throw AccountError.badLength('evm address', ADDRESS_LEN, address.length);
  }
  const lowercase = bytesToHex(address);
  const digest = keccak_256(ENCODER.encode(lowercase));

  let out = '0x';
  for (let i = 0; i < lowercase.length; i += 1) {
    const ch = lowercase.charAt(i);
    // EIP-55 indexes the digest by the hex character position, which for a 40-character string is
    // the nibble index: the high nibble on even positions, the low nibble on odd.
    const digestByte = digest[i >> 1] ?? 0;
    const nibble = i % 2 === 0 ? digestByte >> 4 : digestByte & 0x0f;
    // Digits are never cased; letters follow the digest nibble.
    if ((ch >= '0' && ch <= '9') || nibble < 8) {
      out += ch;
    } else {
      out += ch.toUpperCase();
    }
  }
  return out;
}
