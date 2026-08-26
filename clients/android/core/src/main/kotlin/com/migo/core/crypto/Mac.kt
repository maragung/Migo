package com.migo.core.crypto

/** Full HMAC-SHA256 tag length. */
const val MAC_TAG_LEN = 32

/** Shortest tag the server will accept — a truncated MAC, never below half the output. */
const val MAC_MIN_TAG_LEN = 16

/**
 * A keyed HMAC-SHA256 used for the server's self-issued MACs: session and refresh tokens, resume
 * cursors, signed media URLs, pagination cursors, verification codes, webhook signatures.
 *
 * These are the server authenticating its own bytes to itself, not end-to-end message
 * authentication, but the client mirrors the construction so it can recompute and verify the tags
 * the server hands it. [tagParts] length-prefixes each part with a big-endian u64 before feeding
 * it to the MAC so that `("a", "bc")` and `("ab", "c")` produce different tags — without the
 * framing they would collide, which is a forgery.
 */
class MacKey private constructor(private val key: ByteArray) {
    private var destroyed = false

    companion object {
        const val LABEL_SESSION_TOKEN = "migo-session-token-v1"
        const val LABEL_REFRESH_TOKEN = "migo-refresh-token-v1"
        const val LABEL_RESUME_CURSOR = "migo-resume-cursor-v1"
        const val LABEL_MEDIA_URL = "migo-media-url-v1"
        const val LABEL_PAGINATION = "migo-pagination-v1"
        const val LABEL_VERIFICATION = "migo-verification-v1"
        const val LABEL_WEBHOOK = "migo-webhook-v1"

        private const val KEY_LEN = 32

        /** Derives a MAC key from a root secret under a domain [label]. */
        fun derive(root: ByteArray, label: String): MacKey =
            MacKey(Kdf.derive(root, null, label, KEY_LEN))

        /** Wraps an existing 32-byte key, copying it. */
        fun fromBytes(key: ByteArray): MacKey {
            if (key.size != KEY_LEN) throw CryptoError.badLength("mac key", KEY_LEN, key.size)
            return MacKey(key.copyOf())
        }
    }

    /** Full-length tag over a single message. */
    fun tag(message: ByteArray): ByteArray = Hmac.sha256(live(), message)

    /** Full-length tag over a length-prefixed sequence of parts. */
    fun tagParts(parts: List<ByteArray>): ByteArray {
        val sequence = ArrayList<ByteArray>(parts.size * 2)
        for (part in parts) {
            sequence.add(lengthPrefix(part.size.toLong()))
            sequence.add(part)
        }
        return Hmac.sha256Parts(live(), sequence)
    }

    /** Verifies [tag] against [message] in constant time. Throws on mismatch or bad length. */
    fun verify(message: ByteArray, tag: ByteArray) = verifyExpected(tag(message), tag)

    /** Verifies [tag] against a length-prefixed sequence of parts in constant time. */
    fun verifyParts(parts: List<ByteArray>, tag: ByteArray) = verifyExpected(tagParts(parts), tag)

    /** Zeroes the key. Any later use throws. */
    fun destroy() {
        key.fill(0)
        destroyed = true
    }

    override fun toString(): String = "MacKey(***)"

    private fun live(): ByteArray {
        check(!destroyed) { "MacKey has been destroyed" }
        return key
    }

    /**
     * Compares the caller's [tag] against the [expected] full tag.
     *
     * A caller may present a truncated tag (down to [MAC_MIN_TAG_LEN]); the expected tag is sliced
     * to the presented length before comparison. Anything shorter than the floor or longer than the
     * full tag is a structural error, not a verification failure.
     */
    private fun verifyExpected(expected: ByteArray, tag: ByteArray) {
        if (tag.size < MAC_MIN_TAG_LEN || tag.size > MAC_TAG_LEN) {
            throw CryptoError.badLength("mac tag", MAC_TAG_LEN, tag.size)
        }
        if (!constantTimeEquals(expected.copyOfRange(0, tag.size), tag)) {
            throw CryptoError.badSignature()
        }
    }
}

/** Big-endian u64 length prefix, matching the Rust and TypeScript framing. */
private fun lengthPrefix(length: Long): ByteArray {
    val out = ByteArray(8)
    for (i in 0..7) out[i] = (length ushr (8 * (7 - i))).toByte()
    return out
}

/**
 * Constant-time byte comparison.
 *
 * The loop accumulates differences with no data-dependent branch or early return, so its timing
 * does not leak how many leading bytes matched — the same shape `@noble`'s `equalBytes` and
 * libsodium's `sodium_memcmp` use. It is a comparison, not a cryptographic transform, so
 * implementing it here does not cross ADR-0003's audited-primitives line.
 */
private fun constantTimeEquals(a: ByteArray, b: ByteArray): Boolean {
    if (a.size != b.size) return false
    var diff = 0
    for (i in a.indices) diff = diff or (a[i].toInt() xor b[i].toInt())
    return diff == 0
}
