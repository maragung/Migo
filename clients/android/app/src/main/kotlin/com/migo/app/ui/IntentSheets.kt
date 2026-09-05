package com.migo.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.migo.app.model.RoomLiveInfo
import com.migo.core.protocol.RoomSummary
import com.migo.core.wire.Id

/** The person a user-intent sheet was opened for: who they are, and whether they are a friend. */
data class UserTarget(
    val userId: Id,
    val name: String,
    val friend: Boolean,
)

/**
 * The bottom sheet the mobile reference opens for everything secondary: 18dp top corners, the drag
 * handle, and a title row whose X is the way out alongside the scrim and the back gesture, which
 * the sheet component carries itself.
 *
 * One shape, three residents: the user intent, the room intent, the me sheet, and the strip's
 * reopen list. Everything is a title, a body, and a dismissal — so everything is one component
 * here rather than four.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MigoSheet(
    title: String,
    onDismiss: () -> Unit,
    content: @Composable () -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        shape = RoundedCornerShape(topStart = 18.dp, topEnd = 18.dp),
        containerColor = MaterialTheme.colorScheme.surface,
    ) {
        Column(modifier = Modifier.fillMaxWidth()) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(start = 16.dp, top = 2.dp, end = 10.dp, bottom = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = title,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.onSurface,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                Box(
                    modifier = Modifier
                        .size(32.dp)
                        .background(
                            MaterialTheme.colorScheme.surfaceVariant,
                            RoundedCornerShape(9.dp),
                        )
                        .clickable(onClick = onDismiss),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = "✕",
                        fontSize = 14.sp,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            content()
        }
    }
}

/**
 * One sheet action row: a 36dp glyph chip, the label, an optional line beneath it, and the chevron.
 * 54dp tall minimum, the reference's height, so the rows thumb-hit as easily as they read.
 */
@Composable
fun SheetAction(
    glyph: String,
    label: String,
    sub: String? = null,
    danger: Boolean = false,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    val scheme = MaterialTheme.colorScheme
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 54.dp)
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            modifier = Modifier
                .size(36.dp)
                .background(
                    if (danger) scheme.errorContainer else scheme.surfaceVariant,
                    RoundedCornerShape(10.dp),
                ),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text = glyph,
                fontSize = 16.sp,
                color = if (danger) scheme.onErrorContainer else scheme.onSurfaceVariant,
            )
        }
        Spacer(modifier = Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = label,
                style = MaterialTheme.typography.bodyLarge,
                fontWeight = FontWeight.SemiBold,
                color = if (danger) scheme.error else scheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (sub != null) {
                Text(
                    text = sub,
                    style = MaterialTheme.typography.bodySmall,
                    color = if (enabled) scheme.onSurfaceVariant else scheme.outline,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Text(text = "›", fontSize = 18.sp, color = LocalMigoExtra.current.faint)
    }
}

/**
 * The sheet's primary act: the orange, 48dp-tall, full-width button the reference reserves for the
 * one thing the sheet exists to do.
 */
@Composable
fun SheetPrimaryAction(
    label: String,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    Button(
        onClick = onClick,
        enabled = enabled,
        shape = RoundedCornerShape(12.dp),
        colors = ButtonDefaults.buttonColors(
            containerColor = MaterialTheme.colorScheme.tertiary,
            contentColor = MaterialTheme.colorScheme.onTertiary,
        ),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 14.dp, vertical = 6.dp)
            .heightIn(min = 48.dp),
    ) {
        Text(
            text = label,
            fontWeight = FontWeight.Bold,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/**
 * The friend intent sheet: what tapping a friend anywhere opens. Send message is the act, primary
 * and orange; the row beneath is the relationship's other door.
 *
 * The reference's second row is "Remove from friends", but the wire has no unfriend: blocking is
 * the one call that ends a friendship, and it ends it the heavy way — no messages either way,
 * either direction. The row says what it does rather than wearing the lighter label.
 */
@Composable
fun UserIntentSheet(
    target: UserTarget?,
    busy: Boolean,
    onDismiss: () -> Unit,
    onSend: (UserTarget) -> Unit,
    onAdd: (UserTarget) -> Unit,
    onBlock: (UserTarget) -> Unit,
) {
    if (target == null) return
    MigoSheet(title = target.name, onDismiss = onDismiss) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            ListRowAvatar(name = target.name, online = target.friend)
            Spacer(modifier = Modifier.width(10.dp))
            Column {
                ListRowName(text = target.name)
                ListRowLine(
                    text = if (target.friend) {
                        "Friend"
                    } else {
                        "Not a friend yet — messages open a direct chat either way"
                    },
                )
            }
        }
        SheetPrimaryAction(
            label = "Send message",
            enabled = !busy,
            onClick = { onSend(target) },
        )
        if (target.friend) {
            SheetAction(
                glyph = "⊘",
                label = "Remove & block",
                sub = "Blocking also ends the friendship — there is no lighter unfriend",
                danger = true,
                enabled = !busy,
                onClick = { onBlock(target) },
            )
        } else {
            SheetAction(
                glyph = "+",
                label = "Add to friends",
                sub = "Sends a friend request",
                enabled = !busy,
                onClick = { onAdd(target) },
            )
        }
        Spacer(modifier = Modifier.height(8.dp))
    }
}

/**
 * The room intent sheet: what tapping a directory room opens. The join is the act — primary and
 * orange — over the room's own occupancy, so a full room is visible before the join is attempted
 * rather than after it is refused.
 */
@Composable
fun RoomIntentSheet(
    room: RoomSummary?,
    live: RoomLiveInfo?,
    joined: Boolean,
    onDismiss: () -> Unit,
    onJoin: (RoomSummary) -> Unit,
    onOpen: (RoomSummary) -> Unit,
) {
    if (room == null) return
    // The live counts, when a stream has said better than the directory page did.
    val counted = if (live != null) {
        room.copy(memberCount = live.memberCount, onlineCount = live.onlineCount, maxMembers = live.maxMembers)
    } else {
        room
    }
    val capacity = counted.maxMembers ?: 0L
    val full = capacity > 0L && counted.memberCount >= capacity
    MigoSheet(title = counted.name, onDismiss = onDismiss) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                modifier = Modifier
                    .size(44.dp)
                    .background(
                        MaterialTheme.colorScheme.surfaceVariant,
                        RoundedCornerShape(12.dp),
                    ),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = "#",
                    fontSize = 20.sp,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(modifier = Modifier.width(12.dp))
            Column {
                ListRowName(text = counted.name)
                ListRowLine(
                    text = counted.topic?.takeIf { it.isNotBlank() }
                        ?: (if (capacity > 0L) "No topic set" else "No topic · no member ceiling"),
                )
                Spacer(modifier = Modifier.height(4.dp))
                OccupancyBar(current = counted.onlineCount, capacity = capacity)
            }
        }
        SheetPrimaryAction(
            label = if (full && !joined) {
                "Room is full"
            } else {
                if (joined) "Open room" else "Join room"
            },
            enabled = !full || joined,
            onClick = { if (joined) onOpen(counted) else onJoin(counted) },
        )
        if (!joined) {
            SheetAction(
                glyph = "✦",
                label = "Member list & details",
                sub = "Join first — the roster is members-only",
                enabled = false,
                onClick = {},
            )
        }
        Spacer(modifier = Modifier.height(8.dp))
    }
}
