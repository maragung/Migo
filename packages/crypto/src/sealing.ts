/**
 * Sealing content that outlives the message layer.
 *
 * A ratchet message encrypts for a *session*; media, voice notes, and call signaling encrypt for
 * an *object* — bytes that sit in object storage or ride a relay, opened possibly much later, by
 * a peer whose only credential is the key that arrives beside them. That is a different shape of
 * problem, and this module is its two halves:
 *
 * * {@link seal} mints a fresh key and nonce, encrypts under the house AEAD, and hands back the
 *   key and nonce *as bytes* — because in this one context the key is meant to travel: it rides
 *   the message's own ciphertext (a `MediaRefContent` key slot) or an end-to-end control event,
 *   so the peer who can read the message can read the object, and nobody else can.
 * * {@link open} is the inverse, taking the key and nonce exactly as they arrived in their wire
 *   slots.
 *
 * # Why XChaCha20-Poly1305 and not AES-GCM
 *
 * The brief's sketches name AES-256-GCM, and this module deliberately does not use it: GCM's
 * 12-byte nonce is a bookkeeping problem (random reuse is catastrophic, and a counter is state
 * that must never repeat across devices), while XChaCha's 24-byte nonce is random-per-object with
 * no collision risk worth computing. This is the same reasoning — and the same audited primitive,
 * via {@link aead} — the message layer already relies on, so media sealed here and messages sealed
 * by the ratchet share one cryptographic story instead of two. ADR-0003's audited-implementations
 * rule does the rest: WebCrypto's GCM would be the only unaudited-by-this-project primitive in
 * the client.
 *
 * # The associated data is the caller's domain separation
 *
 * `seal` and `open` take associated data so a caller can bind a ciphertext to its context —
 * `migo-media` for an image, `migo-voice` for a voice note, a call id for signaling. A blob
 * sealed under one domain must not open under another, and the AEAD is what enforces that. The
 * per-object random key already stops one object's bytes opening under another object's key, so
 * the domain label is the whole associated-data story; binding in the object id would buy
 * nothing the random key does not already give.
 *
 * # Why the key leaves as plain bytes here and nowhere else
 *
 * Everywhere else a {@link SymmetricKey} keeps its bytes private and greppable behind `expose`.
 * Here the bytes *are the product* — they go into a wire slot — so the return type is honest
 * about it: `Uint8Array`, documented as key material, with the working copy inside the function
 * destroyed the moment the travelling copy is made. An auditor grepping for `expose(` finds this
 * as the one place raw key bytes are handed to a caller, by design.
 */

import { SymmetricKey, seal as sealUnderKey, openWithNonce, KEY_LEN, NONCE_LEN } from './aead.js';
import { CryptoError } from './errors.js';

/**
 * One sealed object: the bytes to store, and the two values that open them.
 *
 * `sealed` is exactly what {@link aead.seal} produced — `nonce || ciphertext || tag` — so it can
 * be uploaded whole; `nonce` repeats the sealed blob's leading bytes because the message content
 * carries it in its own slot and a receiver should not have to slice it out. `key` is 32 bytes
 * and `nonce` is 24; both lengths are the wire slots' contract, checked on {@link open}.
 */
export interface SealedContent {
  /** The 32-byte content key, for the message's key slot. Key material — never log it. */
  readonly key: Uint8Array;
  /** The 24-byte nonce, for the message's nonce slot. */
  readonly nonce: Uint8Array;
  /** `nonce || ciphertext || tag`: the bytes to upload and store. */
  readonly sealed: Uint8Array;
}

/**
 * Seals `plaintext` under a fresh random key, for storage or relay.
 *
 * The key and nonce are generated here, not supplied, because the whole point of per-object
 * sealing is that no two objects ever share key material: a caller that could pass a key in
 * would eventually pass one it had already used. Determinism belongs in tests of *this* module,
 * which reach the underlying {@link aead.sealWithNonce} instead.
 */
export function seal(plaintext: Uint8Array, associatedData: Uint8Array): SealedContent {
  const key = SymmetricKey.generate();
  const sealed = sealUnderKey(key, associatedData, plaintext);
  // The one place raw key bytes are handed out on purpose. The copy is made first so the
  // SymmetricKey's own buffer can be destroyed: what travels is the travelling copy alone.
  const travelling = key.expose().slice();
  key.destroy();
  return { key: travelling, nonce: sealed.slice(0, NONCE_LEN), sealed };
}

/**
 * Opens a sealed object with the key and nonce as they arrived in the message's slots.
 *
 * The sealed blob embeds its own nonce (it is `aead.seal`'s output), and the slot carries the
 * same 24 bytes; the two are compared rather than trusting either alone, so a message whose
 * slots were spliced from a different object than its bytes fails here instead of decrypting
 * under a nonce the sender never used.
 *
 * Every failure is one `DecryptionFailed` — wrong key, spliced slots, edited bytes, wrong
 * domain — for the same padding-oracle reasoning as {@link aead.open}. Length failures are
 * `BadLength`, as everywhere: they describe the shape of the input, which the sender can see,
 * not the key.
 */
export function open(
  key: Uint8Array,
  nonce: Uint8Array,
  associatedData: Uint8Array,
  sealed: Uint8Array,
): Uint8Array {
  if (key.length !== KEY_LEN) {
    throw CryptoError.badLength('content key', KEY_LEN, key.length);
  }
  if (nonce.length !== NONCE_LEN) {
    throw CryptoError.badLength('content nonce', NONCE_LEN, nonce.length);
  }
  if (sealed.length < NONCE_LEN) {
    throw CryptoError.badLength('sealed content', NONCE_LEN, sealed.length);
  }
  const embedded = sealed.subarray(0, NONCE_LEN);
  for (let i = 0; i < NONCE_LEN; i += 1) {
    if (embedded[i] !== nonce[i]) {
      // Same refusal as a tag failure, for the same reason: telling the two apart says
      // something about the key to whoever is probing.
      throw CryptoError.decryptionFailed();
    }
  }
  return openWithNonce(
    SymmetricKey.fromBytes(key),
    nonce,
    associatedData,
    sealed.subarray(NONCE_LEN),
  );
}
