package com.migo.core.domain

import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.FriendEvent
import com.migo.core.protocol.FriendRespond
import com.migo.core.protocol.FriendTarget
import com.migo.core.protocol.MuteSet
import com.migo.core.protocol.Op
import com.migo.core.protocol.RelationshipEntry
import com.migo.core.protocol.RelationshipKind
import com.migo.core.protocol.RelationshipListReq
import com.migo.core.protocol.SearchReq
import com.migo.core.protocol.SearchResponse
import com.migo.core.protocol.SuggestReq
import com.migo.core.protocol.SuggestedUser
import com.migo.core.wire.Id

/**
 * The social graph: friends, requests, blocks, people search, and suggestions.
 *
 * A port of `packages/sdk/src/domains/social.ts`. The graph is server-owned -- every mutation here
 * asks the server and the caller re-reads [listRelationships] afterwards, because a local mirror
 * would drift the moment either party acted from another device. [onFriendEvent] is the signal that
 * the graph moved; it says who moved, not how, so the answer to it is always a re-read.
 *
 * # Why the client never derives relationship state
 *
 * A friend request accepted from a second device must be reflected by this one, and the only honest
 * source is the server's own list. The event stream exists to tell the client *when* to look, never
 * to spare it the looking: [FriendEvent] carries no direction (incoming vs outgoing) and no removal
 * state, and a client that guessed from it would draw an Accept button on a request that was already
 * answered.
 *
 * # Search is a prefix match on usernames
 *
 * [search] matches username prefixes and returns public profile fields only; display names are not
 * searched. There is deliberately no "list everyone" form -- that would be a directory dump rather
 * than a lookup.
 */
class SocialDomain(
    private val rpc: Rpc,
    onEventError: EventErrorHandler? = null,
) {
    private val friendListeners = ListenerSet<FriendEvent>(Op.FRIEND_EVENT, onEventError)

    @Volatile
    private var subscription: Subscription? = null

    /**
     * Begins delivering friend events to registered handlers. Idempotent.
     *
     * Register handlers with [onFriendEvent] first: nothing is delivered before this is called, so
     * the ordering is what stops a client from missing the first event after connecting.
     */
    fun start() {
        if (subscription != null) return
        subscription = rpc.on(Op.FRIEND_EVENT, { r -> FriendEvent.decode(r) }) { e, _ ->
            friendListeners.deliver(e)
        }
    }

    /** Stops delivery. Registered handlers are kept for a later [start]. */
    fun stop() {
        val live = subscription ?: return
        subscription = null
        live.cancel()
    }

    /**
     * Registers a handler for friendship changes.
     *
     * An event names the other account and a `state` string (`"request"`, `"accepted"`); it is a hint
     * that the graph moved, not a source of truth -- re-read [listRelationships] to draw the right
     * buttons, since the event carries no direction (incoming vs outgoing) and no removal state.
     */
    fun onFriendEvent(listener: Listener<FriendEvent>): Subscription = friendListeners.add(listener)

    /**
     * Sends a friend request.
     *
     * Resolves when the server has accepted the request *for delivery* -- the recipient still has to
     * answer it, and a request the recipient's privacy settings forbid is rejected here with an error.
     * A repeated request while one is pending is idempotent server-side.
     */
    suspend fun friendRequest(userId: Id) {
        val request = FriendTarget(userId)
        rpc.call(Op.FRIEND_REQUEST, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Answers a pending friend request.
     *
     * `accept = false` declines (or withdraws an already-declined request); the edge simply disappears
     * from the graph either way. Only the *recipient* of a request may respond to it.
     */
    suspend fun friendRespond(userId: Id, accept: Boolean) {
        val request = FriendRespond(userId, accept)
        rpc.call(Op.FRIEND_RESPOND, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Blocks an account.
     *
     * One-sided and unnotified: the blocked account is not told, and the block shows only in the
     * blocker's own [listRelationships]. Blocking also tears down any friendship between the two
     * server-side, so a caller that holds the relationship list should refresh it after this resolves.
     */
    suspend fun blockUser(userId: Id) {
        val request = FriendTarget(userId)
        rpc.call(Op.BLOCK_SET, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Mutes or unmutes one account for the caller.
     *
     * A personal choice, not a room's and not the other account's business. Like [blockUser] it is
     * one-sided and unnotified, but softer: a muted account's messages still arrive and simply carry a
     * mark the UI can honour, where a block severs the edge outright. `on = false` lifts the mute. The
     * mute shows only in the caller's own graph, so refresh [listMuted] after this resolves.
     */
    suspend fun muteUser(userId: Id, on: Boolean) {
        val request = MuteSet(userId, on)
        rpc.call(Op.MUTE_SET, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Reads the caller's relationship graph: friends, pending requests in both directions, follows,
     * and blocks, each as a [RelationshipEntry] whose `kind` is a
     * [com.migo.core.protocol.RelationshipKind] value.
     *
     * The list is the caller's own view (the block list of another account is never served), so a
     * caller re-reads it rather than maintaining a local mirror. `limit` bounds the combined result;
     * the default covers a personal graph.
     */
    suspend fun listRelationships(limit: Long = DEFAULT_RELATIONSHIP_LIMIT): List<RelationshipEntry> {
        val request = RelationshipListReq(limit)
        val response = rpc.call(
            Op.RELATIONSHIP_LIST,
            { w -> request.encode(w) },
            { r -> com.migo.core.protocol.RelationshipList.decode(r) },
        )
        return response.entries
    }

    /**
     * Reads the caller's whole relationship graph in one unfiltered list.
     *
     * The wire is the same [listRelationships] call, but the client bounds nothing: `limit` rides as
     * zero, which the server reads as "apply your own page", so every kind the graph holds comes back
     * mixed together and the *caller* filters by `kind` -- the form for a caller that wants the blocks
     * and favourites alongside the friends without naming a page size.
     */
    suspend fun listAllRelationships(): List<RelationshipEntry> = listRelationships(limit = 0L)

    /**
     * The accounts the caller has muted, drawn from the one relationship graph.
     *
     * There is no separate "list mutes" call: mutes ride the same graph as friends and blocks, tagged
     * with [RelationshipKind.Mute], so this reads the whole graph through [listAllRelationships] and
     * keeps only that kind. Each returned entry's `kind` is the mute discriminant and its `userId` is
     * the muted account -- the field a caller drawing a Muted list actually wants.
     */
    suspend fun listMuted(): List<RelationshipEntry> {
        val muted = RelationshipKind.Mute.wire.toLong()
        return listAllRelationships().filter { it.kind == muted }
    }

    /**
     * Friend suggestions: accounts the graph considers relevant, strongest first.
     *
     * Each result carries a mutual-friend count, which is the only signal the server is willing to
     * expose about *why* an account was suggested. `limit` is clamped server-side; null for the
     * server's default page.
     */
    suspend fun suggestions(limit: Long? = null): List<SuggestedUser> {
        val request = SuggestReq(limit = limit)
        val response = rpc.call(
            Op.SUGGESTIONS,
            { w -> request.encode(w) },
            { r -> SearchResponse.decode(r) },
        )
        return response.results
    }

    /**
     * Searches public profiles by username prefix.
     *
     * The match is a prefix match on the username (display names are not searched), and the query is
     * required: there is deliberately no "list everyone" form, since that would be a directory dump
     * rather than a lookup. Results carry only public profile fields.
     */
    suspend fun search(query: String, limit: Long? = null): List<SuggestedUser> {
        val request = SearchReq(query, limit)
        val response = rpc.call(
            Op.SEARCH,
            { w -> request.encode(w) },
            { r -> SearchResponse.decode(r) },
        )
        return response.results
    }

    private companion object {
        /** The personal-graph page the bounded read asks for by default. */
        const val DEFAULT_RELATIONSHIP_LIMIT: Long = 100L
    }
}
