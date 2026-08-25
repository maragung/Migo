package com.migo.core.wire

/**
 * Frame batching.
 *
 * A busy room produces bursts: twenty presence updates and a typing indicator land inside the
 * same 15 milliseconds. Sent individually that is twenty-one WebSocket messages, twenty-one
 * syscalls, twenty-one wake-ups of a phone's radio, and twenty-one frame headers. Batched, it
 * is one.
 *
 * Wire format — the payload of a frame with the `BATCH` flag set:
 *
 * ```text
 * varint  count
 * count × ( varint frame_len, frame_len bytes )
 * ```
 *
 * Each element is a complete frame, header included. That redundancy is deliberate: a batch is
 * a transport optimisation, not a new message type, so the receiver's dispatch loop is
 * identical whether a frame arrived alone or inside a batch. Two rules keep it honest: at most
 * [Limits.MAX_BATCH_ITEMS] elements, and no nesting — a batch inside a batch is rejected,
 * because otherwise a small frame could describe an exponentially large expansion.
 */

/**
 * Opcode reserved for the batch envelope. The batch carries no payload type of its own, so the
 * opcode is a constant rather than something the IDL generates.
 */
const val BATCH_OPCODE = 0L

/** Minimum bytes an element can occupy: one length byte plus a four-byte minimal frame. */
private const val MIN_ELEMENT_BYTES = 5

/**
 * Packs frames into a single batch frame, with the `BATCH` flag set and correlation 0 — the
 * elements carry their own correlation ids, and the envelope has no request of its own to
 * answer. A one-element batch is returned as the bare frame instead: wrapping it would add
 * bytes and buy nothing.
 */
fun encodeBatch(frames: List<Frame>): Frame {
    if (frames.size > Limits.MAX_BATCH_ITEMS) {
        throw WireError.batchTooLarge(frames.size.toLong(), Limits.MAX_BATCH_ITEMS)
    }
    if (frames.size == 1) return frames[0]

    val payload = ByteAccumulator(256)
    Varint.encodeU64(frames.size.toLong(), payload)
    for (frame in frames) {
        if ((frame.header.flags and Flags.BATCH) != 0) throw WireError.nestedBatch()
        val encoded = encodeFrame(frame)
        Varint.encodeU64(encoded.size.toLong(), payload)
        payload.append(encoded)
    }

    val header = frameHeader(BATCH_OPCODE, 0L).copy(flags = Flags.BATCH)
    val body = payload.toByteArray()
    val len = headerEncodedLen(header) + body.size
    if (len > Limits.MAX_FRAME_BYTES) throw WireError.frameTooLarge(len.toLong(), Limits.MAX_FRAME_BYTES)
    return Frame(header, body)
}

/**
 * Turns one received frame into the list of frames to dispatch: inflates a compressed payload,
 * then unpacks a batch. This is the function a transport should call on every inbound frame. A
 * frame without the `BATCH` flag comes back as a one-element list, so the caller keeps a single
 * dispatch path and never has to ask which shape arrived.
 */
fun unpackFrame(frame: Frame): List<Frame> {
    val payload = if ((frame.header.flags and Flags.COMPRESSED) != 0) {
        Compress.inflateRaw(frame.payload, Limits.MAX_FRAME_BYTES)
    } else {
        frame.payload
    }

    if ((frame.header.flags and Flags.BATCH) == 0) {
        return listOf(Frame(frame.header, payload))
    }
    return decodeBatchPayload(payload)
}

/**
 * Unpacks the already-inflated payload of a batch frame. Separate from [unpackFrame] because a
 * caller that already holds plain bytes should not route through the decompression path.
 */
fun decodeBatchPayload(payload: ByteArray): List<Frame> {
    val head = Varint.scan(payload, 0)
    var offset = head.used
    if (head.value > Limits.MAX_BATCH_ITEMS.toULong()) {
        throw WireError.batchTooLarge(head.value.toLong(), Limits.MAX_BATCH_ITEMS)
    }
    val count = head.value.toInt() // safe: bounded above by MAX_BATCH_ITEMS

    // Every element costs at least a length byte plus a four-byte minimal header, so a count
    // larger than the remaining bytes allow is a lie. Checking it here is what stops a
    // preallocation from being turned into an allocation primitive by a 3-byte frame.
    val remaining = payload.size - offset
    if (count.toLong() * MIN_ELEMENT_BYTES > remaining.toLong()) {
        throw WireError.batchTooLarge(count.toLong(), remaining / MIN_ELEMENT_BYTES)
    }

    val frames = ArrayList<Frame>(count)
    for (i in 0 until count) {
        val scanned = Varint.scan(payload, offset)
        offset += scanned.used
        if (scanned.value > Limits.MAX_FRAME_BYTES.toULong()) {
            throw WireError.frameTooLarge(scanned.value.toLong(), Limits.MAX_FRAME_BYTES)
        }
        val len = scanned.value.toInt() // safe: bounded above by MAX_FRAME_BYTES
        val end = offset + len
        if (end > payload.size) throw WireError.unexpectedEnd(offset, len)
        val element = decodeFrame(payload.copyOfRange(offset, end))
        if ((element.header.flags and Flags.BATCH) != 0) throw WireError.nestedBatch()
        frames.add(element)
        offset = end
    }

    if (offset != payload.size) throw WireError.trailingBytes(payload.size - offset)
    return frames
}
