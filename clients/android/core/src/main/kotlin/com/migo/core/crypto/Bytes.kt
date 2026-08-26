package com.migo.core.crypto

/**
 * Byte plumbing shared by the constructions in this package.
 *
 * None of this is cryptography. It exists because the wire forms are byte-exact contracts with the
 * Rust crate and `@migo/crypto`: a big-endian `u32` written as little-endian, or a concatenation
 * built in the wrong order, produces a header that authenticates on this client and nowhere else.
 * Keeping the encoders in one file means there is one place to compare against the reference.
 */

/** The `u32` ceiling, for the saturating arithmetic the ratchet mirrors from Rust. */
internal const val U32_MAX = 0xffff_ffffL

/**
 * The version byte every persisted crypto state snapshot begins with.
 *
 * One constant shared by the ratchet and the sender-key states rather than one each, because a store
 * writes them together and loads them together: a session whose ratchet restored at version 1 next
 * to a sender key at version 2 would be a half-migrated session, which fails later and further from
 * the cause than a snapshot that was simply refused. Bumping this refuses both at once.
 *
 * A version byte rather than a length or a magic number is what makes a layout change safe: an old
 * build reading a new snapshot stops at the first byte instead of misreading a field boundary and
 * loading a chain key that is really half a counter.
 */
internal const val STATE_SNAPSHOT_VERSION = 1

/** Concatenates [parts] into one fresh array. */
internal fun concatBytes(vararg parts: ByteArray): ByteArray {
    var total = 0
    for (part in parts) total += part.size
    val out = ByteArray(total)
    var offset = 0
    for (part in parts) {
        System.arraycopy(part, 0, out, offset, part.size)
        offset += part.size
    }
    return out
}

/**
 * Writes [value] as a big-endian `u32` at [offset].
 *
 * Counters cross the wire as `u32` but are held as [Long] here, because Kotlin's [Int] is signed:
 * a message number above 2^31 would compare as negative and a gap check would pass when it must
 * fail. [Long] carries the full unsigned range with ordinary arithmetic, and only the low 32 bits
 * reach the wire, so the encoding is identical to Rust's `to_be_bytes`.
 */
internal fun putU32Be(out: ByteArray, offset: Int, value: Long) {
    out[offset] = (value ushr 24).toByte()
    out[offset + 1] = (value ushr 16).toByte()
    out[offset + 2] = (value ushr 8).toByte()
    out[offset + 3] = value.toByte()
}

/** Reads a big-endian `u32` at [offset] into the full `0..4294967295` range. */
internal fun readU32Be(bytes: ByteArray, offset: Int): Long {
    var value = 0L
    for (i in 0 until 4) value = (value shl 8) or (bytes[offset + i].toLong() and 0xff)
    return value
}

/** Rejects a counter that would not fit the `u32` it is serialised as. */
internal fun requireU32(value: Long, what: String) {
    if (value < 0L || value > U32_MAX) {
        throw CryptoError.badLength(what, 4, if (value < 0L) 0 else 8)
    }
}

/** `min(a + b, u32::MAX)`, matching Rust's `u32::saturating_add`. */
internal fun saturatingAddU32(a: Long, b: Long): Long = minOf(a + b, U32_MAX)

/** `max(a - b, 0)`, matching Rust's `u32::saturating_sub`. */
internal fun saturatingSubU32(a: Long, b: Long): Long = maxOf(a - b, 0L)

/** Lowercase hex. Public material only — never a seed, a chain key or a plaintext. */
internal fun hexOf(bytes: ByteArray): String {
    val digits = "0123456789abcdef"
    val out = StringBuilder(bytes.size * 2)
    for (b in bytes) {
        val value = b.toInt() and 0xff
        out.append(digits[value ushr 4]).append(digits[value and 0x0f])
    }
    return out.toString()
}
