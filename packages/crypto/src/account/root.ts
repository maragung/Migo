/**
 * The Migo root secret and its domains.
 *
 * The root is 32 bytes from the platform CSPRNG, generated on the device, and it is the only
 * secret a user who loses every device actually needs to have backed up — everything else in the
 * account is a function of it (except per-device credentials, which are deliberately random so
 * that a leaked root alone cannot impersonate a registered device; see
 * {@link module:account/identity}'s `DeviceCredential`).
 *
 * # Domain separation, mechanically
 *
 * Each domain is one HKDF-SHA-256 expansion of the root under its own label. The labels are
 * constants here — not strings at call sites — so the full set is greppable in one place and a new
 * domain is a code review, not a typo. They are versioned (`/V1`) because a derivation that ever
 * needs to change must become `V2` beside the old one, never a silent change under the same name:
 * the day a label changes meaning is the day every existing account's derived keys change too.
 * This is the same set, byte for byte, as `server/crates/migo-account/src/root.rs`.
 *
 * ## Why the labels are strings here and byte slices in Rust
 *
 * The same reason {@link module:kdf} gives: a `string` constant is genuinely immutable, a
 * `Uint8Array` constant is not, and {@link MigoRoot.domainSeed} encodes the label as UTF-8 at the
 * call site. The domain labels are ASCII, so the UTF-8 encoding is byte-identical to the Rust
 * `b"..."` literal.
 */

import { equalBytes } from '@noble/ciphers/utils.js';

import * as kdf from '../kdf.js';
import { randomBytes } from '../random.js';
import { AccountError } from './errors.js';

/** Root secret length in bytes. */
export const ROOT_LEN = 32;

/** The identity domain: login and account authentication (ML-DSA-65). */
export const DOMAIN_IDENTITY = 'MIGO/IDENTITY/V1';
/** The EVM wallet domain: BIP-32 master seed, BIP-44 coin type 60. */
export const DOMAIN_EVM = 'MIGO/EVM/V1';
/** The E2EE domain: the founding device's X3DH identity seeds. */
export const DOMAIN_E2EE = 'MIGO/E2EE/V1';
/** The backup domain: the `.migo` container's key schedule. */
export const DOMAIN_BACKUP = 'MIGO/BACKUP/V1';
/**
 * The device domain label, documented for completeness: device credentials are NOT derived from
 * the root (ADR-0013) — this label exists so the conformance vectors can pin the fact that
 * deriving it is never required, and so a future per-device derivation, if one is ever justified,
 * has an already-reserved name that does not collide with the four live domains.
 */
export const DOMAIN_DEVICE = 'MIGO/DEVICE/V1';

/**
 * Sub-label under `MIGO/E2EE/V1` for the founding device's Ed25519 signing seed. The E2EE domain
 * seed is not used raw: the existing identity format is two independent seeds (signing and
 * exchange), so the domain seed is expanded once more per key, and the E2EE stack above it is
 * untouched.
 */
export const LABEL_E2EE_SIGNING = 'migo-e2ee-signing-v1';
/** Sub-label under `MIGO/E2EE/V1` for the founding device's X25519 exchange seed. */
export const LABEL_E2EE_EXCHANGE = 'migo-e2ee-exchange-v1';

/**
 * The account root secret.
 *
 * Generated with {@link MigoRoot.generate}, restored from a container (via
 * {@link module:account/container}) or from raw bytes with {@link MigoRoot.fromBytes}. The bytes
 * live in a `#private` field and no path in this module renders them: {@link toString} and Node's
 * inspect hook answer `MigoRoot(<32 bytes>)`, for the same reason `SymmetricKey` hides its bytes —
 * a secret that can be printed is eventually printed into a log.
 */
export class MigoRoot {
  readonly #bytes: Uint8Array;

  private constructor(bytes: Uint8Array) {
    this.#bytes = bytes;
  }

  /** Generates a fresh root from the platform CSPRNG. */
  static generate(): MigoRoot {
    return new MigoRoot(randomBytes(ROOT_LEN));
  }

  /**
   * Wraps existing root bytes, copying them — e.g. after opening a container.
   *
   * The copy matters for the same reason as in {@link SymmetricKey.fromBytes}: the caller's buffer
   * and this root must not be the same memory, or clearing one silently clears the other.
   *
   * @throws {AccountError} `BadLength` if the slice is not exactly 32 bytes — a length that is
   * wrong here is a container or a port that is wrong, not an input to round.
   */
  static fromBytes(bytes: Uint8Array): MigoRoot {
    if (bytes.length !== ROOT_LEN) {
      throw AccountError.badLength('root secret', ROOT_LEN, bytes.length);
    }
    return new MigoRoot(bytes.slice());
  }

  /**
   * Borrows the raw bytes, for sealing into a container. The greppable audit point for root
   * material leaving this type, and the same name as Rust's `as_bytes` so `rg 'as_bytes\|asBytes'`
   * finds every site in both languages at once.
   *
   * Returns the live buffer, not a copy — a copy would scatter unzeroized duplicates the way
   * Rust's deliberate absence of `to_vec` prevents. Callers must not write into it.
   */
  asBytes(): Uint8Array {
    return this.#bytes;
  }

  /** Derives the 32-byte seed of one domain. */
  domainSeed(label: kdf.Label): Uint8Array {
    return kdf.derive(this.#bytes, null, label, 32);
  }

  /** Constant-time value equality, for tests and for the container round-trip. */
  equals(other: MigoRoot): boolean {
    return equalBytes(this.#bytes, other.#bytes);
  }

  /** `MigoRoot(<32 bytes>)`. Never the bytes. */
  toString(): string {
    return `MigoRoot(<${ROOT_LEN} bytes>)`;
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return this.toString();
  }
}

/**
 * The founding device's E2EE identity seeds, derived from the E2EE domain.
 *
 * Returns `{ signing, exchange }`: the two 32-byte seeds the existing X3DH identity format is built
 * from (Ed25519 signing, X25519 exchange). The E2EE protocol above them — X3DH, the Double
 * Ratchet, the 64-byte wire form — is unchanged by the account root; only the *origin* of the
 * founding device's seeds is, which is what makes the account's E2EE history recoverable from a
 * container while additional devices keep generating fresh keys and therefore never inherit
 * historical plaintext.
 */
export function foundingDeviceE2eeSeeds(root: MigoRoot): {
  signing: Uint8Array;
  exchange: Uint8Array;
} {
  const domain = root.domainSeed(DOMAIN_E2EE);
  return {
    signing: kdf.derive(domain, null, LABEL_E2EE_SIGNING, 32),
    exchange: kdf.derive(domain, null, LABEL_E2EE_EXCHANGE, 32),
  };
}
