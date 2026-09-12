package com.migo.core.crypto

/**
 * Sealing content that outlives the message layer — a port of `@migo/crypto`'s `sealing` module,
 * and it has to stay a port: an object sealed by this module is opened by the web client's
 * `sealing.open` and the desktop's media path, so the sealed layout (`nonce || ciphertext ||
 * tag`), the slot widths, and the one-refusal error discipline are cross-client contracts.
 *
 * A ratchet message encrypts for a *session*; media, voice notes, and call signaling encrypt for
 * an *object* — bytes that sit in object storage or ride a relay, opened possibly much later, by
 * a peer whose only credential is the key that arrives beside them. That is a different shape of
 * problem, and this object is its two halves:
 *
 *  * [seal] mints a fresh key and nonce, encrypts under the house AEAD, and hands back the key
 *    and nonce *as bytes* — because in this one context the key is meant to travel: it rides the
 *    message's own ciphertext (a `MediaRef` key slot) or an end-to-end control event, so the peer
 *    who can read the message can read the object, and nobody else can.
 *  * [open] is the inverse, taking the key and nonce exactly as they arrived in their wire slots.
 *
 * The associated data is the caller's domain separation — `migo-media` for an image or document,
 * `migo-voice` for a voice note, a call id for signaling. A blob sealed under one domain must not
 * open under another, and the AEAD is what enforces that.
 *
 * ## Why the key leaves as plain bytes here and nowhere else
 *
 * Everywhere else a [SymmetricKey] keeps its bytes private behind [SymmetricKey.expose]. Here the
 * bytes *are the product* — they go into a wire slot — so the return type is honest about it:
 * [ByteArray], documented as key material, with the working copy inside [seal] destroyed the
 * moment the travelling copy is made. An auditor grepping for `expose` finds this as the one
 * place raw key bytes are handed to a caller, on purpose.
 */
object Sealing {
    /**
     * Seals [plaintext] under a fresh random key, for storage or relay.
     *
     * The key and nonce are drawn here, not accepted from a caller, because the whole point of
     * per-object sealing is that no two objects ever share key material: a caller that could pass
     * a key in would eventually pass one it had already used.
     */
    fun seal(plaintext: ByteArray, associatedData: ByteArray): SealedContent {
        val key = SymmetricKey.generate()
        val sealed = Aead.seal(key, associatedData, plaintext)
        // The one place raw key bytes are handed out on purpose. The travelling copy is made
        // before the key's own buffer is destroyed: what leaves this function is the copy alone.
        val travelling = key.expose().copyOf()
        key.destroy()
        return SealedContent(travelling, sealed.copyOfRange(0, AEAD_NONCE_LEN), sealed)
    }

    /**
     * Opens a sealed object with the key and nonce as they arrived in the message's slots.
     *
     * The sealed blob embeds its own nonce (it is [Aead.seal]'s output), and the slot carries the
     * same 24 bytes; the two are compared rather than trusting either alone, so a message whose
     * slots were spliced from a different object than its bytes fails here as a decryption
     * failure — the same refusal for every cause, telling a wrong key from edited bytes apart is
     * a fact the caller must never learn.
     *
     * Every failure is one [CryptoErrorKind.DecryptionFailed] — wrong key, spliced slots, edited
     * bytes, wrong domain. Length failures are [CryptoErrorKind.BadLength], as everywhere: they
     * describe the shape of the input, which the sender can see, not the key.
     */
    fun open(
        key: ByteArray,
        nonce: ByteArray,
        associatedData: ByteArray,
        sealed: ByteArray,
    ): ByteArray {
        if (key.size != AEAD_KEY_LEN) {
            throw CryptoError.badLength("content key", AEAD_KEY_LEN, key.size)
        }
        if (nonce.size != AEAD_NONCE_LEN) {
            throw CryptoError.badLength("content nonce", AEAD_NONCE_LEN, nonce.size)
        }
        if (sealed.size < AEAD_NONCE_LEN) {
            throw CryptoError.badLength("sealed content", AEAD_NONCE_LEN, sealed.size)
        }
        for (i in 0 until AEAD_NONCE_LEN) {
            if (sealed[i] != nonce[i]) {
                throw CryptoError.decryptionFailed()
            }
        }
        return Aead.openWithNonce(
            SymmetricKey.fromBytes(key),
            sealed.copyOfRange(0, AEAD_NONCE_LEN),
            associatedData,
            sealed.copyOfRange(AEAD_NONCE_LEN, sealed.size),
        )
    }
}

/**
 * One sealed object: the bytes to store, and the two values that open them.
 *
 * [SealedContent.sealed] is exactly what [Aead.seal] produced — `nonce || ciphertext || tag` — so
 * it can be uploaded whole; [SealedContent.nonce] repeats the sealed blob's leading bytes because
 * the message content carries it in its own slot and a receiver should not have to slice it out.
 * [SealedContent.key] is 32 bytes and the nonce 24; both lengths are the wire slots' contract,
 * checked on [Sealing.open].
 */
class SealedContent internal constructor(
    /** The 32-byte content key, for the message's key slot. Key material — never log it. */
    val key: ByteArray,
    /** The 24-byte nonce, for the message's nonce slot. */
    val nonce: ByteArray,
    /** `nonce || ciphertext || tag`: the bytes to upload and store. */
    val sealed: ByteArray,
)
