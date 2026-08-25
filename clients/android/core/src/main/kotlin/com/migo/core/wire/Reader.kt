package com.migo.core.wire

import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction

/**
 * MSE decoder.
 *
 * Every method treats its input as hostile. The rule the whole class is built around: **a
 * length is validated against the bytes actually available before a single byte is
 * allocated.** A four-byte frame that claims a four-gigabyte string must cost four bytes to
 * reject, not four gigabytes to discover. That one property is the difference between a codec
 * and a remote out-of-memory primitive.
 *
 * The TypeScript reader hands out `subarray` views for optional fields and opaque byte
 * fields; on the JVM there is no zero-copy sub-array of a `ByteArray`, so every take copies.
 * The decoded values are identical — only the aliasing differs, and a copy is the safer side
 * of that difference.
 */
class Reader(private val input: ByteArray, private var depth: Int = 0) {
    private var pos = 0

    /** Bytes not yet consumed. */
    val remaining: Int get() = (input.size - pos).coerceAtLeast(0)

    /** True when the payload is fully consumed. */
    val isEmpty: Boolean get() = remaining == 0

    /** Current read offset, for error reporting. */
    val position: Int get() = pos

    /**
     * Asserts the payload was fully consumed. Called at the top level only: trailing bytes
     * there mean the sender and receiver disagree about the schema, which is worth failing on
     * — ignoring them silently turns a version mismatch into a mystery bug three releases later.
     */
    fun finish() {
        if (remaining > 0) throw WireError.trailingBytes(remaining)
    }

    /** Opens a struct. Bounds recursion. */
    fun enter() {
        if (depth >= Limits.MAX_NESTING_DEPTH) throw WireError.depthExceeded(Limits.MAX_NESTING_DEPTH)
        depth += 1
    }

    /** Closes a struct. */
    fun leave() {
        depth = maxOf(0, depth - 1)
    }

    /**
     * Reads one byte as a boolean, accepting only `0` and `1`. Anything else is an error
     * rather than "truthy": the codec is canonical by rule, and a byte with 255 legal
     * encodings of `true` breaks that rule the way a padded varint would.
     */
    fun bool(): Boolean = when (val byte = u8()) {
        0 -> false
        1 -> true
        else -> throw WireError.invalidBool(byte)
    }

    /** Reads a varint that must fit in 32 bits. */
    fun u32(): Long {
        val decoded = Varint.decodeU32(input, pos)
        pos += decoded.used
        return decoded.value
    }

    /** Reads a varint that must fit a signed 64-bit integer. */
    fun u64(): Long {
        val decoded = Varint.decodeU64Safe(input, pos)
        pos += decoded.used
        return decoded.value
    }

    /** Reads a varint across the full unsigned 64-bit range. */
    fun u64big(): ULong {
        val scanned = Varint.scan(input, pos)
        pos += scanned.used
        return scanned.value
    }

    /** Reads Migo-epoch milliseconds and returns Unix milliseconds. */
    fun timestamp(): Long {
        val decoded = Varint.decodeU64Safe(input, pos)
        pos += decoded.used
        return WireTime.fromWire(decoded.value)
    }

    /** Reads a 16-byte identifier. */
    fun id(): Id = idFromBytes(take(ID_BYTE_LEN))

    /** Reads a length-prefixed UTF-8 string. */
    fun str(): String {
        val len = readLength(Limits.MAX_STRING_BYTES) { l, m -> WireError.stringTooLong(l, m) }
        return decodeUtf8Strict(take(len))
    }

    /** Reads a length-prefixed opaque byte field, copied so the caller owns it. */
    fun bytes(): ByteArray {
        val len = readLength(Limits.MAX_BYTES_LEN) { l, m -> WireError.bytesTooLong(l, m) }
        return take(len)
    }

    /**
     * Reads a length-prefixed opaque byte field. On the web this returns a view; here it is a
     * copy like [bytes]. Kept as a distinct method so ported code reads the same on both sides
     * and so the intent — "these bytes go straight to the AEAD" — stays legible.
     */
    fun bytesShared(): ByteArray = bytes()

    /**
     * Reads a list header and returns the item count, checked against both
     * [Limits.MAX_LIST_ITEMS] and the remaining bytes: every item costs at least one byte, so
     * a count larger than what is left is a lie — and callers size an array from this number.
     */
    fun listLen(): Int {
        val scanned = Varint.scan(input, pos)
        pos += scanned.used
        if (scanned.value > Limits.MAX_LIST_ITEMS.toULong()) {
            throw WireError.listTooLong(scanned.value.toLong(), Limits.MAX_LIST_ITEMS)
        }
        val len = scanned.value.toInt() // safe: bounded above by MAX_LIST_ITEMS
        if (len > remaining) throw WireError.listTooLong(len.toLong(), remaining)
        return len
    }

    /**
     * Reads one optional field header and returns its id together with a reader scoped to
     * exactly that field's bytes. The scoping is what makes an unknown field safe to ignore:
     * the caller drops the sub-reader and the outer position has already advanced past the
     * whole field, so a malformed unknown field cannot desynchronise the stream — the property
     * that lets a v1 client keep talking to a v2 server.
     *
     * The sub-reader inherits this reader's depth unchanged; recursion is bounded by the
     * [enter]/[leave] pair the generated struct code wraps around each nested value.
     */
    fun optional(): Pair<Long, Reader> {
        val fieldId = u32()
        val len = readLength(Limits.MAX_FRAME_BYTES) { l, m -> WireError.frameTooLarge(l, m) }
        return Pair(fieldId, Reader(take(len), depth))
    }

    private fun u8(): Int {
        if (pos >= input.size) throw WireError.unexpectedEnd(pos, 1)
        val byte = input[pos].toInt() and 0xFF
        pos += 1
        return byte
    }

    /**
     * Reads a length prefix and rejects it against both the configured limit and the bytes
     * actually present, before the caller allocates anything.
     */
    private inline fun readLength(max: Int, tooLong: (Long, Int) -> WireError): Int {
        val scanned = Varint.scan(input, pos)
        pos += scanned.used
        if (scanned.value > max.toULong()) throw tooLong(scanned.value.toLong(), max)
        val len = scanned.value.toInt() // safe: bounded above by max
        if (len > remaining) throw WireError.unexpectedEnd(pos, len - remaining)
        return len
    }

    private fun take(len: Int): ByteArray {
        if (len > remaining) throw WireError.unexpectedEnd(pos, len - remaining)
        val out = input.copyOfRange(pos, pos + len)
        pos += len
        return out
    }

    /**
     * Strict UTF-8. The JVM's default decode replaces malformed input with U+FFFD, which would
     * turn "this peer sent invalid UTF-8" into "this display name contains a replacement
     * character" — a silent mutation of user data where the Rust side returns `InvalidUtf8`.
     * Two implementations of one protocol may not disagree about which frames are valid.
     */
    private fun decodeUtf8Strict(bytes: ByteArray): String {
        val decoder = Charsets.UTF_8.newDecoder()
            .onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT)
        return try {
            decoder.decode(ByteBuffer.wrap(bytes)).toString()
        } catch (_: CharacterCodingException) {
            throw WireError.invalidUtf8()
        }
    }
}
