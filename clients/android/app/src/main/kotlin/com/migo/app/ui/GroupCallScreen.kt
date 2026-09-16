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
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.migo.app.call.ActiveGroupCall
import com.migo.app.call.GroupCallPhase
import com.migo.app.call.GroupCallUiState
import com.migo.core.domain.formatCallDuration
import com.migo.core.domain.groupCallNoteLabel
import com.migo.core.domain.mediaKindLabel
import com.migo.core.wire.Id
import kotlinx.coroutines.delay

/**
 * The group-call screen: the roster, the count, and the words for every way the call can stop.
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
 * # No media, said honestly
 *
 * This build carries the roster, not the media plane. The screen renders avatars and names and
 * says so in one dim line -- a "voice call" that silently played nothing would be the interface
 * lying about what it did.
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
    onLeave: () -> Unit,
    onDismiss: () -> Unit,
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
                onLeave = onLeave,
                onDismiss = onDismiss,
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
    onLeave: () -> Unit,
    onDismiss: () -> Unit,
) {
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
            if (seated) {
                Spacer(modifier = Modifier.height(4.dp))
                Text(
                    text = "Roster only in this build — media arrives later.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }

            Spacer(modifier = Modifier.height(32.dp))

            // The roster, in join order, one line per account. Scrollable rather than capped: a
            // group call's whole point is that the count is not two, and a roster that silently
            // truncated would be the screen lying about who is on it.
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                for (seat in call.seats) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Monogram(name = names(seat.userId), size = 32.dp)
                        Spacer(modifier = Modifier.width(12.dp))
                        Text(
                            text = names(seat.userId),
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.onSurface,
                        )
                        if (meId != null && seat.userId == meId) {
                            Spacer(modifier = Modifier.width(8.dp))
                            Text(
                                text = "You",
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
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
            horizontalArrangement = Arrangement.spacedBy(32.dp),
        ) {
            if (live) {
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
 * One circular action button: a glyph on a colored disc, the call surface's own shape. The glyphs
 * are emoji characters, not an icon font -- the app's own rule, and the web overlay's -- so this
 * screen ships with no asset of its own. The [label] never shows on screen; it is what a screen
 * reader says.
 */
@Composable
private fun GroupCallActionButton(
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
