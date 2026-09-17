package com.migo.core.domain

import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.ModerationEvent
import com.migo.core.protocol.Op
import com.migo.core.protocol.ReportFile
import com.migo.core.wire.Id

/**
 * The moderation domain: pointing the node's warden at something.
 *
 * A port of `packages/sdk/src/domains/moderation.ts`. Section 49 of the brief asks for four kinds of
 * report -- a user, a message, a room, a bot -- and that is exactly the vocabulary [ReportSubject]
 * carries. Filing one is the only thing this domain does, and it is deliberately the only thing a
 * *client* can do: reading the queue, ruling on a case, and applying a takedown are staff powers, and
 * the surface for them is the node's operator API, not this one. A client that could read the queue
 * would be a client that could read who reported whom.
 *
 * # A report is a pointer, never a copy
 *
 * The wire carries a subject kind and a subject id, plus a reason code and an optional note the
 * reporter wrote. It carries no message content and no attachment, and that is not a shortcut: the
 * conversations this system carries are end-to-end encrypted, so a report that quoted the offending
 * text would either be unreadable to the moderator or readable only by shipping the server a
 * plaintext it is not supposed to have. The moderator follows the pointer with their own eyes.
 *
 * The corollary is the note: the note is the *reporter's own words*, typed by them, and it is the
 * only field in this whole path a human wrote. It is stored on the report row and never on the audit
 * entry, which names the reason code instead.
 *
 * # Idempotent, and priced
 *
 * A second report from the same reporter about the same still-open subject does not create a second
 * row and does not fail -- the server recognises it and answers success, because the usual cause is a
 * client whose first answer was lost and telling it the report failed would be a lie about a report
 * that is in fact sitting in the queue. Reporting yourself is refused as a client bug rather than
 * queued for a human to read.
 *
 * Filing is priced (the registry charges `REPORT_CREATE` 20), so a client that offers a report button
 * offers it once per gesture rather than retrying a rejected call in a loop.
 *
 * # Why the reply is a bare acknowledgement
 *
 * `REPORT_CREATE` answers with [Acknowledged], not with the report's id, so [report] returns nothing
 * rather than a handle. The server knows more than it says here -- it knows whether the filing was a
 * duplicate and which row it landed on -- and the decision to keep the reply bare is deliberate: a
 * reporter has no use for a case id (they cannot read the case), and echoing one back would invite a
 * client to present it as a receipt the reporter could chase. The one thing a client legitimately
 * needs -- that the report arrived -- is what the ack carries.
 */

/**
 * What is being reported, as the wire numbers it.
 *
 * The values are the `subject_kind` field of `REPORT_CREATE` and are load-bearing: they are what the
 * node maps to its own storage vocabulary, so they are never renumbered or reused. [Message] is a
 * message id and nothing more -- the conversation it sits in is not on the wire, and a report row
 * stores exactly one id.
 *
 * These are the *wire* numbers and not the store's: `report.subject_kind` in the schema numbers a
 * media object 3 and a bot 4, while the wire numbers a bot 3 and has no media kind at all. A client
 * that echoed the storage numbers would file every bot report as a report about a media object.
 */
enum class ReportSubject(
    /** The value `REPORT_CREATE.subject_kind` carries. */
    val wire: Long,
) {
    /** A whole account, by account id. */
    User(0),

    /** One message, by message id. */
    Message(1),

    /** A room, by room id. */
    Room(2),

    /** A bot, by `bot.bot_id` rather than by the account it signs in as. */
    Bot(3),
}

/**
 * Why something is being reported.
 *
 * The codes mirror the node's own reason vocabulary and are stored as given, so they are never
 * renumbered: a renumbering here would silently rewrite the meaning of every report already in a
 * queue. The four that exist for a legal reason rather than a product one -- [ChildSafety] above all,
 * kept separate from [SexualContent] because the obligations attached to it are not the same and an
 * operator must be able to filter the queue for exactly it -- are the reason this is a code and not a
 * free-text field.
 *
 * [SelfHarm] is routed like any other report and prioritised like none of them: this domain carries
 * the code, and what a deployment does with it afterwards is a staffing question no amount of client
 * code answers.
 */
enum class ReportReason(
    /** The value `REPORT_CREATE.reason` carries. */
    val wire: Long,
) {
    /** Unsolicited bulk content. */
    Spam(0),

    /** Volume rather than content: the same thing, very fast. */
    Flood(1),

    /** An attempt to obtain money or credentials by deception. */
    Scam(2),

    /** A link to malware, phishing, or a credential harvester. */
    MaliciousLink(3),

    /** Harassment, threats, or targeted abuse of a person. */
    Harassment(4),

    /** Hateful content aimed at a group. */
    HateSpeech(5),

    /** Sexual content where it does not belong. */
    SexualContent(6),

    /** Graphic violence. */
    Violence(7),

    /** Self-harm or suicide content. */
    SelfHarm(8),

    /** Somebody pretending to be somebody else. */
    Impersonation(9),

    /** Child sexual abuse material. Kept its own code; see the enum's own note. */
    ChildSafety(10),

    /** A bot misbehaving: a broken integration rather than an abusive person. */
    BotAbuse(11),

    /** None of the above. */
    Other(12),
}

/**
 * The longest note the node accepts, in characters.
 *
 * Mirrors the warden's own ceiling. [ModerationDomain.report] checks it here so an over-long note
 * fails without spending the frame or the report's cost on a call that can only be refused.
 */
const val REPORT_NOTE_MAX_LEN: Int = 500

/** What a report points at: a kind from [ReportSubject] and the id of that thing. */
data class ReportTarget(
    /** Which of the four things is being reported. */
    val kind: ReportSubject,
    /** The id of that thing, in the vocabulary [ReportSubject] documents. */
    val id: Id,
)

/**
 * What a report about *a person* points at, given the account and the bot it may speak as.
 *
 * An account that speaks as a bot is reported as the bot and not as the account behind it, because
 * `bot.bot_id` is a different id from the account id and is the one the node's own bot actions act
 * on: a report filed under the account id would reach a moderator as a report about a bot that names
 * something which is not one, and nothing on the wire would look wrong while it did. `botId` is the
 * `UserProfile.bot_id` the wire carries, and it is null for every ordinary account -- a null here is
 * the absence of a claim and not a claim that the account is human, so an account the node never
 * named a bot for is reported as a user.
 *
 * This is the one place in this client where the choice is made, and every surface that reports a
 * person goes through it for that reason: a row that decided for itself would be a second copy of a
 * rule that has to agree with the first, on the one id in this whole path where being wrong is
 * silent.
 */
fun personTarget(accountId: Id, botId: Id?): ReportTarget =
    if (botId != null) {
        ReportTarget(ReportSubject.Bot, botId)
    } else {
        ReportTarget(ReportSubject.User, accountId)
    }

/**
 * The reason a report about [target] opens on, or null where the reporter has to pick one.
 *
 * Null is the ordinary answer and the sheet's own rule: Send stays dark until a reason is picked,
 * because a menu read with a thumb already moving is not a menu that was read. A bot subject is the
 * one exception, and it is not a default so much as an answer the marking already gave -- the account
 * is a bot, that is what bot abuse means, and a reporter asked to judge "bot abuse" over an account
 * this client has just marked as a bot has been asked a question the mark answered. The sheet offers
 * the code as a picked row where the generic menu does not list it, so the live Send still sits under
 * a row the reporter can see, and every other reason stays available beside it.
 */
fun openingReason(target: ReportTarget): ReportReason? =
    if (target.kind == ReportSubject.Bot) ReportReason.BotAbuse else null

/**
 * Files reports, and receives the node's word when one is ruled on.
 *
 * One instance per client. [report] works on its own; [onModerationEvent] only fires once [start] has
 * been called.
 */
class ModerationDomain(
    private val rpc: Rpc,
    onEventError: EventErrorHandler? = null,
) {
    private val listeners = ListenerSet<ModerationEvent>(Op.MODERATION_EVENT, onEventError)

    @Volatile
    private var subscription: Subscription? = null

    /**
     * Begins delivering moderation events to registered handlers. Idempotent.
     *
     * Register handlers with [onModerationEvent] first: nothing is delivered before this is called,
     * so the ordering is what stops a client from missing the first event after connecting.
     */
    fun start() {
        if (subscription != null) return
        subscription = rpc.on(Op.MODERATION_EVENT, { r -> ModerationEvent.decode(r) }) { e, _ ->
            listeners.deliver(e)
        }
    }

    /** Stops delivery. Registered handlers are kept for a later [start]. */
    fun stop() {
        val live = subscription ?: return
        subscription = null
        live.cancel()
    }

    /**
     * Registers a handler for moderation events -- the node's word that a case was decided.
     *
     * A client renders this as "your report was reviewed"; the event names the case, the ruling code,
     * and its state, and carries nothing about the subject, because what happened to somebody else's
     * account is not the reporter's to read.
     */
    fun onModerationEvent(listener: Listener<ModerationEvent>): Subscription = listeners.add(listener)

    /**
     * Files a report about [target], with [reason] explaining why.
     *
     * Returns when the node has taken the report -- see this file's note on why the answer is an
     * acknowledgement and not a case id. Throws [IllegalArgumentException] when the note is over
     * [REPORT_NOTE_MAX_LEN] *before anything is sent*, and [com.migo.core.net.GatewayError.Refused]
     * when the node refuses: an unknown subject kind, the caller's own account as the subject, or a
     * rate limit the caller has earned.
     *
     * The report is scoped to the caller's account, so the same account filing twice about the same
     * still-open subject succeeds both times and leaves one row.
     */
    suspend fun report(target: ReportTarget, reason: ReportReason, note: String? = null) {
        // Checked here rather than left to the node: a refusal that arrives after the reporter typed
        // the whole thing has already cost them the typing, and this call is priced.
        require(note == null || note.length <= REPORT_NOTE_MAX_LEN) {
            "a report note is at most $REPORT_NOTE_MAX_LEN characters, got ${note?.length}"
        }
        val request = ReportFile(
            subjectKind = target.kind.wire,
            subjectId = target.id,
            reason = reason.wire,
            note = note,
        )
        rpc.call(Op.REPORT_CREATE, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Files a report about one message, identified by its id alone.
     *
     * The conversation is deliberately not a parameter. A report row holds a single id and the wire
     * carries a single id, so naming the conversation here would promise a precision the protocol
     * does not have -- and a caller who passed the wrong one would be filing about the right message
     * under a key nothing reads. A moderator reaches the message through the report's own subject,
     * not through a conversation id the reporter supplied.
     */
    suspend fun reportMessage(messageId: Id, reason: ReportReason, note: String? = null) =
        report(ReportTarget(ReportSubject.Message, messageId), reason, note)

    /** Files a report about one account. The node refuses the caller's own account. */
    suspend fun reportUser(accountId: Id, reason: ReportReason, note: String? = null) =
        report(ReportTarget(ReportSubject.User, accountId), reason, note)

    /** Files a report about one room. */
    suspend fun reportRoom(roomId: Id, reason: ReportReason, note: String? = null) =
        report(ReportTarget(ReportSubject.Room, roomId), reason, note)

    /**
     * Files a report about one bot, by `bot.bot_id`.
     *
     * A bot is reported as a bot and not as its owner's account, because the two are different
     * problems with different remedies: a moderator reading the queue needs to tell "this integration
     * is broken" from "this person is abusive" before deciding anything.
     */
    suspend fun reportBot(botId: Id, reason: ReportReason, note: String? = null) =
        report(ReportTarget(ReportSubject.Bot, botId), reason, note)
}
