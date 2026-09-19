package com.migo.app.call

import android.content.Context
import com.migo.core.MigoClient
import com.migo.core.domain.GroupCallSeat
import com.migo.core.domain.IceCandidateJson
import com.migo.core.domain.SdpDescription
import com.migo.core.domain.Subscription
import com.migo.core.domain.decodeIceBatch
import com.migo.core.domain.decodeSdpDescription
import com.migo.core.domain.encodeIceBatch
import com.migo.core.domain.encodeSdpDescription
import com.migo.core.protocol.CallIce
import com.migo.core.protocol.CallSdp
import com.migo.core.protocol.TurnServer
import com.migo.core.wire.Id
import java.io.IOException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
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
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import org.webrtc.SurfaceTextureHelper
import org.webrtc.VideoSource
import org.webrtc.VideoTrack
import org.webrtc.audio.JavaAudioDeviceModule

/** What the camera is asked for on a group call: the one-to-one plane's own stage size. */
private const val GROUP_VIDEO_WIDTH = 640
private const val GROUP_VIDEO_HEIGHT = 360
private const val GROUP_VIDEO_FPS = 24

/**
 * How long one link's stats sample is given before it is abandoned.
 *
 * A connection closed with a request outstanding never answers it, so a sample with no ceiling would
 * leave a coroutine waiting on a link that is already gone. The next sample is two seconds away
 * ([QUALITY_POLL_MS]); dropping one costs nothing.
 */
private const val GROUP_STATS_TIMEOUT_MS = 1_500L

/**
 * How often a link that is not finished negotiating is looked at again.
 *
 * This is the plane's whole answer to two asynchronous facts it cannot be told about: the call's
 * frame key arriving (a join's answer is delivered by the key domain's own handler, on the key
 * domain's own coroutine, with nothing to notify this object through) and the epoch advancing under
 * a rotation. Both are visible as state -- [GroupCallKeysDomain.epochOf] is null until the key
 * lands, and changes value when it turns over -- so both are read rather than waited for, and a
 * link is built the moment it can be and rebuilt the moment its stamp stops matching.
 */
private const val GROUP_RECONCILE_MS = 1_000L

/**
 * How many of a link's own gathered candidates are held while their frame cannot be sent.
 *
 * They are held rather than dropped because the commonest reason a frame cannot leave is that the
 * call's key has not arrived yet, which resolves in under a second; but a link whose frames never
 * start leaving must not grow a queue for the rest of the call, so the oldest are dropped once the
 * bound is reached. The newest candidates are the ones a fresh link wants: a gathering run emits
 * host candidates first and the server-reflexive and relayed ones -- the ones that actually cross a
 * NAT -- after them.
 */
private const val MAX_HELD_CANDIDATES = 64

/**
 * One seat's link, as the call screen renders it.
 *
 * The remote video track rides here rather than in the manager's state because a mesh has one per
 * peer: the screen draws them side by side, and a single "remote video" slot is a shape only a
 * two-party call has.
 */
data class GroupLinkState(
    /** The seat's device, which is what a mesh link is between -- not the account. */
    val deviceId: Id,
    /** The account that device belongs to, for the tile's label. */
    val userId: Id,
    /** Whether this link's transport has come up. */
    val connected: Boolean,
    /** The rung this link is on, or null before it has produced a measurement to be on. */
    val quality: LinkQuality?,
    /** Whether this link carries a video line, which is what a camera button's absence means. */
    val videoCapable: Boolean,
    /** The peer's camera once it arrives; null while the peer sends none. */
    val video: VideoTrack?,
)

/**
 * The group call's media plane: one WebRTC peer connection per other seat, dialed and sealed here.
 *
 * A port of `clients/web/src/lib/migo/group-media.ts`. The decision half -- who dials whom, who
 * keeps their offer under glare, how many video streams a call carries -- is [GroupMedia.kt]'s and
 * is already pure; the ladder is [LinkQuality]'s and [VideoCaps]'s and is shared with the one-to-one
 * call. What lives here is only what needs a device: a peer connection per seat, the local tracks
 * every one of them carries, and the relays that carry the descriptions between them.
 *
 * # Why a mesh, and what that costs
 *
 * The server has no media transport (section 165's own finding: no frame climbs MWP), so a group
 * call is a full mesh -- every seat holds a connection to every other seat, and the call's upload
 * cost grows with the number of participants. That is the honest shape for this build rather than a
 * limitation to paper over: a mesh needs nothing of the server beyond the relay of an addressed
 * blob, which is an opcode that already exists, so a group call works today against a server that
 * knows nothing about group media at all.
 *
 * # The two uses of `CALL_SDP`, and how they are told apart
 *
 * `CALL_SDP` already carries the group *key exchange* (section 163): a joiner's ask for the running
 * key and a holder's answer, both sealed under the pairwise session crypto. This plane adds a third
 * and fourth payload to the same opcode -- the media descriptions and the candidates -- sealed under
 * the call's own frame key instead.
 *
 * Nothing in the frame says which it is. The key says: the plane opens every relay it receives under
 * the frame key, and AEAD makes that test exact -- a frame sealed under the session crypto cannot
 * open under the frame key, and vice versa, so the two handlers can both look at every frame and
 * exactly one of them will ever recognise it. This is why the plane subscribes to `client.onCallSdp`
 * beside the key domain rather than asking the key domain to route: [ListenerSet] fans out to every
 * handler, the key domain keeps its own dispatch untouched, and a media frame the key domain cannot
 * open is dropped there in the same breath as it is claimed here.
 *
 * # What a rotation means to a link
 *
 * The frame key seals *signaling*, not media: once a link's descriptions have been exchanged, its
 * audio and video flow over DTLS-SRTP under keys the two peers negotiated themselves, and a
 * rotation cannot disturb them. So a rotation resets only the links that had not finished
 * negotiating -- a description sealed under an epoch the other side has already left would never
 * open, and re-dialing under the new key is the only way forward for a link that got caught mid
 * exchange. That is [reconcile]'s second job, and it is why every link is stamped with the epoch it
 * was built under.
 */
class GroupMediaPlane(
    private val context: Context,
    private val client: MigoClient,
    /** The account this device is signed in as -- the roster's own name for this seat. */
    private val accountId: Id,
    /** This session's device, the tie-breaker under glare. */
    private val ownDeviceId: Id,
    /** The session's scope: every link, timer and sampler dies with it. */
    private val scope: CoroutineScope,
) {
    private val _links = MutableStateFlow<List<GroupLinkState>>(emptyList())

    /** One entry per other seat, in the order the links were built. */
    val links: StateFlow<List<GroupLinkState>> = _links.asStateFlow()

    private val _localVideo = MutableStateFlow<VideoTrack?>(null)

    /** This device's own camera track, for the call screen's self-view. */
    val localVideo: StateFlow<VideoTrack?> = _localVideo.asStateFlow()

    /**
     * The GL context every renderer of this call's video must be initialized against: this plane's
     * own, minted with the factories its tracks were created on.
     */
    val eglContext: EglBase.Context?
        get() = eglBase?.eglBaseContext

    private val lock = Any()

    @Volatile private var callId: Id? = null
    @Volatile private var seats: List<GroupCallSeat> = emptyList()
    @Volatile private var relays: List<TurnServer> = emptyList()
    @Volatile private var stopped = false
    @Volatile private var reconcileJob: Job? = null

    /** Whether this device's camera is on. Off is the state a group join starts in. */
    @Volatile private var cameraOn = false

    @Volatile private var muted = false

    private val linksByDevice = LinkedHashMap<Id, Link>()

    // --- the WebRTC engine ---

    @Volatile private var factory: PeerConnectionFactory? = null
    @Volatile private var eglBase: EglBase? = null
    @Volatile private var audioModule: JavaAudioDeviceModule? = null
    @Volatile private var audioSource: AudioSource? = null
    @Volatile private var audioTrack: AudioTrack? = null
    @Volatile private var videoSource: VideoSource? = null
    @Volatile private var videoTrack: VideoTrack? = null
    @Volatile private var videoHelper: SurfaceTextureHelper? = null
    @Volatile private var capturer: CameraVideoCapturer? = null

    private companion object {
        /**
         * The process-global WebRTC initializer, run once. Shared with [CallManager] by value, not
         * by reference -- each keeps its own flag, and the second call is a no-op the platform
         * tolerates -- because a device in a group call is never in a one-to-one call, and the two
         * managers must not have to agree about who goes first.
         */
        @Volatile private var webrtcInitialized = false
    }

    private fun ensureEngine(): PeerConnectionFactory? {
        factory?.let { return it }
        synchronized(this) {
            factory?.let { return it }
            if (stopped) {
                return null
            }
            if (!webrtcInitialized) {
                PeerConnectionFactory.initialize(
                    PeerConnectionFactory.InitializationOptions.builder(context)
                        .setEnableInternalTracer(false)
                        .createInitializationOptions(),
                )
                webrtcInitialized = true
            }
            val module = JavaAudioDeviceModule.builder(context).createAudioDeviceModule()
            val gl = EglBase.create()
            val built = PeerConnectionFactory.builder()
                .setAudioDeviceModule(module)
                .setVideoEncoderFactory(DefaultVideoEncoderFactory(gl.eglBaseContext, true, true))
                .setVideoDecoderFactory(DefaultVideoDecoderFactory(gl.eglBaseContext))
                .createPeerConnectionFactory()
            val source = built.createAudioSource(MediaConstraints())
            val track = built.createAudioTrack("migo-group-voice", source)
            val video = built.createVideoSource(false)
            val picture = built.createVideoTrack("migo-group-video", video)
            // A camera that is off is a track enabled false rather than an absent m-line: the video
            // line is negotiated once, at link creation, so turning the camera on later is a
            // parameter change the peers never see as a renegotiation.
            picture.setEnabled(false)
            audioModule = module
            eglBase = gl
            factory = built
            audioSource = source
            audioTrack = track
            videoSource = video
            videoTrack = picture
            videoHelper = SurfaceTextureHelper.create("migo-group-video", gl.eglBaseContext)
            _localVideo.value = picture
            return built
        }
    }

    // --- lifecycle ---

    /**
     * Bridges the plane onto the client's relays. The returned subscriptions are the caller's to
     * cancel; [stop] does the rest.
     *
     * Both handlers run under the frame key, and both drop a relay they cannot open -- which is
     * every key-exchange frame, and every frame for a call this device is not in. The drop is quiet
     * because it is the common case: see the class doc for why the two uses of `CALL_SDP` cannot be
     * confused for one another.
     */
    fun attach(): List<Subscription> = listOf(
        client.onCallSdp(::handleSealedSdp),
        client.onCallIce(::handleSealedIce),
    )

    /**
     * Takes up a call: the id everything is sealed under, and the seat list to build links for.
     *
     * Safe to call again for the same call (a later roster adds seats through [syncSeats]); calling
     * it for a *different* call tears the first one down, because a device holds one seat and a
     * plane that kept a departed call's links would hold connections nobody is on the other end of.
     */
    fun begin(callId: Id, seats: List<GroupCallSeat>, relays: List<TurnServer>) {
        if (synchronized(lock) { this.callId == callId }) {
            // The same call, opened again by whichever of the join's two halves landed second.
            // Nothing is torn down: the seats are the news, and the relays if they changed.
            setRelays(relays)
            syncSeats(seats)
            return
        }
        stop()
        synchronized(lock) {
            stopped = false
            this.callId = callId
            this.relays = relays
        }
        if (ensureEngine() == null) {
            return
        }
        syncSeats(seats)
        reconcileJob = scope.launch {
            while (isActive) {
                delay(GROUP_RECONCILE_MS)
                reconcile()
            }
        }
    }

    /**
     * The TURN relays the join reply carried.
     *
     * They arrive on a different frame from the roster, and either can land first, so this is a
     * separate step from [begin] rather than an argument to it. A link that was built before they
     * arrived and has not connected is rebuilt over them: a link that came up on STUN alone is not
     * wrong, but one that is still trying is a link the relays might yet save, and rebuilding it
     * costs a negotiation that has not produced anything to lose.
     */
    fun setRelays(relays: List<TurnServer>) {
        val changed = synchronized(lock) {
            if (this.relays == relays) {
                return
            }
            this.relays = relays
            true
        }
        if (changed) {
            reconcile()
        }
    }

    /**
     * Folds a roster into the links: a seat that appeared gets one, a seat that left loses one, and
     * every surviving link is re-checked against the order the dial policy reads.
     *
     * The order matters even for a link that already exists: a seat inserted *before* this one in
     * the roster flips which side dials between this device and every seat after it, and the two
     * sides must agree. A link that was already dialing and should now answer keeps its connection
     * and simply stops offering -- the answer path is the same one it would have used had the glare
     * rule sent it there.
     */
    fun syncSeats(next: List<GroupCallSeat>) {
        if (callId == null) {
            return
        }
        synchronized(lock) { seats = next }
        val others = next.filter { it.deviceId != ownDeviceId }
        val wanted = others.map { it.deviceId }.toSet()
        val opening = ArrayList<Link>()
        val closing = ArrayList<Link>()
        synchronized(lock) {
            for (seat in others) {
                val existing = linksByDevice[seat.deviceId]
                if (existing == null) {
                    val link = newLink(seat, others.size)
                    linksByDevice[seat.deviceId] = link
                    opening.add(link)
                } else {
                    existing.dialer = dialsRemote(next, accountId, seat.deviceId)
                }
            }
            val departed = linksByDevice.keys.filterNot { wanted.contains(it) }
            for (device in departed) {
                linksByDevice.remove(device)?.let { closing.add(it) }
            }
        }
        for (link in opening) {
            openLink(link)
        }
        for (link in closing) {
            closeLink(link)
        }
        publishLinks()
    }

    /**
     * Ends everything: every link, the camera, the tracks, and the engine they were built on.
     *
     * Idempotent, and safe from the session's own teardown: a plane that outlived its call would be
     * a microphone still open on a call the user has left.
     */
    fun stop() {
        val closing: List<Link>
        synchronized(lock) {
            stopped = true
            callId = null
            seats = emptyList()
            relays = emptyList()
            closing = linksByDevice.values.toList()
            linksByDevice.clear()
        }
        reconcileJob?.cancel()
        reconcileJob = null
        for (link in closing) {
            closeLink(link)
        }
        publishLinks()
        releaseEngine()
    }

    // --- the controls the screen offers ---

    /** Mutes or unmutes this device's microphone on every link at once. */
    fun setMuted(next: Boolean) {
        muted = next
        // One track feeds every link, so this is one write rather than one per peer: a mesh sends
        // the same microphone everywhere, and a mute that muted one tile's link would be a lie.
        audioTrack?.setEnabled(!next)
    }

    /** Whether the microphone is currently muted. */
    fun isMuted(): Boolean = muted

    /** Whether this device's camera is on. */
    fun isCameraOn(): Boolean = cameraOn

    /** Whether any link carries a video line, which is what makes a camera control meaningful. */
    fun cameraAvailable(): Boolean = synchronized(lock) {
        linksByDevice.values.any { it.videoSender != null }
    }

    /**
     * Turns this device's camera on or off, on every link.
     *
     * Off stops the capture, which is what hands the camera back to the platform -- a capturer
     * still running is a camera the system counts as in use, and its indicator burns beside a lens
     * whose frames are dropped on the floor. The track and the negotiated line are left alone:
     * with no capture there are no frames to encode, so nothing is on the wire either way, and
     * keeping the line is what makes turning the camera back on a restart rather than a
     * renegotiation every peer would have to answer.
     *
     * On can fail, because another application may have taken the camera while it was released;
     * a failure puts the wish back to off, since a button that keeps claiming a picture the call
     * is not sending is worse than no picture.
     */
    fun setCameraOn(on: Boolean) {
        val track = videoTrack ?: return
        if (on == cameraOn) {
            return
        }
        if (!on) {
            cameraOn = false
            runCatching { capturer?.stopCapture() }
            track.setEnabled(false)
            return
        }
        val started = runCatching { startCapture() }.isSuccess
        cameraOn = started
        track.setEnabled(started)
    }

    /**
     * Flips to the other camera, the front one first, exactly as a one-to-one call's does.
     *
     * A no-op while the camera is off: switching a lens that is not open is a request the user
     * cannot see the result of, and the next camera-on opens on the lens switched to last.
     */
    fun switchCamera() {
        if (!cameraOn) {
            return
        }
        runCatching { (capturer as? CameraVideoCapturer)?.switchCamera(null) }
    }

    // --- the engine's own pieces ---

    /**
     * Opens the camera and starts it, or restarts the one this plane already holds.
     *
     * The distinction is not a nicety: `CameraCapturer.initialize` is a once-per-capturer call and
     * throws `IllegalStateException` on the second one, so a restart that re-initialized would be a
     * camera that never comes back on -- the failure would be swallowed by the `runCatching` around
     * this call and read to the user as a button that does nothing.
     */
    private fun startCapture() {
        capturer?.let {
            it.startCapture(GROUP_VIDEO_WIDTH, GROUP_VIDEO_HEIGHT, GROUP_VIDEO_FPS)
            return
        }
        val source = videoSource ?: throw IOException("no video source")
        val helper = videoHelper ?: throw IOException("no video helper")
        val opened = openCamera()
        try {
            opened.initialize(helper, context, source.capturerObserver)
            opened.startCapture(GROUP_VIDEO_WIDTH, GROUP_VIDEO_HEIGHT, GROUP_VIDEO_FPS)
        } catch (error: Throwable) {
            // A camera that could not be opened leaves nothing behind: the capturer is disposed
            // here rather than stored, so the next attempt opens a fresh one instead of restarting
            // a half-built one.
            runCatching { opened.dispose() }
            throw error
        }
        capturer = opened
    }

    private fun openCamera(): CameraVideoCapturer {
        val enumerator = Camera2Enumerator(context)
        val front = enumerator.deviceNames.firstOrNull(enumerator::isFrontFacing)
            ?: throw IOException("no front-facing camera")
        return enumerator.createCapturer(front, null)
    }

    private fun releaseEngine() {
        synchronized(this) {
            val closing = factory
            factory = null
            videoHelper?.dispose()
            videoHelper = null
            runCatching { capturer?.stopCapture() }
            capturer?.dispose()
            capturer = null
            videoTrack?.dispose()
            videoTrack = null
            videoSource?.dispose()
            videoSource = null
            audioTrack?.dispose()
            audioTrack = null
            audioSource?.dispose()
            audioSource = null
            audioModule?.release()
            audioModule = null
            eglBase?.release()
            eglBase = null
            closing?.dispose()
            _localVideo.value = null
            cameraOn = false
            muted = false
        }
    }

    // --- one link ---

    /**
     * One seat's connection, and the bookkeeping the two directions of a negotiation need.
     *
     * Everything here is touched from both the session's coroutines and WebRTC's own signaling
     * thread, so the mutable flags are volatile and the two candidate lists are guarded by
     * [iceLock]; nothing else is shared.
     */
    private class Link(
        val deviceId: Id,
        val userId: Id,
        val pc: PeerConnection,
        /** The roster order's verdict on which side offers; the other side answers. */
        @Volatile var dialer: Boolean,
        /** The frame key's epoch this link's descriptions are being sealed under. */
        val epoch: Long?,
        val videoSender: RtpSender?,
    ) {
        @Volatile var closed = false
        @Volatile var remoteSet = false
        @Volatile var localSet = false
        @Volatile var connected = false
        @Volatile var offering = false
        @Volatile var dialed = false

        val iceLock = Any()

        /** Candidates gathered here that have not reached the peer yet -- no key, or the linger. */
        val iceBatch = ArrayList<IceCandidate>()

        /** Candidates the peer sent before this side's remote description was set. */
        val heldRemote = ArrayList<IceCandidate>()

        var iceTimer: Job? = null

        /** The sampler's own view of this link, kept per link because each has its own transport. */
        var quality: LinkQuality = LinkQuality.Full
        var qualityChangedAt: Long = 0L
        var counters: LinkCounters? = null
        var sampler: Job? = null

        val remoteVideo = MutableStateFlow<VideoTrack?>(null)
    }

    private fun newLink(seat: GroupCallSeat, remoteSeats: Int): Link {
        val built = factory ?: throw IOException("no peer connection factory")
        val epoch = callId?.let { client.groupCallKeys.epochOf(it) }
        val config = PeerConnection.RTCConfiguration(iceServers()).apply {
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
        }
        val pc = built.createPeerConnection(config, LinkObserver(deviceOf = seat.deviceId))
            ?: throw IOException("no peer connection")
        // The microphone is unconditional -- a call without one is not a call -- while the video
        // line is negotiated only where the product limit admits it. That count is the *other*
        // seats, which is an upper bound on the video publishers among them: the wire carries no
        // media kind, so a seat cannot know how many of its peers publish, and the bound is
        // deliberately conservative in the direction that keeps a call under the limit.
        val wantsVideo = videoAdmitted(remoteSeats, true)
        audioTrack?.let { pc.addTrack(it, listOf("migo")) }
        val picture = if (wantsVideo) videoTrack?.let { pc.addTrack(it, listOf("migo")) } else null
        val link = Link(
            deviceId = seat.deviceId,
            userId = seat.userId,
            pc = pc,
            dialer = dialsRemote(seats, accountId, seat.deviceId),
            epoch = epoch,
            videoSender = picture,
        )
        link.qualityChangedAt = System.currentTimeMillis()
        if (picture != null) {
            shapeVideo(link, link.quality, 0L)
        }
        return link
    }

    private fun openLink(link: Link) {
        if (link.closed) {
            return
        }
        link.sampler = scope.launch {
            while (isActive && !link.closed) {
                delay(QUALITY_POLL_MS)
                sample(link)
            }
        }
        reconcile()
    }

    private fun closeLink(link: Link) {
        link.closed = true
        link.sampler?.cancel()
        link.sampler = null
        link.iceTimer?.cancel()
        link.iceTimer = null
        synchronized(link.iceLock) {
            link.iceBatch.clear()
            link.heldRemote.clear()
        }
        runCatching { link.pc.close() }
        runCatching { link.pc.dispose() }
        link.remoteVideo.value = null
    }

    /**
     * Builds the links that can be built and rebuilds the ones whose epoch has moved on.
     *
     * Run on a tick and after every roster change, because the two facts it acts on arrive without
     * a callback: the call's key turning up (the join's answer is the key domain's frame to handle)
     * and the epoch advancing under a rotation. Neither is worth a notification channel when both
     * are already readable state, and a tick that finds nothing to do costs one null check.
     */
    private fun reconcile() {
        val call = callId ?: return
        val epoch = client.groupCallKeys.epochOf(call) ?: return
        val stale = ArrayList<Link>()
        val redial = ArrayList<Link>()
        synchronized(lock) {
            for (link in linksByDevice.values) {
                if (link.closed || link.connected) {
                    continue
                }
                if (link.epoch != epoch) {
                    stale.add(link)
                } else if (link.dialer && !link.dialed && !link.localSet && !link.remoteSet) {
                    // Never a second description: a link that has offered once is waiting on its
                    // answer, and a link that has a description in play at all -- an answerer's,
                    // made when a peer's offer crossed its roster -- is negotiating as the other
                    // side of the pair, whatever the roster went on to say about who dials.
                    redial.add(link)
                }
            }
            for (link in stale) {
                linksByDevice.remove(link.deviceId)
            }
        }
        if (stale.isEmpty() && redial.isEmpty()) {
            return
        }
        for (link in stale) {
            closeLink(link)
        }
        for (link in stale) {
            val seat = seats.firstOrNull { it.deviceId == link.deviceId } ?: continue
            val rebuilt = runCatching { newLink(seat, seats.size - 1) }.getOrNull() ?: continue
            synchronized(lock) { linksByDevice[seat.deviceId] = rebuilt }
            openLink(rebuilt)
        }
        for (link in redial) {
            offer(link)
        }
        publishLinks()
    }

    private fun iceServers(): List<PeerConnection.IceServer> = groupIceServers(relays)

    private fun publishLinks() {
        val snapshot = synchronized(lock) {
            linksByDevice.values.map { link ->
                GroupLinkState(
                    deviceId = link.deviceId,
                    userId = link.userId,
                    connected = link.connected,
                    quality = if (link.counters == null) null else link.quality,
                    videoCapable = link.videoSender != null,
                    video = link.remoteVideo.value,
                )
            }
        }
        _links.value = snapshot
    }

    // --- the negotiation ---

    /** Offers to a seat this device dials. Guarded so a tick and a roster change cannot both. */
    private fun offer(link: Link) {
        if (link.closed || link.dialed || link.offering) {
            return
        }
        link.offering = true
        link.dialed = true
        link.pc.createOffer(OfferObserver(link), MediaConstraints())
    }

    /** Sends one description to a link, sealed under the call's frame key. */
    private fun sendDescription(link: Link, description: SessionDescription) {
        val call = callId ?: return
        val plaintext = encodeSdpDescription(
            SdpDescription(description.type.canonicalForm(), description.description),
        )
        // No key yet is the ordinary state of a link that was built the moment the roster landed:
        // the join's answer is still crossing. The frame is dropped rather than queued, because a
        // description is only valid against the state that produced it and the re-offer below is
        // the retry that matters -- a stale offer replayed after the key arrives would be an offer
        // against a peer connection that has moved on.
        val sealed = client.groupCallKeys.sealFrame(call, plaintext) ?: return
        scope.launch {
            runCatching { client.groupCalls.sendSdp(call, link.deviceId, sealed) }
        }
    }

    /**
     * Folds one description from a peer into its link.
     *
     * Three things can arrive here: an answer to an offer this side made, an offer from the side the
     * roster told to offer, or an offer from a peer that read the roster the other way round. The
     * first two are the ordinary path. The third is glare, and it is broken by the same rule the web
     * build uses -- the lexicographically smaller device id keeps its offer and the other rolls back
     * to answer -- which both sides compute from the same two ids, so they cannot both keep or both
     * yield.
     */
    private fun receiveDescription(link: Link, type: String, sdp: String) {
        if (link.closed) {
            return
        }
        when (type) {
            "answer" -> link.pc.setRemoteDescription(
                RemoteAnswerObserver(link),
                SessionDescription(SessionDescription.Type.ANSWER, sdp),
            )
            "offer" -> {
                if (link.offering && iKeepMyOffer(ownDeviceId, link.deviceId)) {
                    // This side keeps its offer. The peer's is dropped, not held: by the time its
                    // own rollback finishes it will have answered ours, and a stack holding two
                    // offers has a signaling state neither side can name.
                    return
                }
                val offer = SessionDescription(SessionDescription.Type.OFFER, sdp)
                if (link.offering) {
                    link.offering = false
                    link.pc.setLocalDescription(
                        RollbackObserver(link, offer),
                        SessionDescription(SessionDescription.Type.ROLLBACK, ""),
                    )
                    return
                }
                link.pc.setRemoteDescription(RemoteOfferObserver(link), offer)
            }
            else -> Unit
        }
    }

    private fun applyHeldRemote(link: Link) {
        val held = synchronized(link.iceLock) {
            val copy = ArrayList(link.heldRemote)
            link.heldRemote.clear()
            copy
        }
        for (candidate in held) {
            runCatching { link.pc.addIceCandidate(candidate) }
        }
    }

    // --- the relays, both directions ---

    /**
     * A relay that opens under the call's frame key is this plane's; one that does not belongs to
     * the key exchange, or to a call this device is not in, and is dropped here in silence because
     * the key domain is already looking at it.
     */
    private fun handleSealedSdp(relay: CallSdp) {
        val call = callId ?: return
        // Addressed relays, the same reading the key domain gives the same opcode: a frame naming
        // another device is a sibling's mail, and only a frame naming this one is a link's.
        if (relay.callId != call || relay.toDevice != ownDeviceId) {
            return
        }
        val opened = client.groupCallKeys.openFrame(call, relay.sealedSdp) ?: return
        // The bytes under this key are a *description*: candidates ride their own opcode, so the
        // two shapes never share a channel and nothing here has to guess which arrived. What the
        // decoder refuses is a shape this build does not know -- a future description type, or a
        // member's malformed frame -- and it is dropped rather than half-applied.
        val description = runCatching { decodeSdpDescription(opened) }.getOrNull() ?: return
        val link = linkFor(relay.fromDevice, description.type) ?: return
        receiveDescription(link, description.type, description.sdp)
    }

    /**
     * The link a relay from one device belongs to, building it when an offer arrives first.
     *
     * A link is built from the roster, and the roster reaches the two sides of a link on
     * different frames: the dialer learns the seat from the conversation's announcement and the
     * answerer from the same announcement one device later, so an offer can cross a seat's
     * arrival. Dropping it would not be a retry but a dead link -- the dialer has already offered
     * and nothing on either side offers again -- so an offer for a seat this device knows and has
     * not built yet builds the link here, on the answerer's side of it, which is what the web
     * build does for the same reason.
     *
     * A link made this way answers and never dials, whatever the roster says about who should
     * have: the peer's offer is already here, and a link that went on to offer after answering
     * would put a second description on the wire that no side is waiting for. Two things keep
     * that: the flag is cleared below, and [reconcile] offers only on a link that has never
     * negotiated at all -- which covers the roster *changing its mind*, the transient
     * disagreement between two concurrent joins' projections that also produces glare. Where the
     * roster names this device the dialer, the glare rule decides, exactly as it does for a link
     * that was already built.
     */
    private fun linkFor(device: Id, type: String): Link? {
        synchronized(lock) { linksByDevice[device] }?.let { return it }
        if (type != "offer") {
            return null
        }
        val roster = seats
        val seat = roster.firstOrNull { it.deviceId == device } ?: return null
        if (dialsRemote(roster, accountId, device) && iKeepMyOffer(ownDeviceId, device)) {
            return null
        }
        val built = runCatching { newLink(seat, roster.size - 1) }.getOrNull() ?: return null
        built.dialer = false
        synchronized(lock) {
            // A tick that built the same link between the lookup above and here wins: the link
            // this call made is the duplicate, and closing it is what keeps one connection per
            // seat.
            val existing = linksByDevice.putIfAbsent(device, built)
            if (existing != null) {
                closeLink(built)
                return existing
            }
        }
        openLink(built)
        return built
    }

    private fun handleSealedIce(relay: CallIce) {
        val call = callId ?: return
        if (relay.callId != call || relay.toDevice != ownDeviceId) {
            return
        }
        val link = synchronized(lock) { linksByDevice[relay.fromDevice] } ?: return
        val opened = client.groupCallKeys.openFrame(call, relay.sealedCandidates) ?: return
        val candidates = runCatching { decodeIceBatch(opened) }.getOrNull() ?: return
        for (candidate in candidates) {
            val ice = IceCandidate(candidate.sdpMid, candidate.sdpMLineIndex, candidate.candidate ?: "")
            // A candidate that arrives before the peer's description has nowhere to attach: the
            // stack refuses it and the pair is never tried. Held rather than dropped, and applied
            // the moment the description lands -- which is the same discipline the one-to-one plane
            // keeps, for the same reason.
            if (link.remoteSet) {
                runCatching { link.pc.addIceCandidate(ice) }
            } else {
                synchronized(link.iceLock) {
                    if (link.heldRemote.size < MAX_HELD_CANDIDATES) {
                        link.heldRemote.add(ice)
                    }
                }
            }
        }
    }

    private fun onLocalCandidate(link: Link, candidate: IceCandidate?) {
        if (link.closed) {
            return
        }
        if (candidate == null) {
            flushIce(link)
            return
        }
        val full = synchronized(link.iceLock) {
            link.iceBatch.add(candidate)
            while (link.iceBatch.size > MAX_HELD_CANDIDATES) {
                link.iceBatch.removeAt(0)
            }
            link.iceBatch.size
        }
        if (full > 0 && link.iceTimer == null) {
            link.iceTimer = scope.launch {
                delay(DEFAULT_ICE_LINGER_MS)
                link.iceTimer = null
                flushIce(link)
            }
        }
    }

    /** Relays whatever this link has batched. A frame that cannot leave stays batched for the retry. */
    private fun flushIce(link: Link) {
        val call = callId ?: return
        if (link.closed) {
            return
        }
        val batch = synchronized(link.iceLock) {
            if (link.iceBatch.isEmpty()) {
                return
            }
            val copy = ArrayList(link.iceBatch)
            link.iceBatch.clear()
            copy
        }
        val plaintext = encodeIceBatch(
            batch.map {
                IceCandidateJson(
                    candidate = it.sdp,
                    sdpMid = it.sdpMid,
                    sdpMLineIndex = it.sdpMLineIndex,
                )
            },
        )
        val sealed = client.groupCallKeys.sealFrame(call, plaintext)
        if (sealed == null) {
            // No key yet: the candidates are real and still useful, so they go back to the front of
            // the queue and the tick that rebuilds links will find them when the key lands.
            synchronized(link.iceLock) {
                link.iceBatch.addAll(0, batch)
                while (link.iceBatch.size > MAX_HELD_CANDIDATES) {
                    link.iceBatch.removeAt(0)
                }
            }
            return
        }
        scope.launch {
            runCatching { client.groupCalls.sendIce(call, link.deviceId, sealed) }
        }
    }

    // --- the ladder ---

    /** One sample of one link, turned into a rung and written onto that link's own sender. */
    private fun sample(link: Link) {
        if (link.closed || !link.connected) {
            return
        }
        scope.launch {
            val counters = withTimeoutOrNull(GROUP_STATS_TIMEOUT_MS) {
                readLinkCounters(link.pc)
            } ?: return@launch
            val previous = link.counters
            link.counters = counters
            val at = System.currentTimeMillis()
            val step = advanceMeasurement(
                current = link.quality,
                previous = previous,
                counters = counters,
                changedAtMs = link.qualityChangedAt,
                nowMs = at,
            ) ?: return@launch
            if (!step.changed) {
                return@launch
            }
            link.quality = step.quality
            link.qualityChangedAt = at
            shapeVideo(link, step.quality, step.stats.sentKbps)
            publishLinks()
        }
    }

    /**
     * Writes one rung onto one link's video sender.
     *
     * Per link rather than per call, which is the whole difference between a mesh and a two-party
     * call: a seat has one transport per peer, so the link that is congested is capped while the
     * links that are fine keep sending the full picture. Every rung writes every cap it names, the
     * ones it imposes nothing on included, because a member left out of a sender's parameters is a
     * member the receiver keeps -- a link that climbed back and omitted the cap it was giving back
     * would keep sending the small picture its screen no longer claims.
     *
     * The bottom rung stops the stream without detaching the track, which is what makes the rung
     * that climbs back a parameter change rather than a camera to reopen.
     */
    private fun shapeVideo(link: Link, rung: LinkQuality, measuredKbps: Long) {
        val sender = link.videoSender ?: return
        val caps = videoCaps(rung, measuredKbps)
        val params = sender.parameters ?: return
        val encoding = params.encodings.firstOrNull() ?: return
        encoding.active = caps.active
        encoding.maxBitrateBps = caps.maxBitrateBps
        encoding.scaleResolutionDownBy = caps.scaleResolutionDownBy
        encoding.maxFramerate = caps.maxFramerate
        runCatching { sender.setParameters(params) }
    }

    // --- the peer connection's callbacks ---

    /**
     * One link's callbacks, on WebRTC's own signaling thread.
     *
     * `org.webrtc.PeerConnection.Observer` is a Java interface whose methods are mostly abstract in
     * this artifact (verified against stream-webrtc-android 1.3.10: only the newer hooks ship
     * default bodies), so every method this build has no use for is spelled out as a no-op, in the
     * order the interface declares them, so the next reader can diff against it.
     */
    private inner class LinkObserver(private val deviceOf: Id) : PeerConnection.Observer {
        private fun link(): Link? = synchronized(lock) { linksByDevice[deviceOf] }

        override fun onSignalingChange(newState: PeerConnection.SignalingState) = Unit

        override fun onIceConnectionChange(newState: PeerConnection.IceConnectionState) = Unit

        override fun onIceConnectionReceivingChange(receiving: Boolean) = Unit

        override fun onIceGatheringChange(newState: PeerConnection.IceGatheringState) {
            if (newState == PeerConnection.IceGatheringState.COMPLETE) {
                // org.webrtc signals end-of-gathering here rather than as a null candidate (which is
                // how a browser says it). Whatever is batched is all there will be, so it leaves now
                // rather than waiting the linger out.
                link()?.let { flushIce(it) }
            }
        }

        override fun onIceCandidate(candidate: IceCandidate?) {
            link()?.let { onLocalCandidate(it, candidate) }
        }

        override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>?) = Unit

        override fun onAddStream(stream: MediaStream?) = Unit

        override fun onRemoveStream(stream: MediaStream?) = Unit

        override fun onDataChannel(channel: DataChannel?) = Unit

        override fun onRenegotiationNeeded() = Unit

        override fun onTrack(transceiver: RtpTransceiver?) {
            val track = transceiver?.receiver?.track() ?: return
            if (track !is VideoTrack) {
                // The audio path is owned by the audio module the factory was built on; only a
                // picture is news this plane has to hand anywhere.
                return
            }
            link()?.let { link ->
                link.remoteVideo.value = track
                publishLinks()
            }
        }

        override fun onConnectionChange(newState: PeerConnection.PeerConnectionState) {
            val link = link() ?: return
            when (newState) {
                PeerConnection.PeerConnectionState.CONNECTED -> {
                    link.connected = true
                    publishLinks()
                }
                PeerConnection.PeerConnectionState.FAILED,
                PeerConnection.PeerConnectionState.CLOSED,
                -> {
                    link.connected = false
                    publishLinks()
                }
                else -> Unit
            }
        }
    }

    /** The dialer's half: an offer, and then its local description, and then the relay. */
    private inner class OfferObserver(private val link: Link) : SdpObserver {
        override fun onCreateSuccess(sdp: SessionDescription?) {
            val description = sdp ?: return
            link.pc.setLocalDescription(LocalObserver(link, description), description)
        }

        override fun onSetSuccess() = Unit

        override fun onCreateFailure(error: String?) = Unit

        override fun onSetFailure(error: String?) = Unit
    }

    /**
     * A description that has just become this side's local one.
     *
     * Both directions pass through here -- the dialer's offer and the answerer's answer -- because
     * both are the same fact from the relay's point of view: there is now something of this side's
     * to send, and it goes sealed under the call's frame key.
     */
    private inner class LocalObserver(
        private val link: Link,
        private val description: SessionDescription,
    ) : SdpObserver {
        override fun onCreateSuccess(sdp: SessionDescription?) = Unit

        override fun onSetSuccess() {
            link.localSet = true
            sendDescription(link, description)
        }

        override fun onCreateFailure(error: String?) = Unit

        override fun onSetFailure(error: String?) = Unit
    }

    /**
     * A peer's offer, now this side's remote description: the answer follows.
     *
     * The held candidates attach here, which is the earliest moment the stack can use them -- a
     * candidate with no remote description to attach to is refused and its pair never tried.
     */
    private inner class RemoteOfferObserver(private val link: Link) : SdpObserver {
        override fun onCreateSuccess(sdp: SessionDescription?) {
            val description = sdp ?: return
            link.pc.setLocalDescription(LocalObserver(link, description), description)
        }

        override fun onSetSuccess() {
            link.remoteSet = true
            applyHeldRemote(link)
            link.pc.createAnswer(this, MediaConstraints())
        }

        override fun onCreateFailure(error: String?) = Unit

        override fun onSetFailure(error: String?) = Unit
    }

    /** A peer's answer, now this side's remote description: the link is negotiating no longer. */
    private inner class RemoteAnswerObserver(private val link: Link) : SdpObserver {
        override fun onCreateSuccess(sdp: SessionDescription?) = Unit

        override fun onSetSuccess() {
            link.remoteSet = true
            link.offering = false
            applyHeldRemote(link)
        }

        override fun onCreateFailure(error: String?) = Unit

        override fun onSetFailure(error: String?) = Unit
    }

    /**
     * The glare loser's rollback: this side's own offer is withdrawn, and the peer's offer -- kept
     * for exactly this -- becomes the remote description, on the way to the answer it deserves.
     */
    private inner class RollbackObserver(
        private val link: Link,
        private val offer: SessionDescription,
    ) : SdpObserver {
        override fun onCreateSuccess(sdp: SessionDescription?) = Unit

        override fun onSetSuccess() {
            link.localSet = false
            link.pc.setRemoteDescription(RemoteOfferObserver(link), offer)
        }

        override fun onCreateFailure(error: String?) = Unit

        override fun onSetFailure(error: String?) = Unit
    }
}
