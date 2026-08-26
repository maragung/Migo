package com.migo.core.crypto

/**
 * HKDF-SHA256 (RFC 5869), assembled from [Hmac].
 *
 * Every symmetric key in the protocol is an HKDF expansion of some shared secret under a labelled
 * `info` string, and the labels are a wire contract: change one byte of a label here and this
 * client can no longer read a message the server or the web client wrote, even though every
 * primitive still passes its own tests. The label constants are therefore duplicated verbatim from
 * the Rust crate and `@migo/crypto`, and the kdf vectors pin the resulting bytes.
 */
object Kdf {
    const val LABEL_X3DH = "migo-x3dh-v1"
    const val LABEL_RATCHET_ROOT = "migo-ratchet-root-v1"
    const val LABEL_RATCHET_CHAIN = "migo-ratchet-chain-v1"
    const val LABEL_MESSAGE_KEY = "migo-message-key-v1"
    const val LABEL_SENDER_CHAIN = "migo-sender-chain-v1"
    const val LABEL_SENDER_MESSAGE = "migo-sender-message-v1"
    const val LABEL_BACKUP = "migo-backup-v1"
    const val LABEL_RECOVERY = "migo-recovery-v1"

    /** Derives [length] bytes from [secret] under a UTF-8 [label], with an optional [salt]. */
    fun derive(secret: ByteArray, salt: ByteArray?, label: String, length: Int): ByteArray =
        derive(secret, salt, label.toByteArray(Charsets.UTF_8), length)

    /**
     * Derives [length] bytes from [secret] under a raw-bytes [info], with an optional [salt].
     *
     * A null salt is RFC 5869's "salt not provided": the extract step is keyed with an empty key,
     * which HMAC treats identically to a HashLen-zero key.
     */
    fun derive(secret: ByteArray, salt: ByteArray?, info: ByteArray, length: Int): ByteArray {
        val prk = Hmac.sha256(salt ?: ByteArray(0), secret)
        val out = expand(prk, info, length)
        prk.fill(0)
        return out
    }

    /**
     * Derives two keys from ONE expansion.
     *
     * This is a single HKDF-Expand of `firstLength + secondLength` bytes split at `firstLength`, not
     * two separate derivations. Deriving them separately would make the shorter key a prefix of the
     * longer one whenever they shared a label, which is exactly the mistake the single-expansion
     * form exists to prevent. The ratchet's root and chain steps depend on this split.
     */
    fun derivePair(
        secret: ByteArray,
        salt: ByteArray?,
        label: String,
        firstLength: Int,
        secondLength: Int,
    ): Pair<ByteArray, ByteArray> {
        val combined = derive(secret, salt, label, firstLength + secondLength)
        val first = combined.copyOfRange(0, firstLength)
        val second = combined.copyOfRange(firstLength, firstLength + secondLength)
        combined.fill(0)
        return Pair(first, second)
    }

    private fun expand(prk: ByteArray, info: ByteArray, length: Int): ByteArray {
        require(length <= 255 * Hmac.OUTPUT_LEN) { "HKDF output too long" }
        val out = ByteArray(length)
        var previous = ByteArray(0)
        var filled = 0
        var counter = 1
        while (filled < length) {
            val input = ByteArray(previous.size + info.size + 1)
            System.arraycopy(previous, 0, input, 0, previous.size)
            System.arraycopy(info, 0, input, previous.size, info.size)
            input[input.size - 1] = counter.toByte()
            previous = Hmac.sha256(prk, input)
            val take = minOf(previous.size, length - filled)
            System.arraycopy(previous, 0, out, filled, take)
            filled += take
            counter += 1
        }
        return out
    }
}
