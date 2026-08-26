package com.migo.core.crypto

/** 32-byte XChaCha20-Poly1305 key. */
const val AEAD_KEY_LEN = 32

/** 24-byte XChaCha20 nonce. */
const val AEAD_NONCE_LEN = 24

/** 16-byte Poly1305 tag. */
const val AEAD_TAG_LEN = 16

/**
 * A symmetric AEAD key.
 *
 * Holds its bytes in a mutable array so [destroy] can zero them; the class is otherwise a thin
 * wrapper whose job is to make it impossible to pass a 31-byte key or to log the key by accident
 * ([toString] never renders the bytes). [fromBytes] copies its input so the caller's array is not
 * captured by reference.
 */
class SymmetricKey private constructor(private val bytes: ByteArray) {
    private var destroyed = false

    companion object {
        /** Wraps an existing 32-byte key, copying it. */
        fun fromBytes(bytes: ByteArray): SymmetricKey {
            if (bytes.size != AEAD_KEY_LEN) {
                throw CryptoError.badLength("symmetric key", AEAD_KEY_LEN, bytes.size)
            }
            return SymmetricKey(bytes.copyOf())
        }

        /** Draws a fresh key from the CSPRNG. */
        fun generate(): SymmetricKey = SymmetricKey(Csprng.bytes(AEAD_KEY_LEN))
    }

    /** The raw key bytes, for handing to libsodium. Never log the result. */
    fun expose(): ByteArray {
        check(!destroyed) { "SymmetricKey has been destroyed" }
        return bytes
    }

    /** Zeroes the key. Any later [expose] throws. */
    fun destroy() {
        bytes.fill(0)
        destroyed = true
    }

    override fun toString(): String = "SymmetricKey(***)"
}

/**
 * XChaCha20-Poly1305 authenticated encryption.
 *
 * The sealed form is `nonce || ciphertext || tag`, matching the Rust crate and `@migo/crypto`
 * byte-for-byte so the aead vectors verify all three against one another. The 24-byte XChaCha nonce
 * is large enough to be drawn at random per message without a birthday-bound worry, so [seal] does
 * exactly that; [sealWithNonce] exists for the ratchet and sender-key layers, which derive the
 * nonce from the message key and must never transmit it.
 */
object Aead {
    /** Seals [plaintext] under a fresh random nonce. Returns `nonce || ciphertext || tag`. */
    fun seal(key: SymmetricKey, associatedData: ByteArray, plaintext: ByteArray): ByteArray =
        sealWithNonce(key, Csprng.bytes(AEAD_NONCE_LEN), associatedData, plaintext)

    /** Seals [plaintext] under a caller-supplied [nonce]. Returns `nonce || ciphertext || tag`. */
    fun sealWithNonce(
        key: SymmetricKey,
        nonce: ByteArray,
        associatedData: ByteArray,
        plaintext: ByteArray,
    ): ByteArray {
        requireNonce(nonce)
        val body = ByteArray(plaintext.size + AEAD_TAG_LEN)
        val bodyLen = LongArray(1)
        val ok = Sodium.aead.cryptoAeadXChaCha20Poly1305IetfEncrypt(
            body,
            bodyLen,
            plaintext,
            plaintext.size.toLong(),
            associatedData,
            associatedData.size.toLong(),
            null,
            nonce,
            key.expose(),
        )
        check(ok) { "aead encryption failed" }
        val out = ByteArray(AEAD_NONCE_LEN + body.size)
        System.arraycopy(nonce, 0, out, 0, AEAD_NONCE_LEN)
        System.arraycopy(body, 0, out, AEAD_NONCE_LEN, body.size)
        return out
    }

    /**
     * Opens a `nonce || ciphertext || tag` message.
     *
     * Too-short input is a structural error ([CryptoError.badLength]); a failed tag check is
     * [CryptoError.decryptionFailed], the same error for every cause so nothing distinguishes a
     * wrong key from edited ciphertext.
     */
    fun open(key: SymmetricKey, associatedData: ByteArray, sealed: ByteArray): ByteArray {
        if (sealed.size < AEAD_NONCE_LEN + AEAD_TAG_LEN) {
            throw CryptoError.badLength("sealed message", AEAD_NONCE_LEN + AEAD_TAG_LEN, sealed.size)
        }
        val nonce = sealed.copyOfRange(0, AEAD_NONCE_LEN)
        val body = sealed.copyOfRange(AEAD_NONCE_LEN, sealed.size)
        return openWithNonce(key, nonce, associatedData, body)
    }

    /** Opens a `ciphertext || tag` body under a caller-supplied [nonce]. */
    fun openWithNonce(
        key: SymmetricKey,
        nonce: ByteArray,
        associatedData: ByteArray,
        body: ByteArray,
    ): ByteArray {
        requireNonce(nonce)
        if (body.size < AEAD_TAG_LEN) {
            throw CryptoError.badLength("sealed body", AEAD_TAG_LEN, body.size)
        }
        val message = ByteArray(body.size - AEAD_TAG_LEN)
        val messageLen = LongArray(1)
        val ok = Sodium.aead.cryptoAeadXChaCha20Poly1305IetfDecrypt(
            message,
            messageLen,
            null,
            body,
            body.size.toLong(),
            associatedData,
            associatedData.size.toLong(),
            nonce,
            key.expose(),
        )
        if (!ok) throw CryptoError.decryptionFailed()
        return message
    }

    private fun requireNonce(nonce: ByteArray) {
        if (nonce.size != AEAD_NONCE_LEN) {
            throw CryptoError.badLength("nonce", AEAD_NONCE_LEN, nonce.size)
        }
    }
}
