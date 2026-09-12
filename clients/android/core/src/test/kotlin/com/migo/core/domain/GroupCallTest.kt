package com.migo.core.domain

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.crypto.Sodium
import com.migo.core.net.Inbound
import com.migo.core.net.RealtimeTransport
import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.CallEnd
import com.migo.core.protocol.CallInvite
import com.migo.core.protocol.CallSfuParticipant
import com.migo.core.protocol.CallStateEvent
import com.migo.core.protocol.CallTurnResponse
import com.migo.core.protocol.Op
import com.migo.core.protocol.TurnServer
import com.migo.core.wire.Frame
import com.migo.core.wire.Id
import com.migo.core.wire.NIL_ID
import com.migo.core.wire.Reader
import com.migo.core.wire.Writer
import com.migo.core.wire.frameHeader
import com.migo.core.wire.parseId
import java.time.Instant
import kotlin.reflect.KClass
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.BeforeClass
import org.junit.Test

/**
 * What the group-call surface is allowed to say, and what it is allowed to put on the wire.
 *
 * A port of the web suite's own pins (`clients/web/test/group-call.test.tsx`), because the group
 * call is a cross-client contract twice over: the sealed offer travels in the same version-2
 * envelope a 1:1 offer does (so one build must never see another's blob as plaintext), and the
 * roster fold observes a *server* rule -- one seat per account -- that the client only mirrors.
 * Three layers, each against the regression that would slip past a glance:
 *
 *   1. **The placeholder offer.** This build joins before its media plane has anything to say, but
 *      the wire rule is about what the *server* sees, not about whether media has landed: the join
 *      offer is a real sealed envelope, bound to its call.
 *   2. **The roster projection.** A snapshot keeps the server's join order verbatim; an arrival
 *      appends a new account or replaces a same-account seat (never a second seat); a departure
 *      removes; a departure naming this session's exact account *and* device is the seat being
 *      replaced from this account's other device -- a different fact from "the call ended", the
 *      same way section 180 keeps the 1:1 reasons apart.
 *   3. **The domain over a scripted transport.** Where the web suite drives its fake SDK, this
 *      suite drives the real [GroupCallsDomain] through a fake [RealtimeTransport] -- the seam the
 *      domains are built on, which is what makes the group-call signaling testable without a
 *      second phone: the join frame is the 1:1 invite's shape with group semantics, the leave is
 *      the 1:1 end frame, one opcode carries all three event shapes and the domain classifies
 *      them, and a frame no shape can render goes to the error sink instead of a handler.
 *
 * The AEAD half needs real libsodium, and the Android artifact's native library only loads on a
 * device, so the suite injects the desktop handle through [Sodium.overrideForTesting] -- the same
 * C code the device runs, loaded for the host JVM.
 */
class GroupCallTest {
    companion object {
        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
            Sodium.overrideForTesting(LazySodiumJava(SodiumJava()))
        }

        // The same fixture names the web suite uses, so the two files read side by side.

        private val ME: Id = parseId("0123456789ABCDEFGHJKMNPQRV")
        private val ME_DEVICE: Id = parseId("0123456789ABCDEFGHJKMNPQRW")
        private val ME_LAPTOP: Id = parseId("0123456789ABCDEFGHJKMNPQRX")
        private val ADA: Id = parseId("0123456789ABCDEFGHJKMNPQRY")
        private val ADA_LAPTOP: Id = parseId("0123456789ABCDEFGHJKMNPQRZ")
        private val ADA_PHONE: Id = parseId("0123456789ABCDEFGHJKMNPQ22")
        private val BEN: Id = parseId("0123456789ABCDEFGHJKMNPQ23")
        private val BEN_PHONE: Id = parseId("0123456789ABCDEFGHJKMNPQ24")
        private val CALL: Id = parseId("0123456789ABCDEFGHJKMNPQRS")
        private val OTHER_CALL: Id = parseId("0123456789ABCDEFGHJKMNPQRT")
        private val CONVERSATION: Id = parseId("0123456789ABCDEFGHJKMNPQRU")

        private val NOW: Long = Instant.parse("2026-09-12T12:00:00Z").toEpochMilli()

        /** The blob a participant's line carries: opaque bytes the client never opens. */
        private val SEALED = byteArrayOf(2, 7, 9)

        private fun participant(userId: Id, deviceId: Id, joinedAt: Long): CallSfuParticipant =
            CallSfuParticipant(userId = userId, deviceId = deviceId, joinedAt = joinedAt, sealedOffer = SEALED)

        private fun snapshotEvent(participants: List<CallSfuParticipant>): CallStateEvent =
            CallStateEvent(
                callId = CALL,
                state = CallState.Connected.wire,
                conversationId = CONVERSATION,
                userId = ME,
                deviceId = ME_DEVICE,
                participantCount = participants.size.toLong(),
                participants = participants,
            )

        private fun joinedEvent(
            userId: Id = BEN,
            deviceId: Id = BEN_PHONE,
            participantCount: Long = 3,
        ): CallStateEvent = CallStateEvent(
            callId = CALL,
            state = CallState.Connected.wire,
            conversationId = CONVERSATION,
            userId = userId,
            deviceId = deviceId,
            participantCount = participantCount,
            sealedOffer = SEALED,
        )

        private fun leftEvent(
            userId: Id = ADA,
            deviceId: Id = ADA_LAPTOP,
            participantCount: Long = 2,
        ): CallStateEvent = CallStateEvent(
            callId = CALL,
            state = CallState.Ended.wire,
            reason = CallEndReason.ByCaller.wire,
            conversationId = CONVERSATION,
            userId = userId,
            deviceId = deviceId,
            participantCount = participantCount,
        )

        /**
         * The exception assertion, on the KClass rather than a reified parameter: a reified catch
         * (`catch (expected: T)`) is prohibited in Kotlin, and the call site passes the class
         * through (`CallSignalFormatException::class`), so this shape is the one that reads the
         * same at every site. The same helper [CallSignalTest] carries.
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

    // --- the placeholder offer ---

    @Test
    fun `the join offer is a real sealed envelope, even though it is a placeholder`() {
        val key = generateCallKey()
        val sealed = placeholderSealedOffer(key, CALL)

        // Version 2, the house AEAD's own output behind it -- the same envelope a media-bearing
        // offer travels in, so the server (and every other participant) sees nothing but opaque
        // bytes from the very first frame of the call.
        assertEquals(2.toByte(), sealed[0])
        val opened = decodeSdpDescription(openCallSignal(sealed, key, CALL))
        assertEquals("offer", opened.type)
        assertEquals("the placeholder carries an empty description, honestly", "", opened.sdp)
        // Bound to its call, like every sealed call signal: it must not open as another call's.
        assertThrows(
            "the seal must not open as another call's",
            CallSignalFormatException::class,
        ) { openCallSignal(sealed, key, OTHER_CALL) }
    }

    // --- the roster projection ---

    @Test
    fun `the snapshot builds the roster in the join order the server sent`() {
        val roster = snapshotEvent(
            listOf(
                participant(ADA, ADA_LAPTOP, NOW - 30_000),
                participant(ME, ME_DEVICE, NOW - 60_000),
            ),
        )
        val seats = seatsFromSnapshot(roster.participants!!)
        assertEquals(
            "the snapshot's order is kept verbatim, not re-derived",
            listOf(ADA, ME),
            seats.map { it.userId },
        )
        assertEquals(ADA_LAPTOP, seats[0].deviceId)
        assertEquals(NOW - 30_000, seats[0].joinedAt)
    }

    @Test
    fun `an arrival appends a new account and replaces a same-account seat, never a second seat`() {
        val seats = seatsFromSnapshot(
            listOf(
                participant(ME, ME_DEVICE, NOW - 60_000),
                participant(ADA, ADA_LAPTOP, NOW - 30_000),
            ),
        )

        // A new account: appended at the end, in join order.
        val withBen = seatArrived(seats, BEN, BEN_PHONE, NOW)
        assertEquals(listOf(ME, ADA, BEN), withBen.map { it.userId })

        // The same account on a new device: one seat per account, so the seat is replaced -- and
        // the replacement is a fresh join, so it takes the end of the join order.
        val adaMoved = seatArrived(withBen, ADA, ADA_PHONE, NOW)
        assertEquals(
            listOf("$ME:$ME_DEVICE", "$BEN:$BEN_PHONE", "$ADA:$ADA_PHONE"),
            adaMoved.map { "${it.userId}:${it.deviceId}" },
        )
    }

    @Test
    fun `a departure removes the seat; a count of zero retires the call`() {
        val seats = seatsFromSnapshot(
            listOf(
                participant(ME, ME_DEVICE, NOW - 60_000),
                participant(ADA, ADA_LAPTOP, NOW - 30_000),
            ),
        )
        val withoutAda = seatDeparted(seats, ADA)
        assertEquals(listOf(ME), withoutAda.map { it.userId })
        assertTrue("the last seat retired the call", isCallRetired(0L))
        assertEquals(false, isCallRetired(1L))
    }

    @Test
    fun `only the exact account and device names this own seat`() {
        assertTrue(namesOwnSeat(ME, ME_DEVICE, ME, ME_DEVICE))
        // Same account, another device: that seat's movement is roster news, not this screen's end.
        assertEquals(false, namesOwnSeat(ME, ME_LAPTOP, ME, ME_DEVICE))
        assertEquals(false, namesOwnSeat(ADA, ADA_LAPTOP, ME, ME_DEVICE))
    }

    @Test
    fun `the four notes stay distinct, none a bare call ended`() {
        val labels = GroupCallNote.entries.map { groupCallNoteLabel(it) }.toSet()
        assertEquals("every note is a different fact", 4, labels.size)
        assertEquals("You left the call", groupCallNoteLabel(GroupCallNote.Left))
        assertEquals("The call ended", groupCallNoteLabel(GroupCallNote.Ended))
        assertEquals("Continued on another device", groupCallNoteLabel(GroupCallNote.Moved))
        assertEquals("Connection lost", groupCallNoteLabel(GroupCallNote.Connection))
    }

    // --- the domain, over a scripted transport ---

    @Test
    fun `a join sends the invite shape with group semantics and resolves with the relays`() {
        val fake = FakeTransport(reply = { w -> CallTurnResponse(listOf(RELAY)).encode(w) })
        val domain = GroupCallsDomain(Rpc(fake), ME_DEVICE)
        val sealed = placeholderSealedOffer(generateCallKey(), CALL)

        val result = runBlocking { domain.join(CONVERSATION, CallMediaKind.Audio, sealed, CALL) }

        assertEquals(CALL, result.callId)
        assertEquals(listOf(RELAY), result.servers)

        // One frame went out, and it is the SFU join opcode carrying the 1:1 invite's struct.
        val sent = fake.sent.single()
        assertEquals(Op.CALL_SFU_JOIN, sent.first)
        val request = CallInvite.decode(Reader(sent.second))
        assertEquals(CALL, request.callId)
        assertEquals("a group call has no single callee", NIL_ID, request.calleeId)
        assertEquals(CONVERSATION, request.conversationId)
        assertEquals(ME_DEVICE, request.callerDevice)
        assertEquals(CallMediaKind.Audio.wire, request.mediaKind)
        assertTrue("the sealed offer passes through verbatim", request.sealedOffer.contentEquals(sealed))
    }

    @Test
    fun `a leave is the 1:1 end frame stamped with the caller-withdrew reason`() {
        val fake = FakeTransport(reply = { w -> Acknowledged(ok = true).encode(w) })
        val domain = GroupCallsDomain(Rpc(fake), ME_DEVICE)

        runBlocking { domain.leave(CALL) }

        val sent = fake.sent.single()
        assertEquals(Op.CALL_END, sent.first)
        val request = CallEnd.decode(Reader(sent.second))
        assertEquals(CALL, request.callId)
        assertEquals(CallEndReason.ByCaller.wire, request.reason)
    }

    @Test
    fun `one opcode carries all three event shapes, and the domain hands each to its own listener`() {
        val fake = FakeTransport()
        val sink = ArrayList<Pair<Long, Throwable>>()
        val rpc = Rpc(fake) { opcode, cause -> sink += opcode to cause }
        val domain = GroupCallsDomain(rpc, ME_DEVICE) { opcode, cause -> sink += opcode to cause }
        val rosters = ArrayList<GroupCallRoster>()
        val joins = ArrayList<GroupCallJoinedEvent>()
        val departures = ArrayList<GroupCallLeftEvent>()
        domain.onRoster { rosters += it }
        domain.onParticipantJoined { joins += it }
        domain.onParticipantLeft { departures += it }
        domain.start()

        // The roster snapshot, the join announcement, the departure -- all CALL_SFU_EVENT.
        fake.event(snapshotEvent(listOf(participant(ADA, ADA_LAPTOP, NOW - 30_000), participant(ME, ME_DEVICE, NOW - 60_000))))
        fake.event(joinedEvent())
        fake.event(leftEvent())
        fake.inbound.close()
        runBlocking { rpc.deliver() }

        assertEquals(1, rosters.size)
        val roster = rosters[0]
        assertEquals(CALL, roster.callId)
        assertEquals(CONVERSATION, roster.conversationId)
        assertEquals(ME, roster.userId)
        assertEquals(ME_DEVICE, roster.deviceId)
        assertEquals(2L, roster.participantCount)
        assertEquals("the snapshot keeps the server's order", listOf(ADA, ME), roster.participants.map { it.userId })

        assertEquals(1, joins.size)
        val joined = joins[0]
        assertEquals(BEN, joined.userId)
        assertEquals(3L, joined.participantCount)
        assertTrue("the joiner's sealed offer passes through unopened", joined.sealedOffer!!.contentEquals(SEALED))

        assertEquals(1, departures.size)
        val departed = departures[0]
        assertEquals(ADA, departed.userId)
        assertEquals(2L, departed.participantCount)

        assertTrue("a well-formed frame never reaches the error sink", sink.isEmpty())
    }

    @Test
    fun `a frame no shape can render goes to the error sink, and a shape this version does not know is dropped quietly`() {
        val fake = FakeTransport()
        val sink = ArrayList<Pair<Long, Throwable>>()
        val rpc = Rpc(fake)
        val domain = GroupCallsDomain(rpc, ME_DEVICE) { opcode, cause -> sink += opcode to cause }
        val rosters = ArrayList<GroupCallRoster>()
        val joins = ArrayList<GroupCallJoinedEvent>()
        val departures = ArrayList<GroupCallLeftEvent>()
        domain.onRoster { rosters += it }
        domain.onParticipantJoined { joins += it }
        domain.onParticipantLeft { departures += it }
        domain.start()

        // A join announcement missing the fields no shape can render without: the error sink, not
        // a handler that would then have to guess.
        fake.event(
            CallStateEvent(
                callId = CALL,
                state = CallState.Connected.wire,
                userId = BEN,
                deviceId = BEN_PHONE,
            ),
        )
        // A state this version has no group meaning for: dropped quietly, the same forward
        // tolerance the Rpc decoder shows an unknown optional field.
        fake.event(CallStateEvent(callId = CALL, state = CallState.Reconnecting.wire))
        fake.inbound.close()
        runBlocking { rpc.deliver() }

        assertTrue("no handler saw either frame", rosters.isEmpty() && joins.isEmpty() && departures.isEmpty())
        assertEquals("exactly the unrenderable frame reached the sink", 1, sink.size)
        assertEquals(Op.CALL_SFU_EVENT, sink[0].first)
        assertNotNull(sink[0].second)
    }
}

/** One scripted TURN relay, the same for every reply that wants one. */
private val RELAY = TurnServer(
    url = "turn:turn.example.net:3478",
    username = "user",
    credential = "credential",
    ttlSeconds = 60,
    region = "",
)

/**
 * A [RealtimeTransport] that sends nothing anywhere: every request is answered from a script, every
 * event is pushed by the test, and the sent frames are kept for asserting on. The seam the domains
 * are built on -- they speak this interface, never a socket -- is what makes the group-call
 * signaling testable without a server, the same way the web suite's fake SDK is.
 *
 * [drain][Rpc.deliver] is the test's to run, on the same [Rpc] the domain subscribed through: the
 * channel is closed before the pump starts, so draining runs to completion deterministically and
 * every handler has fired by the time `runBlocking` returns.
 */
private class FakeTransport(
    /** Encodes the payload every `request` is answered with. */
    private val reply: ((Writer) -> Unit)? = null,
) : RealtimeTransport {
    /** What went out, in order: the opcode and its encoded payload. */
    val sent = ArrayList<Pair<Long, ByteArray>>()

    override val inbound = Channel<Inbound>(Channel.UNLIMITED)

    override val sessionId: Id = NIL_ID
    override val lastFrameSeq: Long = 0L

    private var nextCorrelation = 1L

    override fun correlate(): Long = nextCorrelation++

    override suspend fun send(opcode: Long, correlation: Long, encode: (Writer) -> Unit) {
        sent += opcode to encodeToPayload(encode)
    }

    override suspend fun request(opcode: Long, encode: (Writer) -> Unit): Frame {
        sent += opcode to encodeToPayload(encode)
        val answer = reply ?: throw IllegalStateException("no reply scripted for opcode $opcode")
        return Frame(frameHeader(opcode), encodeToPayload(answer))
    }

    override suspend fun acknowledge() {}

    override fun close() {}

    /** Pushes one server event, as the gateway would hand it to the Rpc pump. */
    fun event(payload: CallStateEvent) {
        val frame = Frame(frameHeader(Op.CALL_SFU_EVENT), encodeToPayload { payload.encode(it) })
        inbound.trySend(Inbound(frame))
    }

    private fun encodeToPayload(encode: (Writer) -> Unit): ByteArray {
        val writer = Writer()
        encode(writer)
        return writer.finish()
    }
}
