package com.migo.core.domain

import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.Op
import com.migo.core.protocol.RoomCreate
import com.migo.core.protocol.RoomJoinRequest
import com.migo.core.protocol.RoomJoinResponse
import com.migo.core.protocol.RoomLeaveRequest
import com.migo.core.protocol.RoomListRequest
import com.migo.core.protocol.RoomListResponse
import com.migo.core.protocol.RoomMemberEvent
import com.migo.core.protocol.RoomSanction
import com.migo.core.protocol.RoomStateEvent
import com.migo.core.protocol.RoomVoteEvent
import com.migo.core.protocol.RoomVoteKick
import com.migo.core.protocol.RoomVoteKickResponse
import com.migo.core.protocol.RosterReq
import com.migo.core.protocol.RosterResponse
import com.migo.core.protocol.SanctionAction
import com.migo.core.wire.Id

/**
 * Rooms: the public and semi-public spaces, as opposed to private conversations.
 *
 * A port of `packages/sdk/src/domains/rooms.ts`. The largest of the small domains, because it is the
 * only one with both requests and three separate event streams -- who is in the room, what the room
 * itself is doing, and how a kick vote against a member is running.
 *
 * # A room is not end-to-end encrypted, and this SDK must not imply that it is
 *
 * Section 178 forbids claiming end-to-end encryption for a Public or Managed Room, and the reason is
 * structural rather than an implementation gap: a room anyone can join, with moderation and history
 * for newcomers, cannot also be a space whose contents only the current members can read. Somebody
 * joining tomorrow gets today's history, which means the history has to be readable by the server that
 * serves it.
 *
 * [RoomJoinResponse.encryption] states the mode the joined conversation actually runs in, and it is
 * there so a client can *show* the difference. Rendering a room with the same lock icon a private chat
 * gets would be the one dishonesty that matters in a messaging app, because a person choosing where to
 * say something is relying on that icon.
 *
 * # Joining hands back a conversation, and the messages come from elsewhere
 *
 * A room's traffic is a conversation like any other: [RoomJoinResponse] carries the
 * [RoomJoinResponse.conversationId] and its [RoomJoinResponse.lastSeq], and from that point messages
 * arrive through [MessagingDomain] and history through [SyncDomain]. There is no room-specific send
 * path, which is what keeps one message pipeline in this SDK instead of two.
 *
 * # Streams because they answer different questions
 *
 * [RoomMemberEvent] is a person arriving, leaving, or changing role -- a discrete thing a UI puts in
 * the timeline. [RoomStateEvent] is the room's own shape: the online count ticking, a topic change, a
 * slow mode going on. Folding them into one stream would make every member list update wake up the
 * code that renders the header, and in a busy room those fire at very different rates.
 *
 * [RoomVoteEvent] is the third and the narrowest: the running tally of a kick vote against one member,
 * coalesced per room so a UI can show "3/17" beside the target while the vote is open and take the row
 * down when it closes. It is separate for the same reason -- a vote is a rare, targeted thing, and a
 * client that never renders a tally simply never adds a handler for it.
 */
class RoomsDomain(
    private val rpc: Rpc,
    onEventError: EventErrorHandler? = null,
) {
    private val memberListeners = ListenerSet<RoomMemberEvent>(Op.ROOM_MEMBER_EVENT, onEventError)
    private val stateListeners = ListenerSet<RoomStateEvent>(Op.ROOM_STATE_EVENT, onEventError)
    private val voteListeners = ListenerSet<RoomVoteEvent>(Op.ROOM_VOTE_EVENT, onEventError)

    @Volatile
    private var subscriptions: List<Subscription>? = null

    /**
     * Begins delivering room events to registered handlers. Idempotent.
     *
     * All three streams are subscribed together: a client that wanted only one of them would still be
     * sent the others by the server, and an unsubscribed opcode reaching [Rpc] is a warning rather than
     * a silent discard.
     */
    fun start() {
        if (subscriptions != null) return
        subscriptions = listOf(
            rpc.on(Op.ROOM_MEMBER_EVENT, { r -> RoomMemberEvent.decode(r) }) { event, _ ->
                memberListeners.deliver(event)
            },
            rpc.on(Op.ROOM_STATE_EVENT, { r -> RoomStateEvent.decode(r) }) { event, _ ->
                stateListeners.deliver(event)
            },
            rpc.on(Op.ROOM_VOTE_EVENT, { r -> RoomVoteEvent.decode(r) }) { event, _ ->
                voteListeners.deliver(event)
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
     * Registers a handler for members arriving, leaving, and changing role.
     *
     * [RoomMemberEvent.memberCount] is the room's total after the change when the server chose to send
     * it, so a client can correct a drifting local count instead of incrementing forever.
     */
    fun onMember(listener: Listener<RoomMemberEvent>): Subscription = memberListeners.add(listener)

    /**
     * Registers a handler for the room's own state.
     *
     * Every field on a [RoomStateEvent] is optional and only what changed is present. An absent field
     * means "unchanged", not "zero" -- treating a missing [RoomStateEvent.onlineCount] as 0 is how a
     * busy room comes to show as empty.
     */
    fun onState(listener: Listener<RoomStateEvent>): Subscription = stateListeners.add(listener)

    /**
     * Registers a handler for the running tally of a kick vote.
     *
     * A [RoomVoteEvent] arrives each time a voice lands on an open vote and once more when it ends.
     * [RoomVoteEvent.closed] tells the two endings apart: true when the vote expired or the target
     * left, absent (null) while the vote is still open, and the kick that a *passed* vote causes
     * arrives separately as a [RoomMemberEvent] with [com.migo.core.protocol.MemberChange.Kicked]. So a
     * UI shows the tally against the target while events keep coming and clears it on any close.
     */
    fun onVote(listener: Listener<RoomVoteEvent>): Subscription = voteListeners.add(listener)

    /**
     * Creates a room and enters it, resolving with the same join handle [join] returns.
     *
     * The caller becomes the room's Owner ([com.migo.core.protocol.RoomRole.Owner]): the one role
     * that can appoint managers and the one a room cannot lose. `slug` is the room's permanent
     * address and must be unique server-side; `kind` picks the governance line —
     * [com.migo.core.protocol.RoomKind.Public] for a community room,
     * [com.migo.core.protocol.RoomKind.Managed] for one under server moderation. The reply is a
     * join response because creation is entry: the creator is the first member.
     */
    suspend fun create(
        slug: String,
        name: String,
        kind: com.migo.core.protocol.RoomKind,
        topic: String? = null,
    ): RoomJoinResponse {
        val request = RoomCreate(slug, name, kind.wire.toLong(), topic)
        return rpc.call(
            Op.ROOM_CREATE,
            { w -> request.encode(w) },
            { r -> RoomJoinResponse.decode(r) },
        )
    }

    /**
     * Joins a room.
     *
     * [inviteCode] is required for a room that is not open to everyone and ignored for one that is.
     * After this returns, subscribe to the returned conversation's topic and sync it from
     * [RoomJoinResponse.lastSeq] to show recent history.
     *
     * Joining a room already joined succeeds and returns the current state rather than failing, so a
     * client reconnecting does not have to track whether it is a member.
     */
    suspend fun join(roomId: Id, inviteCode: String? = null): RoomJoinResponse {
        val request = RoomJoinRequest(roomId, inviteCode)
        return rpc.call(
            Op.ROOM_JOIN,
            { w -> request.encode(w) },
            { r -> RoomJoinResponse.decode(r) },
        )
    }

    /**
     * Leaves a room.
     *
     * The conversation stops delivering, and a client should drop its local membership state on
     * success. Sender-key material for a room conversation is not this domain's to clean up; a client
     * that keeps per-conversation crypto state calls [MessagingDomain.forget] for the conversation
     * afterwards.
     */
    suspend fun leave(roomId: Id): Acknowledged {
        val request = RoomLeaveRequest(roomId)
        return rpc.call(
            Op.ROOM_LEAVE,
            { w -> request.encode(w) },
            { r -> Acknowledged.decode(r) },
        )
    }

    /**
     * Browses or searches rooms.
     *
     * [query] searches names and topics; [category], [language] and [country] narrow the directory.
     * All four are independent filters combined with and, and a request with none of them is the plain
     * directory listing. Paginate with [RoomListResponse.nextCursor], exactly as in
     * [ConversationsDomain.list].
     */
    suspend fun list(
        limit: Long,
        query: String? = null,
        category: String? = null,
        language: String? = null,
        country: String? = null,
        cursor: String? = null,
    ): RoomListResponse {
        val request = RoomListRequest(limit, query, category, language, country, cursor)
        return rpc.call(
            Op.ROOM_LIST,
            { w -> request.encode(w) },
            { r -> RoomListResponse.decode(r) },
        )
    }

    /**
     * Reads a room's roster: its members, highest role first.
     *
     * [limit] bounds the page and 0 asks for the server's own page size; [after] is the cursor -- the
     * last member's account id from the previous page. Each [com.migo.core.protocol.RosterEntry]
     * carries the member's role as a raw wire value (a [com.migo.core.protocol.RoomRole] discriminant),
     * because the roster is a bulk listing and a role the server adds tomorrow should page through an
     * old client rather than fail to decode.
     */
    suspend fun getRoster(roomId: Id, limit: Int = 0, after: Id? = null): RosterResponse {
        val request = RosterReq(roomId, if (limit > 0) limit.toLong() else null, after)
        return rpc.call(
            Op.ROOM_ROSTER,
            { w -> request.encode(w) },
            { r -> RosterResponse.decode(r) },
        )
    }

    /**
     * Casts a voice in a kick vote against a member, opening the vote if it is the first.
     *
     * The reply is the tally the moment this voice landed: [RoomVoteKickResponse.votes] of
     * [RoomVoteKickResponse.needed], with [RoomVoteKickResponse.open] false once the vote has passed
     * and the kick already landed. Every other member's voice reaches all watchers as a
     * [RoomVoteEvent], so a client that renders a live tally leans on [onVote] for the rest and uses
     * this reply only to reflect the caller's own vote at once.
     */
    suspend fun voteKick(roomId: Id, targetId: Id): RoomVoteKickResponse {
        val request = RoomVoteKick(roomId, targetId)
        return rpc.call(
            Op.ROOM_VOTE_KICK,
            { w -> request.encode(w) },
            { r -> RoomVoteKickResponse.decode(r) },
        )
    }

    /**
     * Applies a moderation action to a member: mute, unmute, kick, ban, or unban.
     *
     * A staff power rather than a vote: the server enforces that the caller outranks the target, so a
     * client should only offer the actions a [com.migo.core.protocol.RoomRole] of Moderator or above
     * holds over a strictly lower role. [reason] is optional and, where a room keeps a moderation log,
     * is what gets written to it. The reply is a bare acknowledgement, so nothing is returned here --
     * the membership change the action causes arrives on its own as a [RoomMemberEvent], and that is
     * what a UI reacts to.
     */
    suspend fun sanction(
        roomId: Id,
        targetId: Id,
        action: SanctionAction,
        reason: String? = null,
    ) {
        val request = RoomSanction(roomId, targetId, action, reason)
        rpc.call(
            Op.ROOM_SANCTION,
            { w -> request.encode(w) },
            { r -> Acknowledged.decode(r) },
        )
    }
}
