package com.migo.core.domain

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.crypto.Sodium
import com.migo.core.protocol.CallInviteEvent
import com.migo.core.protocol.CallStateEvent
import com.migo.core.wire.Id
import com.migo.core.wire.NIL_ID
import com.migo.core.wire.parseId
import java.time.Instant
import kotlin.reflect.KClass
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.BeforeClass
import org.junit.Test

/**
 * The pure halves of the call stack, pinned the way the web suite pins them
 * (`clients/web/test/calls.test.tsx`) — and several of these cases are not this module's word
 * against itself but this module's word against the *web build's*, because the seal is a
 * cross-client contract: an SDP blob sealed by one build must open on the other, which pins the
 * envelope shape (`2 || nonce || ciphertext || tag`), the AEAD domain string
 * (`migo-call-signal:<id>`), and the call-key event's 16+32 layout exactly.
 *
 * The AEAD half needs real libsodium, and the Android artifact's native library only loads on a
 * device, so the suite injects the desktop handle through [Sodium.overrideForTesting] — the same C
 * code the device runs, loaded for the host JVM.
 *
 * What is deliberately *not* here: anything that needs a peer connection, an audio track, or an
 * [com.migo.core.domain.Rpc]. The seal and the words are pure functions precisely so this file can
 * hold every case that would otherwise need a second phone.
 */
class CallSignalTest {
    companion object {
        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
            Sodium.overrideForTesting(LazySodiumJava(SodiumJava()))
        }

        /** The same id text the web suite uses, so the AEAD domain bytes are byte-identical. */
        private val CALL: Id = parseId("0123456789ABCDEFGHJKMNPQRS")

        /** A second, different call id — for seals that must not open across calls. */
        private val OTHER_CALL: Id = parseId("0123456789ABCDEFGHJKMNPQRT")

        /** The same instant the web suite uses, so the expiry arithmetic cases line up. */
        private val NOW: Long = Instant.parse("2026-08-30T12:00:00Z").toEpochMilli()

        private val KEY = ByteArray(32) { (it + 1).toByte() }
        private val WRONG_KEY = ByteArray(32) { (it + 65).toByte() }
        private val PAYLOAD = "v=0\r\no=- 46117317 2 IN IP4 127.0.0.1\r\n".encodeToByteArray()

        /** The version-2 envelope's fixed overhead: version byte, 24-byte nonce, 16-byte tag. */
        private const val V2_OVERHEAD = 1 + 24 + 16

        /** The legacy envelope's prefix: version byte, 32-byte key slot, 12-byte nonce slot. */
        private const val LEGACY_PREFIX = 1 + 32 + 12

        private fun inviteEvent(
            callId: Id = CALL,
            expiresAt: Long = NOW + 45_000,
        ): CallInviteEvent = CallInviteEvent(
            callId = callId,
            conversationId = NIL_ID,
            callerId = NIL_ID,
            callerDevice = NIL_ID,
            mediaKind = 0,
            expiresAt = expiresAt,
            sealedOffer = ByteArray(0),
        )

        private fun stateEvent(callId: Id = CALL, state: Long, reason: Long? = null): CallStateEvent =
            CallStateEvent(callId = callId, state = state, reason = reason)

        /** Whether [other] appears in [this] as a contiguous run of bytes. */
        private fun ByteArray.containsRun(other: ByteArray): Boolean {
            if (other.isEmpty()) return true
            if (size < other.size) return false
            outer@ for (i in 0..size - other.size) {
                for (j in other.indices) {
                    if (this[i + j] != other[j]) continue@outer
                }
                return true
            }
            return false
        }

        /**
         * The exception assertion, on the KClass rather than a reified parameter: a reified catch
         * (`catch (expected: T)`) is prohibited in Kotlin, and the call sites all pass the class
         * through (`CallSignalFormatException::class`), so this shape is the one that reads the
         * same at every site.
         */
        private fun assertThrows(what: String, type: KClass<out Throwable>, block: () -> Unit) {
            try {
                block()
                fail("$what: expected ${type.simpleName}")
            } catch (expected: Throwable) {
                if (!type.isInstance(expected)) {
                    fail("$what: expected ${type.simpleName}, got ${expected::class.simpleName}")
                }
            }
        }
    }

    // --- the seal ---

    @Test
    fun `a sealed signal is the version byte then the AEAD output, and opens back to its payload`() {
        val sealed = sealCallSignal(PAYLOAD, KEY, CALL)
        assertEquals("the envelope is version || nonce || ciphertext || tag", V2_OVERHEAD + PAYLOAD.size, sealed.size)
        assertEquals(2.toByte(), sealed[0])
        assertTrue(
            "the payload never rides in the clear",
            !sealed.containsRun(PAYLOAD),
        )
        assertTrue(PAYLOAD.contentEquals(openCallSignal(sealed, KEY, CALL)))
    }

    @Test
    fun `each seal uses a fresh nonce, so two seals of one payload differ`() {
        val first = sealCallSignal(PAYLOAD, KEY, CALL)
        val second = sealCallSignal(PAYLOAD, KEY, CALL)
        assertNotEquals(first.toList(), second.toList())
        assertTrue(PAYLOAD.contentEquals(openCallSignal(first, KEY, CALL)))
        assertTrue(PAYLOAD.contentEquals(openCallSignal(second, KEY, CALL)))
    }

    @Test
    fun `a seal refuses to open under the wrong key, for the wrong call, or after an edit`() {
        val sealed = sealCallSignal(PAYLOAD, KEY, CALL)
        assertThrows("wrong key", CallSignalFormatException::class) { openCallSignal(sealed, WRONG_KEY, CALL) }
        assertThrows("wrong call id", CallSignalFormatException::class) { openCallSignal(sealed, KEY, OTHER_CALL) }
        val edited = sealed.copyOf()
        edited[sealed.size / 2] = (edited[sealed.size / 2] + 1).toByte()
        assertThrows("edited byte", CallSignalFormatException::class) { openCallSignal(edited, KEY, CALL) }
    }

    @Test
    fun `an envelope this build cannot read throws rather than returning nonsense`() {
        assertThrows("empty", CallSignalFormatException::class) { openCallSignal(ByteArray(0), KEY, CALL) }
        assertThrows("too short to hold its own header", CallSignalFormatException::class) {
            openCallSignal(ByteArray(10) { if (it == 0) 2 else 0 }, KEY, CALL)
        }
        assertThrows("version 99", CallSignalFormatException::class) {
            openCallSignal(byteArrayOf(99) + ByteArray(V2_OVERHEAD + PAYLOAD.size), KEY, CALL)
        }
        // Version 3 is *reserved*, not legacy: a future envelope is refused, not misread.
        assertThrows("future version 3", CallSignalFormatException::class) {
            openCallSignal(byteArrayOf(3) + ByteArray(V2_OVERHEAD + PAYLOAD.size), KEY, CALL)
        }
    }

    @Test
    fun `a legacy version-1 envelope opens under any key, because it was never encrypted`() {
        val legacy = ByteArray(LEGACY_PREFIX) { if (it == 0) 1 else 0 } + PAYLOAD
        assertTrue(PAYLOAD.contentEquals(openCallSignal(legacy, KEY, CALL)))
        assertTrue(PAYLOAD.contentEquals(openCallSignal(legacy, WRONG_KEY, OTHER_CALL)))
        // Even the legacy envelope must be long enough for its own framing slots.
        assertThrows("shorter than its own header", CallSignalFormatException::class) {
            openCallSignal(byteArrayOf(1) + ByteArray(10), KEY, CALL)
        }
    }

    // --- the call key's channel ---

    @Test
    fun `a call-key event is 16 id bytes then 32 key bytes, and round-trips`() {
        val event = encodeCallKeyEvent(CALL, KEY)
        assertEquals(48, event.size)
        val decoded = decodeCallKeyEvent(event)
        assertEquals(CALL, decoded?.first)
        assertTrue(KEY.contentEquals(decoded?.second))
    }

    @Test
    fun `a call-key event of the wrong width is dropped, not thrown`() {
        assertNull(decodeCallKeyEvent(ByteArray(16)))
        assertNull(decodeCallKeyEvent(ByteArray(49)))
    }

    @Test
    fun `a minted call key is 32 random bytes, and two mints differ`() {
        val a = generateCallKey()
        val b = generateCallKey()
        assertEquals(32, a.size)
        assertNotEquals(a.toList(), b.toList())
    }

    // --- the sealed payloads ---

    @Test
    fun `an SDP description round-trips through the sealed payload's JSON shape`() {
        val description = SdpDescription(type = "offer", sdp = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n")
        val bytes = encodeSdpDescription(description)
        assertEquals(description, decodeSdpDescription(bytes))
    }

    @Test
    fun `an ICE batch round-trips, and its JSON field names match the web build's`() {
        val batch = listOf(
            IceCandidateJson(
                candidate = "candidate:1 1 UDP 2130706431 192.168.1.4 8998 typ host",
                sdpMid = "0",
                sdpMLineIndex = 0,
            ),
            // A candidate end-of-gathering notification: neither field, just the sentinel.
            IceCandidateJson(),
        )
        val bytes = encodeIceBatch(batch)
        assertEquals(batch, decodeIceBatch(bytes))
        val text = bytes.decodeToString()
        assertTrue("the field names are the cross-client contract", text.contains("\"sdpMLineIndex\""))
        assertTrue("an empty candidate stays null, not the string \"null\"", !text.contains("\"candidate\":null"))
    }

    @Test
    fun `bytes that are not the payload they claim to be throw, not parse`() {
        assertThrows("not an SDP description", CallSignalFormatException::class) {
            decodeSdpDescription("not json".encodeToByteArray())
        }
        assertThrows("an SDP object where a batch belongs", CallSignalFormatException::class) {
            decodeIceBatch("""{"type":"offer","sdp":"v=0"}""".encodeToByteArray())
        }
    }

    // --- the words and numbers ---

    @Test
    fun `a call duration reads M-SS with minutes unbounded and negative time floored`() {
        assertEquals("0:00", formatCallDuration(0))
        assertEquals("1:23", formatCallDuration(83_000))
        assertEquals("1:05", formatCallDuration(65_000))
        assertEquals("61:01", formatCallDuration(3_661_000))
        assertEquals("0:00", formatCallDuration(-5_000))
    }

    @Test
    fun `a wire state narrows or refuses, and degraded is a display judgement not a wire fact`() {
        assertEquals(CallState.Ringing, CallState.fromWire(0))
        assertEquals(CallState.Connected, CallState.fromWire(2))
        assertEquals(CallState.Ended, CallState.fromWire(4))
        assertNull("an unknown wire state is never a guess", CallState.fromWire(9))
        assertEquals(CallDisplayState.Connected, displayStateOf(CallState.Connected, degraded = false))
        assertEquals(CallDisplayState.Degraded, displayStateOf(CallState.Connected, degraded = true))
        assertEquals(CallDisplayState.Ringing, displayStateOf(CallState.Ringing, degraded = true))
        assertEquals("Connecting…", callStateLabel(CallDisplayState.Connecting))
    }

    @Test
    fun `every end reason has its own line, and a blocked invite says Unavailable`() {
        assertEquals("Declined", endReasonLabel(CallEndReason.Declined))
        assertEquals("No answer", endReasonLabel(CallEndReason.NoAnswer))
        assertEquals("Failed to connect", endReasonLabel(CallEndReason.Failed))
        assertEquals("Connection lost", endReasonLabel(CallEndReason.Network))
        assertEquals("Busy", endReasonLabel(CallEndReason.Busy))
        assertEquals("Call ended", endReasonLabel(null))
        // The reason enum has no Blocked member: the wire drew the distinction in the invite
        // status, and the screen must not state a human refusal that never happened.
        assertEquals("Unavailable", endedReasonLine(inviteStatus = INVITE_BLOCKED, endReason = null))
        assertEquals("Declined", endedReasonLine(inviteStatus = INVITE_DECLINED, endReason = null))
        assertEquals("No answer", endedReasonLine(inviteStatus = null, endReason = CallEndReason.NoAnswer))
    }

    @Test
    fun `an invite that never rang maps to an end reason, and a media kind degrades to audio`() {
        assertEquals(CallEndReason.NoAnswer, inviteEndReason(INVITE_EXPIRED))
        assertEquals(CallEndReason.Busy, inviteEndReason(INVITE_BUSY))
        assertEquals(CallEndReason.Declined, inviteEndReason(INVITE_DECLINED))
        assertEquals(CallEndReason.Declined, inviteEndReason(INVITE_BLOCKED))
        assertEquals(CallMediaKind.Video, CallMediaKind.fromWire(1))
        assertEquals(CallMediaKind.Audio, CallMediaKind.fromWire(0))
        assertEquals("an unknown media kind is the call as audio, not no call", CallMediaKind.Audio, CallMediaKind.fromWire(7))
        assertEquals("voice call", mediaKindLabel(CallMediaKind.Audio))
        assertEquals("video call", mediaKindLabel(CallMediaKind.Video))
    }

    // --- the ring's lifecycle ---

    @Test
    fun `the local ring mirror clamps at zero so a late reply fires at once`() {
        assertEquals(45_000, ringTimeoutMs(expiresAt = NOW + 45_000, now = NOW))
        assertEquals(0, ringTimeoutMs(expiresAt = NOW - 1_000, now = NOW))
    }

    @Test
    fun `a sibling device answering retires the ring without ending the call`() {
        val ringing = CALL
        assertTrue(answersRingingCall(stateEvent(ringing, state = 1), ringing))
        assertTrue(answersRingingCall(stateEvent(ringing, state = 2), ringing))
        assertTrue("a Ringing transition is not an answer", !answersRingingCall(stateEvent(ringing, state = 0), ringing))
        assertTrue("another call's state is not ours", !answersRingingCall(stateEvent(OTHER_CALL, state = 2), ringing))
        assertTrue("no ring tracked, nothing to retire", !answersRingingCall(stateEvent(ringing, state = 2), null))
        assertTrue(endsRingingCall(stateEvent(ringing, state = 4), ringing))
        assertTrue("a live transition does not end the ring", !endsRingingCall(stateEvent(ringing, state = 3), ringing))
        assertTrue("another call's end is not ours", !endsRingingCall(stateEvent(OTHER_CALL, state = 4), ringing))
    }

    @Test
    fun `an inbound invite is placed against this device's occupancy`() {
        // Expired in flight: rings nobody, declines nobody.
        assertEquals(
            IncomingInviteDisposition.Ignore,
            incomingInviteDisposition(inviteEvent(expiresAt = NOW), ringingCallId = null, activeCallId = null, busy = false, now = NOW),
        )
        // The call already ringing, or already answered on this device: a redelivery, never news.
        assertEquals(
            IncomingInviteDisposition.Ignore,
            incomingInviteDisposition(inviteEvent(), ringingCallId = CALL, activeCallId = null, busy = false, now = NOW),
        )
        assertEquals(
            IncomingInviteDisposition.Ignore,
            incomingInviteDisposition(inviteEvent(), ringingCallId = null, activeCallId = CALL, busy = false, now = NOW),
        )
        // A different call while this device is occupied — by a call in progress or a ring already
        // showing — is answered Busy, which stops the new caller's ring without implying a refusal.
        assertEquals(
            IncomingInviteDisposition.DeclineBusy,
            incomingInviteDisposition(inviteEvent(OTHER_CALL), ringingCallId = null, activeCallId = CALL, busy = false, now = NOW),
        )
        assertEquals(
            IncomingInviteDisposition.DeclineBusy,
            incomingInviteDisposition(inviteEvent(OTHER_CALL), ringingCallId = CALL, activeCallId = null, busy = false, now = NOW),
        )
        assertEquals(
            IncomingInviteDisposition.DeclineBusy,
            incomingInviteDisposition(inviteEvent(), ringingCallId = null, activeCallId = null, busy = true, now = NOW),
        )
        // Fresh and free: ring.
        assertEquals(
            IncomingInviteDisposition.Ring,
            incomingInviteDisposition(inviteEvent(), ringingCallId = null, activeCallId = null, busy = false, now = NOW),
        )
    }
}
