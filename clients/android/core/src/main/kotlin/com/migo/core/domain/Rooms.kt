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
import com.migo.core.protocol.RoomStateEvent
import com.migo.core.wire.Id

/**
 * Rooms: the public and semi-public spaces, as opposed to private conversations.
 *
 * A port of `packages/sdk/src/domains/rooms.ts`. The largest of the small domains, because it is the
 * only one with both requests and two separate event streams -- who is in the room, and what the room
 * itself is doing.
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
 * # Two streams because they answer different questions
 *
 * [RoomMemberEvent] is a person arriving, leaving, or changing role -- a discrete thing a UI puts in
 * the timeline. [RoomStateEvent] is the room's own shape: the online count ticking, a topic change, a
 * slow mode going on. Folding them into one stream would make every member list update wake up the
 * code that renders the header, and in a busy room those fire at very different rates.
 */
class RoomsDomain(
    private val rpc: Rpc,
    onEventError: EventErrorHandler? = null,
) {
    private val memberListeners = ListenerSet<RoomMemberEvent>(Op.ROOM_MEMBER_EVENT, onEventError)
    private val stateListeners = ListenerSet<RoomStateEvent>(Op.ROOM_STATE_EVENT, onEventError)

    @Volatile
    private var subscriptions: List<Subscription>? = null

    /**
     * Begins delivering room events to registered handlers. Idempotent.
     *
     * Both streams are subscribed together: a client that wanted only one of them would still be sent
     * the other by the server, and an unsubscribed opcode reaching [Rpc] is a warning rather than a
     * silent discard.
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
        )
    }

    /** Stops delivery of both streams. Registered handlers are kept for a later [start]. */
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
}
