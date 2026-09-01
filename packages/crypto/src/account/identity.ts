/**
 * The ML-DSA-65 account identity and the per-device credential.
 *
 * # How a key is born (and how it is not)
 *
 * FIPS 204 defines key generation from a 32-byte seed (Algorithm 6). The identity domain seed goes
 * *into that algorithm* via `ml_dsa65.keygen(seed)` — it is never hashed into a "private key",
 * because ML-DSA has no such format and inventing one is exactly what the brief forbids (§182,
 * spec #3). The practical consequence: the public key is a pure function of the seed, so restoring
 * the root on any device reproduces the same identity, and the ports (Rust, TypeScript, Kotlin)
 * agree by construction rather than by convention.
 *
 * # Context strings
 *
 * Every signature carries an ML-DSA context, mixed into the message digest: a signature made over
 * a login challenge can never be replayed as a rotation approval, because the two purposes sign
 * under different context strings. Login signs under {@link CONTEXT_LOGIN}, rotation under
 * {@link CONTEXT_ROTATE}, and a device credential under {@link CONTEXT_LOGIN_DEVICE}. The contexts
 * are constants — 255 bytes is the FIPS 204 ceiling, and a caller-supplied context is a caller
 * that can pick the empty one.
 *
 * # Deterministic signing
 *
 * `@noble/post-quantum` signs with a fresh random `rnd` by default; the account signs with
 * `extraEntropy: false`, which sets `rnd` to 32 zero bytes — the FIPS 204 deterministic variant.
 * That is what lets the conformance vectors pin the signature bytes: the same seed, payload, and
 * context produce the same signature in every port, and against the Rust reference's
 * `sign_deterministic` byte for byte.
 *
 * # Device credentials are random, not derived
 *
 * {@link DeviceCredential} holds a seed from the platform CSPRNG, not from the root (ADR-0013).
 * The login challenge requires both the account identity signature *and* the device credential
 * signature, so a root secret that leaks from a backup alone cannot log in as a device that is
 * still registered — the thief has the account half of the ceremony and none of the device half.
 */

import { ml_dsa65 } from '@noble/post-quantum/ml-dsa.js';

import { randomBytes } from '../random.js';
import { AccountError } from './errors.js';
import { DOMAIN_IDENTITY, type MigoRoot } from './root.js';

/**
 * The algorithm name recorded beside every identity public key. A string, not an enum, because
 * algorithm agility (spec #55) means the *next* algorithm is data this schema already holds, not a
 * migration.
 */
export const IDENTITY_ALGORITHM = 'ML-DSA-65';
/** The key format version this build generates. A future format is version 2 beside version 1. */
export const KEY_VERSION_ONE = 1;
/** ML-DSA-65 public key length in bytes. */
export const PUBLIC_KEY_LEN = 1952;
/** ML-DSA-65 signature length in bytes. */
export const SIGNATURE_LEN = 3309;
/** Seed length for every ML-DSA parameter set. */
export const SEED_LEN = 32;

const ENCODER = new TextEncoder();

/**
 * The ML-DSA context for login challenge signatures.
 *
 * A `Uint8Array` rather than a `string` because it is passed straight to the signer's `context`
 * option; the bytes are `b"migo-auth-login-v1"`, character for character the Rust constant.
 */
export const CONTEXT_LOGIN = ENCODER.encode('migo-auth-login-v1');
/** The ML-DSA context for identity rotation approvals. */
export const CONTEXT_ROTATE = ENCODER.encode('migo-auth-rotate-v1');
/** The ML-DSA context for device-credential signatures in the login ceremony. */
export const CONTEXT_LOGIN_DEVICE = ENCODER.encode('migo-auth-device-v1');

/**
 * Signs `payload` under `context` with the ML-DSA-65 key generated from `seed`, deterministically.
 *
 * Shared by {@link IdentityKey} and {@link DeviceCredential}: both reconstruct the expanded key
 * from their seed on demand — `keygen(seed)` *is* FIPS 204 key generation, which is what a seed is
 * for — so the one secret each type owns is the whole secret, and there is no expanded-key struct
 * lingering after use.
 */
function signWithSeed(seed: Uint8Array, payload: Uint8Array, context: Uint8Array): Uint8Array {
  const { secretKey } = ml_dsa65.keygen(seed);
  // `extraEntropy: false` selects the deterministic variant (rnd = 32 zero bytes); the context is
  // folded into the formatted message before signing, exactly as FIPS 204 §5.2 prescribes.
  const signature = ml_dsa65.sign(payload, secretKey, { context, extraEntropy: false });
  secretKey.fill(0);
  return signature;
}

/** The encoded public key of the ML-DSA-65 key generated from `seed`. */
function publicKeyOfSeed(seed: Uint8Array): Uint8Array {
  return ml_dsa65.keygen(seed).publicKey;
}

/**
 * The account identity signing key: the ML-DSA-65 key the `MIGO/IDENTITY/V1` domain seed becomes.
 *
 * Holds the seed and only the seed, in a `#private` field. {@link toString} and Node's inspect
 * hook answer `IdentityKey(<ML-DSA-65>)` and never the seed.
 */
export class IdentityKey {
  readonly #seed: Uint8Array;

  private constructor(seed: Uint8Array) {
    this.#seed = seed;
  }

  /** Derives the identity key from a root secret. */
  static fromRoot(root: MigoRoot): IdentityKey {
    return new IdentityKey(root.domainSeed(DOMAIN_IDENTITY));
  }

  /**
   * Reconstructs the identity key from its 32-byte seed, copying it.
   *
   * @throws {AccountError} `BadLength` if the seed is not exactly 32 bytes.
   */
  static fromSeed(seed: Uint8Array): IdentityKey {
    if (seed.length !== SEED_LEN) {
      throw AccountError.badLength('identity seed', SEED_LEN, seed.length);
    }
    return new IdentityKey(seed.slice());
  }

  /** The seed, for sealing into a container. */
  exposeSeed(): Uint8Array {
    return this.#seed.slice();
  }

  /** The encoded public key (1952 bytes), the only form the server ever stores. */
  publicKey(): Uint8Array {
    return publicKeyOfSeed(this.#seed);
  }

  /**
   * Signs a challenge payload under the login context.
   *
   * The payload is the server's canonical challenge bytes, signed exactly as received — the client
   * never re-encodes a challenge, so two implementations cannot disagree about what was signed.
   */
  signLogin(payload: Uint8Array): Uint8Array {
    return signWithSeed(this.#seed, payload, CONTEXT_LOGIN);
  }

  /** Signs under the rotation context. */
  signRotate(payload: Uint8Array): Uint8Array {
    return signWithSeed(this.#seed, payload, CONTEXT_ROTATE);
  }

  /** `IdentityKey(<ML-DSA-65>)`. Never the seed. */
  toString(): string {
    return `IdentityKey(<${IDENTITY_ALGORITHM}>)`;
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return this.toString();
  }
}

/**
 * Verifies an identity signature against a public key.
 *
 * Throws rather than returning a boolean, so a caller cannot forget to check the result — an
 * ignored `false` is a forged signature accepted. The public key is the server's stored form (the
 * encoded verifying key); the `context` must be the one the signature was made under, which is why
 * callers pass a constant rather than reaching for the empty context.
 *
 * @throws {AccountError} `BadLength` if the public key or signature is not the encoded ML-DSA-65
 * length — a wrong length is a client that is wrong, not an input to trim. `BadSignature` if the
 * key or signature does not decode, or the signature does not verify: the cases are one refusal,
 * so a caller cannot use the difference as an oracle.
 */
export function verifyIdentity(
  publicKey: Uint8Array,
  payload: Uint8Array,
  context: Uint8Array,
  signature: Uint8Array,
): void {
  if (publicKey.length !== PUBLIC_KEY_LEN) {
    throw AccountError.badLength('identity public key', PUBLIC_KEY_LEN, publicKey.length);
  }
  if (signature.length !== SIGNATURE_LEN) {
    throw AccountError.badLength('identity signature', SIGNATURE_LEN, signature.length);
  }
  let ok: boolean;
  try {
    ok = ml_dsa65.verify(signature, payload, publicKey, { context });
  } catch {
    // A signature or key that does not decode surfaces as a thrown error from the library; to a
    // caller it is the same outcome as a well-formed signature that does not verify — the
    // message is rejected — so it collapses to the same refusal rather than leaking which it was.
    throw AccountError.badSignature();
  }
  if (!ok) {
    throw AccountError.badSignature();
  }
}

/**
 * A per-device signing credential, generated from a random seed on the device it belongs to.
 *
 * Same algorithm and wire forms as {@link IdentityKey}; what differs is the origin of the seed,
 * which is the whole point — see the module docs.
 */
export class DeviceCredential {
  readonly #seed: Uint8Array;

  private constructor(seed: Uint8Array) {
    this.#seed = seed;
  }

  /** Generates a fresh credential from the platform CSPRNG. */
  static generate(): DeviceCredential {
    return new DeviceCredential(randomBytes(SEED_LEN));
  }

  /**
   * Reconstructs a credential from its stored seed, copying it.
   *
   * @throws {AccountError} `BadLength` if the seed is not exactly 32 bytes.
   */
  static fromSeed(seed: Uint8Array): DeviceCredential {
    if (seed.length !== SEED_LEN) {
      throw AccountError.badLength('device credential seed', SEED_LEN, seed.length);
    }
    return new DeviceCredential(seed.slice());
  }

  /** The seed, for the device vault. */
  exposeSeed(): Uint8Array {
    return this.#seed.slice();
  }

  /** The encoded public key (1952 bytes) registered on the device row. */
  publicKey(): Uint8Array {
    return publicKeyOfSeed(this.#seed);
  }

  /**
   * Signs a login challenge under the device context. Login challenges are signed by both keys
   * (account and device), each under its own context, so one signature can never be stood in for
   * the other.
   */
  signLogin(payload: Uint8Array): Uint8Array {
    return signWithSeed(this.#seed, payload, CONTEXT_LOGIN_DEVICE);
  }

  /** `DeviceCredential(<ML-DSA-65>)`. Never the seed. */
  toString(): string {
    return `DeviceCredential(<${IDENTITY_ALGORITHM}>)`;
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return this.toString();
  }
}
