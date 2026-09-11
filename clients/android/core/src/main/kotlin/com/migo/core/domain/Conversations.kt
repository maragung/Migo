package com.migo.core.domain

import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.ConversationCreateRequest
import com.migo.core.protocol.ConversationInviteRequest
import com.migo.core.protocol.ConversationKickRequest
import com.migo.core.protocol.ConversationKind
import com.migo.core.protocol.ConversationLeaveRequest
import com.migo.core.protocol.ConversationListRequest
import com.migo.core.protocol.ConversationListResponse
import com.migo.core.protocol.ConversationMemberEvent
import com.migo.core.protocol.ConversationMuteRequest
import com.migo.core.protocol.ConversationRole
import com.migo.core.protocol.ConversationRosterEntry
import com.migo.core.protocol.ConversationRosterRequest
import com.migo.core.protocol.ConversationRosterResponse
import com.migo.core.protocol.ConversationStateEvent
import com.migo.core.protocol.ConversationSummary
import com.migo.core.protocol.ConversationUpdateRequest
import com.migo.core.protocol.ConversationVoteEvent
import com.migo.core.protocol.ConversationVoteKickRequest
import com.migo.core.protocol.ConversationVoteKickResponse
import com.migo.core.protocol.Op
import com.migo.core.wire.Id

/**
 * Listing the conversations this account is in, creating new ones, and running the group lifecycle
 * around them.
 *
 * A port of `packages/sdk/src/domains/conversations.ts`. Requests and responses only for the
 * lifecycle calls; the three group event streams are delivered through the [start]-driven listener
 * sets, exactly as [RoomsDomain] does it.
 *
 * # The property worth knowing
 *
 * [create] is idempotent for a [ConversationKind.Direct] chat. The server derives the conversation
 * id deterministically from the sorted member ids, so "the conversation with Alice" resolves to the
 * same conversation every time it is asked for. That is what makes it safe to call before a first
 * send rather than tracking whether one has been created, and it is why there is no
 * `ensureConversation` helper here: create already is one.
 *
 * # The group lifecycle
 *
 * A group has two founders -- the creator and the first person they named -- and the founders are
 * the group's memory of who built it. [invite] is every member's right; [mute], [kick], and
 * [rename] are the founders'; [voteKick] is the members' own recourse, carrying at half the roster
 * rounded up. Leaving ([leave]) is nobody's to gate: when the last founder walks out, the
 * longest-standing member inherits the role, so a group never reaches a state where nobody can
 * rename it or answer a report.
 *
 * Three live streams follow from [start]: [onMember] for joins, departures, and removals -- rotate
 * sender keys on every one of these, exactly as for a room -- [onVote] for a running kick tally,
 * and [onState] for coalesced metadata deltas such as a rename. All three arrive only on a
 * conversation the client has subscribed to through the client's `watchConversation`.
 *
 * # What the encryption mode in a summary does and does not say
 *
 * [ConversationSummary.encryption] is a claim about the conversation's transport policy, and a client
 * may show it. It is not the end-to-end guarantee: content is sealed by the crypto layers on the way
 * out regardless of what a summary advertises, so a summary that came back with an unexpected mode
 * cannot cause plaintext to be sent. Treating the field as the source of truth for "is this
 * encrypted" would put a security decision in the hands of the party the encryption protects
 * against.
 */
class ConversationsDomain(
    private val rpc: Rpc,
    onEventError: EventErrorHandler? = null,
) {
    private val memberListeners =
        ListenerSet<ConversationMemberEvent>(Op.CONVERSATION_MEMBER_EVENT, onEventError)
    private val voteListeners =
        ListenerSet<ConversationVoteEvent>(Op.CONVERSATION_VOTE_EVENT, onEventError)
    private val stateListeners =
        ListenerSet<ConversationStateEvent>(Op.CONVERSATION_STATE_EVENT, onEventError)

    @Volatile
    private var subscriptions: List<Subscription>? = null

    /**
     * Begins delivering the group event streams to registered handlers. Idempotent.
     *
     * All three are subscribed together, for the same reason [RoomsDomain.start] takes the room's
     * three as one: the server sends them whether or not a handler asked, and an opcode reaching
     * [Rpc] with no subscriber is a warning rather than a silent discard.
     */
    fun start() {
        if (subscriptions != null) return
        subscriptions = listOf(
            rpc.on(Op.CONVERSATION_MEMBER_EVENT, { r -> ConversationMemberEvent.decode(r) }) { event, _ ->
                memberListeners.deliver(event)
            },
            rpc.on(Op.CONVERSATION_VOTE_EVENT, { r -> ConversationVoteEvent.decode(r) }) { event, _ ->
                voteListeners.deliver(event)
            },
            rpc.on(Op.CONVERSATION_STATE_EVENT, { r -> ConversationStateEvent.decode(r) }) { event, _ ->
                stateListeners.deliver(event)
            },
        )
    }

    /** Stops delivery of all three streams. Registered handlers are kept for a later [start]. */
    fun stop() {
        val live = subscriptions ?: return
        subscriptions = null
        for (subscription in live) subscription.cancel()
    }

    /**
     * Registers a handler for group membership movement: joins, departures, and removals.
     *
     * Membership churn is a crypto event before it is a UI one -- rotate the conversation's sender
     * key on every one of these, exactly as for a room, so a removed member cannot read what is
     * sent next.
     */
    fun onMember(listener: Listener<ConversationMemberEvent>): Subscription = memberListeners.add(listener)

    /**
     * Registers a handler for group kick-vote tallies.
     *
     * Each event names the target and the count so far against the threshold; [ConversationVoteEvent.closed]
     * marks the vote's end (it passed, expired, or the target walked out). The stream is coalesced
     * per conversation, so a handler holds one tally per target and replaces it as counts arrive
     * rather than accumulating.
     */
    fun onVote(listener: Listener<ConversationVoteEvent>): Subscription = voteListeners.add(listener)

    /**
     * Registers a handler for coalesced group metadata deltas.
     *
     * Each event carries only the fields that changed -- today, a title when a founder renames the
     * group. Apply them onto held state rather than replacing it.
     */
    fun onState(listener: Listener<ConversationStateEvent>): Subscription = stateListeners.add(listener)

    /**
     * Lists the account's conversations, most recent activity first.
     *
     * Pass the [ConversationListResponse.nextCursor] of a previous page as [cursor] for the next one;
     * a response without a cursor is the last page. [limit] bounds one page, and the server may
     * return fewer.
     */
    suspend fun list(limit: Long, cursor: String? = null): ConversationListResponse {
        val request = ConversationListRequest(limit, cursor)
        return rpc.call(
            Op.CONVERSATION_LIST,
            { w -> request.encode(w) },
            { r -> ConversationListResponse.decode(r) },
        )
    }

    /**
     * Creates a conversation, or returns the existing one for a direct chat.
     *
     * [members] is the *other* participants: the server adds the caller, and a client that included
     * itself would be asking for a two-person conversation with one person in it. [title] is
     * meaningful for a group and ignored for a direct chat, where the name shown is the other
     * person's. A group needs at least one named member besides the caller -- the two of them are
     * the group's founders -- so an empty member list is refused by the server, not padded here.
     */
    suspend fun create(
        kind: ConversationKind,
        members: List<Id>,
        title: String? = null,
    ): ConversationSummary {
        val request = ConversationCreateRequest(kind, members, title)
        return rpc.call(
            Op.CONVERSATION_CREATE,
            { w -> request.encode(w) },
            { r -> ConversationSummary.decode(r) },
        )
    }

    /**
     * Adds members to a group, resolving with the group's summary as it now stands.
     *
     * Any current member may invite; the new seats arrive as plain members. Already-seated members
     * and the caller are quietly skipped, and each person who actually landed is announced to the
     * group on the [onMember] stream, so the roster stays true without a refetch.
     */
    suspend fun invite(conversationId: Id, members: List<Id>): ConversationSummary {
        val request = ConversationInviteRequest(conversationId, members)
        return rpc.call(
            Op.CONVERSATION_INVITE,
            { w -> request.encode(w) },
            { r -> ConversationSummary.decode(r) },
        )
    }

    /**
     * Leaves a group. Nobody's permission is asked -- leaving is a right, not a request.
     *
     * After leaving, forget the conversation's crypto state through the messaging domain, exactly as
     * for a room: the group rotates its sender key on the departure, and the local receiver state is
     * no longer useful. When the last founder leaves, the longest-standing member silently inherits
     * the role, so the group never ends up with nobody able to rename it or answer a report.
     */
    suspend fun leave(conversationId: Id): Acknowledged {
        val request = ConversationLeaveRequest(conversationId)
        return rpc.call(
            Op.CONVERSATION_LEAVE,
            { w -> request.encode(w) },
            { r -> Acknowledged.decode(r) },
        )
    }

    /**
     * Reads a group's roster: active members first by join time, then the departed.
     *
     * The whole membership, where a conversation-list row's members field is a capped preview --
     * which is why the client's sender-key audience is chosen from this answer and never from a
     * list row. A departure carries a non-null [ConversationRosterEntry.leftAt]; the caller that
     * wants "who is in the group now" filters on it.
     */
    suspend fun getRoster(conversationId: Id): List<ConversationRosterEntry> {
        val request = ConversationRosterRequest(conversationId)
        val response = rpc.call(
            Op.CONVERSATION_ROSTER,
            { w -> request.encode(w) },
            { r -> ConversationRosterResponse.decode(r) },
        )
        return response.entries
    }

    /**
     * Mutes or unmutes one member of a group -- a founder's action, not a vote.
     *
     * While the mute runs, the target cannot send to the group; they keep every other right,
     * including the vote. Omit [until] to lift a mute early; pass a future epoch-milliseconds
     * timestamp to set one. Founders are beyond each other's reach, and neither the caller nor a
     * founder may be the target. There is no event for this -- the roster is the record -- so a
     * client that needs to show the change refetches the roster.
     */
    suspend fun mute(conversationId: Id, targetId: Id, until: Long? = null): Acknowledged {
        val request = ConversationMuteRequest(conversationId, targetId, until)
        return rpc.call(
            Op.CONVERSATION_MUTE,
            { w -> request.encode(w) },
            { r -> Acknowledged.decode(r) },
        )
    }

    /**
     * Removes a member outright, no vote -- a founder's action.
     *
     * The other founder is beyond this reach: a group built by two cannot be halved by one of them.
     * The removal is announced to the group on the [onMember] stream, and the group rotates its
     * sender key, so rotate local crypto state exactly as for any membership change.
     */
    suspend fun kick(conversationId: Id, targetId: Id): Acknowledged {
        val request = ConversationKickRequest(conversationId, targetId)
        return rpc.call(
            Op.CONVERSATION_KICK,
            { w -> request.encode(w) },
            { r -> Acknowledged.decode(r) },
        )
    }

    /**
     * Casts a voice to kick a member by vote, resolving with the tally after this voice landed.
     *
     * The vote is the members' own recourse, needing no rank: the first call opens the vote, each
     * further member's call adds to it, and when [ConversationVoteKickResponse.votes] reaches
     * [ConversationVoteKickResponse.needed] -- half the group, rounded up -- the kick lands and
     * [ConversationVoteKickResponse.open] turns false. A caller's repeated voice is idempotent, a
     * muted member still votes, and founders are immune. One vote may run per group at a time, and
     * a vote nobody finishes expires after a minute. The same tally is broadcast on the [onVote]
     * stream, so a member who voted and one who only watched converge on the same count.
     */
    suspend fun voteKick(conversationId: Id, targetId: Id): ConversationVoteKickResponse {
        val request = ConversationVoteKickRequest(conversationId, targetId)
        return rpc.call(
            Op.CONVERSATION_VOTE_KICK,
            { w -> request.encode(w) },
            { r -> ConversationVoteKickResponse.decode(r) },
        )
    }

    /**
     * Renames a group, resolving with the summary carrying the new title.
     *
     * A founder's action. The new title travels to every member on the [onState] stream as a
     * coalesced delta, so a client applies it onto the summary it already holds. Direct
     * conversations carry no title to change.
     */
    suspend fun rename(conversationId: Id, title: String): ConversationSummary {
        val request = ConversationUpdateRequest(conversationId, title)
        return rpc.call(
            Op.CONVERSATION_UPDATE,
            { w -> request.encode(w) },
            { r -> ConversationSummary.decode(r) },
        )
    }
}

/**
 * Whether the founder controls (mute, kick) belong on a member's row for this viewer.
 *
 * Pure, so a test can pin it. The viewer must be a founder, the target must not be -- the two
 * builders are beyond each other's reach -- and nobody acts on their own row.
 */
fun canFounderAct(viewerRole: ConversationRole, targetRole: ConversationRole, isSelf: Boolean): Boolean =
    !isSelf && viewerRole == ConversationRole.Founder && targetRole != ConversationRole.Founder

/**
 * Whether the "Vote kick" control belongs on a member's row.
 *
 * Pure, so a test can pin it. The vote is every member's own recourse -- but never against
 * yourself, and never against a founder, whom a show of hands cannot unseat.
 */
fun canVoteKickGroup(targetRole: ConversationRole, isSelf: Boolean): Boolean =
    !isSelf && targetRole != ConversationRole.Founder
