package com.migo.app.call

import android.content.Context
import com.migo.core.ConnectionState
import com.migo.core.MigoClient
import com.migo.core.crypto.Content
import com.migo.core.domain.CALL_KEY_EVENT
import com.migo.core.domain.CallDeclineReason
import com.migo.core.domain.CallEndReason
import com.migo.core.domain.CallMediaKind
import com.migo.core.domain.CallState
import com.migo.core.domain.INVITE_RINGING
import com.migo.core.domain.IceCandidateJson
import com.migo.core.domain.IncomingInviteDisposition
import com.migo.core.domain.SdpDescription
import com.migo.core.domain.Subscription
import com.migo.core.domain.answersRingingCall
import com.migo.core.domain.decodeCallKeyEvent
import com.migo.core.domain.decodeIceBatch
import com.migo.core.domain.decodeSdpDescription
import com.migo.core.domain.encodeCallKeyEvent
import com.migo.core.domain.encodeIceBatch
import com.migo.core.domain.encodeSdpDescription
import com.migo.core.domain.endsRingingCall
import com.migo.core.domain.generateCallKey
import com.migo.core.domain.incomingInviteDisposition
import com.migo.core.domain.inviteEndReason
import com.migo.core.domain.openCallSignal
import com.migo.core.domain.ringTimeoutMs
import com.migo.core.domain.sealCallSignal
import com.migo.core.protocol.CallIce
import com.migo.core.protocol.CallInviteEvent
import com.migo.core.protocol.CallSdp
import com.migo.core.protocol.CallStateEvent
import com.migo.core.wire.Id
import com.migo.core.wire.newId
import java.io.IOException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import org.webrtc.AddIceObserver
import org.webrtc.AudioSource
import org.webrtc.AudioTrack
import org.webrtc.DataChannel
import org.webrtc.IceCandidate
import org.webrtc.MediaConstraints
import org.webrtc.MediaStream
import org.webrtc.PeerConnection
import org.webrtc.PeerConnectionFactory
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import org.webrtc.audio.JavaAudioDeviceModule

/**
 * The call manager: one instance per live session, owning this device's single live call.
 *
 * A port of `clients/web/src/lib/migo/call-manager.tsx`. The core's calls domain is pure signaling
 * -- it sends what it is handed and delivers what arrives. This class is the piece above it a call
 * UI actually needs: it drives a WebRTC peer connection in step with the signaling (offer on
 * invite, answer on accept, candidates relaying in both directions), projects the tracked
 * [ActiveCall] the overlay renders through a single [StateFlow], and owns the teardown so no path
 * -- hang up, decline, cancel, expiry, a dropped session -- leaves a microphone open or a peer
 * connection half-alive. One instance is created by [com.migo.app.AppViewModel] where the session
 * is established and closed where it ends; the screens never see the peer connection, only the
 * state and the action methods.
 *
 * This build carries **voice only**: the UI offers the call button on a direct chat and it always
 * places audio. A *video* invite from a newer peer is still answered -- an answer with fewer
 * m-lines than the offer rejects the extra ones, which is ordinary WebRTC, so the caller keeps the
 * conversation and simply sees no video -- and the [ActiveCall.mediaKind] field keeps the invited
 * kind so a future video build has a place to land.
 *
 * # Why the caller cannot send ICE until the answer arrives
 *
 * An invite names the callee *account*, and the server rings every device on it; which device
 * answers is not knowable in advance. The answering device names itself in the answer relay's
 * `fromDevice`, so until that arrives a caller's gathered candidates have nowhere to go -- they
 * batch here, and flush the moment the answer lands. A callee has no such wait: the invite event
 * already carried `callerDevice`.
 *
 * # The reconnect window
 *
 * A transport blip must not end a call: the peer connection going disconnected shows *Reconnecting*
 * and starts a window; media coming back cancels it; the window expiring ends the call as
 * [CallEndReason.Network] -- and, since the server and the peer still think the call is live, fires
 * a best-effort `CALL_END` so the other side is spared the whole window. This build does not yet
 * attempt the ICE restart inside the window, so the window is a grace period, not a recovery
 * attempt.
 *
 * # The ring's lifecycle
 *
 * Three facts keep a ring honest. Invites are Critical frames, delivered at least once, so a
 * redelivered invite names a call this device already knows -- it is ignored, never declined, or
 * the decline would hang up the very ring it re-announces. An unanswered invite ends itself at
 * `expiresAt`; both sides arm a local mirror of that deadline -- the caller's so its "Calling…"
 * screen never outlives the invite, the callee's so a ring never outlives it either -- even if the
 * server's `Ended` event is late or lost. And an event for the call still ringing inbound retires
 * the ring with a note: an `Ended` says the caller gave up (a missed call), while a `Connecting` or
 * `Connected` says a sibling device answered -- the call moved, it was not missed -- because a
 * screen that keeps ringing a dead call teaches its user to distrust every ring after it.
 *
 * # The call key
 *
 * Every SDP and ICE blob this manager sends is sealed by [sealCallSignal] under a per-call key,
 * and the server relays bytes it cannot read. The key is minted here, by the caller, and reaches
 * the conversation inside the E2EE message layer -- a control event sent through the messaging
 * domain *before* the invite, so every device that may answer holds the key by the time the ring
 * arrives. The callee's side of that contract is the accept path's wait: a key that has not landed
 * yet (the frames crossed, briefly) is waited for, not failed over. The key lives in [callKeys]
 * for the session's calls and is forgotten when its call ends.
 *
 * Media content is never logged here -- not the SDP, not the candidates, not the key; failures are
 * recorded as facts ("could not start the call"), never payloads. The WebRTC observers below keep
 * that rule too: their failure strings are swallowed, not printed, because an SDP failure line can
 * quote the very bytes the seal exists to protect.
 *
 * # Threading
 *
 * Actions arrive on the main thread from Compose; the core's listeners and the WebRTC observers
 * arrive on the SDK's and the signaling thread. Everything shared across them is either
 * `@Volatile`, a `StateFlow`, a `ConcurrentHashMap`, or guarded by [iceLock] -- the same
 * refs-first discipline the web build keeps with its refs and the Kotlin mirrors with volatile
 * fields, because a handler registered once per session reads state synchronously, not through
 * recomposition.
 */
class CallManager(
    context: Context,
    private val client: MigoClient,
    private val accountId: Id,
    /** The session's own scope: every ordinary launch, and every timer, dies with it. */
    private val scope: CoroutineScope,
    /**
     * A scope that outlives the session's own -- the application's. [close] fires its last
     * `CALL_END` here, because the web build's `beforeunload` analogue is the view model being
     * cleared, and by then the session scope is already cancelled.
     */
    private val closingScope: CoroutineScope,
) {
    /** How long gathered ICE candidates linger before one relay carries them (batch, briefly). */
    private val iceLingerMs = 250L

    /** How long a disconnected transport gets before the call ends as a network failure. */
    private val reconnectWindowMs = 30_000L

    /**
     * How long an accept waits for the call's key before giving up on answering. The caller sends
     * the key message and *then* the invite, so in the ordinary case the key is already here when
     * the user taps accept -- the wait is for the frames crossing on a slow connection.
     */
    private val callKeyWaitMs = 5_000L

    // --- the state the screens read ---

    private val _state = MutableStateFlow(CallUiState())

    /** The one thing the call overlay is a function of. */
    val state: StateFlow<CallUiState> = _state.asStateFlow()

    /** Whether a call occupies this device; an ended one on screen does not block a new one. */
    private fun callInProgress(): Boolean = active != null && active.state != CallState.Ended

    // --- shared refs (see the threading note in the class doc) ---

    @Volatile private var active: ActiveCall? = null
    @Volatile private var incoming: CallInviteEvent? = null
    @Volatile private var peerDevice: Id? = null
    @Volatile private var remoteDescriptionSet = false
    @Volatile private var muted = false
    /** The placement guard: closed from the first synchronous step of [startCall] to the last. */
    @Volatile private var starting = false
    /** Whether a `CALL_END` has already gone out -- a hang-up, a network death, and a closing
     * session all end the call locally; only the first of them should reach the server. */
    @Volatile private var endSent = false
    @Volatile private var setupStart: Long? = null

    private val iceLock = Any()
    /** Candidates gathered but not yet relayed, waiting for the batch linger or a target device. */
    private val iceBatch = ArrayList<IceCandidateJson>()
    /** Candidates the peer relayed before this side's remote description was set, applied after. */
    private val heldIce = ArrayList<IceCandidateJson>()

    @Volatile private var iceTimer: Job? = null
    @Volatile private var reconnectTimer: Job? = null
    /** The caller's local mirror of the invite's expiry, while its call still rings unanswered. */
    @Volatile private var ringTimer: Job? = null
    /** The callee's mirror of the same deadline, for a ring the server's `Ended` may never reap. */
    @Volatile private var incomingTimer: Job? = null

    /** The sealing key of each call this session has seen, forgotten with its call. */
    private val callKeys = HashMap<Id, ByteArray>()

    /** Accepts waiting for a call's key, completed the moment a key event adopts one. */
    private val callKeyWaiters = HashMap<Id, CompletableDeferred<ByteArray>>()

    private val keyLock = Any()

    // --- the WebRTC engine ---

    /**
     * The process-global WebRTC initializer, run once. PeerConnectionFactory.initialize is
     * idempotent-in-effect but noisy when repeated, and a second manager (a re-signed-in session)
     * must not pay for the native library twice.
     */
    private companion object {
        @Volatile private var webrtcInitialized = false
    }

    /**
     * The media device module: the microphone and the speaker. Built per manager rather than per
     * call so the audio focus the module requests belongs to the session that owns the calls --
     * JavaAudioDeviceModule puts the phone in communication mode and takes audio focus for
     * itself, which is exactly the behaviour a call app wants and exactly what must not be
     * replicated per call.
     */
    private val audioModule: JavaAudioDeviceModule

    private val factory: PeerConnectionFactory

    @Volatile private var peer: PeerConnection? = null
    @Volatile private var audioSource: AudioSource? = null
    @Volatile private var audioTrack: AudioTrack? = null

    init {
        synchronized(CallManager::class.java) {
            if (!webrtcInitialized) {
                PeerConnectionFactory.initialize(
                    PeerConnectionFactory.InitializationOptions.builder(context)
                        .setEnableInternalTracer(false)
                        .createInitializationOptions(),
                )
                webrtcInitialized = true
            }
        }
        audioModule = JavaAudioDeviceModule.builder(context).createAudioDeviceModule()
        factory = PeerConnectionFactory.builder()
            .setAudioDeviceModule(audioModule)
            .createPeerConnectionFactory()
    }

    // --- lifecycle ---

    /**
     * Bridges the manager onto the client's reconnect-surviving streams. The returned
     * subscriptions are the caller's to cancel; [close] does the rest (the media teardown, the
     * best-effort end).
     */
    fun attach(): List<Subscription> = listOf(
        // The call key rides the message layer, not the call streams, so its subscription lives
        // beside them: a control event naming a call-key event adopts the key, waking an accept
        // that is waiting on it. Anything else -- every ordinary message -- is not this manager's
        // business.
        client.onMessage { message ->
            val content = message.content
            if (content is Content.ControlEvent && content.event == CALL_KEY_EVENT) {
                content.data?.let(::decodeCallKeyEvent)?.let { (callId, key) ->
                    adoptCallKey(callId, key)
                }
            }
        },
        client.onIncomingCall(::handleIncoming),
        client.onCallState(::handleStateEvent),
        client.onCallSdp(::handleSdp),
        client.onCallIce(::handleIceRelay),
    )

    /**
     * Ends everything this manager holds: timers, candidates, the peer connection, the microphone.
     *
     * A call still live is ended on the wire first (best effort, on the outliving scope -- this
     * runs from sign-out and from the view model's clearing, and in the latter the session scope
     * is already dead). The reason is this side's hang-up, not `Network`: the user chose to leave,
     * and the peer's screen should say the call ended, not that a connection was lost.
     */
    fun close() {
        val call = active
        if (call != null && call.state != CallState.Ended && !endSent) {
            endSent = true
            val reason = if (call.isCaller) CallEndReason.ByCaller else CallEndReason.ByCallee
            closingScope.launch {
                try {
                    client.calls.end(call.callId, reason)
                } catch (_: Exception) {
                    // Whether the frame beats the socket's death is the same race the web build's
                    // beforeunload fires into; losing it costs only the peer's wait.
                }
            }
        }
        teardownMedia()
        active?.let { forgetCallKey(it.callId) }
        active = null
        incoming = null
        _state.value = CallUiState()
        // The native audio module and the factory are the session's own; a later session builds
        // its own. Disposing them frees the native threads a leaked factory would pin forever.
        audioModule.release()
        factory.dispose()
    }

    /**
     * The session's connection state, relayed from the session hooks. A *Closed* session is the
     * web build's "client is gone": no signaling survives it, so a live call ends here as
     * [CallEndReason.Network]. A *Reconnecting* session does not -- media keeps flowing peer to
     * peer while the signaling reconnects, which is the whole point of the two-plane split.
     */
    fun onConnectionState(next: ConnectionState) {
        if (next == ConnectionState.Closed && callInProgress()) {
            finishCall(CallEndReason.Network)
        }
    }

    // --- the flows the UI calls ---

    /**
     * Places a call: key, media, offer, invite -- and tracks it under the id the reply echoes.
     *
     * The placement guard is a synchronous flag, not the tracked call: `active` only exists once
     * the invite replies, so a second tap during the TURN fetch would otherwise open a second
     * peer connection -- a microphone the user revoked the call of, on a line nobody will ever
     * answer. [starting] closes that window from the first synchronous step to the last.
     */
    fun startCall(conversationId: Id, calleeId: Id, mediaKind: CallMediaKind) {
        if (starting || callInProgress() || incoming != null) {
            return
        }
        starting = true
        scope.launch {
            // Minted before the try so the catch can forget the key it adopts: a placement that
            // dies mid-flight must not leave its key in the session map, and the id is the map's
            // handle for it. The id is minted here rather than inside the invite so the TURN
            // fetch can name the call it belongs to.
            val callId = newId()
            try {
                setupStart = now()
                _state.update { it.copy(callError = null) }
                val callKey = generateCallKey()
                client.messaging.send(
                    conversationId,
                    Content.ControlEvent(CALL_KEY_EVENT, encodeCallKeyEvent(callId, callKey)),
                )
                adoptCallKey(callId, callKey)

                val pc = createPeer(iceServersForCall(callId))
                val source = factory.createAudioSource(MediaConstraints())
                val track = factory.createAudioTrack("migo-voice", source)
                pc.addTrack(track, listOf("migo"))
                audioSource = source
                audioTrack = track

                val offer = pc.offer()
                pc.setLocalDescription(offer)
                val result = client.calls.invite(
                    conversationId,
                    calleeId,
                    mediaKind,
                    sealCallSignal(
                        encodeSdpDescription(offerDescription(offer)),
                        callKey,
                        callId,
                    ),
                    callId,
                )

                if (result.status != INVITE_RINGING) {
                    // Never rang: the callee's settings (or the invite's expiry) answered first.
                    teardownMedia()
                    forgetCallKey(callId)
                    active = ActiveCall(
                        callId = result.callId,
                        conversationId = conversationId,
                        callerId = accountId,
                        calleeId = calleeId,
                        mediaKind = mediaKind,
                        state = CallState.Ended,
                        // The wire's own refusal vocabulary rides alongside the reason: a blocked
                        // refusal must read differently from a declined one, and the reason enum
                        // cannot say which.
                        endReason = inviteEndReason(result.status),
                        inviteStatus = result.status,
                        isCaller = true,
                    )
                    _state.update {
                        it.copy(call = active, endedAt = now(), incoming = null, callError = null)
                    }
                    return@launch
                }
                active = ActiveCall(
                    callId = result.callId,
                    conversationId = conversationId,
                    callerId = accountId,
                    calleeId = calleeId,
                    mediaKind = mediaKind,
                    state = CallState.Ringing,
                    isCaller = true,
                )
                // A previous call's ended screen may still be up; its timestamps belong to it.
                _state.update {
                    it.copy(call = active, endedAt = null, incoming = null, callError = null)
                }
                armRingTimeout(result.callId, result.expiresAt)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                // Nothing was invited (permissions, no device, no key message) or the invite never
                // landed: no call exists to show, so state the failure as a fact instead of a dead
                // button.
                teardownMedia()
                forgetCallKey(callId)
                _state.update {
                    it.copy(
                        callError = if (failure is SecurityException) {
                            MICROPHONE_UNAVAILABLE
                        } else {
                            "Could not start the call."
                        },
                    )
                }
            } finally {
                starting = false
            }
        }
    }

    /**
     * Answers the ringing call: media, the peer's offer applied, our answer sealed and relayed.
     *
     * The ring is retired synchronously before the launch, so a second tap on accept meets no ring
     * to answer -- the same guard [startCall]'s flag provides for placement, expressed through the
     * thing being consumed.
     */
    fun acceptCall() {
        val invite = incoming ?: return
        if (callInProgress()) {
            return
        }
        setIncoming(null)
        _state.update { it.copy(endedAt = null, callError = null) }
        scope.launch {
            val mediaKind = CallMediaKind.fromWire(invite.mediaKind)
            setupStart = now()
            // Whether the answer itself reached the server. The catch path's honesty depends on
            // it: before the answer lands, "cannot answer" genuinely is a busy fact -- this device
            // never picked up, so the ring is free to die as Busy; after it lands, the call is
            // *connecting* and a media failure is a failure, and reporting it as a busy (let
            // alone a human decline) would tell the caller a person refused what a device merely
            // failed to do.
            var answerLanded = false
            active = ActiveCall(
                callId = invite.callId,
                conversationId = invite.conversationId,
                callerId = invite.callerId,
                calleeId = accountId,
                mediaKind = mediaKind,
                state = CallState.Connecting,
                isCaller = false,
            )
            _state.update { it.copy(call = active) }
            // The invite already named the calling device, so this side's relays have a target at
            // once. A video invite is answered with the microphone this build has: the extra
            // m-lines are rejected by the answer, which is how WebRTC says "audio only".
            peerDevice = invite.callerDevice
            try {
                // The call's key arrives through the message layer, sent before the invite; on the
                // rare crossing it is waited for here rather than failed over.
                val callKey = waitForCallKey(invite.callId, callKeyWaitMs)
                    ?: throw IOException("the call key has not arrived")

                val pc = createPeer(iceServersForCall(invite.callId))
                val source = factory.createAudioSource(MediaConstraints())
                val track = factory.createAudioTrack("migo-voice", source)
                pc.addTrack(track, listOf("migo"))
                audioSource = source
                audioTrack = track

                val offer = decodeSdpDescription(
                    openCallSignal(invite.sealedOffer, callKey, invite.callId),
                )
                pc.setRemoteDescription(offer)
                remoteDescriptionSet = true
                drainHeldIce()

                val answer = pc.answer()
                pc.setLocalDescription(answer)
                val sealedAnswer =
                    sealCallSignal(
                        encodeSdpDescription(answerDescription(answer)),
                        callKey,
                        invite.callId,
                    )
                client.calls.answer(invite.callId, sealedAnswer)
                client.calls.sendSdp(invite.callId, invite.callerDevice, sealedAnswer)
                answerLanded = true
                flushIce()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                // Could not answer (permissions, malformed or unopenable offer, a key that never
                // arrived): give the caller their "no" and show the failure here -- a ring that
                // can never be picked up is worse than a decline.
                if (!answerLanded) {
                    scope.launch {
                        try {
                            client.calls.decline(invite.callId, CallDeclineReason.Busy)
                        } catch (_: Exception) {
                            // The invite expires on its own; a failed decline changes nothing here.
                        }
                    }
                } else {
                    scope.launch {
                        try {
                            client.calls.end(invite.callId, CallEndReason.Failed)
                        } catch (_: Exception) {
                            // The server sweeps its own call expiry; this end is the prompt exit.
                        }
                    }
                }
                finishCall(CallEndReason.Failed)
            }
        }
    }

    /** Declines the ringing inbound call. */
    fun declineCall() {
        val invite = incoming ?: return
        setIncoming(null)
        scope.launch {
            try {
                client.calls.decline(invite.callId)
            } catch (_: Exception) {
                // The invite expires on its own; a failed decline changes nothing on our side.
            }
        }
    }

    /** Cancels the call this device placed while it still rings. */
    fun cancelCall() {
        val call = active ?: return
        if (call.state == CallState.Ended) {
            return
        }
        finishCall(CallEndReason.ByCaller)
        scope.launch {
            try {
                client.calls.cancel(call.callId)
            } catch (_: Exception) {
                // The server sweeps its own expiry; this cancel is the prompt exit.
            }
        }
    }

    /** Hangs up the established call, with the reason that says whose button ended it. */
    fun hangUp() {
        val call = active ?: return
        if (call.state == CallState.Ended) {
            return
        }
        endCall(if (call.isCaller) CallEndReason.ByCaller else CallEndReason.ByCallee)
    }

    /** Ends the established call with an explicit reason. */
    fun endCall(reason: CallEndReason) {
        val call = active ?: return
        if (call.state == CallState.Ended) {
            return
        }
        // Marked before the local teardown so a `Network` reason inside finishCall knows this end
        // is already on its way and does not pay for the same exit twice.
        endSent = true
        finishCall(reason)
        scope.launch {
            try {
                client.calls.end(call.callId, reason)
            } catch (_: Exception) {
                // The peer's reconnect window is the cost of a lost end; the server's own sweep
                // bounds it.
            }
        }
    }

    /** Mutes or unmutes this side's microphone. */
    fun toggleMute() {
        muted = !muted
        audioTrack?.setEnabled(!muted)
        _state.update { it.copy(muted = muted) }
    }

    /** Dismisses the ended screen (or a placement error), leaving no call tracked. */
    fun dismissCall() {
        val call = active
        if (call != null && call.state != CallState.Ended) {
            return
        }
        active = null
        _state.update { CallUiState() }
    }

    /**
     * States a placement failure the UI discovered before the manager was involved -- the one
     * case this build has is the microphone permission being refused, which the activity learns
     * from the permission launcher, not from an exception.
     */
    fun placementFailed(message: String) {
        _state.update { it.copy(callError = message) }
    }

    // --- tracked-state writers ---

    /**
     * Sets the ringing inbound invite, arming the callee's mirror of the invite's expiry: the
     * server's sweep ends the call on its side, but its `Ended` can be late or lost, and a ring
     * must never outlive the invite that backs it. Firing with a *different* invite showing (or
     * none) does nothing -- the timer belongs to the call it was armed for, not to whatever rings
     * next.
     */
    private fun setIncoming(event: CallInviteEvent?) {
        incomingTimer?.cancel()
        incomingTimer = null
        if (event != null) {
            val callId = event.callId
            incomingTimer = scope.launch {
                delay(ringTimeoutMs(event.expiresAt, now()))
                if (incoming?.callId == callId) {
                    incoming = null
                    _state.update { it.copy(incoming = null, callError = MISSED_CALL_MESSAGE) }
                }
            }
        }
        incoming = event
        _state.update { it.copy(incoming = event) }
    }

    /**
     * Marks the call connected, zeroing the duration timer exactly once and reporting setup time.
     */
    private fun markConnected() {
        val call = active ?: return
        if (call.state == CallState.Connected) {
            return
        }
        val at = now()
        active = call.copy(state = CallState.Connected, startedAt = call.startedAt ?: at)
        _state.update { it.copy(call = active) }
        val beganAt = setupStart
        if (beganAt != null) {
            setupStart = null
            scope.launch {
                try {
                    client.calls.reportStats(call.callId, setupMs = at - beganAt)
                } catch (_: Exception) {
                    // CALL_STATS is Droppable: a lost report costs nothing.
                }
            }
        }
    }

    /**
     * Ends the tracked call locally with a reason, keeping `startedAt` for the duration line.
     *
     * A `Network` reason also reaches the server as a best-effort `CALL_END` (once per call),
     * the peer otherwise waits out its whole reconnect window for a call this side already gave
     * up on.
     */
    private fun finishCall(reason: CallEndReason?) {
        val call = active ?: return
        if (call.state == CallState.Ended) {
            return
        }
        if (reason == CallEndReason.Network && !endSent) {
            endSent = true
            scope.launch {
                try {
                    client.calls.end(call.callId, CallEndReason.Network)
                } catch (_: Exception) {
                    // Same race as every best-effort end; the server's sweep bounds the loss.
                }
            }
        }
        teardownMedia()
        forgetCallKey(call.callId)
        active = call.copy(state = CallState.Ended, endReason = reason ?: call.endReason)
        _state.update { it.copy(call = active, endedAt = now()) }
    }

    /**
     * Arms the local mirror of the invite's expiry: when it fires with the call still ringing,
     * the call ends here as [CallEndReason.NoAnswer] and a cancel tells the server -- a callee
     * whose devices are offline is exactly who never answers, and the caller's screen must not
     * ring a call the invite no longer backs.
     */
    private fun armRingTimeout(callId: Id, expiresAt: Long) {
        ringTimer?.cancel()
        ringTimer = scope.launch {
            delay(ringTimeoutMs(expiresAt, now()))
            val call = active
            if (call == null || call.callId != callId || call.state != CallState.Ringing) {
                // Answered, canceled, or already ended since the timer was armed: the mirror has
                // no job left, and firing now would end a call that moved on without it.
                return@launch
            }
            finishCall(CallEndReason.NoAnswer)
            scope.launch {
                try {
                    client.calls.cancel(callId)
                } catch (_: Exception) {
                    // The server sweeps its own expiry; this cancel is the prompt exit.
                }
            }
        }
    }

    /** Disarms the caller's local expiry mirror; safe to call when nothing is armed. */
    private fun clearRingTimeout() {
        ringTimer?.cancel()
        ringTimer = null
    }

    /** Stops every resource a call held: timers, candidates, the connection, the microphone. */
    private fun teardownMedia() {
        clearRingTimeout()
        iceTimer?.cancel()
        iceTimer = null
        reconnectTimer?.cancel()
        reconnectTimer = null
        synchronized(iceLock) {
            iceBatch.clear()
            heldIce.clear()
        }
        remoteDescriptionSet = false
        peerDevice = null
        setupStart = null
        endSent = false

        val pc = peer
        peer = null
        pc?.close()

        audioTrack?.dispose()
        audioTrack = null
        audioSource?.dispose()
        audioSource = null
        muted = false
        _state.update { it.copy(muted = false) }
    }

    // --- the call key: adopt, forget, wait ---

    /**
     * Adopts a call's sealing key -- minted here for a call we place, or arrived through the E2EE
     * message layer for a call we are rung for -- and wakes any accept waiting on it.
     */
    private fun adoptCallKey(callId: Id, key: ByteArray) {
        synchronized(keyLock) {
            callKeys[callId] = key
            callKeyWaiters.remove(callId)?.complete(key)
        }
    }

    /**
     * Waits for a call's key to arrive, resolving with it -- or `null` once the timeout passed
     * without one. A wait that runs its full course means the frames crossed badly; the accept
     * path treats `null` as "cannot answer", exactly like any other unopenable call.
     */
    private suspend fun waitForCallKey(callId: Id, timeoutMs: Long): ByteArray? {
        val present = synchronized(keyLock) { callKeys[callId] }
        if (present != null) {
            return present
        }
        val waiter = synchronized(keyLock) {
            callKeyWaiters.getOrPut(callId) { CompletableDeferred() }
        }
        // The final re-read covers the one race: the key arriving between the check and the arm
        // completes and *removes* the deferred we then re-installed, and a waiter installed after
        // its completion would otherwise wait out the clock for a key that is already here.
        return withTimeoutOrNull(timeoutMs) { waiter.await() }
            ?: synchronized(keyLock) { callKeys[callId] }
    }

    /** Forgets a call's key. Called when the call ends -- the key's only job was that call. */
    private fun forgetCallKey(callId: Id) {
        synchronized(keyLock) {
            callKeys.remove(callId)
            callKeyWaiters.remove(callId)
        }
    }

    // --- ICE, both directions ---

    /** Sends the gathered candidate batch if it can be addressed and sealed; else queued. */
    private fun flushIce() {
        val call = active ?: return
        val target = peerDevice ?: return
        val key = synchronized(keyLock) { callKeys[call.callId] } ?: return
        val batch = synchronized(iceLock) {
            if (iceBatch.isEmpty()) {
                return
            }
            val taken = ArrayList(iceBatch)
            iceBatch.clear()
            taken
        }
        scope.launch {
            try {
                client.calls.sendIce(
                    call.callId,
                    target,
                    sealCallSignal(encodeIceBatch(batch), key, call.callId),
                )
            } catch (_: Exception) {
                // A lost batch is recovered by the next one; never fatal.
            }
        }
    }

    /** Applies the candidates the peer sent before this side's remote description existed. */
    private fun drainHeldIce() {
        val pc = peer ?: return
        if (!remoteDescriptionSet) {
            return
        }
        val held = synchronized(iceLock) {
            if (heldIce.isEmpty()) {
                return
            }
            val taken = ArrayList(heldIce)
            heldIce.clear()
            taken
        }
        for (candidate in held) {
            pc.addIceCandidate(candidate)
        }
    }

    /** Batches one gathered candidate, lingering so a trickle leaves as few frames as it can. */
    private fun handleIceCandidate(candidate: IceCandidate?) {
        if (candidate == null) {
            // Gathering finished: whatever is batched is all there will be.
            flushIce()
            return
        }
        synchronized(iceLock) {
            iceBatch.add(
                IceCandidateJson(
                    candidate = candidate.sdp,
                    sdpMid = candidate.sdpMid,
                    sdpMLineIndex = candidate.sdpMLineIndex,
                ),
            )
            if (iceTimer == null) {
                iceTimer = scope.launch {
                    delay(iceLingerMs)
                    synchronized(iceLock) { iceTimer = null }
                    flushIce()
                }
            }
        }
    }

    // --- the peer connection ---

    /**
     * The ICE servers for one call's peer connection: the configured TURN relays, then the public
     * STUN fallback. A TURN fetch that fails or returns nothing still yields the fallback -- a
     * call that must relay will fail to connect either way, but a call that only needed STUN must
     * not be refused because the relay list was unreachable.
     */
    private suspend fun iceServersForCall(callId: Id): List<PeerConnection.IceServer> {
        val servers = ArrayList<PeerConnection.IceServer>()
        try {
            for (relay in client.calls.turnServers(callId)) {
                val builder = PeerConnection.IceServer.builder(relay.url)
                if (relay.username.isNotEmpty()) {
                    builder.setUsername(relay.username)
                }
                if (relay.credential.isNotEmpty()) {
                    builder.setPassword(relay.credential)
                }
                servers.add(builder.createIceServer())
            }
        } catch (_: Exception) {
            // Direct connections still work with the STUN fallback below.
        }
        servers.add(STUN_FALLBACK)
        return servers
    }

    /** The public STUN fallback every peer connection carries. */
    private val STUN_FALLBACK: PeerConnection.IceServer =
        PeerConnection.IceServer.builder("stun:stun.l.google.com:19302").createIceServer()

    /**
     * Builds this call's peer connection over the given ICE servers. UNIFIED_PLAN, because the
     * legacy plan is on its way out of libwebrtc and this build has nothing riding on it.
     */
    private fun createPeer(iceServers: List<PeerConnection.IceServer>): PeerConnection {
        val config = PeerConnection.RTCConfiguration(iceServers).apply {
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
        }
        val pc = factory.createPeerConnection(config, PeerObserver())
            ?: throw IOException("no peer connection")
        peer = pc
        return pc
    }

    /**
     * The peer connection's callbacks, arriving on WebRTC's own signaling thread. Everything they
     * do is thread-safe by the class doc's rules, and nothing they receive is ever logged.
     *
     * `org.webrtc.PeerConnection.Observer` is a Java interface whose methods are mostly *abstract*
     * in this artifact (verified against stream-webrtc-android 1.3.10: only the newer hooks --
     * [onConnectionChange] among them -- ship default no-op bodies), so a Kotlin implementation
     * must spell out every method this build has no use for. They are no-ops below, in the order
     * the interface declares them, so the next reader can diff against it.
     */
    private inner class PeerObserver : PeerConnection.Observer {
        override fun onSignalingChange(newState: PeerConnection.SignalingState) = Unit

        override fun onIceConnectionChange(newState: PeerConnection.IceConnectionState) = Unit

        override fun onIceConnectionReceivingChange(receiving: Boolean) = Unit

        override fun onIceGatheringChange(newState: PeerConnection.IceGatheringState) {
            if (newState == PeerConnection.IceGatheringState.COMPLETE) {
                // The browser API this manager is ported from signals end-of-gathering as a null
                // candidate; org.webrtc signals it here instead. Whatever is batched is all there
                // will be, so it leaves now rather than waiting out the linger.
                flushIce()
            }
        }

        override fun onIceCandidate(candidate: IceCandidate?) {
            handleIceCandidate(candidate)
        }

        override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>?) = Unit

        override fun onAddStream(stream: MediaStream?) = Unit

        override fun onRemoveStream(stream: MediaStream?) = Unit

        override fun onDataChannel(channel: DataChannel?) = Unit

        override fun onRenegotiationNeeded() = Unit

        override fun onConnectionChange(newState: PeerConnection.PeerConnectionState) {
            val call = active ?: return
            if (call.state == CallState.Ended) {
                return
            }
            when (newState) {
                PeerConnection.PeerConnectionState.CONNECTED -> {
                    reconnectTimer?.cancel()
                    reconnectTimer = null
                    markConnected()
                }

                PeerConnection.PeerConnectionState.DISCONNECTED -> {
                    // A blip is not an end. Show Reconnecting and open the window; media back
                    // cancels it, the deadline ends the call as a network failure.
                    if (call.state != CallState.Reconnecting) {
                        active = call.copy(state = CallState.Reconnecting)
                        _state.update { it.copy(call = active) }
                    }
                    if (reconnectTimer == null) {
                        reconnectTimer = scope.launch {
                            delay(reconnectWindowMs)
                            reconnectTimer = null
                            finishCall(CallEndReason.Network)
                        }
                    }
                }

                PeerConnection.PeerConnectionState.FAILED -> finishCall(CallEndReason.Network)

                else -> Unit
            }
        }
    }

    // --- the streams' handlers ---

    /**
     * A new invite: ring us, answer Busy for a different call if this device is occupied, and
     * ignore the redeliveries at-least-once delivery guarantees will bring. The decision is
     * [incomingInviteDisposition]'s: the redelivery of the ring already showing (or of the call
     * already answered or over) must be ignored -- declining it would hang up the very call the
     * user is being rung for.
     */
    private fun handleIncoming(event: CallInviteEvent) {
        val disposition = incomingInviteDisposition(
            event,
            incoming?.callId,
            active?.callId,
            callInProgress(),
            now(),
        )
        when (disposition) {
            IncomingInviteDisposition.Ring -> setIncoming(event)
            IncomingInviteDisposition.DeclineBusy -> scope.launch {
                try {
                    client.calls.decline(event.callId, CallDeclineReason.Busy)
                } catch (_: Exception) {
                    // Occupied is stated once; a lost decline is the ring expiring.
                }
            }

            IncomingInviteDisposition.Ignore -> Unit
        }
    }

    /**
     * The server's authoritative state transitions: for the tracked call as before, plus the two
     * events that can name a call this device never tracked -- the ring's own end and a sibling's
     * answer.
     */
    private fun handleStateEvent(event: CallStateEvent) {
        val state = CallState.fromWire(event.state)
        if (state == null) {
            // A state a newer server added: not ours to guess at, and not ours to end a live call
            // over.
            return
        }
        val ringingCallId = incoming?.callId
        if (endsRingingCall(event, ringingCallId)) {
            // The call this ring belongs to ended before anyone answered here -- the caller
            // canceled, or the invite expired. Retire the ring and state the fact as a note; no
            // tracked call is created, this device was never in the call.
            forgetCallKey(event.callId)
            setIncoming(null)
            _state.update { it.copy(callError = MISSED_CALL_MESSAGE) }
            return
        }
        if (answersRingingCall(event, ringingCallId)) {
            // Another device on this account answered. The server rings every device and publishes
            // the answer to both parties, so this one hears the call move on without it -- retire
            // the ring and say where the call went, because "missed" would be a lie about a call
            // that connected. The key is still forgotten: a sibling answered *this* call, so the
            // only device that may keep using the key is the answering one, and this one is not it.
            forgetCallKey(event.callId)
            setIncoming(null)
            _state.update { it.copy(callError = ANSWERED_ELSEWHERE_MESSAGE) }
            return
        }
        val call = active ?: return
        if (event.callId != call.callId || call.state == CallState.Ended) {
            return
        }
        if (state == CallState.Ended) {
            // The reason is optional on the wire; its absence is the server's plain "ended", which
            // [endReasonLabel] states as exactly that.
            finishCall(event.reason?.let(CallEndReason::fromWire))
            return
        }
        if (state != CallState.Ringing) {
            // The call moved for real -- answered, connecting, connected -- so the invite's expiry
            // no longer ends anything and the local mirror retires.
            clearRingTimeout()
        }
        if (state == CallState.Connected) {
            markConnected()
            return
        }
        if (state != call.state) {
            active = call.copy(state = state)
            _state.update { it.copy(call = active) }
        }
    }

    /** An SDP relay: for a caller this is the answer naming the device everything now addresses. */
    private fun handleSdp(relay: CallSdp) {
        val call = active
        val pc = peer
        if (call == null || relay.callId != call.callId || pc == null) {
            return
        }
        if (!call.isCaller || remoteDescriptionSet) {
            // A renegotiated offer mid-call is a future flow, not this build's.
            return
        }
        scope.launch {
            val callKey = synchronized(keyLock) { callKeys[call.callId] }
            if (callKey == null) {
                // No key, no answer: the key message the caller sent before the invite was lost,
                // and an answer we cannot open is worse than a call that fails loudly.
                finishCall(CallEndReason.Failed)
                return@launch
            }
            val answer = try {
                decodeSdpDescription(openCallSignal(relay.sealedSdp, callKey, relay.callId))
            } catch (_: Exception) {
                finishCall(CallEndReason.Failed)
                return@launch
            }
            try {
                pc.setRemoteDescription(answer)
                remoteDescriptionSet = true
                peerDevice = relay.fromDevice
                flushIce()
                drainHeldIce()
                if (call.state == CallState.Ringing) {
                    // The answer landed, so the invite's expiry has no call left to end: the
                    // ring's local mirror retires with the state it was arming against.
                    clearRingTimeout()
                    active = call.copy(state = CallState.Connecting)
                    _state.update { it.copy(call = active) }
                }
            } catch (_: Exception) {
                finishCall(CallEndReason.Failed)
            }
        }
    }

    /** A batch of the peer's candidates, applied now or held for the remote description. */
    private fun handleIceRelay(relay: CallIce) {
        val call = active
        val pc = peer
        if (call == null || relay.callId != call.callId || pc == null) {
            return
        }
        val callKey = synchronized(keyLock) { callKeys[call.callId] } ?: return
        val candidates = try {
            decodeIceBatch(openCallSignal(relay.sealedCandidates, callKey, relay.callId))
        } catch (_: Exception) {
            // One malformed batch is dropped, not fatal: the next batch or the connection's own
            // gathering carries the call.
            return
        }
        if (remoteDescriptionSet) {
            for (candidate in candidates) {
                pc.addIceCandidate(candidate)
            }
        } else {
            synchronized(iceLock) { heldIce.addAll(candidates) }
        }
    }

    // --- small WebRTC bridges ---

    /** Applies one relayed candidate; a candidate the connection no longer wants is normal near
     * the end of gathering, and the failure is swallowed without being logged. */
    private fun PeerConnection.addIceCandidate(candidate: IceCandidateJson) {
        addIceCandidate(
            IceCandidate(candidate.sdpMid, candidate.sdpMLineIndex, candidate.candidate ?: ""),
            object : AddIceObserver {
                override fun onAddSuccess() = Unit

                override fun onAddFailure(error: String?) = Unit
            },
        )
    }

    /** The offer this side's audio-only call produces. */
    private suspend fun PeerConnection.offer(): SessionDescription =
        suspendCancellableCoroutine { cont ->
        createOffer(
            object : SdpObserver {
                override fun onCreateSuccess(description: SessionDescription) {
                    cont.resume(description)
                }

                override fun onSetSuccess() = Unit

                override fun onCreateFailure(error: String?) {
                    // The error string can quote SDP; it is swallowed, never logged.
                    cont.resumeWithException(IOException("createOffer failed"))
                }

                override fun onSetFailure(error: String?) = Unit
            },
            MediaConstraints(),
        )
    }

    /** The answer this side produces for the peer's offer. */
    private suspend fun PeerConnection.answer(): SessionDescription =
        suspendCancellableCoroutine { cont ->
        createAnswer(
            object : SdpObserver {
                override fun onCreateSuccess(description: SessionDescription) {
                    cont.resume(description)
                }

                override fun onSetSuccess() = Unit

                override fun onCreateFailure(error: String?) {
                    cont.resumeWithException(IOException("createAnswer failed"))
                }

                override fun onSetFailure(error: String?) = Unit
            },
            MediaConstraints(),
        )
    }

    private suspend fun PeerConnection.setLocalDescription(description: SessionDescription) {
        suspendCancellableCoroutine { cont ->
            setLocalDescription(
                object : SdpObserver {
                    override fun onCreateSuccess(description: SessionDescription) = Unit

                    override fun onSetSuccess() {
                        cont.resume(Unit)
                    }

                    override fun onCreateFailure(error: String?) = Unit

                    override fun onSetFailure(error: String?) {
                        cont.resumeWithException(IOException("setLocalDescription failed"))
                    }
                },
                description,
            )
        }
    }

    private suspend fun PeerConnection.setRemoteDescription(description: SdpDescription) {
        val session = SessionDescription(
            SessionDescription.Type.fromCanonicalForm(description.type),
            description.sdp,
        )
        suspendCancellableCoroutine { cont ->
            setRemoteDescription(
                object : SdpObserver {
                    override fun onCreateSuccess(description: SessionDescription) = Unit

                    override fun onSetSuccess() {
                        cont.resume(Unit)
                    }

                    override fun onCreateFailure(error: String?) = Unit

                    override fun onSetFailure(error: String?) {
                        cont.resumeWithException(IOException("setRemoteDescription failed"))
                    }
                },
                session,
            )
        }
    }

    private fun now(): Long = System.currentTimeMillis()
}

/**
 * The one placement failure a user can fix themselves, for the permission-refused case. Shared
 * with the view model, which learns the refusal from the permission launcher rather than from an
 * exception.
 */
internal const val MICROPHONE_UNAVAILABLE: String =
    "Microphone unavailable. Check permissions and try again."

/** The tracked call, as the overlay renders it. */
data class ActiveCall(
    val callId: Id,
    val conversationId: Id,
    val callerId: Id,
    val calleeId: Id,
    val mediaKind: CallMediaKind,
    val state: CallState,
    val endReason: CallEndReason? = null,
    /** The wire's own refusal vocabulary, when the invite never rang: a blocked refusal must read
     * differently from a declined one, and the reason enum cannot say which. */
    val inviteStatus: Long? = null,
    val isCaller: Boolean,
    /** When media first connected, for the running duration and the ended screen's total. */
    val startedAt: Long? = null,
)

/** The slice of the call manager the screens read. */
data class CallUiState(
    /** The call this device is in, including one that just ended (until dismissed). */
    val call: ActiveCall? = null,
    /** A ringing inbound call nobody has answered; takes the screen over any tracked call. */
    val incoming: CallInviteEvent? = null,
    /** Whether this side's microphone is muted. */
    val muted: Boolean = false,
    /** When the current (or just-ended) call ended, for the ended screen's duration. */
    val endedAt: Long? = null,
    /** Why a call could not even be placed, when nothing else is showing. */
    val callError: String? = null,
)

/**
 * What the missed-call note says when an inbound ring retires because the call ended before it
 * was answered. Exported so the overlay can label its card with the same fact the manager states.
 */
const val MISSED_CALL_MESSAGE: String = "Missed call"

/**
 * What the note says when an inbound ring retires because a sibling device answered: the call was
 * not missed, it moved -- and a screen that says "missed" for a call being spoken on elsewhere
 * sends its user to the phone that is already in the conversation.
 */
const val ANSWERED_ELSEWHERE_MESSAGE: String = "Answered on another device"

/** The SDP description of an offer, in the sealed payload's own JSON shape. */
private fun offerDescription(offer: SessionDescription): SdpDescription =
    SdpDescription(
        type = SessionDescription.Type.OFFER.canonicalForm(),
        sdp = offer.description,
    )

/** The SDP description of an answer, in the sealed payload's own JSON shape. */
private fun answerDescription(answer: SessionDescription): SdpDescription =
    SdpDescription(
        type = SessionDescription.Type.ANSWER.canonicalForm(),
        sdp = answer.description,
    )
