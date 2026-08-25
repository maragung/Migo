package com.migo.core.wire

/**
 * MWP/1 frame header.
 *
 * Every message on every transport is one frame:
 *
 * ```text
 * u8      version          protocol version, 1
 * u8      flags            see Flags
 * varint  opcode           what this frame is
 * varint  correlation      request/response pairing, 0 for unsolicited
 * [16]u8  trace_id         only when TRACED
 * [8]u8   span_id          only when TRACED
 * varint  fragment_index   only when FRAGMENT
 * varint  fragment_total   only when FRAGMENT
 * ...     payload          the remainder of the frame
 * ```
 *
 * Note what is *not* here: a length. The payload runs to the end of the frame and the
 * transport supplies the boundary. WebSocket and QUIC datagrams already frame messages, so a
 * length field would be redundant bytes on every single message. Stream transports that do
 * not frame prepend a `u32` big-endian length ([encodeFrameLengthPrefixed]).
 *
 * `opcode` and `correlation` are held as `Long`, not `Int`, because they are 32-bit unsigned
 * on the wire: a correlation of `0xFFFFFFFF` is legal and does not fit a signed `Int`. The
 * conformance vectors probe exactly that boundary, so a narrower type here would fail them.
 */

/** The wire protocol version this build speaks. */
const val PROTOCOL_VERSION = 1

/** Encoded size of a trace context in a frame header. */
const val TRACE_ENCODED_LEN = 24

/**
 * Distributed-tracing identifiers carried on a frame: a 16-byte trace id and an 8-byte span
 * id, sent as raw bytes rather than the 55-character `traceparent` string. Most frames omit
 * this — sampling happens at the edge.
 *
 * A plain class, not a `data class`: the codec never compares two trace contexts, and a
 * `data class` over `ByteArray` fields would advertise a structural `equals` it does not
 * actually provide (arrays compare by reference).
 */
class TraceContext(val traceId: ByteArray, val spanId: ByteArray)

/** Position of a frame within a fragmented message. Chat messages never fragment. */
data class Fragment(val index: Long, val total: Long)

/** The parsed header of an MWP/1 frame. */
data class FrameHeader(
    /** Protocol version byte as it appeared on the wire. */
    val version: Int,
    /** Raw flag bits. Use the [Flags] helpers rather than testing bits inline. */
    val flags: Int,
    /** The operation this frame carries. */
    val opcode: Long,
    /** Correlates a response with its request; 0 for server-initiated frames. */
    val correlation: Long,
    /** Present when the `TRACED` flag is set. */
    val trace: TraceContext?,
    /** Present when the `FRAGMENT` flag is set. */
    val fragment: Fragment?,
)

/** A complete frame: header plus payload bytes, still compressed if `COMPRESSED` is set. */
class Frame(val header: FrameHeader, val payload: ByteArray)

/** A minimal header: current version, no flags, no trace, no fragment. */
fun frameHeader(opcode: Long, correlation: Long = 0L): FrameHeader =
    FrameHeader(PROTOCOL_VERSION, 0, opcode, correlation, null, null)

/** True when both identifiers are all zero, which W3C Trace Context defines as invalid. */
fun isInvalidTrace(trace: TraceContext): Boolean =
    trace.traceId.all { it.toInt() == 0 } && trace.spanId.all { it.toInt() == 0 }

/** True when this is the last fragment of its message. */
fun isLastFragment(fragment: Fragment): Boolean = fragment.index + 1 >= fragment.total

/** Encoded size of this header in bytes. */
fun headerEncodedLen(header: FrameHeader): Int {
    var len = 2 + Varint.encodedLen(header.opcode) + Varint.encodedLen(header.correlation)
    if (header.trace != null) len += TRACE_ENCODED_LEN
    val fragment = header.fragment
    if (fragment != null) len += Varint.encodedLen(fragment.index) + Varint.encodedLen(fragment.total)
    return len
}

/**
 * Appends this header to [out].
 *
 * The flag bits and the optional blocks are written from the same source of truth — the
 * nullable fields — so a header can never claim `TRACED` without carrying a trace. A
 * caller-supplied `TRACED` bit with no trace is therefore corrected, not honoured.
 */
fun encodeHeader(header: FrameHeader, out: ByteSink) {
    var bits = header.flags and (Flags.TRACED or Flags.FRAGMENT).inv()
    if (header.trace != null) bits = bits or Flags.TRACED
    if (header.fragment != null) bits = bits or Flags.FRAGMENT
    val reserved = bits and Flags.RESERVED_MASK
    if (reserved != 0) throw WireError.reservedFlags(reserved)

    out.push(header.version and 0xFF)
    out.push(bits and 0xFF)
    Varint.encodeU64(header.opcode, out)
    Varint.encodeU64(header.correlation, out)

    val trace = header.trace
    if (trace != null) {
        assertLen(trace.traceId, 16, "trace_id")
        assertLen(trace.spanId, 8, "span_id")
        for (b in trace.traceId) out.push(b.toInt() and 0xFF)
        for (b in trace.spanId) out.push(b.toInt() and 0xFF)
    }
    val fragment = header.fragment
    if (fragment != null) {
        validateFragment(fragment)
        Varint.encodeU64(fragment.index, out)
        Varint.encodeU64(fragment.total, out)
    }
}

/** The result of [decodeHeader]: the header and where its payload begins. */
data class DecodedHeader(val header: FrameHeader, val offset: Int)

/**
 * Parses a header from the front of [input].
 *
 * Rejects, in this order: a short frame, an unsupported version, reserved flag bits, a
 * truncated trace block, and an impossible fragment pair. Reserved bits are an error and not
 * something to ignore — a peer that sets them is speaking a dialect we do not know, and
 * silently discarding the bits would let a future extension be stripped by an old node that
 * had no idea it was doing so.
 */
fun decodeHeader(input: ByteArray): DecodedHeader {
    if (input.size < 2) throw WireError.unexpectedEnd(input.size, 2)
    val version = input[0].toInt() and 0xFF
    if (version != PROTOCOL_VERSION) throw WireError.unsupportedVersion(version, PROTOCOL_VERSION)
    val bits = input[1].toInt() and 0xFF
    val reserved = bits and Flags.RESERVED_MASK
    if (reserved != 0) throw WireError.reservedFlags(reserved)

    var offset = 2
    val opcodeRaw = Varint.scan(input, offset)
    offset += opcodeRaw.used
    val correlationRaw = Varint.scan(input, offset)
    offset += correlationRaw.used
    val opcode = narrow(opcodeRaw, "opcode")
    val correlation = narrow(correlationRaw, "correlation")

    var trace: TraceContext? = null
    if ((bits and Flags.TRACED) != 0) {
        val end = offset + TRACE_ENCODED_LEN
        if (input.size < end) throw WireError.unexpectedEnd(offset, TRACE_ENCODED_LEN)
        trace = TraceContext(
            traceId = input.copyOfRange(offset, offset + 16),
            spanId = input.copyOfRange(offset + 16, end),
        )
        offset = end
    }

    var fragment: Fragment? = null
    if ((bits and Flags.FRAGMENT) != 0) {
        val indexRaw = Varint.scan(input, offset)
        offset += indexRaw.used
        val totalRaw = Varint.scan(input, offset)
        offset += totalRaw.used
        fragment = Fragment(narrow(indexRaw, "fragment_index"), narrow(totalRaw, "fragment_total"))
        validateFragment(fragment)
    }

    return DecodedHeader(
        FrameHeader(version, bits, opcode, correlation, trace, fragment),
        offset,
    )
}

/** Encodes the frame for a transport that supplies its own message boundaries. */
fun encodeFrame(frame: Frame): ByteArray {
    val headerLen = headerEncodedLen(frame.header)
    val total = headerLen + frame.payload.size
    if (total > Limits.MAX_FRAME_BYTES) throw WireError.frameTooLarge(total.toLong(), Limits.MAX_FRAME_BYTES)
    val head = ByteAccumulator(headerLen)
    encodeHeader(frame.header, head)
    val headBytes = head.toByteArray()
    val out = ByteArray(headBytes.size + frame.payload.size)
    headBytes.copyInto(out, 0)
    frame.payload.copyInto(out, headBytes.size)
    return out
}

/** Encodes the frame with a `u32` big-endian length prefix, for non-framing stream transports. */
fun encodeFrameLengthPrefixed(frame: Frame): ByteArray {
    val body = encodeFrame(frame)
    val out = ByteArray(4 + body.size)
    writeU32Be(out, 0, body.size)
    body.copyInto(out, 4)
    return out
}

/**
 * Parses one frame from a complete transport message. The size limit is checked before
 * anything is parsed, because the cheapest place to reject an oversized frame is before
 * touching it.
 */
fun decodeFrame(input: ByteArray): Frame {
    if (input.size > Limits.MAX_FRAME_BYTES) throw WireError.frameTooLarge(input.size.toLong(), Limits.MAX_FRAME_BYTES)
    val decoded = decodeHeader(input)
    return Frame(decoded.header, input.copyOfRange(decoded.offset, input.size))
}

/** The result of [decodeFrameLengthPrefixed]: one frame and how many bytes it consumed. */
data class DecodedFrame(val frame: Frame, val consumed: Int)

/**
 * Parses one length-prefixed frame. Returns `null` when the buffer does not yet hold a whole
 * frame, which is the normal state of a stream transport rather than an error.
 */
fun decodeFrameLengthPrefixed(input: ByteArray): DecodedFrame? {
    if (input.size < 4) return null
    val len = readU32Be(input, 0)
    if (len > Limits.MAX_FRAME_BYTES) throw WireError.frameTooLarge(len, Limits.MAX_FRAME_BYTES)
    if (input.size < 4L + len) return null
    val lenInt = len.toInt() // safe: len <= MAX_FRAME_BYTES
    return DecodedFrame(decodeFrame(input.copyOfRange(4, 4 + lenInt)), 4 + lenInt)
}

/**
 * Validates a fragment pair. A total of zero, or an index at or past the total, is a protocol
 * error rather than something to reassemble optimistically: accepting it would let a peer keep
 * a reassembly buffer alive forever, which is a memory leak a stranger gets to trigger.
 */
private fun validateFragment(fragment: Fragment) {
    if (fragment.total == 0L || fragment.index >= fragment.total) {
        throw WireError.invalidFragment(fragment.index, fragment.total)
    }
}

/** Narrows a scanned varint to a `u32` field, naming the field when it does not fit. */
private fun narrow(scanned: Varint.Scanned, field: String): Long {
    if (scanned.value > 0xFFFFFFFFuL) throw WireError.fieldOverflow(field)
    return scanned.value.toLong()
}

/**
 * A trace id of the wrong width is a bug in this process, not a wire event, so it throws like
 * the varint encoder does rather than a [WireError] a peer could be blamed for.
 */
private fun assertLen(bytes: ByteArray, expected: Int, what: String) {
    require(bytes.size == expected) { "$what must be $expected bytes, got ${bytes.size}" }
}

private fun writeU32Be(out: ByteArray, at: Int, value: Int) {
    out[at] = (value ushr 24).toByte()
    out[at + 1] = (value ushr 16).toByte()
    out[at + 2] = (value ushr 8).toByte()
    out[at + 3] = value.toByte()
}

private fun readU32Be(input: ByteArray, at: Int): Long =
    ((input[at].toLong() and 0xFF) shl 24) or
        ((input[at + 1].toLong() and 0xFF) shl 16) or
        ((input[at + 2].toLong() and 0xFF) shl 8) or
        (input[at + 3].toLong() and 0xFF)
