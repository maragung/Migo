package com.migo.core.domain

import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.CallEnd
import com.migo.core.protocol.CallInvite
import com.migo.core.protocol.CallSfuParticipant
import com.migo.core.protocol.CallStateEvent
import com.migo.core.protocol.CallTurnResponse
import com.migo.core.protocol.Op
import com.migo.core.protocol.TurnServer
import com.migo.core.wire.Id
import com.migo.core.wire.NIL_ID
import com.migo.core.wire.newId

/**
 * The roster snapshot a joiner's own user topic receives: everything a call screen builds its
 * participant list from, in one frame.
 *
 * [userId]/[deviceId] name the joiner the snapshot was published to -- this account, on this
 * device -- so a UI that wants to mark "you" in the roster can find its own line without asking
 * the server who it is.
 */
data class GroupCallRoster(
    val callId: Id,
    /** The conversation whose members may join this call. */
    val conversationId: Id,
    /** The joiner this snapshot was published to. */
    val userId: Id,
    val deviceId: Id,
    /** The call's size, as the server counted it when publishing. */
    val participantCount: Long,
    /** The full roster, in join order. */
    val participants: List<CallSfuParticipant>,
)

/**
 * A participant joined, as the conversation's topic announces it.
 *
 * [sealedOffer] is the joiner's media description, sealed for the call's members -- the blob a
 * participant's media stack dials the joiner with. Absent only in a future shape this version
 * does not send.
 */
data class GroupCallJoinedEvent(
    val callId: Id,
    val conversationId: Id,
    val userId: Id,
    val deviceId: Id,
    /** The call's size after the join. */
    val participantCount: Long,
    val sealedOffer: ByteArray? = null,
)

/**
 * A participant left, as the conversation's topic announces it.
 *
 * The wire has no "left" state, so a departure rides as `Ended` with a `ByCaller` reason -- the
 * participant withdrew themselves. [participantCount] of zero means the last seat has left and the
 * call is retired: there is nothing to dial back into, and a UI should clear the call.
 */
data class GroupCallLeftEvent(
    val callId: Id,
    val conversationId: Id,
    val userId: Id,
    val deviceId: Id,
    /** The call's size after the departure; zero means the call itself is gone. */
    val participantCount: Long,
)

/** What [GroupCallsDomain.join] resolves with: the call's id and the relays to dial through. */
data class GroupCallJoinResult(
    /** The id the server dedupes the join on -- the one to leave with, and the one the events carry. */
    val callId: Id,
    /** Short-lived TURN relay credentials, as a 1:1 `CALL_TURN_FETCH` would return. */
    val servers: List<TurnServer>,
)

/**
 * The group-call domain: the signaling half of an SFU (server-forwarded) group call.
 *
 * A port of `packages/sdk/src/domains/group-calls.ts`. Where a 1:1 call is two devices exchanging
 * sealed media descriptions with each other, a group call is a *roster*: every participant
 * publishes one sealed offer to the server, the server stores and re-serves those blobs without
 * ever opening them (the same mail-slot promise the rest of the protocol makes -- the server routes
 * bytes, it does not read them), and each client dials the others directly. What lives here is the
 * membership signaling around that: joining a call, leaving it, and the three events a roster UI
 * renders -- the snapshot a joiner builds a screen from, and the join/leave announcements everyone
 * else keeps their list true with.
 *
 * # The two topics, one opcode
 *
 * Every group-call event arrives as `CALL_SFU_EVENT` (a [CallStateEvent] payload, the same struct
 * the 1:1 domain's `CALL_STATE_EVENT` uses), but on two different topics with different shapes:
 *
 *  - The joiner's **own user topic** receives the roster snapshot -- the full participant list --
 *    published by the server as part of answering the join. It is the one frame a joining screen
 *    builds the call from, and it is published rather than replied because the join's reply slot
 *    is already spent on the TURN relay list.
 *  - The **conversation's topic** receives the announcements: one `Connected` frame when a
 *    participant joins (carrying their sealed offer -- the E2E media-description hand-off) and one
 *    `Ended` frame when a participant leaves. A seat replacement -- the same account joining from a
 *    new device -- is a departure followed by an arrival, and the roster hears both facts, in that
 *    order.
 *
 * The domain classifies each frame by shape (the snapshot is the one carrying a participant list)
 * and fans it out to three typed listeners, so a roster UI never has to re-derive which kind of
 * frame it just got.
 *
 * # What this domain does not do
 *
 * Like the 1:1 domain, it holds no call state -- which calls this account is seated in is the
 * application's projection, fed by these events. It never opens or seals anything: the sealed
 * offers pass through in both directions verbatim, and the end-to-end media encryption is the
 * caller's crypto, never the server's and never this domain's. One seat per account is a server
 * rule (a second join from the same account replaces the seat); the client observes it as the
 * departure/arrival pair above, it does not enforce it.
 */
class GroupCallsDomain(
    private val rpc: Rpc,
    private val deviceId: Id,
    private val onEventError: EventErrorHandler? = null,
) {
    private val rosterListeners = ListenerSet<GroupCallRoster>(Op.CALL_SFU_EVENT, onEventError)
    private val joinedListeners = ListenerSet<GroupCallJoinedEvent>(Op.CALL_SFU_EVENT, onEventError)
    private val leftListeners = ListenerSet<GroupCallLeftEvent>(Op.CALL_SFU_EVENT, onEventError)

    private val subscriptions = ArrayList<Subscription>()

    /** Begins delivering group-call events to registered handlers. Idempotent. */
    fun start() {
        if (subscriptions.isNotEmpty()) return
        // One opcode carries all three shapes (see the class doc); the classification happens here,
        // once, so each listener set hands its handlers exactly one typed fact. Any other shape is
        // a future server's; this version has nothing honest to hand a handler for it, and dropping
        // it quietly keeps the roster true to what it can render.
        subscriptions += rpc.on(Op.CALL_SFU_EVENT, { r -> CallStateEvent.decode(r) }) { event, _ ->
            when {
                event.participants != null -> deliverRoster(event)
                event.state == CallState.Connected.wire -> deliverJoined(event)
                event.state == CallState.Ended.wire -> deliverLeft(event)
            }
        }
    }

    /** Stops delivering group-call events. Registered handlers are kept for a later [start]. */
    fun stop() {
        for (subscription in subscriptions) {
            subscription.cancel()
        }
        subscriptions.clear()
    }

    /**
     * Registers a handler for roster snapshots. Returns its unsubscribe.
     *
     * The server publishes one to this account's own topic for every accepted join -- including a
     * re-join after a seat replacement -- so this fires once per [join], never for other
     * participants' movement.
     */
    fun onRoster(listener: Listener<GroupCallRoster>): Subscription = rosterListeners.add(listener)

    /**
     * Registers a handler for join announcements. Returns its unsubscribe.
     *
     * Every seated participant hears these on the conversation's topic, except the joiner's own
     * connection (the snapshot above is that join's event). A seat replacement arrives as a
     * departure ([onParticipantLeft]) followed by one of these, naming the same account on the new
     * device.
     */
    fun onParticipantJoined(listener: Listener<GroupCallJoinedEvent>): Subscription =
        joinedListeners.add(listener)

    /**
     * Registers a handler for departure announcements. Returns its unsubscribe.
     *
     * [GroupCallLeftEvent.participantCount] of zero is the retirement: the last seat has left and
     * the call no longer exists server-side. A leave sent from *this* connection is answered by the
     * reply, not an announcement -- but this account's other devices hear it, which is the point.
     */
    fun onParticipantLeft(listener: Listener<GroupCallLeftEvent>): Subscription =
        leftListeners.add(listener)

    /**
     * Joins (or re-joins) a group call, publishing this device's sealed offer.
     *
     * The `callId` is minted here unless passed -- client-minted ids are the protocol's idempotency
     * key, so a retried join re-seats the same call -- and the reply carries the TURN relays the
     * media plane should dial through, exactly as a 1:1 fetch would. The roster itself does not
     * come back on the reply: the server *publishes* the full participant list to this account's
     * own topic as the [onRoster] snapshot, so register that handler before joining or the snapshot
     * can race a late subscriber.
     *
     * The join frame is the 1:1 invite's shape read with group semantics: `calleeId` is the nil id
     * (a group call has no single callee) and `capabilities` rides as zero, the same as a 1:1
     * invite. `callerDevice` stamps this session's device even though the server takes the joining
     * device from the connection -- an honest frame beats a slot the server fills itself.
     */
    suspend fun join(
        conversationId: Id,
        mediaKind: CallMediaKind,
        sealedOffer: ByteArray,
        callId: Id = newId(),
    ): GroupCallJoinResult {
        // The wire struct is the 1:1 `CallInvite`; the group reader ignores `calleeId` and
        // `callerDevice`, but they are required slots, so they carry their honest values.
        val request = CallInvite(
            callId = callId,
            conversationId = conversationId,
            calleeId = NIL_ID,
            mediaKind = mediaKind.wire,
            callerDevice = deviceId,
            // No codec or feature bits are negotiated in this version; the field rides as zero so a
            // future negotiation has its slot without a wire change.
            capabilities = 0uL,
            sealedOffer = sealedOffer,
        )
        val response =
            rpc.call(Op.CALL_SFU_JOIN, { w -> request.encode(w) }, { r -> CallTurnResponse.decode(r) })
        return GroupCallJoinResult(callId = callId, servers = response.servers)
    }

    /**
     * Leaves a group call.
     *
     * This is the 1:1 end frame -- the server routes a `CALL_END` to the group service when the id
     * names a group call -- stamped with the same `ByCaller` reason the departure announcement
     * carries, because that is the fact: the participant withdrew themselves. When the last seat
     * leaves, the server retires the call; there is nothing to re-join under that id.
     */
    suspend fun leave(callId: Id) {
        val request = CallEnd(callId, CallEndReason.ByCaller.wire)
        rpc.call(Op.CALL_END, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Hands a decoded snapshot to the roster listeners, after checking the fields a roster cannot
     * render without. The server always sets them; a frame missing one is malformed in the same way
     * a frame that fails to decode is, so it goes to the error sink rather than a handler.
     */
    private fun deliverRoster(event: CallStateEvent) {
        val malformed = malformed("roster snapshot", event)
        if (malformed != null) {
            onEventError?.invoke(Op.CALL_SFU_EVENT, malformed)
            return
        }
        rosterListeners.deliver(
            GroupCallRoster(
                callId = event.callId,
                conversationId = requireNotNull(event.conversationId),
                userId = requireNotNull(event.userId),
                deviceId = requireNotNull(event.deviceId),
                participantCount = requireNotNull(event.participantCount),
                participants = event.participants ?: emptyList(),
            ),
        )
    }

    /** Hands a decoded join announcement to the joined listeners, with the same malformed guard. */
    private fun deliverJoined(event: CallStateEvent) {
        val malformed = malformed("join announcement", event)
        if (malformed != null) {
            onEventError?.invoke(Op.CALL_SFU_EVENT, malformed)
            return
        }
        joinedListeners.deliver(
            GroupCallJoinedEvent(
                callId = event.callId,
                conversationId = requireNotNull(event.conversationId),
                userId = requireNotNull(event.userId),
                deviceId = requireNotNull(event.deviceId),
                participantCount = requireNotNull(event.participantCount),
                sealedOffer = event.sealedOffer,
            ),
        )
    }

    /** Hands a decoded departure announcement to the left listeners, with the same guard. */
    private fun deliverLeft(event: CallStateEvent) {
        val malformed = malformed("departure announcement", event)
        if (malformed != null) {
            onEventError?.invoke(Op.CALL_SFU_EVENT, malformed)
            return
        }
        leftListeners.deliver(
            GroupCallLeftEvent(
                callId = event.callId,
                conversationId = requireNotNull(event.conversationId),
                userId = requireNotNull(event.userId),
                deviceId = requireNotNull(event.deviceId),
                participantCount = requireNotNull(event.participantCount),
            ),
        )
    }

    /**
     * The one guard all three deliveries share: a frame missing any field no shape can render
     * without. Returns the exception to report, or null when the frame is well-formed enough to
     * deliver -- the `requireNotNull` calls below only run on the null branch's complement.
     */
    private fun malformed(shape: String, event: CallStateEvent): Exception? =
        if (event.conversationId == null || event.userId == null ||
            event.deviceId == null || event.participantCount == null
        ) {
            Exception("migo: $shape missing required fields")
        } else {
            null
        }
}
