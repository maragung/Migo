package com.migo.app.call

import java.io.ByteArrayOutputStream

/**
 * The two plaintext shapes one mesh link's relays carry, and the codec that lays them out.
 *
 * A group call is a mesh, so its media descriptions travel as *relays* -- one seat naming another
 * device -- over the same `CALL_SDP` and `CALL_ICE` opcodes the one-to-one call uses. Both opcodes
 * are opaque blobs to the server, and in a group call they are sealed under the call's own frame
 * key rather than under any pairwise session key (see [GroupMediaPlane] for why that is what makes
 * the two uses of `CALL_SDP` tellable apart). What travels *inside* that seal is this file's
 * business.
 *
 * The layout is deliberately not JSON. The codec runs on every relay a call receives -- hundreds of
 * candidates per link across a mesh -- and an SDP body is megabytes of text that a JSON encoder
 * would escape and re-escape for no gain. Instead every variable-length field is length-prefixed in
 * bytes, so the reader copies exactly what the writer wrote, and the only parsing a hostile frame
 * can reach is an integer conversion.
 *
 * A frame that does not parse is refused as a whole ([decodeGroupMediaFrame] returns null) rather
 * than partly applied: the bytes are authenticated to a call member, but a member is not a
 * trusted parser, and half a description is worse than none of one.
 */

/**
 * The first line of every frame: this build's shape tag.
 *
 * Versioned from the first release because a mesh has no negotiation channel of its own -- a peer
 * that cannot read a frame has nothing to answer with, and the honest answer is to drop it. A later
 * shape changes this tag, and a peer running the older build refuses the new frames by tag rather
 * than by discovering mid-parse that a field it expected is a field the other side stopped sending.
 */
const val GROUP_MEDIA_FRAME_TAG: String = "migo-group-media/1"

/**
 * How many candidates one batch frame may carry before the rest are refused.
 *
 * A real gathering run produces a handful; the bound exists so a frame cannot name a count that
 * makes this device allocate for a link that is already gone.
 */
const val MAX_ICE_CANDIDATES: Int = 256

/**
 * One ICE candidate as the relay carries it: the same three fields `RTCIceCandidate` is built from
 * on either end, because the receiver hands them straight back to its own stack.
 */
data class GroupIceCandidate(
    /** The media line's mid, which is how a candidate finds its m-line in a bundle. */
    val sdpMid: String,
    /** The media line's index, the older of the two ways to say the same thing. */
    val sdpMLineIndex: Int,
    /** The candidate's own `a=candidate` text, verbatim. */
    val candidate: String,
)

/** One relay's plaintext, before it is sealed. */
sealed interface GroupMediaFrame {
    /**
     * A session description. [type] is the SDP's own name for itself -- `offer` or `answer` -- kept
     * as text so a future description type needs no wire change.
     */
    data class Description(val type: String, val sdp: String) : GroupMediaFrame

    /**
     * One gathering run's candidates, batched.
     *
     * A batch rather than a frame per candidate for the same reason the one-to-one plane batches:
     * candidates arrive in bursts, and the wire's frame count should be proportional to a call's
     * links rather than to the number of network interfaces each device happens to have.
     */
    data class Candidates(val candidates: List<GroupIceCandidate>) : GroupMediaFrame
}

/** Lays one frame out as the bytes that get sealed. */
fun encodeGroupMediaFrame(frame: GroupMediaFrame): ByteArray {
    val out = ByteArrayOutputStream()
    fun line(text: String) {
        out.write(text.toByteArray(Charsets.UTF_8))
        out.write('\n'.code)
    }
    // A length in bytes on its own line, then exactly that many bytes with no terminator: the
    // reader never has to look for a delimiter inside a body, which is what lets an SDP keep its
    // own newlines.
    fun blob(bytes: ByteArray) {
        line(bytes.size.toString())
        out.write(bytes)
    }

    line(GROUP_MEDIA_FRAME_TAG)
    when (frame) {
        is GroupMediaFrame.Description -> {
            line("sdp")
            line(frame.type)
            blob(frame.sdp.toByteArray(Charsets.UTF_8))
        }
        is GroupMediaFrame.Candidates -> {
            line("ice")
            line(frame.candidates.size.toString())
            for (candidate in frame.candidates) {
                line(candidate.sdpMid)
                line(candidate.sdpMLineIndex.toString())
                blob(candidate.candidate.toByteArray(Charsets.UTF_8))
            }
        }
    }
    return out.toByteArray()
}

/**
 * Reads one frame back, or null when the bytes are not a frame this build wrote.
 *
 * Every failure is the same failure: an unknown tag, a kind this build does not know, a count past
 * [MAX_ICE_CANDIDATES], a field that is not a number, or a body that runs past the end of the
 * frame. None of them is distinguishable to a caller and none of them should be -- all of them mean
 * "this relay is not for this build", and the caller's only honest move is to drop it.
 */
fun decodeGroupMediaFrame(bytes: ByteArray): GroupMediaFrame? {
    val cursor = FrameCursor(bytes)
    if (cursor.line() != GROUP_MEDIA_FRAME_TAG) {
        return null
    }
    return when (cursor.line()) {
        "sdp" -> {
            val type = cursor.line() ?: return null
            val sdp = cursor.blob()?.toString(Charsets.UTF_8) ?: return null
            GroupMediaFrame.Description(type, sdp)
        }
        "ice" -> {
            val count = cursor.line()?.toIntOrNull() ?: return null
            if (count < 0 || count > MAX_ICE_CANDIDATES) {
                return null
            }
            val candidates = ArrayList<GroupIceCandidate>(count)
            repeat(count) {
                val mid = cursor.line() ?: return null
                val index = cursor.line()?.toIntOrNull() ?: return null
                val candidate = cursor.blob()?.toString(Charsets.UTF_8) ?: return null
                candidates.add(GroupIceCandidate(mid, index, candidate))
            }
            GroupMediaFrame.Candidates(candidates)
        }
        else -> null
    }
}

/** A read head over one frame's bytes: whole lines, and length-prefixed bodies. */
private class FrameCursor(private val bytes: ByteArray) {
    private var at = 0

    /** The next line without its terminator, or null when the frame ends before one. */
    fun line(): String? {
        var end = -1
        for (index in at until bytes.size) {
            if (bytes[index] == '\n'.code.toByte()) {
                end = index
                break
            }
        }
        if (end < 0) {
            return null
        }
        val text = String(bytes, at, end - at, Charsets.UTF_8)
        at = end + 1
        return text
    }

    /** The next length-prefixed body, or null when its line is missing or its length overruns. */
    fun blob(): ByteArray? {
        val length = line()?.toIntOrNull() ?: return null
        if (length < 0 || length > bytes.size - at) {
            return null
        }
        val body = bytes.copyOfRange(at, at + length)
        at += length
        return body
    }
}
