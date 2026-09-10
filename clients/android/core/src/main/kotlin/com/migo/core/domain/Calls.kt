package com.migo.core.domain

import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.CallAnswer
import com.migo.core.protocol.CallCancel
import com.migo.core.protocol.CallDecline
import com.migo.core.protocol.CallEnd
import com.migo.core.protocol.CallIce
import com.migo.core.protocol.CallInvite
import com.migo.core.protocol.CallInviteEvent
import com.migo.core.protocol.CallInviteResult
import com.migo.core.protocol.CallSdp
import com.migo.core.protocol.CallStateEvent
import com.migo.core.protocol.CallStats
import com.migo.core.protocol.CallTurnFetch
import com.migo.core.protocol.CallTurnResponse
import com.migo.core.protocol.Op
import com.migo.core.protocol.TurnServer
import com.migo.core.wire.Id
import com.migo.core.wire.newId

/**
 * The life of a call as the server reports it. These are the wire's five states; the product
 * surface (section 180) adds a sixth, *degraded*, which is a connected call whose quality has
 * dropped — a client-side judgement from live media statistics, not a signaling fact, so it is not
 * on the wire and not in this enum (see [CallDisplayState.Degraded]).
 */
enum class CallState(val wire: Long) {
    /** The invite is out and the callee's client is (or will be) ringing. */
    Ringing(0),

    /** Answered; SDP and ICE are being exchanged but media is not flowing yet. */
    Connecting(1),

    /** Media is flowing. */
    Connected(2),

    /** The transport dropped and the reconnect window (ICE restart, renegotiation) is running. */
    Reconnecting(3),

    /** Terminal, always with a reason ([CallEndReason]). */
    Ended(4),
    ;

    companion object {
        /** Narrows a wire state; an unknown value yields `null`, never a guess. */
        fun fromWire(wire: Long): CallState? = entries.firstOrNull { it.wire == wire }
    }
}

/**
 * The calls domain: the signaling half of a 1:1 voice or video call.
 *
 * A port of `packages/sdk/src/domains/calls.ts`. A call is two planes with nothing in common but the
 * `callId`. The media plane is WebRTC: SDP and ICE candidates exchanged *through* this domain as
 * opaque sealed bytes, then media flowing directly between the two devices, encrypted end to end,
 * never touching a Migo server. The signaling plane is what lives here: invite, answer, decline,
 * cancel, end, and the relaying of those sealed SDP and ICE blobs — the server routes them by device
 * id but cannot read them, because an SDP body carries DTLS fingerprints and ICE candidates carry
 * the two parties' network addresses, and a signaling server that could read them would learn
 * exactly what the end-to-end promise exists to protect (§165).
 *
 * # Why the caller learns the callee's device from the answer, not the invite
 *
 * [invite] names the callee *account*, and the server fans the invite out to that account's devices;
 * which device answers is not knowable in advance. So a caller cannot address [sendSdp] or [sendIce]
 * until an answer comes back — the answer's relay ([onSdp]) carries the answering device in
 * `fromDevice`, and only then does the caller have a target for its own candidates. A callee has no
 * such wait: the invite event already names `callerDevice`.
 *
 * # What this domain does not do
 *
 * It holds no call state. Which calls are ringing, connected, or ended is a *product* projection the
 * application keeps; the server pushes the authoritative transitions through [onCallState]. It also
 * never opens or seals anything: the bytes handed to [invite], [answer], [sendSdp], and [sendIce]
 * are already sealed by the caller (see `CallSignal.kt` for this build's seal), and the bytes handed
 * back through [onIncomingCall] and the relay listeners are passed through verbatim.
 */
class CallsDomain(
    private val rpc: Rpc,
    private val deviceId: Id,
    onEventError: EventErrorHandler? = null,
) {
    private val inviteListeners = ListenerSet<CallInviteEvent>(Op.CALL_INVITE_EVENT, onEventError)
    private val stateListeners = ListenerSet<CallStateEvent>(Op.CALL_STATE_EVENT, onEventError)
    private val sdpListeners = ListenerSet<CallSdp>(Op.CALL_SDP, onEventError)
    private val iceListeners = ListenerSet<CallIce>(Op.CALL_ICE, onEventError)

    private val subscriptions = ArrayList<Subscription>()

    /** Begins delivering call events to registered handlers. Idempotent. */
    fun start() {
        if (subscriptions.isNotEmpty()) return
        subscriptions += rpc.on(Op.CALL_INVITE_EVENT, { r -> CallInviteEvent.decode(r) }) { event, _ ->
            inviteListeners.deliver(event)
        }
        subscriptions += rpc.on(Op.CALL_STATE_EVENT, { r -> CallStateEvent.decode(r) }) { event, _ ->
            stateListeners.deliver(event)
        }
        // A relay is addressed to one device; one sealed for another device (the server fans an
        // invite out to every device of the callee account, and answers may come from any of them)
        // is not ours to open. Delivering only relays addressed to this device keeps a handler from
        // ever seeing a blob it has no session for.
        subscriptions += rpc.on(Op.CALL_SDP, { r -> CallSdp.decode(r) }) { event, _ ->
            if (event.toDevice == deviceId) {
                sdpListeners.deliver(event)
            }
        }
        subscriptions += rpc.on(Op.CALL_ICE, { r -> CallIce.decode(r) }) { event, _ ->
            if (event.toDevice == deviceId) {
                iceListeners.deliver(event)
            }
        }
    }

    /** Stops delivering call events. Registered handlers are kept for a later [start]. */
    fun stop() {
        for (subscription in subscriptions) {
            subscription.cancel()
        }
        subscriptions.clear()
    }

    /** Registers a handler for inbound invites (another account is calling us). */
    fun onIncomingCall(listener: Listener<CallInviteEvent>): Subscription = inviteListeners.add(listener)

    /**
     * Registers a handler for a call's state transitions.
     *
     * The server is the authority on Ringing/Connecting/Connected/Ended; a client's own transport
     * observations refine *between* these events (its reconnect window, its degraded judgement) but
     * must not contradict an `Ended`, which is terminal.
     */
    fun onCallState(listener: Listener<CallStateEvent>): Subscription = stateListeners.add(listener)

    /**
     * Registers a handler for SDP relays addressed to this device.
     *
     * For a caller this is how the answer arrives; for a callee, a renegotiated offer (a future
     * flow). The blob is sealed for this device — the handler receives it verbatim and opening it is
     * the application's crypto, never the server's.
     */
    fun onSdp(listener: Listener<CallSdp>): Subscription = sdpListeners.add(listener)

    /**
     * Registers a handler for batched ICE candidate relays addressed to this device.
     *
     * One relay carries a whole batch, not one candidate — a session's gathering can produce tens of
     * candidates, and one frame each is exactly the signaling storm the batch shape exists to avoid.
     */
    fun onIce(listener: Listener<CallIce>): Subscription = iceListeners.add(listener)

    /**
     * Places a call: sends the sealed offer and returns the server's verdict.
     *
     * The `callId` is minted here when the caller does not supply one — client-minted ids are the
     * protocol's idempotency key, so a retried invite re-rings the same call rather than placing a
     * second one — and is echoed in the [CallInviteResult]; track the call under that id. A caller
     * that needs the id *before* the invite lands (to fetch TURN relays for the peer connection that
     * will produce the offer — [turnServers] must be addressed before the call exists server-side,
     * and its handler charges nothing and reads no call state, so the id of the call about to be
     * placed is the honest key) passes its own minted `callId`; the reply echoes whichever id was
     * sent. The result's `status` says whether the callee is being rung ([INVITE_RINGING]), or why
     * not (declined, expired, blocked); `expiresAt` is the moment an unanswered invite ends itself
     * with [CallEndReason.NoAnswer].
     */
    suspend fun invite(
        conversationId: Id,
        calleeId: Id,
        mediaKind: CallMediaKind,
        sealedOffer: ByteArray,
        callId: Id = newId(),
    ): CallInviteResult {
        val request = CallInvite(
            callId = callId,
            conversationId = conversationId,
            calleeId = calleeId,
            mediaKind = mediaKind.wire,
            callerDevice = deviceId,
            // No codec or feature bits are negotiated in this version; the field rides as zero so a
            // future negotiation has its slot without a wire change.
            capabilities = 0uL,
            sealedOffer = sealedOffer,
        )
        return rpc.call(Op.CALL_INVITE, { w -> request.encode(w) }, { r -> CallInviteResult.decode(r) })
    }

    /**
     * Answers a ringing call with the sealed SDP answer.
     *
     * This tells the *server* the call is answered (the caller's client learns of it through the
     * state stream); the answer itself reaches the caller's WebRTC stack through a `CALL_SDP` relay,
     * which the application sends separately via [sendSdp] — this method does not send it, because
     * the relay needs the caller's device id, which lives in the invite event the application holds,
     * not in this domain.
     */
    suspend fun answer(callId: Id, sealedAnswer: ByteArray) {
        val request = CallAnswer(callId, deviceId, sealedAnswer)
        rpc.call(Op.CALL_ANSWER, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Declines a ringing call.
     *
     * [CallDeclineReason.Busy] tells the caller's client to stop ringing immediately without
     * implying a human refusal; the default, [CallDeclineReason.Declined], is the human's "no".
     */
    suspend fun decline(callId: Id, reason: CallDeclineReason = CallDeclineReason.Declined) {
        val request = CallDecline(callId, reason.wire)
        rpc.call(Op.CALL_DECLINE, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Cancels a call we placed while it is still ringing.
     *
     * Cancel is the caller's pre-answer exit; after media connects the same intent is [end] with
     * [CallEndReason.ByCaller].
     */
    suspend fun cancel(callId: Id) {
        val request = CallCancel(callId)
        rpc.call(Op.CALL_CANCEL, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Ends an established call, always with a reason.
     *
     * The reason is what the other side's ended screen states, and the server records it as call
     * history metadata. Hang up as [CallEndReason.ByCaller] or [CallEndReason.ByCallee]; the system
     * reasons are for failures this side detected.
     */
    suspend fun end(callId: Id, reason: CallEndReason) {
        val request = CallEnd(callId, reason.wire)
        rpc.call(Op.CALL_END, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Relays a sealed SDP offer or answer to one device of the peer.
     *
     * `toDevice` is the peer device the blob is sealed for — for a caller, the answering device from
     * the relayed answer's `fromDevice`; for a callee, the invite event's `callerDevice`. This
     * domain stamps `fromDevice` with this session's device id.
     */
    suspend fun sendSdp(callId: Id, toDevice: Id, sealedSdp: ByteArray) {
        val request = CallSdp(callId, deviceId, toDevice, sealedSdp)
        rpc.call(Op.CALL_SDP, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Relays a batch of sealed ICE candidates to one device of the peer.
     *
     * Batch before calling this — one frame per candidate is the anti-pattern the wire comment calls
     * out — and hold a short linger so a trickling batch leaves as few frames as it can.
     */
    suspend fun sendIce(callId: Id, toDevice: Id, sealedCandidates: ByteArray) {
        val request = CallIce(callId, deviceId, toDevice, sealedCandidates)
        rpc.call(Op.CALL_ICE, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    /**
     * Fetches short-lived TURN relay credentials for a call.
     *
     * For when direct P2P fails (symmetric NAT, corporate firewall, UDP blocked): credentials are
     * minted per call and never embedded in the client. v1 clients call P2P-only and do not fetch;
     * the method is here so the fallback path needs no domain change.
     */
    suspend fun turnServers(callId: Id): List<TurnServer> {
        val request = CallTurnFetch(callId)
        val response = rpc.call(Op.CALL_TURN_FETCH, { w -> request.encode(w) }, { r -> CallTurnResponse.decode(r) })
        return response.servers
    }

    /**
     * Reports aggregate call-quality numbers for a call.
     *
     * `CALL_STATS` is Droppable — a lost report costs nothing — and carries only aggregate numbers:
     * setup time, round-trip time, loss, jitter, whether TURN was used. Never any call content. The
     * fields are optional; send what this call measured and leave the rest unset.
     */
    suspend fun reportStats(
        callId: Id,
        setupMs: Long? = null,
        rttMs: Long? = null,
        packetLoss: Long? = null,
        jitterMs: Long? = null,
        usedTurn: Boolean? = null,
    ) {
        val request = CallStats(callId, setupMs, rttMs, packetLoss, jitterMs, usedTurn)
        rpc.call(Op.CALL_STATS, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }
}
