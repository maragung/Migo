/**
 * Authenticated encryption.
 *
 * XChaCha20-Poly1305, with a 24-byte nonce. The extended nonce is the reason for the choice: at
 * 24 bytes a randomly generated nonce has no meaningful collision risk, so nonces can be random
 * per message and no component has to maintain a counter that must never repeat. AES-GCM's
 * 12-byte nonce would force exactly that bookkeeping, and nonce reuse under GCM is catastrophic
 * — it leaks the authentication key, not just one message.
 *
 * ChaCha20 is also the right shape for the target device: a software stream cipher, fast and
 * constant-time without hardware AES, which many of the cheap Android phones Migo targets do
 * not have.
 *
 * Associated data is always supplied and always authenticated. For a ratchet message it is the
 * header; for a stored blob it is the record identity. This is what stops a valid ciphertext
 * from being replayed into a different context.
 *
 * ## Why not WebCrypto
 *
 * `crypto.subtle` has no ChaCha20 at all — it offers AES-GCM, whose 12-byte nonce is the
 * bookkeeping problem above — and every one of its operations is a `Promise`. The ratchet runs
 * inside a synchronous decode path, so an async AEAD would force the entire message pipeline to
 * be async for no security benefit. `@noble/ciphers` is audited, has no dependencies, and is
 * synchronous, which is why ADR-0003 names it. WebCrypto is still used for one thing: the
 * random bytes, in {@link SymmetricKey.generate} and {@link seal}, because a CSPRNG is exactly
 * the primitive that should come from the platform.
 */

import { xchacha20poly1305 } from '@noble/ciphers/chacha.js';

import { CryptoError } from './errors.js';

/** Symmetric key length in bytes. */
export const KEY_LEN = 32;
/** Nonce length in bytes. */
export const NONCE_LEN = 24;
/** Poly1305 authentication tag length in bytes. */
export const TAG_LEN = 16;

/**
 * Fills `out` with cryptographically secure random bytes.
 *
 * Unlike the Rust side, the generator is not an injectable parameter. There, `seal` takes
 * `&mut dyn Random` so that a caller cannot reach for the seeded generator used in
 * deterministic simulation; in JavaScript the equivalent hazard is a test helper that returns
 * `Math.random()` bytes and a reviewer who does not notice. Making the source unreachable and
 * routing determinism through {@link sealWithNonce} instead removes the parameter that could
 * carry the mistake.
 */
function randomBytes(out: Uint8Array): void {
  const source = globalThis.crypto;
  if (typeof source?.getRandomValues !== 'function') {
    // Refusing is the only safe answer. A fallback to a non-cryptographic generator would
    // produce keys that look random in every test and are predictable in production.
    throw new TypeError('no cryptographic random source: crypto.getRandomValues is unavailable');
  }
  source.getRandomValues(out);
}

/**
 * A 256-bit symmetric key.
 *
 * There is no `Display` and no useful default inspection: a key that can be printed eventually
 * is printed, into a log that is retained for ninety days. The bytes live in a `#private` field
 * and {@link toString}, {@link toJSON} and Node's inspect hook all answer `SymmetricKey(***)`.
 */
export class SymmetricKey {
  readonly #bytes: Uint8Array;
  #destroyed = false;

  private constructor(bytes: Uint8Array) {
    this.#bytes = bytes;
  }

  /**
   * Wraps existing key bytes, copying them.
   *
   * Rust has both `from_bytes([u8; KEY_LEN])` and `parse(&[u8])` because its type system can
   * express the fixed width; TypeScript cannot, so the two collapse into one function that
   * checks the length at runtime. The copy matters for the same reason as in
   * {@link MacKey.fromBytes}: the caller's buffer and this key must not be the same memory, or
   * clearing one silently clears the other.
   */
  static fromBytes(bytes: Uint8Array): SymmetricKey {
    if (bytes.length !== KEY_LEN) {
      throw CryptoError.badLength('symmetric key', KEY_LEN, bytes.length);
    }
    return new SymmetricKey(bytes.slice());
  }

  /** Generates a fresh key from the platform CSPRNG. */
  static generate(): SymmetricKey {
    const bytes = new Uint8Array(KEY_LEN);
    randomBytes(bytes);
    return new SymmetricKey(bytes);
  }

  /**
   * Borrows the raw bytes. The greppable audit point for key material leaving this type.
   *
   * Returns the live buffer, not a copy — a copy would defeat {@link destroy} by scattering
   * duplicates, and would hide from an auditor how many of them exist. Callers must not write
   * into it. The same name as Rust's `expose`, so that `rg 'expose\('` finds every site in both
   * languages at once.
   */
  expose(): Uint8Array {
    if (this.#destroyed) {
      throw new TypeError('SymmetricKey has been destroyed');
    }
    return this.#bytes;
  }

  /**
   * Clears the key bytes and makes every later use throw.
   *
   * Best-effort in the same way {@link MacKey.destroy} is: a garbage collector may have moved
   * this buffer and left a copy behind, and nothing in the language can reach it. What is
   * guaranteed is that the live buffer stops holding a key when the session ends.
   */
  destroy(): void {
    this.#bytes.fill(0);
    this.#destroyed = true;
  }

  /** `SymmetricKey(***)`. Never the key. */
  toString(): string {
    return 'SymmetricKey(***)';
  }

  /** `JSON.stringify` of anything holding a key must not produce the key. */
  toJSON(): string {
    return 'SymmetricKey(***)';
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return 'SymmetricKey(***)';
  }
}

/**
 * Encrypts `plaintext`, returning `nonce || ciphertext || tag`.
 *
 * The nonce is prefixed rather than tracked separately because every caller needs it and a
 * caller that has to remember to store it separately eventually will not.
 */
export function seal(
  key: SymmetricKey,
  associatedData: Uint8Array,
  plaintext: Uint8Array,
): Uint8Array {
  const nonce = new Uint8Array(NONCE_LEN);
  randomBytes(nonce);
  return sealWithNonce(key, nonce, associatedData, plaintext);
}

/**
 * Encrypts with a caller-supplied nonce.
 *
 * Exists for test vectors and for the ratchet, which derives its nonce from the message key so
 * that both sides agree without transmitting it. Application code should call {@link seal} and
 * let the nonce be random.
 */
export function sealWithNonce(
  key: SymmetricKey,
  nonce: Uint8Array,
  associatedData: Uint8Array,
  plaintext: Uint8Array,
): Uint8Array {
  requireNonce(nonce);
  const cipher = xchacha20poly1305(key.expose(), nonce, associatedData);
  const body = cipher.encrypt(plaintext);
  const out = new Uint8Array(NONCE_LEN + body.length);
  out.set(nonce, 0);
  out.set(body, NONCE_LEN);
  return out;
}

/** Decrypts `nonce || ciphertext || tag`. */
export function open(
  key: SymmetricKey,
  associatedData: Uint8Array,
  sealed: Uint8Array,
): Uint8Array {
  if (sealed.length < NONCE_LEN + TAG_LEN) {
    // Too short to contain a nonce and a tag, so there is nothing to authenticate. Reported as
    // a length problem rather than a decryption failure because it says something about the
    // shape of the input, which the sender can already see, and not about the key.
    throw CryptoError.badLength('sealed message', NONCE_LEN + TAG_LEN, sealed.length);
  }
  return openWithNonce(
    key,
    sealed.subarray(0, NONCE_LEN),
    associatedData,
    sealed.subarray(NONCE_LEN),
  );
}

/** Decrypts with an explicit nonce, for ciphertext stored without its nonce. */
export function openWithNonce(
  key: SymmetricKey,
  nonce: Uint8Array,
  associatedData: Uint8Array,
  body: Uint8Array,
): Uint8Array {
  requireNonce(nonce);
  if (body.length < TAG_LEN) {
    throw CryptoError.badLength('sealed body', TAG_LEN, body.length);
  }
  const cipher = xchacha20poly1305(key.expose(), nonce, associatedData);
  try {
    return cipher.decrypt(body);
  } catch {
    // Every cause collapses to one error: wrong key, wrong nonce, edited ciphertext, edited
    // associated data. Distinguishing them is exactly the information an attacker wants — that
    // is the shape of every padding oracle — and the receiver's action is the same in all four
    // cases. The library's own message is discarded rather than wrapped, because it is written
    // for a developer debugging a call, and this call's input is a hostile peer's bytes.
    throw CryptoError.decryptionFailed();
  }
}

function requireNonce(nonce: Uint8Array): void {
  if (nonce.length !== NONCE_LEN) {
    throw CryptoError.badLength('nonce', NONCE_LEN, nonce.length);
  }
}
