package com.migo.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import com.migo.app.call.ActiveCall
import com.migo.app.call.CallUiState
import com.migo.core.domain.CallDisplayState
import com.migo.core.domain.CallMediaKind
import com.migo.core.domain.CallState
import com.migo.core.domain.callStateLabel
import com.migo.core.domain.displayStateOf
import com.migo.core.domain.endedReasonLine
import com.migo.core.domain.formatCallDuration
import com.migo.core.domain.mediaKindLabel
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
 * transport blips, *Degraded* while quality holds media back (always false in this build,
 * but the plumbing lands in [displayStateOf]), and *Ended* always with the reason -- a declined
 * call, a failed call, and a network death are different facts, and calling them all "Call ended"
 * throws away the one thing the user needs before calling back.
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
    onDismiss: () -> Unit,
    localVideo: VideoTrack?,
    remoteVideo: VideoTrack?,
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
                localVideo = localVideo,
                remoteVideo = remoteVideo,
                onCancel = onCancel,
                onHangUp = onHangUp,
                onToggleMute = onToggleMute,
                onDismiss = onDismiss,
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
    onDismiss: () -> Unit,
) {
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

    // Degraded is always false in this build; the sixth state's plumbing is here for the
    // statistics feed that will flip it.
    val display = displayStateOf(call.state, degraded = false)
    val durationMs = call.startedAt?.let { (state.endedAt ?: nowMs) - it }

    // The video stage, on the web overlay's own rule: a video call shows it while media is
    // flowing or trying to -- connected, reconnecting, degraded -- and not while ringing,
    // connecting, or ended, where the voice layout says what the call is doing better than a
    // black rectangle would. A video call whose camera never opened still shows the stage once
    // the *peer's* track arrives (the answer path's audio-only fallback), and a voice call
    // never does, whatever the peer managed to send.
    val showVideos = call.mediaKind == CallMediaKind.Video && remoteVideo != null

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
        }

        // The actions each state offers, in the web overlay's own order: a ringing caller cancels,
        // a ringing callee hangs up, connecting and reconnecting hang up, connected offers the mute
        // beside the hang-up, and ended offers the way back. They ride the bottom of the screen
        // rather than the center column, so a video call's face is never covered by its buttons.
        Row(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .padding(bottom = 48.dp),
            horizontalArrangement = Arrangement.spacedBy(32.dp),
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
                    .clip(RoundedCornerShape(8.dp)),
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
        Text(text = glyph, color = contentColor, fontSize = 26.sp)
    }
}
