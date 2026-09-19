package com.migo.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.migo.app.call.ActiveGroupCall
import com.migo.app.call.AudioOutput
import com.migo.app.call.GroupCallPhase
import com.migo.app.call.GroupCallUiState
import com.migo.app.call.GroupLinkState
import com.migo.app.call.qualityTierLabel
import com.migo.core.domain.formatCallDuration
import com.migo.core.domain.groupCallNoteLabel
import com.migo.core.domain.mediaKindLabel
import com.migo.core.wire.Id
import kotlinx.coroutines.delay
import org.webrtc.EglBase
import org.webrtc.VideoTrack

/**
 * The group call's own video GL context, provided by the host that composes this overlay.
 *
 * Separate from [LocalCallEglContext] because a group call is a second WebRTC engine with an EGL
 * context of its own: a renderer initialized against the one-to-one plane's would be drawing
 * through a context its tracks were never minted on.
 */
val LocalGroupEglContext = staticCompositionLocalOf<EglBase.Context?> { null }

/**
 * The group-call screen: the roster, the tiles, and the words for every way the call can stop.
 *
 * A port of `clients/web/src/components/group-call-overlay.tsx`. It renders over the whole shell
 * while this device holds a group-call seat (or shows why it lost one), and nothing at all
 * otherwise -- the composable's absence *is* the "no group call" state.
 *
 * # The states, and why each names itself
 *
 * Section 180's rule for 1:1 calls is the rule here: a call screen that goes silent without a
 * sentence is a screen its user closes and distrusts. So *Joining…* while the seat is requested,
 * the participant count once seated, and -- when the call stops -- one of four distinct notes: a
 * leave, the call's retirement, the seat continuing on this account's other device, or a lost
 * session. None of them collapses into "call ended", because they are different facts about what
 * the user should do next.
 *
 * # The media, and what each line says about it
 *
 * The call is a mesh ([com.migo.app.call.GroupMediaPlane]), so this screen draws one tile per link
 * -- a seat whose camera is off is a monogram, a seat that is sending one is its picture -- beside
 * a self-view of this device's own camera. Every tile carries its link's own state rather than the
 * call's: a mesh has one transport per peer, and a screen that said "connected" for a call would be
 * saying something true of no particular link. A seat that is still negotiating says so, and a seat
 * whose link has produced a measurement says which rung it is on -- the same ladder, and the same
 * words, the one-to-one call screen uses.
 *
 * # Back
 *
 * The back gesture is handled where it means something, the shell's own rule: back dismisses an
 * ended screen or a failure card, and on a live seat is consumed without acting -- a pocket
 * gesture must never hang up on the whole group.
 */
@Composable
fun GroupCallOverlay(
    state: GroupCallUiState,
    names: (Id) -> String,
    meId: Id?,
    /** One entry per other seat, as the mesh has them. Empty until the first link is built. */
    links: List<GroupLinkState>,
    /** This device's own camera track, for the self-view tile; null when the camera is off. */
    localVideo: VideoTrack?,
    onLeave: () -> Unit,
    onDismiss: () -> Unit,
    onToggleMute: () -> Unit,
    onToggleCamera: () -> Unit,
    /** Null where the device has one camera, so the control is absent rather than inert. */
    onSwitchCamera: (() -> Unit)?,
    /**
     * The routes this phone offers the call right now, and the one it is on.
     *
     * The same pair the one-to-one screen takes, read from the same layer
     * ([com.migo.app.call.CallAudioRoute]): a route is a property of the phone carrying a call, and
     * this call is carried by this phone. Empty below Android 12, which is why the control is drawn
     * only above one entry rather than as an empty menu.
     */
    outputs: List<AudioOutput>,
    chosenOutput: Int?,
    onChooseOutput: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    val call = state.call

    if (call == null && state.error == null) {
        return
    }

    BackHandler(enabled = true, onBack = {
        when {
            // A live seat: the gesture is consumed, never acted on.
            call != null && call.note == null -> Unit
            else -> onDismiss()
        }
    })

    Surface(
        modifier = modifier.fillMaxSize(),
        color = MaterialTheme.colorScheme.background,
    ) {
        when (call) {
            null -> GroupCallErrorCard(message = state.error ?: "", onDismiss = onDismiss)
            else -> GroupCallScreen(
                call = call,
                names = names,
                meId = meId,
                links = links,
                localVideo = localVideo,
                muted = state.muted,
                cameraOn = state.cameraOn,
                cameraAvailable = state.cameraAvailable,
                onLeave = onLeave,
                onDismiss = onDismiss,
                onToggleMute = onToggleMute,
                onToggleCamera = onToggleCamera,
                onSwitchCamera = onSwitchCamera,
                outputs = outputs,
                chosenOutput = chosenOutput,
                onChooseOutput = onChooseOutput,
            )
        }
    }
}

/**
 * The screen every state of a tracked group call renders through, pure: strings in, layout out,
 * and every callback already the manager's. The roster is shown in join order for every phase --
 * a joining screen simply has an empty list -- and the actions follow the phase: a hang-up while
 * the seat is live, a way back to the app once a note has replaced it.
 */
@Composable
private fun GroupCallScreen(
    call: ActiveGroupCall,
    names: (Id) -> String,
    meId: Id?,
    links: List<GroupLinkState>,
    localVideo: VideoTrack?,
    muted: Boolean,
    cameraOn: Boolean,
    cameraAvailable: Boolean,
    onLeave: () -> Unit,
    onDismiss: () -> Unit,
    onToggleMute: () -> Unit,
    onToggleCamera: () -> Unit,
    onSwitchCamera: (() -> Unit)?,
    outputs: List<AudioOutput>,
    chosenOutput: Int?,
    onChooseOutput: (Int) -> Unit,
) {
    val glContext = LocalGroupEglContext.current
    // One tick per second while seated: the duration is the only number on screen that moves.
    var nowMs by remember { mutableLongStateOf(System.currentTimeMillis()) }
    val live = call.note == null
    val seated = live && call.phase == GroupCallPhase.Seated
    LaunchedEffect(seated) {
        if (!seated) {
            return@LaunchedEffect
        }
        // Re-zero on entering seated, so the first shown second is this call's, not the mount's.
        nowMs = System.currentTimeMillis()
        while (true) {
            delay(1_000)
            nowMs = System.currentTimeMillis()
        }
    }

    Box(modifier = Modifier.fillMaxSize()) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(32.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            // Built outside the string template: the title is two facts joined -- the kind this
            // build names itself with ("Group") and the label the core derives from the kind.
            val title = "Group " + mediaKindLabel(call.mediaKind)
            Text(
                text = title,
                style = MaterialTheme.typography.headlineMedium,
                color = MaterialTheme.colorScheme.onSurface,
                textAlign = TextAlign.Center,
            )
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                text = when {
                    call.note != null -> groupCallNoteLabel(call.note)
                    call.phase == GroupCallPhase.Joining -> "Joining…"
                    else -> "${call.participantCount} in this call"
                },
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            if (seated && call.joinedAt != null) {
                Text(
                    text = formatCallDuration(nowMs - call.joinedAt),
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                )
            }
            if (seated && links.isNotEmpty() && links.none { it.connected || it.quality != null }) {
                Spacer(modifier = Modifier.height(4.dp))
                Text(
                    // Said only while it is the whole truth: every seat is a link, every link is
                    // still negotiating, and a screen that stayed silent here would look like one
                    // that had connected and found nothing to draw.
                    text = "Connecting to ${links.size} other seat${if (links.size == 1) "" else "s"}…",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }

            Spacer(modifier = Modifier.height(32.dp))

            // The roster, in join order, one line per account -- and, beside each seat, the state
            // of *its* link and its picture if it is sending one. Scrollable rather than capped: a
            // group call's whole point is that the count is not two, and a roster that silently
            // truncated would be the screen lying about who is on it.
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                // This device's own line heads the roster, and it is the one line drawn from the
                // camera rather than from a link -- there is no link to oneself, so the roster's own
                // entry would otherwise read "Connecting…" for the whole call. A monogram stands in
                // while the camera is off, which is the same tile every other seat without a
                // picture gets; drawing the picture's frame empty would claim a camera that is off.
                val selfName = if (meId != null) names(meId) else null
                if (seated && selfName != null) {
                    GroupSeatTile(
                        name = selfName,
                        isSelf = true,
                        video = if (cameraOn) localVideo else null,
                        // This seat's own state is never a link's: the mesh's links are between
                        // devices, and a self-view that reported one would be reporting a transport
                        // it does not have.
                        status = null,
                        glContext = glContext,
                    )
                }
                for (seat in call.seats) {
                    // Skipped where it is this device's own seat: it is the line above, drawn from
                    // the camera, and two lines for one person would be the roster miscounting.
                    if (meId != null && seat.userId == meId) {
                        continue
                    }
                    val link = links.firstOrNull { it.deviceId == seat.deviceId }
                    GroupSeatTile(
                        name = names(seat.userId),
                        isSelf = false,
                        video = link?.video,
                        status = linkStatus(link = link, live = live),
                        glContext = glContext,
                    )
                }
            }
        }

        // The actions each phase offers: a hang-up while the seat is live (the words differ for a
        // join still in flight -- canceling a request is not leaving a call), and the way back
        // once a note has replaced the seat.
        Row(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .padding(bottom = 48.dp),
            horizontalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            if (live) {
                // The microphone is on every call, so its control is too. The camera is not: a call
                // past the product's stream limit negotiated no video line, and a control that
                // could not send anything is absent rather than present and inert.
                GroupCallActionButton(
                    glyph = if (muted) "🔇" else "🎙",
                    label = if (muted) "Unmute your microphone" else "Mute your microphone",
                    background = if (muted) {
                        MaterialTheme.colorScheme.surfaceVariant
                    } else {
                        MaterialTheme.colorScheme.primary
                    },
                    contentColor = if (muted) {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    } else {
                        MaterialTheme.colorScheme.onPrimary
                    },
                    onClick = onToggleMute,
                )
                if (cameraAvailable && call.phase == GroupCallPhase.Seated) {
                    GroupCallActionButton(
                        glyph = if (cameraOn) "📹" else "📷",
                        label = if (cameraOn) "Turn your camera off" else "Turn your camera on",
                        background = if (cameraOn) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.surfaceVariant
                        },
                        contentColor = if (cameraOn) {
                            MaterialTheme.colorScheme.onPrimary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                        onClick = onToggleCamera,
                    )
                    if (cameraOn && onSwitchCamera != null) {
                        GroupCallActionButton(
                            glyph = "🔄",
                            label = "Switch camera",
                            background = MaterialTheme.colorScheme.surfaceVariant,
                            contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                            onClick = onSwitchCamera,
                        )
                    }
                }
                // The routing menu keeps the one-to-one screen's own rule -- offered only where the
                // phone listed more than one route -- and sits after the camera controls rather
                // than among them, because where the call is played is not a property of what it
                // carries: a voice seat gets it exactly as a video one does.
                if (outputs.size > 1) {
                    AudioRouteButton(
                        outputs = outputs,
                        chosenOutput = chosenOutput,
                        onChooseOutput = onChooseOutput,
                        size = 64.dp,
                    )
                }
                GroupCallActionButton(
                    glyph = "✕",
                    label = if (call.phase == GroupCallPhase.Joining) {
                        "Cancel joining the call"
                    } else {
                        "Leave the call"
                    },
                    background = MaterialTheme.colorScheme.error,
                    contentColor = Color.White,
                    onClick = onLeave,
                )
            } else {
                TextButton(onClick = onDismiss) {
                    Text("Back to chats")
                }
            }
        }
    }
}

/**
 * One seat's line: its picture where it is sending one, its monogram where it is not, its name,
 * and the state of *its* link.
 *
 * The state is the link's own and never the call's. A mesh has one transport per peer, so a seat
 * can be connected while another is still negotiating, and a line that reported the call's state
 * would be reporting a fact about no particular link. The words are the ones the one-to-one screen
 * already uses for the same ladder ([qualityTierLabel]), because a rung is a rung whether or not a
 * third person is on the other end of it.
 *
 * A link that has produced no measurement yet says so rather than claiming a rung: the ladder is a
 * measurement, and a tile that opened on "Full" would be claiming a link quality nobody measured.
 */
@Composable
private fun GroupSeatTile(
    name: String,
    isSelf: Boolean,
    video: VideoTrack?,
    status: String?,
    glContext: EglBase.Context?,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (video != null) {
            VideoSurface(
                track = video,
                glContext = glContext,
                modifier = Modifier
                    .size(width = 96.dp, height = 72.dp)
                    .clip(RoundedCornerShape(MigoRadius.md)),
                contentDescription = "$name's video",
            )
        } else {
            Box(
                modifier = Modifier.size(width = 96.dp, height = 72.dp),
                contentAlignment = Alignment.Center,
            ) {
                Monogram(name = name, size = 32.dp)
            }
        }
        Spacer(modifier = Modifier.width(12.dp))
        Column {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = name,
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                if (isSelf) {
                    Spacer(modifier = Modifier.width(8.dp))
                    Text(
                        text = "You",
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            // Null where there is nothing honest to say: a note has ended the screen and the links
            // went with it, and this seat is the one with no link at all.
            if (status != null) {
                Text(
                    text = status,
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/**
 * What one seat's line says about its link, or null where it should say nothing.
 *
 * The words are the ones the one-to-one screen already uses for the same ladder
 * ([qualityTierLabel]), because a rung is a rung whether or not a third person is on the other end
 * of it. A link that has produced no measurement says "Connected" rather than naming a rung: a tile
 * that opened on "Full" would be claiming a link quality nobody had measured. A link that is not
 * there yet -- the seat arrived a moment ago -- says so instead of claiming one either way.
 *
 * Null once the call has a note: the links were torn down with the seat, so there is no link state
 * left to report, and a line about a connection that is gone is the sort of thing a screen should
 * not say.
 */
private fun linkStatus(link: GroupLinkState?, live: Boolean): String? = when {
    !live -> null
    link == null || !link.connected -> "Connecting…"
    link.quality == null -> "Connected"
    else -> qualityTierLabel(link.quality)
}

/**
 * The small card for a group call that could not even be joined: the fact, and a way past it.
 * The same shape as the call surface's own error card, because it is the same kind of fact -- a
 * placement that failed, not a call that ended.
 */
@Composable
private fun GroupCallErrorCard(message: String, onDismiss: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize().padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            text = "Group call failed",
            style = MaterialTheme.typography.headlineSmall,
            color = MaterialTheme.colorScheme.onSurface,
            textAlign = TextAlign.Center,
        )
        Spacer(modifier = Modifier.height(8.dp))
        Text(
            text = message,
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        Spacer(modifier = Modifier.height(24.dp))
        TextButton(onClick = onDismiss) {
            Text("Close")
        }
    }
}

/**
 * This screen's own control disc: the shared one at the size this overlay uses.
 *
 * The size is the whole difference and it is deliberate. The one-to-one call's control row is the
 * only row on its screen and takes the larger disc; this overlay holds a roster, a column of tiles
 * and a row of controls at once, and the denser disc is what keeps that row from crowding the
 * tiles above it. Everything else -- the glyph, the label a screen reader reads, the disc itself --
 * is the same control, because it is the same product.
 */
@Composable
private fun GroupCallActionButton(
    glyph: String,
    label: String,
    background: Color,
    contentColor: Color,
    onClick: () -> Unit,
) {
    ActionDisc(
        glyph = glyph,
        label = label,
        background = background,
        contentColor = contentColor,
        onClick = onClick,
        size = 64.dp,
    )
}
