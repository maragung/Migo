package com.migo.app.call

import android.content.Context
import android.content.Intent
import android.media.projection.MediaProjection
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
import com.migo.core.protocol.CallRating
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
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.webrtc.AddIceObserver
import org.webrtc.AudioSource
import org.webrtc.AudioTrack
import org.webrtc.Camera2Enumerator
import org.webrtc.CameraVideoCapturer
import org.webrtc.DataChannel
import org.webrtc.DefaultVideoDecoderFactory
import org.webrtc.DefaultVideoEncoderFactory
import org.webrtc.EglBase
import org.webrtc.IceCandidate
import org.webrtc.MediaConstraints
import org.webrtc.MediaStream
import org.webrtc.PeerConnection
import org.webrtc.PeerConnectionFactory
import org.webrtc.RtpSender
import org.webrtc.RtpTransceiver
import org.webrtc.ScreenCapturerAndroid
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import org.webrtc.SurfaceTextureHelper
import org.webrtc.VideoSource
import org.webrtc.VideoTrack
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
 * This build carries **voice and video** for 1-on-1 calls, mirroring the web overlay: the call
 * button offers both kinds, a video call opens the front camera as a second track beside the
 * microphone's, and the screen shows the peer full-bleed with this side's camera as a small
 * self-view. A *video* invite answered on a device with no camera (or a camera that cannot open)
 * is still answered -- audio carries the conversation and the caller simply sees no video, the
 * answer's fewer m-lines rejecting the extra ones, which is ordinary WebRTC. Placement does not
 * fall back the same way: a person who pressed the video button asked for video, and silently
 * handing them a voice call would be the interface lying about what it did.
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
    private val context: Context,
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
    private fun callInProgress(): Boolean {
        // A local read, so the null check and the state check see the same call: `active` is a
        // volatile another thread may retire between the two, and a smart cast cannot bridge it.
        val call = active ?: return false
        return call.state != CallState.Ended
    }

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

    /** The timer that samples this call's own link for as long as it is connected. */
    @Volatile private var qualityTimer: Job? = null
    /** The last reading of that link; the stretch a report measures is between two of these. */
    @Volatile private var linkCounters: LinkCounters? = null
    /** The rung this side has classified its own link as, and when it moved there. */
    @Volatile private var linkQuality = LinkQuality.Full
    @Volatile private var linkQualityChangedAt = 0L
    /** The last measurement this call produced, which is the one a call that is ending reports. */
    @Volatile private var lastMeasurement: QualityAdvance? = null

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

        /** What the camera is asked for on a video call: the web build's own stage size, and a
         * resolution low enough that the encoder keeps up on the phones that need it most. */
        const val VIDEO_WIDTH = 640
        const val VIDEO_HEIGHT = 480
        const val VIDEO_FPS = 30

        /**
         * What a shared screen is asked for: a wide stage rather than the camera's squarer one,
         * because a desk screen is wide and a phone screen is tall, and a rate below the camera's
         * because a screen changes in bursts -- a page of text nobody is touching costs nothing to
         * hold, and the encoder's budget is better spent on the moment it moves than on sending
         * the same still frame thirty times a second.
         */
        const val SHARE_WIDTH = 1280
        const val SHARE_HEIGHT = 720
        const val SHARE_FPS = 15

        /**
         * How long a share waits for the foreground service that has to be running before the
         * platform will hand over a projection. Long enough for the platform to start a service in
         * this same process, short enough that a service which never comes up fails the share
         * rather than hanging it.
         */
        const val SHARE_SERVICE_WAIT_MS = 3000L

        /**
         * How long one sample of the call's own link waits for the connection's stats to come back.
         * The platform answers a stats request off the signalling thread, so the wait is normally
         * short; the ceiling is here for the case where it never answers at all, because a
         * connection closed with a request outstanding leaves the request unanswered forever.
         */
        const val STATS_TIMEOUT_MS = 2000L
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
    @Volatile private var videoCapturer: CameraVideoCapturer? = null
    @Volatile private var videoHelper: SurfaceTextureHelper? = null
    @Volatile private var videoSource: VideoSource? = null
    @Volatile private var videoTrack: VideoTrack? = null
    /** The sender carrying [videoTrack], which is what the ladder's caps are written on. */
    @Volatile private var videoSender: RtpSender? = null
    /**
     * The sender carrying the microphone, which low bandwidth mode caps directly.
     *
     * Audio is never a rung of the ladder -- the bottom of the ladder is "video off, audio
     * alive" -- so on a voice call this is the only place the mode can mean anything.
     */
    @Volatile private var audioSender: RtpSender? = null
    /**
     * The rung the user pinned this call to, or null for automatic.
     *
     * Per call and not a standing preference: a ceiling pinned for one call would silently cap
     * the next one, which the user never asked for. Cleared with the media it applied to.
     */
    @Volatile private var qualityCeiling: LinkQuality? = null
    /** Whether low bandwidth mode is on for this call. Per call, for the same reason. */
    @Volatile private var lowBandwidthOn = false

    /**
     * The capturer sending this device's screen, while one is. A field of its own rather than a
     * replacement for [videoCapturer], because the camera's capturer is *kept* while the screen is
     * on the track: it is stopped, not disposed, so that ending the share restarts something that
     * still exists instead of opening a camera another app may have taken in the meantime.
     */
    @Volatile private var screenCapturer: ScreenCapturerAndroid? = null

    /**
     * Whether this side wants its camera open. Its own flag rather than a read of the capturer,
     * because the two disagree for as long as a failing re-open takes: the wish is what the button
     * shows and what the screen has to agree with, and a capturer that could not be restarted is a
     * wish corrected back to off rather than a button that keeps claiming a camera which is not
     * there. Reset with the rest of the per-call state.
     */
    @Volatile private var cameraWanted: Boolean = true

    /**
     * The shared OpenGL context every video surface in this app renders through. One per manager
     * (and so per session), created lazily on first use and released with the factory: the native
     * resources an `EglBase` pins must not leak per call, and the surfaces the UI attaches
     * (local preview, remote renderer) all share the one context so the frames never copy through
     * the CPU.
     */
    @Volatile private var eglBase: EglBase? = null

    /**
     * The shared GL context's handle, for the surfaces the call screens attach. The context
     * itself stays the manager's -- created with the video factories, released with the factory
     * -- so a screen can never outlive the resources it renders through.
     */
    val eglContext: EglBase.Context?
        get() = eglBase?.eglBaseContext

    /** The session's local camera track, for the call screen's self-view. Null on a voice call. */
    val localVideo: VideoTrack?
        get() = videoTrack

    /**
     * The peer's camera track once it arrives, for the call screen's main view. The [StateFlow]
     * carries it because a track that lands mid-call must reach a screen that is already showing.
     */
    private val _remoteVideo = MutableStateFlow<VideoTrack?>(null)

    /** See [_remoteVideo]. */
    val remoteVideo: StateFlow<VideoTrack?> = _remoteVideo.asStateFlow()

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
        // The video encoder and decoder factories need the shared GL context up front, so it is
        // minted here with the factory rather than lazily at the first video call -- one context
        // per session, shared by the factories and every surface the call screens attach.
        eglBase = EglBase.create()
        factory = PeerConnectionFactory.builder()
            .setAudioDeviceModule(audioModule)
            .setVideoEncoderFactory(
                DefaultVideoEncoderFactory(eglBase!!.eglBaseContext, true, true),
            )
            .setVideoDecoderFactory(DefaultVideoDecoderFactory(eglBase!!.eglBaseContext))
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
        // The native audio module, the video factories' GL context, and the factory itself are the
        // session's own; a later session builds its own. Disposing them frees the native threads
        // and the GL resources a leaked factory would pin forever.
        audioModule.release()
        eglBase?.release()
        eglBase = null
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
                val camera = if (mediaKind == CallMediaKind.Video) openFrontCamera() else null
                attachMedia(pc, camera)

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
                // A video invite is answered with whatever camera this device can open: none, or
                // one that fails to start, and the answer still goes out with the microphone's
                // track alone -- the caller keeps the conversation and simply sees no video.
                // Falling back here (and not on placement) is the web build's own rule: a person
                // who tapped the accept button asked for the call, not the camera.
                val camera = if (mediaKind == CallMediaKind.Video) {
                    try {
                        openFrontCamera()
                    } catch (_: Exception) {
                        null
                    }
                } else {
                    null
                }
                attachMedia(pc, camera)

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

    /**
     * Whether this device has a camera other than the front one.
     *
     * A device fact, asked once when the manager is built rather than per call, because the answer
     * cannot change while the app is running and a control that comes and goes between calls is a
     * control nobody learns. The front camera is the one a call opens on, so the question the call
     * screen needs answered is whether there is anywhere to switch to.
     */
    val canSwitchCamera: Boolean = runCatching {
        val enumerator = Camera2Enumerator(context)
        enumerator.deviceNames.any { !enumerator.isFrontFacing(it) }
    }.getOrDefault(false)

    /**
     * Flips the live call between the front camera and the back one.
     *
     * The switch happens inside the capturer, on the track that is already carrying frames, so
     * nothing about the peer connection changes: no renegotiation, no new track, no frame the
     * other side can see a seam in. That is the reason this is a control rather than a second
     * capture path -- replacing a track mid-call would cost a renegotiation and a visible gap for
     * a change of direction.
     *
     * A no-op when no camera is open, which is the honest answer for a voice call: there is
     * nothing to point.
     */
    fun switchCamera() {
        videoCapturer?.switchCamera(null)
    }

    /**
     * Puts this device's screen on the call's video track, in the camera's place.
     *
     * A share here is the m-line the camera was already using and not a second one: the screen
     * capturer is attached to the very source the video track is built on, so the peer sees the
     * screen the moment the frames change and nothing is renegotiated -- which is what the web
     * build does when it swaps the track on the video sender, and the only shape available here,
     * since this manager has no renegotiation path to add a track with. The camera's capturer is
     * stopped rather than disposed for that same reason turned around: ending the share has to put
     * the picture back, and a capturer that still exists is one that can simply be started again.
     *
     * Refused where there is no track to put a screen on -- a voice call, and a video call whose
     * camera never opened -- because half a share is a share nobody can see.
     */
    fun startScreenShare(projection: Intent) {
        val call = active ?: return
        if (call.mediaKind != CallMediaKind.Video || screenCapturer != null) {
            return
        }
        val source = videoSource ?: return
        val helper = videoHelper ?: return
        val service = Intent(context, ScreenShareService::class.java)
        scope.launch {
            val started = withContext(Dispatchers.IO) {
                // The service goes up first, on its own thread: the projection is claimed on the
                // line after it and the platform refuses the claim outright where no service of
                // the type is running yet, so the order is a requirement and not a preference.
                runCatching { context.startForegroundService(service) }
                if (!ScreenShareService.awaitUp(SHARE_SERVICE_WAIT_MS)) {
                    runCatching { context.stopService(service) }
                    return@withContext false
                }
                val capturer = ScreenCapturerAndroid(
                    projection,
                    object : MediaProjection.Callback() {
                        // The user can end a share from the platform's own controls rather than
                        // from this app, and this is how that arrives: the projection stops and
                        // the call is told, so the control follows the screen that is really being
                        // sent instead of claiming one that is not.
                        override fun onStop() {
                            stopScreenShare()
                        }
                    },
                )
                val attached = runCatching {
                    videoCapturer?.stopCapture()
                    capturer.initialize(helper, context, source.capturerObserver)
                    capturer.startCapture(SHARE_WIDTH, SHARE_HEIGHT, SHARE_FPS)
                }.isSuccess
                if (!attached) {
                    // A share that could not be built leaves nothing behind: the capturer is
                    // disposed so the projection is released, and the service goes with it, since
                    // a notification about a screen nobody is sending is worse than none.
                    runCatching { capturer.dispose() }
                    runCatching { context.stopService(service) }
                    return@withContext false
                }
                screenCapturer = capturer
                // The call can end while a share is being built, and this is the last moment the
                // two can be reconciled: the source is the call's own and a call that is gone has
                // taken its source with it, so a capturer stored onto that source would hold a
                // projection nothing would ever release.
                if (videoSource !== source) {
                    screenCapturer = null
                    runCatching { capturer.stopCapture() }
                    runCatching { capturer.dispose() }
                    runCatching { context.stopService(service) }
                    return@withContext false
                }
                true
            }
            if (started) {
                _state.update { it.copy(screenSharing = true) }
                return@launch
            }
            // The camera goes back on the track it left, and only where it was on it: somebody who
            // turned their camera off before sharing asked for no picture of themselves, and a
            // share that failed is no reason to hand them one.
            if (cameraWanted) {
                val restored = runCatching {
                    videoCapturer?.startCapture(VIDEO_WIDTH, VIDEO_HEIGHT, VIDEO_FPS)
                }.isSuccess
                if (!restored) {
                    cameraWanted = false
                }
            }
            _state.update {
                it.copy(cameraOn = if (videoCapturer == null) null else cameraWanted)
            }
        }
    }

    /**
     * Takes the screen off the call's video track and gives that track back to the camera.
     *
     * The field is cleared before anything is stopped, and that order is the whole of the
     * reentrancy: disposing the capturer stops the projection, a projection that stops calls back
     * into the callback above, and a release that read the field afterwards would find a capturer
     * on its way out and run this a second time. A camera that was on before the share is started
     * again here, which is the same restart the camera button does and can fail the same way -- a
     * failure leaves the wish off rather than a button claiming a picture that is not being sent.
     */
    fun stopScreenShare() {
        val capturer = screenCapturer ?: return
        screenCapturer = null
        val service = Intent(context, ScreenShareService::class.java)
        scope.launch {
            withContext(Dispatchers.IO) {
                runCatching { capturer.stopCapture() }
                runCatching { capturer.dispose() }
                runCatching { context.stopService(service) }
                if (cameraWanted) {
                    val restored = runCatching {
                        videoCapturer?.startCapture(VIDEO_WIDTH, VIDEO_HEIGHT, VIDEO_FPS)
                    }.isSuccess
                    if (!restored) {
                        cameraWanted = false
                    }
                }
            }
            _state.update {
                it.copy(
                    screenSharing = false,
                    cameraOn = if (videoCapturer == null) null else cameraWanted,
                )
            }
        }
    }

    /**
     * Where this call is played, as the phone's own routing layer.
     *
     * The layer itself is [CallAudioRoute]'s rather than this class's, because a group call needs
     * the same one and two copies of one question drift apart. What stays here is this call's part
     * in it: the watch starts where a call connects and stops with its media, because a routing
     * menu only exists while there is a call to move.
     */
    private val audioRoute = CallAudioRoute(context)

    /** The routes this phone offers this call right now, in the order the platform lists them. */
    val outputs: StateFlow<List<AudioOutput>> = audioRoute.outputs

    /** The route this call is playing through, or null while the phone is choosing for itself. */
    val chosenOutputId: StateFlow<Int?> = audioRoute.chosenOutputId

    /**
     * Plays the call through one of the routes the phone listed; false means the phone refused.
     *
     * The refusal is returned rather than swallowed so the caller can re-read the list instead of
     * leaving the menu showing a choice the call is not on.
     */
    fun chooseOutput(deviceId: Int): Boolean = audioRoute.choose(deviceId)

    /** Mutes or unmutes this side's microphone. */
    fun toggleMute() {
        muted = !muted
        audioTrack?.setEnabled(!muted)
        _state.update { it.copy(muted = muted) }
    }

    /**
     * Turns this side's camera off or back on.
     *
     * Off means the capture is stopped, which is what hands the camera back to the platform: a
     * capturer that is still running is a camera the system considers in use, so its indicator goes
     * on burning beside a lens whose frames are being dropped on the floor. The sender and the
     * m-line are deliberately left alone -- with no capture there are no frames to encode, so
     * nothing is on the wire either way, and keeping the negotiated video line is what makes turning
     * the camera back on a restart rather than a renegotiation the other side can see.
     *
     * On is a restart of the same capturer and it can fail, because another application may have
     * taken the camera while it was released; a failure puts the wish back to off, since a button
     * that keeps claiming a picture the call is not sending is the one answer worse than no picture.
     *
     * A no-op on a voice call, and on a video call whose camera never opened: there is no capture to
     * stop and nothing that could be opened.
     */
    fun toggleCamera() {
        val call = active ?: return
        if (call.mediaKind != CallMediaKind.Video || videoCapturer == null) {
            return
        }
        if (cameraWanted) {
            cameraWanted = false
            runCatching { videoCapturer?.stopCapture() }
            _state.update { it.copy(cameraOn = false) }
            return
        }
        cameraWanted = true
        val restarted = runCatching {
            videoCapturer?.startCapture(VIDEO_WIDTH, VIDEO_HEIGHT, VIDEO_FPS)
        }.isSuccess
        if (!restarted) {
            cameraWanted = false
        }
        _state.update { it.copy(cameraOn = cameraWanted) }
    }

    /**
     * Sends the user's own verdict on the call that just ended.
     *
     * It rides the same `CALL_STATS` frame the setup time used, which is the one report already
     * leaving the device around a call, and the same opcode is Droppable, so a lost verdict costs
     * nothing beyond the question the user already answered. The flag is set before the send rather
     * than after, because the send is fire-and-forget in a scope that outlives the screen: a second
     * tap while the first is in flight would otherwise put the same verdict on the wire twice, and
     * the aggregate would count one user's opinion as two.
     *
     * Ratings are asked only of a call that connected and are accepted only while that call is
     * still the tracked one -- once the screen is dismissed there is no call to attach a verdict to.
     * `issues` is a bitmask and zero means none: absent and zero would otherwise be two spellings
     * of "nothing went wrong" on the wire.
     */
    fun rateCall(rating: CallRating, issues: ULong = 0uL) {
        val call = active ?: return
        if (!call.canRate) {
            return
        }
        active = call.copy(ratingSent = true)
        _state.update { it.copy(call = active) }
        scope.launch {
            try {
                client.calls.reportStats(
                    call.callId,
                    rating = rating,
                    issues = if (issues == 0uL) null else issues,
                )
            } catch (_: Exception) {
                // CALL_STATS is Droppable: a lost verdict costs nothing.
            }
        }
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
        // A call that has just connected is a call the phone has just entered communication mode
        // for, and that mode is what makes some routes exist at all -- a Bluetooth headset that
        // shows up as a call route only once the phone knows it is carrying a call. The watch
        // starts here rather than with the manager because the list belongs to the call: a menu
        // only exists while one is up, and a callback registered for a call that has not started
        // would be the platform telling the app about routes nobody can choose.
        audioRoute.start()
        startMeasuring()
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
     * Samples this call's own link for as long as it is connected, and reports what it measures.
     *
     * The sample is this side's reading of its own transport -- loss, round-trip time, jitter, what
     * it is sending, what the receiver dropped -- so it is a number a client can truly report, and
     * the only one it can report about a leg whose far end it does not own. What leaves the device
     * is bounded the way the web build bounds it: a reading goes out when the ladder moves this link
     * to another rung, and one last reading goes out when the call ends. A call on a steady link
     * therefore costs one report rather than one per sample, and CALL_STATS is Droppable, so a lost
     * one costs nothing.
     *
     * The first sample establishes the baseline and reports nothing: a stretch needs two readings to
     * measure, and the counters are cumulative, so loss and drop percentages are only honest as
     * deltas between two of them.
     */
    private fun startMeasuring() {
        qualityTimer?.cancel()
        linkCounters = null
        linkQuality = LinkQuality.Full
        linkQualityChangedAt = now()
        lastMeasurement = null
        // A new call starts with no measured tier and no degradation: the indicator is a
        // measurement, and carrying the last call's rung onto this one would be inventing it.
        _state.update { it.copy(quality = null, degraded = false) }
        qualityTimer = scope.launch {
            while (true) {
                delay(QUALITY_POLL_MS)
                val call = active ?: continue
                if (call.state != CallState.Connected) {
                    continue
                }
                val pc = peer ?: continue
                // A connection closed with a stats request outstanding never answers it, so each
                // sample is given a ceiling: the next one is two seconds away, and a call that ends
                // mid-sample must not leave a coroutine waiting on a connection that is already gone.
                val counters =
                    withTimeoutOrNull(STATS_TIMEOUT_MS) { readLinkCounters(pc) } ?: continue
                val previous = linkCounters
                linkCounters = counters
                val at = now()
                val step = advanceMeasurement(
                    current = linkQuality,
                    previous = previous,
                    counters = counters,
                    changedAtMs = linkQualityChangedAt,
                    nowMs = at,
                ) ?: continue
                lastMeasurement = step
                if (!step.changed) {
                    continue
                }
                linkQuality = step.quality
                linkQualityChangedAt = at
                applyQuality(step.quality, step.stats.sentKbps)
                reportQuality(call.callId, step)
            }
        }
    }

    /**
     * Puts one rung onto this side's own video and onto the screen.
     *
     * What the ladder measured is what this call does about its own link, so the rung becomes the
     * encoder's ceiling, the resolution it scales to, and the frame rate it may send -- and the
     * bottom rung stops the stream *without* detaching the track, which is what makes the rung that
     * climbs back a parameter change rather than a camera to reopen. The camera keeps capturing
     * either way, so the self-view stays alive while the peer receives nothing, and a user who turned
     * their camera off stays a different fact from a link that did.
     *
     * Every rung writes every cap it names, the ones it imposes nothing on included, because a
     * member left out of a sender's parameters is a member the receiver keeps: a climbing call that
     * omitted the cap it was giving back would keep sending the small picture the screen no longer
     * claims. A sender that refuses the change is not retried here -- the next rung that moves writes
     * again -- and a call that cannot be shaped keeps sending what it was sending rather than losing
     * video over a refused parameter.
     */
    private fun applyQuality(measured: LinkQuality, measuredKbps: Long) {
        // What the ladder measured, lowered by whatever the user pinned. A ceiling and never a
        // floor, so the ladder may still descend below either control: no control makes a link carry
        // more than it can, and a screen that claimed otherwise would show a tier the call is not on.
        val rung = cappedQuality(measured, qualityCeiling, lowBandwidthOn)
        val isVideo = active?.mediaKind == CallMediaKind.Video
        val caps = videoCaps(rung, measuredKbps)
        val sender = videoSender
        if (sender != null) {
            val params = sender.parameters
            val encoding = params?.encodings?.firstOrNull()
            if (params != null && encoding != null) {
                encoding.active = caps.active
                encoding.maxBitrateBps = caps.maxBitrateBps
                encoding.scaleResolutionDownBy = caps.scaleResolutionDownBy
                encoding.maxFramerate = caps.maxFramerate
                runCatching { sender.setParameters(params) }
            }
        }
        _state.update { it.copy(quality = rung, degraded = degradedAt(rung, isVideo)) }
    }

    /**
     * Pins this call to a rung, or returns it to automatic with null.
     *
     * A ceiling and not a floor, so the call still descends if its link does; what the pin buys is
     * that it never rises above the chosen rung, which is what a user on a metered or thin link is
     * asking for. The rung is re-applied at once rather than at the next sample, because a control
     * that appears to do nothing for two seconds reads as broken.
     */
    fun setQualityCeiling(ceiling: LinkQuality?) {
        qualityCeiling = ceiling
        _state.update { it.copy(qualityCeiling = ceiling) }
        reapplyQuality()
    }

    /**
     * Turns low bandwidth mode on or off for this call.
     *
     * On a video call the ladder is capped at its lowest rung that still carries video; on a voice
     * call there is no video to give up and the mode caps the audio bitrate instead, because a mode
     * that did nothing on the call the user is actually on would be a lie. Both are written now,
     * since which kind of call this is is already known.
     */
    fun setLowBandwidth(on: Boolean) {
        lowBandwidthOn = on
        _state.update { it.copy(lowBandwidth = on) }
        capAudioSender()
        reapplyQuality()
    }

    /** Re-shapes the senders at the rung the call is on now, without waiting for the next sample. */
    private fun reapplyQuality() {
        applyQuality(linkQuality, lastMeasurement?.stats?.sentKbps ?: 0L)
    }

    /**
     * Writes the mode's audio cap onto the microphone's sender, or lifts it.
     *
     * Written in both directions rather than deleted when the mode goes off: a member left out of a
     * sender's parameters is a member the receiver keeps, so clearing it by omission would leave the
     * call in narrowband for the rest of its life.
     */
    private fun capAudioSender() {
        val sender = audioSender ?: return
        val params = sender.parameters
        val encoding = params?.encodings?.firstOrNull()
        if (params != null && encoding != null) {
            encoding.maxBitrateBps = audioCaps(lowBandwidthOn)
            runCatching { sender.setParameters(params) }
        }
    }

    /** Stops sampling the link; safe to call when nothing is armed. */
    private fun stopMeasuring() {
        qualityTimer?.cancel()
        qualityTimer = null
    }

    /** Sends one measurement as a call-stats report; a dropped report costs nothing. */
    private fun reportQuality(callId: Id, step: QualityAdvance) {
        val report = qualityReport(step.stats, step.counters)
        scope.launch {
            try {
                client.calls.reportStats(
                    callId,
                    rttMs = report.rttMs,
                    packetLoss = report.packetLoss,
                    jitterMs = report.jitterMs,
                    usedTurn = report.usedTurn,
                )
            } catch (_: Exception) {
                // CALL_STATS is Droppable: a lost report costs nothing.
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
        // The last reading of a call that measured anything goes out here rather than only where the
        // rung moved: a call that ran its whole life on a steady link would otherwise report its
        // setup time and nothing else, and the numbers a call ended on are the numbers it is
        // remembered by. The report is queued before the media goes, because the stretch it
        // describes is already measured.
        val measured = lastMeasurement
        if (measured != null) {
            reportQuality(call.callId, measured)
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
        // The call's media is going, so the route it was played through goes with it: this is the
        // one place every ending passes through -- a hang-up, a peer's end, a lost session, a
        // sign-out -- and a routing pin that outlived it would be a phone stuck on a speaker.
        audioRoute.stop()
        // The two quality controls go with the call they capped. A ceiling pinned for one call
        // is not a standing preference -- the next call starts on the best rung its own link can
        // carry and re-measures from there -- and a mode carried over would silently cap a call
        // nobody asked to cap.
        qualityCeiling = null
        lowBandwidthOn = false
        _state.update { it.copy(qualityCeiling = null, lowBandwidth = false) }
        stopMeasuring()
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

        // The senders go with the tracks they were carrying; a shape written onto a released
        // sender is a parameter change against a connection that is already gone.
        audioSender = null
        videoSender = null
        audioTrack?.dispose()
        audioTrack = null
        audioSource?.dispose()
        audioSource = null
        // A screen on the track is released in the same order as the camera and before it, because
        // it draws through the same helper and the same source: the projection goes first, then the
        // capturer that holds it, then the foreground service that exists only to keep the platform
        // willing to grant one -- a share that outlived its call would be a notification telling
        // the user their screen is being sent after it stopped being sent. The field is cleared
        // before any of that runs, since disposing the capturer stops the projection and a stopped
        // projection calls back into the release that would otherwise run this twice.
        val sharing = screenCapturer
        screenCapturer = null
        if (sharing != null) {
            runCatching { sharing.stopCapture() }
            runCatching { sharing.dispose() }
            runCatching { context.stopService(Intent(context, ScreenShareService::class.java)) }
        }
        // The camera stops next -- a capturer stopped after its source is disposed is a native
        // crash on some devices -- then the helper that carried its frames, then the track and
        // the source, then the remote track is dropped from the state so no screen keeps
        // rendering a dead one.
        runCatching { videoCapturer?.stopCapture() }
        videoCapturer?.dispose()
        videoCapturer = null
        videoHelper?.dispose()
        videoHelper = null
        videoTrack?.dispose()
        videoTrack = null
        videoSource?.dispose()
        videoSource = null
        _remoteVideo.value = null
        muted = false
        // A camera that was off when the call ended is a camera this device does not want opened
        // for the next one, so the wish goes back to its starting state with the rest of the call's.
        cameraWanted = true
        _state.update { it.copy(muted = false, cameraOn = null, screenSharing = null) }
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
     * Opens the front camera, the one a call is made on. The back camera is a document scanner,
     * not a face; a phone with no front camera (or no camera at all) is a voice call waiting to
     * happen, so the failure is the caller's to fall back on.
     */
    private fun openFrontCamera(): CameraVideoCapturer {
        val enumerator = Camera2Enumerator(context)
        val front = enumerator.deviceNames.firstOrNull(enumerator::isFrontFacing)
            ?: throw IOException("no front-facing camera")
        return enumerator.createCapturer(front, null)
    }

    /**
     * Builds this call's local media onto the peer connection: the microphone always, the camera
     * when one was opened. The audio is unconditional -- a call without a microphone is not a
     * call -- while `camera` is nullable because both a voice call and a video invite answered
     * without a working camera reach here with nothing to attach, and the answer's fewer m-lines
     * are how WebRTC itself says "audio only".
     */
    private fun attachMedia(pc: PeerConnection, camera: CameraVideoCapturer?) {
        val source = factory.createAudioSource(MediaConstraints())
        val track = factory.createAudioTrack("migo-voice", source)
        audioSender = pc.addTrack(track, listOf("migo"))
        audioSource = source
        audioTrack = track
        if (camera == null) {
            return
        }
        val surface = factory.createVideoSource(camera.isScreencast)
        // The capturer hands its frames over through a surface-texture thread of its own, built
        // on the shared GL context so the camera's frames reach the encoder without a CPU copy.
        val helper = SurfaceTextureHelper.create("migo-video", eglBase?.eglBaseContext)
        camera.initialize(helper, context, surface.capturerObserver)
        camera.startCapture(VIDEO_WIDTH, VIDEO_HEIGHT, VIDEO_FPS)
        val video = factory.createVideoTrack("migo-video", surface)
        // The sender is kept rather than dropped: it is the one place this call's own video can
        // be shaped, and every rung of the ladder is written onto it.
        videoSender = pc.addTrack(video, listOf("migo"))
        videoCapturer = camera
        videoHelper = helper
        videoSource = surface
        videoTrack = video
        // The button exists from the moment the call has a camera to give back, and the wish starts
        // on because a video call opens its camera rather than waiting to be asked.
        cameraWanted = true
        // The share control arrives with the track it needs and not a moment before: a call with
        // no video m-line has nowhere to put a screen, so the state stays null there and the
        // control is absent rather than present and unable to send anything.
        _state.update { it.copy(cameraOn = true, screenSharing = false) }
    }

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

        override fun onTrack(transceiver: RtpTransceiver?) {
            // The peer's camera, arriving as a track on a call that may already be connected. The
            // track is handed to the state the screen renders from rather than rendered here --
            // the manager knows media, not surfaces -- and only a video track is news: the audio
            // path is owned by the audio module the factory was built on. The SDK's transceiver
            // has no track getter of its own; the receiver it wraps carries the incoming one.
            val track = transceiver?.receiver?.track() ?: return
            if (track is VideoTrack) {
                _remoteVideo.value = track
            }
        }

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
    /**
     * Whether the user's post-call verdict has gone out. Its own flag rather than a second use of
     * the setup-time report, because the two leave at opposite ends of the call: the setup time at
     * the connect, the verdict after the end, and a call that never connected sends neither.
     */
    val ratingSent: Boolean = false,
) {
    /**
     * Whether this ended call may still be rated. True only for a call that really connected and
     * has not been rated yet, because a call nobody experienced is a call nobody can judge -- and a
     * question asked about one would be answered by a guess about something that never happened.
     */
    val canRate: Boolean
        get() = state == CallState.Ended && startedAt != null && !ratingSent
}

/** The slice of the call manager the screens read. */
data class CallUiState(
    /** The call this device is in, including one that just ended (until dismissed). */
    val call: ActiveCall? = null,
    /** A ringing inbound call nobody has answered; takes the screen over any tracked call. */
    val incoming: CallInviteEvent? = null,
    /** Whether this side's microphone is muted. */
    val muted: Boolean = false,
    /**
     * Whether this side's camera is open, for the call screen's camera button. Null when there is no
     * camera to toggle at all -- a voice call, or a video call whose camera could not be opened --
     * because a button that cannot change anything is a control that lies about what it does.
     */
    val cameraOn: Boolean? = null,
    /**
     * Whether this device's screen is on the call's video track, for the share control. Null where
     * the call has no such track at all -- a voice call, or a video call whose camera never opened
     * -- for the same reason [cameraOn] is null there: a control that cannot put a picture anywhere
     * is a control that lies about what it does.
     */
    val screenSharing: Boolean? = null,
    /**
     * The rung the call's own link is on, for the quality indicator: what the ladder measured,
     * lowered by whatever the user pinned. Null until one of the two has said something, which is why
     * a call nothing has classified and no control has touched shows nothing rather than the top
     * rung: a tier nobody measured and nobody chose is not a tier.
     */
    val quality: LinkQuality? = null,
    /**
     * The rung the user pinned this call to, or null for automatic. What the quality control reads
     * its tick from, and what the call's caps are lowered to.
     */
    val qualityCeiling: LinkQuality? = null,
    /** Whether low bandwidth mode is on for this call, for the control that toggles it. */
    val lowBandwidth: Boolean = false,
    /**
     * Whether the call is degraded in section 180's sense: connected, with video paused because the
     * quality dropped. Never true for a voice call, which has no video to pause.
     */
    val degraded: Boolean = false,
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
