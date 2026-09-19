package com.migo.app.call

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The relay codec, pinned at its edges.
 *
 * Everything that reaches [decodeGroupMediaFrame] arrives from another call member over an
 * authenticated seal, which makes it *authentic* and not *trustworthy*: the bytes are known to come
 * from a seat, and a seat is free to be running a different build, a buggy build, or a build that
 * means this device harm. So the cases that matter are the malformed ones -- a frame that half-parses
 * would hand the peer connection a description assembled from a stranger's arithmetic, and the
 * decoder's whole contract is that it prefers to drop a relay than to apply part of one.
 *
 * The round-trips matter for the mirror reason: an SDP is newlines inside a format whose structure
 * is newlines, and a codec that lost that distinction would pass every malformed-frame test above
 * while corrupting every real call.
 */
class GroupMediaFrameTest {

    private fun description(type: String, sdp: String) = GroupMediaFrame.Description(type, sdp)

    private fun candidate(mid: String, index: Int, text: String) =
        GroupIceCandidate(sdpMid = mid, sdpMLineIndex = index, candidate = text)

    // ---- the shapes that must survive the round trip -------------------------------------------------

    @Test
    fun anSdpKeepsItsOwnNewlinesThroughTheCodec() {
        // The case the length prefix exists for. A line-oriented body would end at the first of
        // these newlines and drop the rest of the description on the floor.
        val sdp = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=mid:0\r\n"
        val decoded = decodeGroupMediaFrame(encodeGroupMediaFrame(description("offer", sdp)))
        assertEquals(description("offer", sdp), decoded)
    }

    @Test
    fun aDescriptionTypeIsCarriedVerbatimRatherThanAsAnEnum() {
        // Kept as text so a description type this build has never heard of still survives the codec:
        // the plane decodes the shape it can and lets the peer connection reject the type.
        val decoded = decodeGroupMediaFrame(encodeGroupMediaFrame(description("answer", "")))
        assertEquals(description("answer", ""), decoded)
    }

    @Test
    fun aBatchOfCandidatesComesBackInOrderAndInFull() {
        val frame = GroupMediaFrame.Candidates(
            listOf(
                candidate("0", 0, "candidate:1 1 udp 2122260223 10.0.0.1 5000 typ host"),
                candidate("1", 1, "candidate:2 1 udp 1686052607 203.0.113.7 61000 typ srflx"),
                candidate("2", 2, "candidate:3 1 tcp 1518280447 10.0.0.1 9 typ host tcptype active"),
            ),
        )
        val decoded = decodeGroupMediaFrame(encodeGroupMediaFrame(frame))
        assertEquals(frame, decoded)
    }

    @Test
    fun anEmptyBatchIsAFrameAndNotAnError() {
        // A gathering run that produces nothing -- a link that never finds a path, which the caller
        // ends up reporting as a failure to connect -- still has to be sayable, and the empty batch
        // is how. It must not be confused with the truncation the checks below refuse.
        val frame = GroupMediaFrame.Candidates(emptyList())
        assertEquals(frame, decodeGroupMediaFrame(encodeGroupMediaFrame(frame)))
    }

    @Test
    fun aCandidateAtTheCountBoundIsCarried() {
        val frame = GroupMediaFrame.Candidates(
            (0 until MAX_ICE_CANDIDATES).map { candidate("0", 0, "candidate:$it") },
        )
        val decoded = decodeGroupMediaFrame(encodeGroupMediaFrame(frame))
        assertEquals(MAX_ICE_CANDIDATES, (decoded as GroupMediaFrame.Candidates).candidates.size)
    }

    // ---- the shapes that must be refused -------------------------------------------------------------

    @Test
    fun aFrameFromAnotherBuildIsRefusedByItsTag() {
        val foreign = encodeGroupMediaFrame(description("offer", "v=0")).copyOf()
        // Rewrite the tag line, keeping it exactly as long so the rest of the frame still parses:
        // the tag is what makes the refusal happen, not the damage behind it.
        val tag = GROUP_MEDIA_FRAME_TAG.toByteArray(Charsets.UTF_8)
        "migo-group-media/2".toByteArray(Charsets.UTF_8).copyInto(foreign, 0)
        assertEquals(tag.size, "migo-group-media/2".toByteArray(Charsets.UTF_8).size)
        assertNull(decodeGroupMediaFrame(foreign))
    }

    @Test
    fun aKindThisBuildDoesNotKnowIsRefused() {
        assertNull(decodeGroupMediaFrame("$GROUP_MEDIA_FRAME_TAG\ntrickle\n".toByteArray()))
    }

    @Test
    fun aCountPastTheBoundIsRefusedBeforeAnythingIsAllocatedFor() {
        // The bound exists so a frame cannot name a count that makes this device allocate for a link
        // that is already gone. The count is refused on its own line, before the loop that would
        // have built a list of that size.
        val overBound = "$GROUP_MEDIA_FRAME_TAG\nice\n${MAX_ICE_CANDIDATES + 1}\n"
        assertNull(decodeGroupMediaFrame(overBound.toByteArray()))
    }

    @Test
    fun aNegativeCountIsRefused() {
        assertNull(decodeGroupMediaFrame("$GROUP_MEDIA_FRAME_TAG\nice\n-1\n".toByteArray()))
    }

    @Test
    fun aBodyThatRunsPastTheEndOfTheFrameIsRefused() {
        // The length prefix promises more than the frame holds. Reading it anyway is the classic
        // over-read, and the cursor refuses instead -- and refuses the whole frame, so the caller
        // never sees the candidates that did parse.
        val frame = encodeGroupMediaFrame(
            GroupMediaFrame.Candidates(listOf(candidate("0", 0, "candidate:1 1 udp"))),
        )
        val lying = frame.copyOf()
        // The first digit of the body's length is the byte after the tag, the kind, the count one,
        // the mid and the index -- find it by searching for the length line that precedes it.
        val marker = "\ncandidate:1 1 udp".toByteArray(Charsets.UTF_8)
        val at = indexOf(lying, marker)
        assertTrue(at > 0)
        // One past the length line's own newline is its first digit, which is the last byte before
        // the body: widening it by one digit makes the promise outrun the frame.
        lying[at - 1] = '9'.code.toByte()
        assertNull(decodeGroupMediaFrame(lying))
    }

    @Test
    fun aTruncatedFrameIsRefused() {
        val frame = encodeGroupMediaFrame(description("offer", "v=0\r\n"))
        for (cut in 1 until frame.size) {
            assertNull(
                "a frame cut at $cut of ${frame.size} must not parse",
                decodeGroupMediaFrame(frame.copyOfRange(0, cut)),
            )
        }
    }

    @Test
    fun anEmptyFrameIsRefused() {
        assertNull(decodeGroupMediaFrame(ByteArray(0)))
    }

    @Test
    fun aFieldThatIsNotANumberIsRefused() {
        assertNull(decodeGroupMediaFrame("$GROUP_MEDIA_FRAME_TAG\nice\n1\n0\nx\n1\nabc\n".toByteArray()))
        assertNull(decodeGroupMediaFrame("$GROUP_MEDIA_FRAME_TAG\nsdp\noffer\nx\n".toByteArray()))
    }

    /** The offset of [needle] in [hay], or -1 -- the frames here are far too short for a real search. */
    private fun indexOf(hay: ByteArray, needle: ByteArray): Int {
        outer@ for (start in 0..hay.size - needle.size) {
            for (offset in needle.indices) {
                if (hay[start + offset] != needle[offset]) continue@outer
            }
            return start
        }
        return -1
    }
}
