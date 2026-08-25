/**
 * HMAC-SHA256 for values the server authenticates to itself.
 *
 * This is a different job from the AEAD in {@link module:aead}. That one keeps a message
 * unreadable. This one keeps a value *unforgeable* while leaving it perfectly readable: a
 * session token, a resume cursor, a signed media URL, a pagination key. The server hands the
 * value to a client, the client hands it back, and the server needs to know it was not edited
 * on the way.
 *
 * The construction, the labels and the truncation floor are the same as
 * `server/crates/migo-crypto/src/mac.rs`, and `shared/protocol/vectors/crypto/mac.json` holds
 * both sides to it. This module exists on the client side mostly to *verify* — a web client
 * checks a resume cursor it was handed rather than minting one — but the code is symmetric
 * because a verifier that cannot also produce a tag is a verifier nobody can test.
 *
 * # Why not a JWT
 *
 * Migo issues opaque tokens with a MAC over a compact payload instead of JWTs, for three
 * reasons that all come from having watched JWTs go wrong:
 *
 * 1. **`alg` is attacker-controlled input.** A JWT tells the verifier which algorithm to use,
 *    which is how `alg: none` and RS256-verified-as-HS256 happened. Here the algorithm is a
 *    fact about the source file.
 * 2. **A JWT is a bag of claims that everyone extends.** Once the format is self-describing,
 *    unrelated data ends up inside it and rides along on every request. A fixed payload stays
 *    the size it was designed to be — which matters when the target is a phone on a metered
 *    3G plan.
 * 3. **Revocation was always going to be needed anyway.** The stateless-token argument
 *    dissolves the first time a user taps "log out my other devices", so Migo checks a session
 *    record it has to keep regardless. The MAC's job is only to make the lookup key
 *    untamperable.
 *
 * # Domain separation
 *
 * Every purpose gets its own label, and the label is mixed into the *key* rather than into the
 * message. Mixing it into the message would still be sound, but deriving a per-purpose subkey
 * means a bug that leaks one subkey does not hand over the others, and it makes rotating a
 * single purpose possible.
 */

import { hmac } from '@noble/hashes/hmac.js';
import { sha256 } from '@noble/hashes/sha2.js';
// `equalBytes` lives in the ciphers package rather than the hashes one. It is imported from
// there instead of being written here because a hand-rolled comparison is exactly the kind of
// primitive ADR-0003 forbids: the correct version has no early exit, and every incorrect
// version looks the same.
import { equalBytes } from '@noble/ciphers/utils.js';

import { CryptoError } from './errors.js';
import * as kdf from './kdf.js';
import type { Label } from './kdf.js';

/** Length of a full-width tag. */
export const TAG_LEN = 32;

/**
 * Minimum accepted truncated tag length, in bytes.
 *
 * 16 bytes is the same 128-bit forgery margin the AEAD tag carries. Anything shorter starts to
 * be brute-forceable by an attacker who can retry, and Migo has no purpose that needs to save
 * those bytes badly enough.
 */
export const MIN_TAG_LEN = 16;

/** Session tokens handed to clients. */
export const LABEL_SESSION_TOKEN = 'migo-session-token-v1';
/**
 * Refresh tokens, which are stored as a tag rather than as themselves.
 *
 * A separate label from {@link LABEL_SESSION_TOKEN} because the two keys protect different
 * things: one authenticates a value the client sends back, the other turns a value into a
 * database lookup key. Sharing a key would mean a stored refresh tag is also a valid
 * access-token signature over the same bytes, which is not exploitable today only because the
 * two formats differ — and "not exploitable today" is not a property worth relying on.
 */
export const LABEL_REFRESH_TOKEN = 'migo-refresh-token-v1';
/** Resume cursors, which a client echoes back after a reconnect. */
export const LABEL_RESUME_CURSOR = 'migo-resume-cursor-v1';
/** Signed media URLs. */
export const LABEL_MEDIA_URL = 'migo-media-url-v1';
/** Pagination cursors on list endpoints. */
export const LABEL_PAGINATION = 'migo-pagination-v1';
/** Single-use email and phone verification codes. */
export const LABEL_VERIFICATION = 'migo-verification-v1';
/** Webhook bodies delivered to bot owners. */
export const LABEL_WEBHOOK = 'migo-webhook-v1';

/** Bytes in a MAC subkey. */
const KEY_LEN = 32;

/** Bytes in the big-endian length prefix {@link MacKey.tagParts} writes before each part. */
const LENGTH_PREFIX_LEN = 8;

/**
 * A key for one purpose, derived from the deployment's root secret.
 *
 * Construct one per purpose at startup and keep it; the derivation is cheap but it is not free,
 * and doing it per request would show up in a flame graph long before the HMAC itself did.
 *
 * The key bytes are held in a `#private` field, so they are unreachable from outside this class
 * — not by property access, not by `Object.keys`, and not by a `JSON.stringify` that walks an
 * object graph a token happens to be attached to. {@link toJSON} and {@link toString} are
 * overridden as well, because the failure mode being defended against is not an attacker
 * reading the field: it is a developer logging the object that owns it.
 */
export class MacKey {
  readonly #key: Uint8Array;
  #destroyed = false;

  private constructor(key: Uint8Array) {
    this.#key = key;
  }

  /**
   * Derives the key for one purpose from the deployment root secret.
   *
   * `root` is the raw bytes of the configured signing secret. `migod` refuses to start in
   * production if that secret is empty or still the development default, so this function does
   * not have to guess whether it is safe.
   */
  static derive(root: Uint8Array, label: Label): MacKey {
    return new MacKey(kdf.derive(root, null, label, KEY_LEN));
  }

  /**
   * Wraps raw key bytes, for tests and for keys loaded from a KMS.
   *
   * Copies, so that a caller who clears its own buffer afterwards — which it should — does not
   * clear this key out from under the object. Rust takes a `[u8; 32]` by value and gets the
   * copy from the type system; here it has to be explicit.
   */
  static fromBytes(key: Uint8Array): MacKey {
    if (key.length !== KEY_LEN) {
      throw CryptoError.badLength('mac key', KEY_LEN, key.length);
    }
    return new MacKey(key.slice());
  }

  /** Full-width tag over `message`. */
  tag(message: Uint8Array): Uint8Array {
    return hmac(sha256, this.#live(), message);
  }

  /**
   * Tag over several parts, length-prefixed so the split is unambiguous.
   *
   * Concatenating parts directly would let `("ab", "c")` and `("a", "bc")` produce the same
   * tag. That is not hypothetical: it is how a token for user `1` device `23` becomes a valid
   * token for user `12` device `3`.
   *
   * The prefix is eight bytes big-endian, matching Rust's `(part.len() as u64).to_be_bytes()`.
   * A shorter prefix — or a varint — would have been fine on its own, but it has to be the same
   * on both sides, and "the same" is easier to keep true when there is nothing to decide.
   */
  tagParts(parts: readonly Uint8Array[]): Uint8Array {
    const mac = hmac.create(sha256, this.#live());
    const prefix = new Uint8Array(LENGTH_PREFIX_LEN);
    const view = new DataView(prefix.buffer);
    for (const part of parts) {
      view.setBigUint64(0, BigInt(part.length), false);
      mac.update(prefix);
      mac.update(part);
    }
    return mac.digest();
  }

  /**
   * Verifies a tag in constant time, throwing {@link CryptoError} when it does not match.
   *
   * Throwing rather than returning a boolean is deliberate. A `verify` that returns `false` is
   * one forgotten `if` away from accepting every forgery, and that missing `if` is invisible in
   * review; an unhandled throw is not. It also matches Rust's `Result`, which `#[must_use]`
   * makes equally impossible to ignore.
   *
   * A truncated tag is accepted as long as it is at least {@link MIN_TAG_LEN}, so a caller who
   * needs a shorter cursor can have one without reimplementing the comparison. A shorter tag
   * than that is refused rather than quietly weakened.
   */
  verify(message: Uint8Array, tag: Uint8Array): void {
    this.#verifyExpected(this.tag(message), tag);
  }

  /** Verifies a tag produced by {@link MacKey.tagParts}. */
  verifyParts(parts: readonly Uint8Array[], tag: Uint8Array): void {
    this.#verifyExpected(this.tagParts(parts), tag);
  }

  /**
   * Clears the key bytes and makes every later use throw.
   *
   * Best-effort, and the limitation is worth stating plainly rather than papering over: a
   * JavaScript runtime may have copied this buffer during a garbage-collection cycle, and
   * nothing here can reach those copies. What this method does buy is that the *live* buffer
   * stops holding a key once the session it belongs to ends, which shortens the window in which
   * a heap snapshot is worth taking.
   *
   * Use after destruction throws instead of tagging under an all-zero key. A zeroed key still
   * produces perfectly plausible tags, and every one of them would verify against any other
   * zeroed key in the deployment — a silent downgrade to no authentication at all.
   */
  destroy(): void {
    this.#key.fill(0);
    this.#destroyed = true;
  }

  /** `MacKey(***)`. Never the key. */
  toString(): string {
    return 'MacKey(***)';
  }

  /** `JSON.stringify` of anything holding a key must not produce the key. */
  toJSON(): string {
    return 'MacKey(***)';
  }

  /** `console.log` and `util.inspect` in Node. */
  [Symbol.for('nodejs.util.inspect.custom')](): string {
    return 'MacKey(***)';
  }

  #live(): Uint8Array {
    if (this.#destroyed) {
      throw new TypeError('MacKey has been destroyed');
    }
    return this.#key;
  }

  #verifyExpected(expected: Uint8Array, tag: Uint8Array): void {
    if (tag.length < MIN_TAG_LEN || tag.length > TAG_LEN) {
      throw CryptoError.badLength('mac tag', TAG_LEN, tag.length);
    }
    // Compare only as many bytes as the caller sent, but compare all of them. `equalBytes`
    // accumulates the difference and has no early exit, which is the whole point: a
    // byte-by-byte `===` here leaks the length of the correct prefix and turns forgery into 32
    // sequential guessing games of 256 tries each. Its one length check is reached with both
    // lengths already equal, so it cannot short-circuit on this path.
    if (!equalBytes(expected.subarray(0, tag.length), tag)) {
      throw CryptoError.badSignature();
    }
  }
}
