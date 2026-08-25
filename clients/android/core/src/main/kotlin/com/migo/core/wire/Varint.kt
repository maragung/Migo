package com.migo.core.wire

/**
 * LEB128, and the two decisions that make it safe.
 *
 * **Canonical.** A padded encoding of a value is rejected, not accepted-and-normalised.
 * Mesh handshake frames are signed over their bytes and deduplicated by hash, so two
 * spellings of one value would mean two valid signatures for one logical message.
 *
 * **Bounded before allocation.** Ten bytes is the longest legal encoding of a 64-bit value,
 * and the eleventh byte is refused before it is read. A decoder that keeps shifting for as
 * long as the continuation bit is set is a remote hang.
 *
 * Where the TypeScript codec splits a value into two doubles (JavaScript has no 64-bit
 * integer), Kotlin accumulates straight into a [ULong]. The bytes produced and accepted are
 * identical; only the in-memory arithmetic differs.
 */
object Varint {
    /** A decoded varint: its value, and how many bytes the encoding occupied. */
    data class Scanned(val value: ULong, val used: Int)

    /**
     * Reads one varint at [offset].
     *
     * Rejects, in this order: an eleventh byte, a truncated encoding, a tenth byte carrying
     * more than one payload bit, and a terminal byte of zero after the first — the padded form.
     */
    fun scan(input: ByteArray, offset: Int): Scanned {
        var value = 0uL
        var index = 0
        while (true) {
            if (index >= Limits.MAX_VARINT_BYTES) {
                throw WireError.varintTooLong(offset, Limits.MAX_VARINT_BYTES)
            }
            val pos = offset + index
            if (pos >= input.size) {
                throw WireError.unexpectedEnd(pos, 1)
            }
            val byte = input[pos].toInt() and 0xFF
            val shift = index * 7
            index += 1
            val payload = byte and 0x7F

            // At shift 63 only bit 63 is left, so a payload above 1 describes a value that
            // does not exist in 64 bits. Caught here rather than silently wrapping.
            if (shift == 63 && payload > 1) {
                throw WireError.varintTooLong(offset, Limits.MAX_VARINT_BYTES)
            }

            value = value or (payload.toULong() shl shift)

            if ((byte and 0x80) == 0) {
                if (index > 1 && byte == 0) {
                    throw WireError.nonMinimalVarint(offset)
                }
                return Scanned(value, index)
            }
        }
    }

    /** A varint value together with the byte count it occupied. */
    data class Decoded(val value: Long, val used: Int)

    /** Reads a varint that must fit `u32`. */
    fun decodeU32(input: ByteArray, offset: Int): Decoded {
        val scanned = scan(input, offset)
        if (scanned.value > 0xFFFFFFFFuL) {
            throw WireError.lengthOverflow(scanned.value)
        }
        return Decoded(scanned.value.toLong(), scanned.used)
    }

    /**
     * Reads a varint as a signed `Long`.
     *
     * Refuses a value with the top bit set (one that a signed 64-bit integer cannot hold)
     * rather than wrapping it negative. Callers that genuinely need the full unsigned 64-bit
     * range read the [Scanned.value] from [scan] directly.
     */
    fun decodeU64Safe(input: ByteArray, offset: Int): Decoded {
        val scanned = scan(input, offset)
        if (scanned.value > Long.MAX_VALUE.toULong()) {
            throw WireError.lengthOverflow(scanned.value)
        }
        return Decoded(scanned.value.toLong(), scanned.used)
    }

    /** Appends [value] as a varint. */
    fun encodeU64(value: ULong, out: ByteSink) {
        var remaining = value
        while (remaining >= 0x80uL) {
            out.push(((remaining and 0x7FuL).toInt()) or 0x80)
            remaining = remaining shr 7
        }
        out.push(remaining.toInt())
    }

    /** Appends a non-negative [value] as a varint. */
    fun encodeU64(value: Long, out: ByteSink) {
        require(value >= 0L) { "varint value must be non-negative: $value" }
        encodeU64(value.toULong(), out)
    }

    /** Bytes [encodeU64] would append. Used to size a buffer before writing into it. */
    fun encodedLen(value: ULong): Int {
        var remaining = value
        var len = 1
        while (remaining >= 0x80uL) {
            remaining = remaining shr 7
            len += 1
        }
        return len
    }

    fun encodedLen(value: Long): Int {
        require(value >= 0L) { "varint value must be non-negative: $value" }
        return encodedLen(value.toULong())
    }

    /**
     * Maps a signed value onto an unsigned one so small negatives stay small:
     * `0, -1, 1, -2` become `0, 1, 2, 3`. Kotlin's `Long` is exact 64-bit two's complement,
     * so the arithmetic shift on the sign is all the masking the TypeScript version needs a
     * `& 0xffff…` for.
     */
    fun zigzagEncode(value: Long): ULong = ((value shl 1) xor (value shr 63)).toULong()

    /** Inverse of [zigzagEncode]. */
    fun zigzagDecode(value: ULong): Long {
        val z = value.toLong()
        return (z ushr 1) xor -(z and 1L)
    }
}
