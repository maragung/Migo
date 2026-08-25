package com.migo.core.wire

/**
 * MSE encoder.
 *
 * Every limit is checked *before* the bytes are appended, not after the buffer has grown. The
 * difference matters when the value came from a peer: a check afterwards has already allocated
 * whatever it was about to reject.
 *
 * The TypeScript encoder writes optional fields by swapping the single backing buffer out from
 * under itself and restoring it in a `finally`. This port gives each optional field its own
 * child [Writer] instead: the length prefix is still discovered by writing the body first and
 * measuring it, but a throw from the body callback cannot corrupt the parent's buffer because
 * the parent never lent it out. The bytes produced are identical.
 */
class Writer private constructor(capacity: Int, private val depth: Int) : ByteSink {
    constructor(capacity: Int = 256) : this(capacity, 0)

    private var buf = ByteArray(capacity.coerceIn(16, Limits.MAX_FRAME_BYTES))
    private var len = 0

    /** Bytes held in enclosing optional scopes, so [length] is the whole frame. */
    private var outerLen = 0

    /** Struct nesting for [enter]/[leave], independent of optional-field nesting. */
    private var structDepth = 0

    /** Bytes written so far, across all open scopes. */
    val length: Int get() = outerLen + len

    /** Appends one raw byte. Present so a [Writer] satisfies [ByteSink]. */
    override fun push(byte: Int) {
        reserve(1)
        buf[len] = byte.toByte()
        len += 1
    }

    /**
     * Opens a struct scope. Nothing is written — the grammar has no struct delimiter. This
     * exists purely to bound recursion, stopping a hostile 200-deep nesting from becoming 200
     * stack frames in a generated encoder.
     */
    fun enter() {
        if (structDepth >= Limits.MAX_NESTING_DEPTH) throw WireError.depthExceeded(Limits.MAX_NESTING_DEPTH)
        structDepth += 1
    }

    /** Closes a struct scope. */
    fun leave() {
        structDepth = maxOf(0, structDepth - 1)
    }

    /** One byte, `0` or `1`. */
    fun bool(value: Boolean) = push(if (value) 1 else 0)

    /** Unsigned 32-bit, LEB128. */
    fun u32(value: Long) = Varint.encodeU64(value, this)
    fun u32(value: Int) = Varint.encodeU64(value.toLong(), this)

    /**
     * Unsigned 64-bit, LEB128. Values that need all 64 bits are declared `bitmask64` in the
     * schema and go through [u64big]; this one takes the signed [Long] the common fields use.
     */
    fun u64(value: Long) = Varint.encodeU64(value, this)
    fun u64(value: Int) = Varint.encodeU64(value.toLong(), this)

    /** Unsigned 64-bit, LEB128, across the full range. */
    fun u64big(value: ULong) = Varint.encodeU64(value, this)

    /** Unix milliseconds, written relative to the Migo epoch. */
    fun timestamp(unixMs: Long) = Varint.encodeU64(WireTime.toWire(unixMs), this)

    /** Sixteen raw bytes, with no length prefix — the width is fixed by the schema. */
    fun id(value: Id) {
        val bytes = idToBytes(value)
        guardGrowth(bytes.size)
        extend(bytes)
    }

    /** Varint byte length, then UTF-8. */
    fun str(value: String) {
        val bytes = value.toByteArray(Charsets.UTF_8)
        if (bytes.size > Limits.MAX_STRING_BYTES) throw WireError.stringTooLong(bytes.size.toLong(), Limits.MAX_STRING_BYTES)
        guardGrowth(bytes.size)
        Varint.encodeU64(bytes.size.toLong(), this)
        extend(bytes)
    }

    /** Varint byte length, then the bytes. */
    fun bytes(value: ByteArray) {
        if (value.size > Limits.MAX_BYTES_LEN) throw WireError.bytesTooLong(value.size.toLong(), Limits.MAX_BYTES_LEN)
        guardGrowth(value.size)
        Varint.encodeU64(value.size.toLong(), this)
        extend(value)
    }

    /** Item count for a `list<T>`. The items follow, written by the caller. */
    fun listLen(count: Int) {
        if (count > Limits.MAX_LIST_ITEMS) throw WireError.listTooLong(count.toLong(), Limits.MAX_LIST_ITEMS)
        Varint.encodeU64(count.toLong(), this)
    }

    /**
     * Writes one optional field as `field_id, byte_len, bytes`.
     *
     * The length prefix is the whole point of the design: it lets a decoder skip a field id it
     * has never heard of, which is how a v1 client survives a v2 server without renegotiating a
     * version for every added field. Because the prefix is a varint its width is not known
     * until the body has been written — hence writing the body into a child first and measuring
     * it. The *count* of present optional fields is written by the caller, before the first of
     * these; that is the struct's business, not the field's.
     */
    fun optional(fieldId: Int, write: (Writer) -> Unit) {
        if (depth >= Limits.MAX_NESTING_DEPTH) throw WireError.depthExceeded(Limits.MAX_NESTING_DEPTH)
        val child = Writer(64, depth + 1)
        child.outerLen = this.length
        write(child)
        val body = child.finishBody()
        guardGrowth(body.size)
        Varint.encodeU64(fieldId.toLong(), this)
        Varint.encodeU64(body.size.toLong(), this)
        extend(body)
    }

    /** Finishes the frame, checking the size limit one last time. */
    fun finish(): ByteArray {
        if (len > Limits.MAX_FRAME_BYTES) throw WireError.frameTooLarge(len.toLong(), Limits.MAX_FRAME_BYTES)
        return buf.copyOf(len)
    }

    private fun finishBody(): ByteArray = buf.copyOf(len)

    private fun extend(bytes: ByteArray) {
        if (bytes.isEmpty()) return
        reserve(bytes.size)
        bytes.copyInto(buf, len)
        len += bytes.size
    }

    private fun reserve(additional: Int) {
        val needed = len + additional
        if (needed <= buf.size) return
        var capacity = maxOf(buf.size, 16) * 2
        while (capacity < needed) capacity *= 2
        buf = buf.copyOf(capacity)
    }

    /**
     * Refuses a write that would take the frame past the limit, checked against the projected
     * total rather than the current one so a 300 KiB string is rejected before it is copied.
     */
    private fun guardGrowth(additional: Int) {
        val projected = length.toLong() + additional.toLong()
        if (projected > Limits.MAX_FRAME_BYTES) throw WireError.frameTooLarge(projected, Limits.MAX_FRAME_BYTES)
    }
}
