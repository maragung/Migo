package com.migo.core.domain

import com.migo.core.net.Inbound
import com.migo.core.net.RealtimeTransport
import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.ModerationEvent
import com.migo.core.protocol.Op
import com.migo.core.protocol.ReportFile
import com.migo.core.wire.Frame
import com.migo.core.wire.Id
import com.migo.core.wire.NIL_ID
import com.migo.core.wire.Reader
import com.migo.core.wire.Writer
import com.migo.core.wire.frameHeader
import com.migo.core.wire.parseId
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * What a client is allowed to put on the wire when it points the warden at something.
 *
 * A port of the web suite's own pins (`packages/sdk/test/moderation.test.ts`), because the report
 * path is a cross-client contract twice over and both halves are the kind that a glance would call
 * correct:
 *
 *   1. **The numbering.** The subject kinds are the *wire's* numbers, and they are deliberately not
 *      the storage numbers the same words have on the other side: `report.subject_kind` in the schema
 *      numbers a media object 3 and a bot 4, while `REPORT_CREATE` numbers a bot 3 and has no media
 *      kind at all. A client that echoed the storage numbering would file every bot report as a report
 *      about a media object, and the queue would show a bot complaint against a picture. Nothing on
 *      the wire would look wrong.
 *   2. **What a report carries.** A report is a pointer: a kind, an id, a reason code, and the
 *      reporter's own note. It carries no message body, no attachment, and -- the assertion that
 *      matters most here -- *no conversation id*, because the message report's single id is all the
 *      frozen `ReportFile` has room for and a client that supplied a second one would be promising a
 *      precision the protocol does not have.
 *   3. **The refusal that must happen locally.** The note ceiling is checked before anything is sent,
 *      because a refusal that arrives after the reporter typed the whole thing has already cost them
 *      the typing on a call the registry prices.
 *
 * The domain is driven over a fake [RealtimeTransport], the seam the domains are built on -- the same
 * arrangement `GroupCallTest` uses, and the reason any of this is testable without a node.
 */
class ModerationTest {
    companion object {
        private val ADA: Id = parseId("0123456789ABCDEFGHJKMNPQRY")
        private val MESSAGE: Id = parseId("0123456789ABCDEFGHJKMNPQ25")
        private val ROOM: Id = parseId("0123456789ABCDEFGHJKMNPQ26")
        private val BOT: Id = parseId("0123456789ABCDEFGHJKMNPQ27")
        private val CASE: Id = parseId("0123456789ABCDEFGHJKMNPQ28")
    }

    // --- the numbering the wire actually reads ---

    @Test
    fun `the four subject kinds are the wire's numbers, and a bot is not the store's media object`() {
        assertEquals(0L, ReportSubject.User.wire)
        assertEquals(1L, ReportSubject.Message.wire)
        assertEquals(2L, ReportSubject.Room.wire)
        assertEquals(3L, ReportSubject.Bot.wire)

        // The store numbers a media object 3 and a bot 4 (`report.subject_kind` in the schema).
        // Bot holding 4 here would be that collision; media having any kind here would be inventing
        // a subject the wire cannot carry.
        assertEquals("a bot is reported as a bot, which the wire numbers 3", 3L, ReportSubject.Bot.wire)
        assertEquals(
            "four kinds, and no media kind among them -- the wire has none",
            4,
            ReportSubject.entries.size,
        )
    }

    @Test
    fun `a reason code is never renumbered, and the catch-all stays last`() {
        val offered = ReportReason.entries
        assertEquals("the node's reason vocabulary is thirteen codes", 13, offered.size)
        assertEquals(listOf(0L, 1L, 2L, 3L, 4L, 5L, 6L, 7L, 8L, 9L, 10L, 11L, 12L), offered.map { it.wire })
        assertEquals("codes are unique", 13, offered.map { it.wire }.toSet().size)

        // BotAbuse 11 before Other 12: a renumbering that swapped them would silently rewrite the
        // meaning of every report already sitting in a queue.
        assertEquals(ReportReason.BotAbuse, offered[11])
        assertEquals(ReportReason.Other, offered.last())
        assertEquals("Other is the fallback and must be the largest code", offered.maxOf { it.wire }, ReportReason.Other.wire)

        // ChildSafety keeps its own code rather than folding into SexualContent: the obligations
        // attached to it are not the same, and an operator filters the queue for exactly it.
        assertTrue(
            "child safety is its own code, not a synonym for sexual content",
            ReportReason.ChildSafety.wire != ReportReason.SexualContent.wire,
        )
    }

    // --- what a report actually carries ---

    @Test
    fun `a message report carries one id and no conversation`() {
        val fake = ScriptedReportTransport(reply = { w -> Acknowledged(ok = true).encode(w) })
        val domain = ModerationDomain(Rpc(fake))

        runBlocking {
            domain.reportMessage(MESSAGE, ReportReason.MaliciousLink, "the link goes to a credential harvester")
        }

        val sent = fake.sent.single()
        assertEquals(Op.REPORT_CREATE, sent.first)
        // One reader, decoded and then measured. Measuring a second, fresh Reader would report the
        // whole payload as left over, because it has read nothing -- a passing-looking line that
        // can never fail, which is worse than no line at all.
        val reader = Reader(sent.second)
        val request = ReportFile.decode(reader)
        assertEquals(ReportSubject.Message.wire, request.subjectKind)
        assertEquals(MESSAGE, request.subjectId)
        assertEquals(ReportReason.MaliciousLink.wire, request.reason)
        assertEquals("the reporter's own words travel unaltered", "the link goes to a credential harvester", request.note)

        // The whole payload is those four fields and nothing else: a message report is one id, so
        // there is no second id on the frame for a conversation to hide in.
        assertEquals("the frame is the struct and nothing after it", 0, reader.remaining)
    }

    @Test
    fun `a report with no note says so rather than sending an empty one`() {
        val fake = ScriptedReportTransport(reply = { w -> Acknowledged(ok = true).encode(w) })
        val domain = ModerationDomain(Rpc(fake))

        runBlocking { domain.reportUser(ADA, ReportReason.Impersonation) }

        val request = ReportFile.decode(Reader(fake.sent.single().second))
        assertEquals(ReportSubject.User.wire, request.subjectKind)
        assertEquals(ADA, request.subjectId)
        assertEquals(ReportReason.Impersonation.wire, request.reason)
        assertEquals("absent, not empty -- an empty note is a note the reporter wrote", null, request.note)
    }

    @Test
    fun `each helper points at its own kind`() {
        val user = kindOf { it.reportUser(ADA, ReportReason.Harassment) }
        assertEquals(ReportSubject.User.wire, user.subjectKind)
        assertEquals(ADA, user.subjectId)

        val message = kindOf { it.reportMessage(MESSAGE, ReportReason.Harassment) }
        assertEquals(ReportSubject.Message.wire, message.subjectKind)
        assertEquals(MESSAGE, message.subjectId)

        val room = kindOf { it.reportRoom(ROOM, ReportReason.Harassment) }
        assertEquals(ReportSubject.Room.wire, room.subjectKind)
        assertEquals(ROOM, room.subjectId)

        val bot = kindOf { it.reportBot(BOT, ReportReason.Harassment) }
        assertEquals(ReportSubject.Bot.wire, bot.subjectKind)
        assertEquals(BOT, bot.subjectId)
    }

    /** Files one report and hands back whatever the domain put on the wire. */
    private fun kindOf(file: suspend (ModerationDomain) -> Unit): ReportFile {
        val fake = ScriptedReportTransport(reply = { w -> Acknowledged(ok = true).encode(w) })
        runBlocking { file(ModerationDomain(Rpc(fake))) }
        return ReportFile.decode(Reader(fake.sent.single().second))
    }

    @Test
    fun `a bot is reported by its bot id, which is not an account id`() {
        val fake = ScriptedReportTransport(reply = { w -> Acknowledged(ok = true).encode(w) })
        val domain = ModerationDomain(Rpc(fake))

        runBlocking { domain.reportBot(BOT, ReportReason.BotAbuse, "it DMs everyone who joins") }

        val request = ReportFile.decode(Reader(fake.sent.single().second))
        assertEquals("the id passes through as given -- the caller's, not the owner's account", BOT, request.subjectId)
        assertEquals(
            "a broken integration is its own reason, not harassment by a person",
            ReportReason.BotAbuse.wire,
            request.reason,
        )
    }

    // --- the refusals that must not cost a frame ---

    @Test
    fun `an over-long note is refused locally, and nothing reaches the wire`() {
        val fake = ScriptedReportTransport(reply = { w -> Acknowledged(ok = true).encode(w) })
        val domain = ModerationDomain(Rpc(fake))
        val tooLong = "x".repeat(REPORT_NOTE_MAX_LEN + 1)

        try {
            runBlocking { domain.reportMessage(MESSAGE, ReportReason.Spam, tooLong) }
            fail("a note over the ceiling must not be sent")
        } catch (expected: IllegalArgumentException) {
            assertTrue(
                "the refusal names the ceiling so the caller can trim to it",
                expected.message!!.contains(REPORT_NOTE_MAX_LEN.toString()),
            )
        }

        // The point of checking here rather than at the node: the frame is never spent, and this
        // call is priced by the registry.
        assertTrue("nothing was sent", fake.sent.isEmpty())
    }

    @Test
    fun `a note exactly at the ceiling is accepted`() {
        val fake = ScriptedReportTransport(reply = { w -> Acknowledged(ok = true).encode(w) })
        val domain = ModerationDomain(Rpc(fake))
        val atCeiling = "x".repeat(REPORT_NOTE_MAX_LEN)

        runBlocking { domain.reportMessage(MESSAGE, ReportReason.Spam, atCeiling) }

        val request = ReportFile.decode(Reader(fake.sent.single().second))
        assertEquals("the boundary is inclusive", REPORT_NOTE_MAX_LEN, request.note!!.length)
    }

    @Test
    fun `the reply is a bare acknowledgement the caller does not have to unwrap`() {
        // Returns nothing: a reporter has no use for a case id, cannot read the case, and echoing
        // one back would invite a client to present it as a receipt the reporter could chase. The
        // one thing a client legitimately needs -- that the report arrived -- is what the ack
        // carries, so `report` completes and the frame is the report opcode and nothing else.
        val ok = ScriptedReportTransport(reply = { w -> Acknowledged(ok = true).encode(w) })
        runBlocking { ModerationDomain(Rpc(ok)).reportUser(ADA, ReportReason.Spam) }
        assertEquals(1, ok.sent.size)
        assertEquals(Op.REPORT_CREATE, ok.sent.single().first)
    }

    // --- the node's word when a case is decided ---

    @Test
    fun `moderation events reach handlers only between start and stop`() {
        val fake = ScriptedReportTransport()
        val sink = ArrayList<Pair<Long, Throwable>>()
        val rpc = Rpc(fake) { opcode, cause -> sink += opcode to cause }
        val domain = ModerationDomain(rpc) { opcode, cause -> sink += opcode to cause }
        val seen = ArrayList<ModerationEvent>()
        domain.onModerationEvent { seen += it }

        domain.start()
        domain.start() // idempotent: a second start must not double-deliver
        fake.event(ModerationEvent(caseId = CASE, action = 1, state = "resolved"))
        fake.inbound.close()
        runBlocking { rpc.deliver() }

        assertEquals("one event, one handler call", 1, seen.size)
        assertEquals(CASE, seen[0].caseId)
        assertEquals(1L, seen[0].action)
        assertEquals("resolved", seen[0].state)
        assertTrue("a well-formed frame never reaches the error sink", sink.isEmpty())

        // Stopped, the subscription is gone -- and stopping twice is not an error. The event that
        // arrives afterwards reaches nobody, and reaches the error sink no more than it reaches a
        // handler: a client that stopped listening understood the frame perfectly well.
        domain.stop()
        domain.stop()
        val afterStop = ScriptedReportTransport()
        val quietRpc = Rpc(afterStop)
        val quiet = ModerationDomain(quietRpc) { opcode, cause -> sink += opcode to cause }
        var after = 0
        quiet.onModerationEvent { after++ }
        quiet.start()
        quiet.stop()
        afterStop.event(ModerationEvent(caseId = CASE, action = 1, state = "resolved"))
        afterStop.inbound.close()
        runBlocking { quietRpc.deliver() }

        assertEquals("a stopped domain delivered nothing", 0, after)
        assertTrue("and reported nothing as broken", sink.isEmpty())
    }

    @Test
    fun `a frame this build cannot render goes to the error sink, not to a handler`() {
        val fake = ScriptedReportTransport()
        val sink = ArrayList<Pair<Long, Throwable>>()
        // Both sinks, and they are two different ones. The Rpc decodes an event before the domain
        // ever sees it, so a frame that cannot be rendered fails inside the Rpc and is reported
        // there; the domain's own sink only hears about a handler that threw. Handing the sink to
        // the domain alone leaves the interesting failure -- a frame this build cannot render --
        // swallowed in silence, which is exactly what this test exists to catch.
        val rpc = Rpc(fake) { opcode, cause -> sink += opcode to cause }
        val domain = ModerationDomain(rpc) { opcode, cause -> sink += opcode to cause }
        val seen = ArrayList<ModerationEvent>()
        domain.onModerationEvent { seen += it }
        domain.start()

        // A MODERATION_EVENT truncated mid-struct: every field the domain would hand on is a guess.
        fake.raw(Op.MODERATION_EVENT, byteArrayOf(0x01))
        fake.inbound.close()
        runBlocking { rpc.deliver() }

        assertTrue("no handler saw it", seen.isEmpty())
        assertEquals("exactly one frame reached the sink", 1, sink.size)
        assertEquals(Op.MODERATION_EVENT, sink[0].first)
        assertNotNull(sink[0].second)
    }
}

/**
 * A [RealtimeTransport] that sends nothing anywhere: every request is answered from a script, every
 * event is pushed by the test, and the sent frames are kept for asserting on. The domains speak this
 * interface and never a socket, which is what makes the report path testable without a node.
 */
private class ScriptedReportTransport(
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

    /** Pushes one decoded server event, as the gateway would hand it to the Rpc pump. */
    fun event(payload: ModerationEvent) {
        raw(Op.MODERATION_EVENT, encodeToPayload { payload.encode(it) })
    }

    /** Pushes raw payload bytes, for a frame no decoder can render. */
    fun raw(opcode: Long, payload: ByteArray) {
        inbound.trySend(Inbound(Frame(frameHeader(opcode), payload)))
    }

    private fun encodeToPayload(encode: (Writer) -> Unit): ByteArray {
        val writer = Writer()
        encode(writer)
        return writer.finish()
    }
}
