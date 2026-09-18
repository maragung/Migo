package com.migo.app.ui

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.media.projection.MediaProjectionManager
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import com.migo.app.call.ActiveCall
import com.migo.app.call.CallManager
import com.migo.app.call.CallUiState
import com.migo.app.call.LinkQuality
import com.migo.app.call.QUALITY_CEILINGS
import com.migo.app.call.qualityTierLabel
import com.migo.core.domain.CallDisplayState
import com.migo.core.domain.CallMediaKind
import com.migo.core.domain.CallState
import com.migo.core.domain.callStateLabel
import com.migo.core.domain.displayStateOf
import com.migo.core.domain.endedReasonLine
import com.migo.core.domain.formatCallDuration
import com.migo.core.domain.mediaKindLabel
import com.migo.core.protocol.CallRating
import kotlinx.coroutines.delay
import org.webrtc.EglBase
import org.webrtc.RendererCommon
import org.webrtc.SurfaceViewRenderer
import org.webrtc.VideoTrack

/**
 * The session's shared video GL context, provided by the composition root from the call
 * manager and read by every video surface below. A composition-local rather than a global,
 * because the context belongs to the session that minted it and dies with it -- a static would
 * hand a dead context to the next session's first frame.
 */
val LocalCallEglContext = staticCompositionLocalOf<EglBase.Context?> { null }

/**
 * The call screen: everything a user sees while a call is ringing, connected, or just ended.
 *
 * A port of `clients/web/src/components/call-overlay.tsx`, voice and video. The video stage
 * fills the screen once a video call is connected or reconnecting -- the peer's camera
 * full-bleed, this side's as a small self-view in the corner -- with the identity and the
 * actions riding above it; the states before connection render the voice layout, because a
 * video that has not connected is a call, not a stage. The overlay renders above the whole
 * shell whenever a call is ringing, live, just ended, or failed to start, and nothing at all
 * otherwise: the composable's absence *is* the "no call" state.
 *
 * # The six states (section 180)
 *
 * A call screen must never go silent with no explanation -- a user who cannot tell ringing from
 * dead hangs up and redials. So every state names itself: *Ringing* ("Calling…" out, "Incoming
 * voice call" in), *Connecting*, *Connected* with a running duration, *Reconnecting* while the
 * transport blips, *Degraded* while quality holds media back -- a video call whose link fell to the
 * ladder's bottom rung, which this build now reaches -- and *Ended* always with the reason: a
 * declined call, a failed call, and a network death are different facts, and calling them all
 * "Call ended" throws away the one thing the user needs before calling back.
 *
 * # The clock
 *
 * The duration is the only number on screen that moves, so the overlay keeps its own one-second
 * tick while connected -- re-zeroed on entering the state, so the first shown second is this
 * call's, not the composition's.
 *
 * # Back
 *
 * The back gesture is handled where it means something, the shell's own rule: back declines an
 * incoming ring, dismisses an ended screen or a failure card, and on a live call is consumed
 * without acting -- a pocket gesture must never hang up on anybody.
 *
 * # The small window
 *
 * A video call can be handed to the system's picture-in-picture window, which is the phone's own
 * answer to leaving the app without leaving the call. In that window this screen drops everything
 * drawn for a full one -- the identity, the status line, the controls -- and shows the picture,
 * because a thumbnail has room for a picture and nothing else. The layout is chosen by the same
 * `CallDisplayState` as everywhere below, so a call that ends while minimised says so in the
 * window rather than going black.
 */
@Composable
fun CallOverlay(
    state: CallUiState,
    peerName: String,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
    onCancel: () -> Unit,
    onHangUp: () -> Unit,
    onToggleMute: () -> Unit,
    onToggleCamera: () -> Unit,
    onDismiss: () -> Unit,
    onRateCall: (CallRating, ULong) -> Unit,
    localVideo: VideoTrack?,
    remoteVideo: VideoTrack?,
    onMinimize: (() -> Unit)? = null,
    onSwitchCamera: (() -> Unit)? = null,
    outputs: List<CallManager.AudioOutput> = emptyList(),
    chosenOutput: Int? = null,
    onChooseOutput: ((Int) -> Unit)? = null,
    onStartScreenShare: ((Intent) -> Unit)? = null,
    onStopScreenShare: (() -> Unit)? = null,
    onChooseQuality: ((LinkQuality?) -> Unit)? = null,
    onToggleLowBandwidth: ((Boolean) -> Unit)? = null,
    inPictureInPicture: Boolean = false,
    modifier: Modifier = Modifier,
) {
    val incoming = state.incoming
    val call = state.call

    if (incoming == null && call == null && state.callError == null) {
        return
    }

    // Back means "close this, not the app", in the order a person reads the screen. Deeper
    // handlers (a chat's member sheet) are composed beneath the shell, so the overlay wins while
    // it is up -- which is the point: a dialog's back cannot fall through to the chat behind it.
    BackHandler(enabled = true, onBack = {
        when {
            incoming != null -> onDecline()
            call == null || call.state == CallState.Ended -> onDismiss()
            else -> Unit // A live call: the gesture is consumed, never acted on.
        }
    })

    Surface(
        modifier = modifier.fillMaxSize(),
        color = MaterialTheme.colorScheme.background,
    ) {
        when {
            incoming != null -> IncomingCallScreen(
                state = state,
                peerName = peerName,
                onAccept = onAccept,
                onDecline = onDecline,
            )

            call != null -> ActiveCallScreen(
                state = state,
                call = call,
                peerName = peerName,
                // The self-view is hidden while the camera is off, because a stopped capturer
                // leaves the last frame frozen on the surface and a still picture of somebody's
                // face is a lie about a camera that is not on -- but a screen on that track is a
                // picture of its own, and it keeps the self-view alive.
                localVideo = if (state.cameraOn == false && state.screenSharing != true) {
                    null
                } else {
                    localVideo
                },
                remoteVideo = remoteVideo,
                onCancel = onCancel,
                onHangUp = onHangUp,
                onToggleMute = onToggleMute,
                onToggleCamera = onToggleCamera,
                onDismiss = onDismiss,
                onRateCall = onRateCall,
                onMinimize = onMinimize,
                onSwitchCamera = onSwitchCamera,
                outputs = outputs,
                chosenOutput = chosenOutput,
                onChooseOutput = onChooseOutput,
                onStartScreenShare = onStartScreenShare,
                onStopScreenShare = onStopScreenShare,
                onChooseQuality = onChooseQuality,
                onToggleLowBandwidth = onToggleLowBandwidth,
                inPictureInPicture = inPictureInPicture,
            )

            else -> CallErrorCard(message = state.callError ?: "", onDismiss = onDismiss)
        }
    }
}

/** The incoming screen: who is calling, what kind of call, and the two honest answers. */
@Composable
private fun IncomingCallScreen(
    state: CallUiState,
    peerName: String,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
) {
    val incoming = state.incoming ?: return
    val kind = mediaKindLabel(CallMediaKind.fromWire(incoming.mediaKind))
    CallStage(
        peerName = peerName,
        status = "Incoming $kind",
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(32.dp)) {
            CallActionButton(
                glyph = "📞",
                label = "Accept $kind",
                background = MaterialTheme.colorScheme.secondary,
                contentColor = Color.White,
                onClick = onAccept,
            )
            CallActionButton(
                glyph = "✕",
                label = "Decline call",
                background = MaterialTheme.colorScheme.error,
                contentColor = Color.White,
                onClick = onDecline,
            )
        }
    }
}

/** The screen every state of a tracked call renders through. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ActiveCallScreen(
    state: CallUiState,
    call: ActiveCall,
    peerName: String,
    localVideo: VideoTrack?,
    remoteVideo: VideoTrack?,
    onCancel: () -> Unit,
    onHangUp: () -> Unit,
    onToggleMute: () -> Unit,
    onToggleCamera: () -> Unit,
    onDismiss: () -> Unit,
    onRateCall: (CallRating, ULong) -> Unit,
    onMinimize: (() -> Unit)?,
    onSwitchCamera: (() -> Unit)?,
    outputs: List<CallManager.AudioOutput>,
    chosenOutput: Int?,
    onChooseOutput: ((Int) -> Unit)?,
    onStartScreenShare: ((Intent) -> Unit)?,
    onStopScreenShare: (() -> Unit)?,
    onChooseQuality: ((LinkQuality?) -> Unit)?,
    onToggleLowBandwidth: ((Boolean) -> Unit)?,
    inPictureInPicture: Boolean,
) {
    // Read into a name of its own because the control row asks the question twice -- whether a
    // share is possible at all, and whether one is running -- and a property of the state cannot
    // be smart cast the way a local can, so the two questions would have to be asked of a nullable
    // value that only one of them has already ruled out.
    val screenSharing = state.screenSharing
    // One tick per second while connected: the duration is the only number on screen that moves.
    var nowMs by remember { mutableLongStateOf(System.currentTimeMillis()) }
    val connected = call.state == CallState.Connected
    LaunchedEffect(connected) {
        if (!connected) {
            return@LaunchedEffect
        }
        // Re-zero on entering connected, so the first shown second is this call's, not the mount's.
        nowMs = System.currentTimeMillis()
        while (true) {
            delay(1_000)
            nowMs = System.currentTimeMillis()
        }
    }

    // Degraded is what the ladder measured, never a guess the screen makes for itself: a video
    // call whose rung fell to the bottom is connected with its video paused, and a voice call on the
    // same rung is a lossy call that gave nothing up.
    val display = displayStateOf(call.state, degraded = state.degraded)
    val durationMs = call.startedAt?.let { (state.endedAt ?: nowMs) - it }

    // The video stage, on the web overlay's own rule: a video call shows it while media is
    // flowing or trying to -- connected, reconnecting, degraded -- and not while ringing,
    // connecting, or ended, where the voice layout says what the call is doing better than a
    // black rectangle would. A video call whose camera never opened still shows the stage once
    // the *peer's* track arrives (the answer path's audio-only fallback), and a voice call
    // never does, whatever the peer managed to send.
    val showVideos = call.mediaKind == CallMediaKind.Video && remoteVideo != null

    // The picture-in-picture window is the size of a thumbnail, so it shows the picture and
    // nothing else. The identity, the status line and every control were drawn for a full screen
    // and would be unreadable here; the system's own window controls expand it or close it, and
    // closing it leaves the call up for the notification and the lock screen to return to. A call
    // in this window that is not sending video -- a voice call minimised by mistake, or a video
    // call whose camera never opened -- falls back to the voice layout's own two facts, because a
    // window that is entirely blank is worse than one that is merely small.
    if (inPictureInPicture) {
        val stage = remoteVideo
        if (showVideos && stage != null) {
            VideoStage(remoteVideo = stage, localVideo = localVideo, peerName = peerName)
        } else {
            CallStage(peerName = peerName, status = callStateLabel(display)) {}
        }
        return
    }

    Box(modifier = Modifier.fillMaxSize()) {
        if (showVideos) {
            VideoStage(
                remoteVideo = remoteVideo,
                localVideo = localVideo,
                peerName = peerName,
            )
        }

        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(32.dp)
                .align(Alignment.Center),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            if (!showVideos) {
                Monogram(name = peerName, size = 88.dp)
                Spacer(modifier = Modifier.height(20.dp))
            }
            Text(
                text = peerName,
                style = MaterialTheme.typography.headlineMedium,
                color = MaterialTheme.colorScheme.onSurface,
                textAlign = TextAlign.Center,
            )
            Spacer(modifier = Modifier.height(8.dp))
            if (display == CallDisplayState.Ended) {
                Text(
                    text = endedReasonLine(call.inviteStatus, call.endReason),
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            } else {
                Text(
                    text = if (call.isCaller && display == CallDisplayState.Ringing) {
                        "Calling…"
                    } else {
                        callStateLabel(display)
                    },
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
            val running =
                display == CallDisplayState.Connected || display == CallDisplayState.Degraded
            if (durationMs != null && running) {
                Text(
                    text = formatCallDuration(durationMs),
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                )
            }
            if (display == CallDisplayState.Ended && durationMs != null) {
                Text(
                    text = formatCallDuration(durationMs),
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // The network indicator, drawn while the call is up and the ladder has a tier for it.
            // The same word for a voice call and a video call, because a lossy link is a fact about
            // the call whether or not a camera is on it; a call the ladder has never moved shows
            // nothing rather than the best tier, since a tier nobody measured is not a tier.
            val quality = state.quality
            if (quality != null && (running || display == CallDisplayState.Reconnecting)) {
                Text(
                    text = qualityTierLabel(quality),
                    fontSize = MigoType.meta,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
        }

        // The actions each state offers, in the web overlay's own order: a ringing caller cancels,
        // a ringing callee hangs up, connecting and reconnecting hang up, connected offers the mute
        // beside the hang-up, and ended offers the way back. They ride the bottom of the screen
        // rather than the center column, so a video call's face is never covered by its buttons.
        // The post-call question rides above the ended screen's own action, on the web overlay's
        // rule: it is asked only of a call that connected, and it is the last thing this screen
        // says before the user leaves it. A call that never connected asked nobody anything.
        if (display == CallDisplayState.Ended && call.canRate) {
            CallRatingQuestion(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(start = 24.dp, end = 24.dp, bottom = 128.dp),
                onRate = onRateCall,
            )
        }

        // The controls wrap rather than clip, the same rule the group chat's header follows and
        // for the same reason: the widest call carries nine of them -- share, camera, switch,
        // minimise, audio route, quality, low bandwidth, mute and hang up -- and nine circles in
        // one line is wider than any phone this draws on. A wrapped row is the whole row; a
        // clipped one silently drops whichever control happened to be last, and which control that
        // is depends on the device. Centring is what keeps a row that does fit reading exactly
        // where it did before the wrapping arrived.
        FlowRow(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .fillMaxWidth()
                .padding(start = 24.dp, end = 24.dp, bottom = 40.dp),
            horizontalArrangement = Arrangement.spacedBy(24.dp, Alignment.CenterHorizontally),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            when (display) {
                CallDisplayState.Ringing -> CallActionButton(
                    glyph = "✕",
                    label = if (call.isCaller) "Cancel call" else "End call",
                    background = MaterialTheme.colorScheme.error,
                    contentColor = Color.White,
                    onClick = if (call.isCaller) onCancel else onHangUp,
                )

                CallDisplayState.Connecting, CallDisplayState.Reconnecting -> CallActionButton(
                    glyph = "✕",
                    label = "End call",
                    background = MaterialTheme.colorScheme.error,
                    contentColor = Color.White,
                    onClick = onHangUp,
                )

                CallDisplayState.Connected, CallDisplayState.Degraded -> {
                    // The share control stands where the camera's do and takes their place while
                    // it is on: a call whose video track is carrying the screen has a camera
                    // button that would stop a camera already stopped and a switch button that
                    // would move a camera nobody is watching, so both are gone for as long as the
                    // screen is on the track and come back with it.
                    if (
                        screenSharing != null &&
                        onStartScreenShare != null &&
                        onStopScreenShare != null
                    ) {
                        ScreenShareButton(
                            sharing = screenSharing,
                            onStart = onStartScreenShare,
                            onStop = onStopScreenShare,
                        )
                    }
                    // The camera button is offered only where the call really has a camera to give
                    // back: `cameraOn` is null for a voice call and for a video call whose camera
                    // never opened, and both of those would get a control that cannot change
                    // anything.
                    if (state.cameraOn != null && screenSharing != true) {
                        CallActionButton(
                            glyph = if (state.cameraOn) "📷" else "🚫",
                            label = if (state.cameraOn) "Turn camera off" else "Turn camera on",
                            background = MaterialTheme.colorScheme.surfaceVariant,
                            contentColor = MaterialTheme.colorScheme.onSurface,
                            onClick = onToggleCamera,
                        )
                    }
                    // The camera switch is offered for a video call on a device that has somewhere
                    // to switch to; a phone with one camera gets no control rather than one that
                    // cannot move.
                    if (
                        onSwitchCamera != null &&
                        call.mediaKind == CallMediaKind.Video &&
                        screenSharing != true
                    ) {
                        CallActionButton(
                            glyph = "🔄",
                            label = "Switch camera",
                            background = MaterialTheme.colorScheme.surfaceVariant,
                            contentColor = MaterialTheme.colorScheme.onSurface,
                            onClick = onSwitchCamera,
                        )
                    }
                    // The minimise control is offered for a video call and only where the device
                    // can actually draw the window it opens: a control that does nothing is worse
                    // than no control. A voice call has no picture to carry into a thumbnail.
                    if (onMinimize != null && call.mediaKind == CallMediaKind.Video) {
                        CallActionButton(
                            glyph = "▭",
                            label = "Minimize call",
                            background = MaterialTheme.colorScheme.surfaceVariant,
                            contentColor = MaterialTheme.colorScheme.onSurface,
                            onClick = onMinimize,
                        )
                    }
                    // The routing menu is offered only where the phone listed more than one route
                    // and the app can act on the list: a phone whose only output is the one it is
                    // already using gets no control rather than a menu with a single row, and a
                    // platform with no honest way to route a call gets none at all. Voice and
                    // video both take it, because where a call is played is not a property of
                    // whether it has a picture.
                    if (onChooseOutput != null && outputs.size > 1) {
                        AudioRouteButton(
                            outputs = outputs,
                            chosenOutput = chosenOutput,
                            onChooseOutput = onChooseOutput,
                        )
                    }
                    // The two quality controls sit beside the routing menu rather than among the
                    // microphone and camera buttons, because they are the same kind of thing: a
                    // property of the call the user may choose, offered on a voice call as well as
                    // a video one. The ceiling is null for automatic, and the mode is offered even
                    // where the ladder has nothing to give up, because on a voice call the audio
                    // cap is the whole of what it can mean.
                    if (onChooseQuality != null && onToggleLowBandwidth != null) {
                        QualityCeilingButton(
                            ceiling = state.qualityCeiling,
                            onChoose = onChooseQuality,
                        )
                        CallActionButton(
                            glyph = if (state.lowBandwidth) "🐢" else "🐇",
                            label = if (state.lowBandwidth) {
                                "Leave low bandwidth mode"
                            } else {
                                "Use low bandwidth mode"
                            },
                            background = MaterialTheme.colorScheme.surfaceVariant,
                            contentColor = MaterialTheme.colorScheme.onSurface,
                            onClick = { onToggleLowBandwidth(!state.lowBandwidth) },
                        )
                    }
                    CallActionButton(
                        glyph = if (state.muted) "🔇" else "🎙️",
                        label = if (state.muted) "Unmute microphone" else "Mute microphone",
                        background = MaterialTheme.colorScheme.surfaceVariant,
                        contentColor = MaterialTheme.colorScheme.onSurface,
                        onClick = onToggleMute,
                    )
                    CallActionButton(
                        glyph = "✕",
                        label = "End call",
                        background = MaterialTheme.colorScheme.error,
                        contentColor = Color.White,
                        onClick = onHangUp,
                    )
                }

                CallDisplayState.Ended -> TextButton(onClick = onDismiss) {
                    Text("Back to chats")
                }
            }
        }
    }
}

/**
 * The video stage: the peer's camera full-bleed, this side's as a small self-view in the corner.
 *
 * Each surface is a [SurfaceViewRenderer] bound to its track for as long as both exist. The
 * renderers are released on dispose, because a leaked renderer pins the GL context the whole
 * session's video shares; the track side is the manager's to dispose, and a track outliving its
 * surface is only ever a few frames rendered nowhere.
 */
@Composable
private fun VideoStage(
    remoteVideo: VideoTrack,
    localVideo: VideoTrack?,
    peerName: String,
) {
    Box(modifier = Modifier.fillMaxSize()) {
        VideoSurface(
            track = remoteVideo,
            modifier = Modifier.fillMaxSize(),
            contentDescription = "$peerName's video",
        )
        if (localVideo != null) {
            VideoSurface(
                track = localVideo,
                // The web overlay's own corner: small, portrait, and above the actions' row.
                modifier = Modifier
                    .align(Alignment.BottomEnd)
                    .padding(end = 12.dp, bottom = 140.dp)
                    .size(width = 120.dp, height = 160.dp)
                    .clip(RoundedCornerShape(MigoRadius.md)),
                contentDescription = "Your video",
            )
        }
    }
}

/** One renderer bound to one track, released when either leaves the composition. */
@Composable
private fun VideoSurface(
    track: VideoTrack,
    modifier: Modifier = Modifier,
    contentDescription: String,
) {
    // The GL context the renderer initializes with, handed down from the call manager's own
    // session-scoped one so every video surface in the app shares it.
    val glContext = LocalCallEglContext.current
    // Keyed on the track: a new call's track gets a fresh surface rather than a renderer still
    // bound to the old one.
    key(track) {
        AndroidView(
            modifier = modifier.semantics { this.contentDescription = contentDescription },
            factory = { ctx ->
                SurfaceViewRenderer(ctx).apply {
                    init(glContext, null)
                    setScalingType(RendererCommon.ScalingType.SCALE_ASPECT_FIT)
                }
            },
            update = { view ->
                // The guard keeps `update` idempotent: recomposition re-runs this block, and a
                // sink added twice to the same track is an exception, not a no-op.
                if (view.tag == null) {
                    view.tag = true
                    track.addSink(view)
                }
            },
            onRelease = { view ->
                runCatching { track.removeSink(view) }
                view.release()
            },
        )
    }
}

/**
 * The small card for a call that could not even be placed: the fact, and a way past it. The
 * missed-call note and the answered-elsewhere note land here too -- a notice about a call that
 * ended elsewhere, not a placement failure -- and the words themselves keep the two facts apart.
 */
@Composable
private fun CallErrorCard(message: String, onDismiss: () -> Unit) {
    CallStage(peerName = null, status = message) {
        TextButton(onClick = onDismiss) {
            Text("Close")
        }
    }
}

/**
 * The shared spine of the incoming and notice screens: the avatar, the name, and the status line,
 * stacked center-screen with the actions below.
 */
@Composable
private fun CallStage(
    peerName: String?,
    status: String,
    actions: @Composable () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        if (peerName != null) {
            Monogram(name = peerName, size = 88.dp)
            Spacer(modifier = Modifier.height(20.dp))
            Text(
                text = peerName,
                style = MaterialTheme.typography.headlineMedium,
                color = MaterialTheme.colorScheme.onSurface,
                textAlign = TextAlign.Center,
            )
            Spacer(modifier = Modifier.height(8.dp))
        }
        Text(
            text = status,
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        Spacer(modifier = Modifier.height(48.dp))
        actions()
    }
}

/**
 * One circular action button: a glyph on a colored disc. The glyphs are emoji characters, not an
 * icon font -- the app's own rule, and the web overlay's -- so a call screen ships with no asset
 * of its own. The [label] never shows on screen; it is what a screen reader says.
 */
@Composable
private fun CallActionButton(
    glyph: String,
    label: String,
    background: Color,
    contentColor: Color,
    onClick: () -> Unit,
) {
    Box(
        modifier = Modifier
            .size(72.dp)
            .clip(CircleShape)
            .background(background)
            .clickable(onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Text(text = glyph, color = contentColor, fontSize = MigoGlyph.control)
    }
}

/**
 * Where the call is played, as a menu of the routes the phone listed.
 *
 * Drawn in the same circle as the neighbouring controls rather than as a text button, because the
 * row is a row of round controls and a labelled button beside them would read as belonging to a
 * different bar. The tick marks the route the call is on, and no tick is a real state rather than a
 * missing one: the app has not routed the call anywhere itself, so the phone is choosing, and
 * ticking a device on a guess would tell the user their sound is somewhere it may not be.
 */
@Composable
private fun AudioRouteButton(
    outputs: List<CallManager.AudioOutput>,
    chosenOutput: Int?,
    onChooseOutput: (Int) -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    Box(contentAlignment = Alignment.Center) {
        CallActionButton(
            glyph = "🔊",
            label = "Call audio",
            background = MaterialTheme.colorScheme.surfaceVariant,
            contentColor = MaterialTheme.colorScheme.onSurface,
            onClick = { open = true },
        )
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            outputs.forEach { output ->
                val marked = if (output.id == chosenOutput) "✓ ${output.label}" else output.label
                DropdownMenuItem(
                    text = { Text(marked) },
                    onClick = {
                        onChooseOutput(output.id)
                        open = false
                    },
                )
            }
        }
    }
}

/**
 * The rung the user pins the call to, as a menu of the tiers it may be held at.
 *
 * A ceiling and never a floor, so a call pinned high still descends if its link does; what the pin
 * buys is that it never rises above the chosen rung, which is what somebody on a metered or thin
 * link is asking for. Automatic is the absence of a pin rather than a rung of its own, which is why
 * it is its own row and not the top tier: a call with nothing pinned sits wherever the ladder
 * measures, and that is not always the top.
 *
 * The bottom rung is deliberately not offered. A user who wants no video has the camera button,
 * which says so plainly; a ceiling of "video off" would instead put the call into Degraded, a state
 * that means video paused because the quality dropped -- and a sacrifice the user chose is not a
 * drop, so offering it here would make the screen say something untrue about why the camera is off.
 */
@Composable
private fun QualityCeilingButton(
    ceiling: LinkQuality?,
    onChoose: (LinkQuality?) -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    Box(contentAlignment = Alignment.Center) {
        CallActionButton(
            glyph = "📶",
            label = "Call quality",
            background = MaterialTheme.colorScheme.surfaceVariant,
            contentColor = MaterialTheme.colorScheme.onSurface,
            onClick = { open = true },
        )
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            DropdownMenuItem(
                text = { Text(if (ceiling == null) "✓ Automatic" else "Automatic") },
                onClick = {
                    onChoose(null)
                    open = false
                },
            )
            QUALITY_CEILINGS.forEach { rung ->
                val label = qualityTierLabel(rung)
                DropdownMenuItem(
                    text = { Text(if (rung == ceiling) "✓ $label" else label) },
                    onClick = {
                        onChoose(rung)
                        open = false
                    },
                )
            }
        }
    }
}

/**
 * The screen-share control: it asks the platform for permission to record the display and hands
 * the granted projection to the call.
 *
 * The asking happens here rather than in the activity for the same reason the button does: the
 * consent belongs to the moment somebody presses it, and a permission prompt that opened on its
 * own -- at a call's start, say -- would be asking about a screen the user has not offered yet.
 * The platform answers on its own activity, which is why this is a launcher and not a call: a
 * launch that comes back refused, or with no projection at all, leaves the share where it was,
 * since a share nobody consented to is not a share this app can build.
 *
 * The stop side takes no permission and shows no prompt: ending a share is always allowed, and
 * leaving is the one thing a control like this must never be able to refuse.
 */
@Composable
private fun ScreenShareButton(
    sharing: Boolean,
    onStart: (Intent) -> Unit,
    onStop: () -> Unit,
) {
    val context = LocalContext.current
    val picker = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) { answer ->
        val projection = answer.data
        if (answer.resultCode == Activity.RESULT_OK && projection != null) {
            onStart(projection)
        }
    }
    CallActionButton(
        glyph = if (sharing) "🛑" else "🖥️",
        label = if (sharing) "Stop sharing screen" else "Share screen",
        background = if (sharing) {
            MaterialTheme.colorScheme.error
        } else {
            MaterialTheme.colorScheme.surfaceVariant
        },
        contentColor = if (sharing) Color.White else MaterialTheme.colorScheme.onSurface,
        onClick = {
            if (sharing) {
                onStop()
            } else {
                val manager = context.getSystemService(Context.MEDIA_PROJECTION_SERVICE)
                    as? MediaProjectionManager
                if (manager != null) {
                    picker.launch(manager.createScreenCaptureIntent())
                }
            }
        },
    )
}

/**
 * The four verdicts, in the order the wire numbers them, so the list a reader compares against
 * `CallRating` is the list on screen. `Unknown` is deliberately absent: it is what a build that
 * does not recognise a value decodes to, and a user cannot choose not to know.
 */
private val RATING_CHOICES: List<Pair<CallRating, String>> = listOf(
    CallRating.Excellent to "Excellent",
    CallRating.Good to "Good",
    CallRating.Average to "Average",
    CallRating.Poor to "Poor",
)

/**
 * What went wrong, as the bitmask the wire carries: bit 0 audio, 1 video, 2 connection, 3 dropped.
 * None of them is exclusive and none is required, because a call can be rated excellent and still
 * have dropped once, and a user with nothing to report should be able to say so by saying nothing.
 */
private val RATING_ISSUES: List<Pair<ULong, String>> = listOf(
    1uL to "Audio",
    2uL to "Video",
    4uL to "Connection",
    8uL to "Dropped",
)

/**
 * The post-call question: how was the call, and optionally what went wrong.
 *
 * Asked once the call has ended and only of a call that connected. Nothing is sent until the user
 * taps send, so a user who swipes the screen away sends no verdict at all -- an absent verdict
 * means "did not rate", which is a different fact from a neutral one and must stay that way in the
 * aggregate. The issue row appears only after a verdict is picked, because a question about what
 * went wrong is a strange thing to ask somebody who has not said anything did.
 */
@Composable
private fun CallRatingQuestion(
    modifier: Modifier = Modifier,
    onRate: (CallRating, ULong) -> Unit,
) {
    var rating by remember { mutableStateOf<CallRating?>(null) }
    var issues by remember { mutableStateOf(0uL) }
    var sent by remember { mutableStateOf(false) }

    Surface(
        modifier = modifier,
        color = MaterialTheme.colorScheme.surfaceVariant,
        shape = RoundedCornerShape(MigoRadius.md),
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 14.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text(
                text = if (sent) "Thanks. Your rating is on its way." else "How was this call?",
                style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.onSurface,
                textAlign = TextAlign.Center,
            )
            if (!sent) {
                Spacer(Modifier.height(10.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    for ((choice, label) in RATING_CHOICES) {
                        RatingChip(
                            label = label,
                            selected = rating == choice,
                            onClick = { rating = choice },
                        )
                    }
                }
            }
            val chosen = rating
            if (!sent && chosen != null) {
                Spacer(Modifier.height(12.dp))
                Text(
                    text = "Anything go wrong? Optional.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
                Spacer(Modifier.height(8.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    for ((bit, label) in RATING_ISSUES) {
                        RatingChip(
                            label = label,
                            selected = issues and bit != 0uL,
                            onClick = { issues = issues xor bit },
                        )
                    }
                }
                Spacer(Modifier.height(4.dp))
                TextButton(onClick = {
                    sent = true
                    onRate(chosen, issues)
                }) {
                    Text("Send rating")
                }
                Text(
                    text = "Only these answers leave your device, never anything that was said.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
        }
    }
}

/** One selectable answer: a label whose fill says whether it is on, in the privacy rows' own shape. */
@Composable
private fun RatingChip(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
) {
    val fill = if (selected) {
        MaterialTheme.colorScheme.primary
    } else {
        MaterialTheme.colorScheme.surface
    }
    val ink = if (selected) {
        MaterialTheme.colorScheme.onPrimary
    } else {
        MaterialTheme.colorScheme.onSurface
    }
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(MigoRadius.md))
            .background(fill)
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text = label, color = ink, style = MaterialTheme.typography.labelLarge)
    }
}
