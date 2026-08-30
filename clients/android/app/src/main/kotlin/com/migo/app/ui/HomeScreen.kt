package com.migo.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.app.model.ConversationRow
import com.migo.core.protocol.RoomSummary
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.temporal.ChronoUnit

/**
 * The Home dashboard: a glance, not a destination.
 *
 * Every block states a fact the session already knows or can read in one round trip, and every row
 * is a door into the section that owns it. The blocks are compact by contract — one row per fact, no
 * card chrome around a single number — and each loads independently, so a slow block is a
 * placeholder rather than a blank dashboard.
 */
@Composable
fun HomeScreen(
    state: AppState.SignedIn,
    onOpenConversation: (ConversationRow) -> Unit,
    onOpenSection: (AppState.Section) -> Unit,
    onJoinRoom: (RoomSummary) -> Unit,
    onStartDirect: (com.migo.core.wire.Id) -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(modifier = modifier.fillMaxSize()) {
        // The hero: who you are, what you have, and where you stand.
        item {
            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Monogram(name = state.username, size = 44.dp)
                Spacer(modifier = Modifier.width(12.dp))
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = state.username,
                        style = MaterialTheme.typography.titleMedium,
                        color = MaterialTheme.colorScheme.onSurface,
                    )
                    ConnectionBadge(state = state.connection)
                }
                if (state.home.balance != null) {
                    Surface(
                        color = MaterialTheme.colorScheme.primaryContainer,
                        contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
                        shape = MaterialTheme.shapes.large,
                    ) {
                        Text(
                            text = "MIG " + state.home.balance.toString(),
                            style = MaterialTheme.typography.labelMedium,
                            fontWeight = FontWeight.Bold,
                            modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
                        )
                    }
                }
            }
        }

        // The quick actions: the three moves a session most often starts with.
        item {
            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                QuickAction(label = "Search", weight = 1f) { onOpenSection(AppState.Section.SEARCH) }
                QuickAction(label = "Rooms", weight = 1f) { onOpenSection(AppState.Section.ROOMS) }
                QuickAction(label = "Wallet", weight = 1f) { onOpenSection(AppState.Section.WALLET) }
            }
        }

        // Recent chats: the conversation list's own top.
        if (state.conversations.isNotEmpty()) {
            item { SectionHead(title = "Recent chats", action = "All chats") { onOpenSection(AppState.Section.CHATS) } }
            items(state.conversations.take(4), key = { it.conversationId.value }) { row ->
                ConversationRowItem(row = row, onOpen = { onOpenConversation(row) })
            }
        }

        // Trending rooms: the catalogue's liveliest page, offered for a join.
        if (state.home.trending.isNotEmpty()) {
            item { SectionHead(title = "Trending rooms", action = "All rooms") { onOpenSection(AppState.Section.ROOMS) } }
            items(state.home.trending, key = { it.roomId.value }) { room ->
                RoomSummaryRow(room = room, onJoin = { onJoinRoom(room) })
            }
        }

        // People to meet: the social graph's own recommendations.
        if (state.home.suggestions.isNotEmpty()) {
            item { SectionHead(title = "People to meet", action = "Friends") { onOpenSection(AppState.Section.FRIENDS) } }
            items(state.home.suggestions, key = { it.accountId.value }) { person ->
                PersonSummaryRow(
                    name = person.displayName,
                    handle = person.username,
                    note = if (person.mutualFriends > 0) "${person.mutualFriends} mutual" else null,
                    action = "Message",
                    onAction = { onStartDirect(person.accountId) },
                )
            }
        }

        // The alerts digest.
        if (state.home.notifications.isNotEmpty()) {
            item { SectionHead(title = "Alerts", action = "View all") { onOpenSection(AppState.Section.ALERTS) } }
            items(state.home.notifications, key = { it.id.value }) { item ->
                ActivityLine(
                    title = item.title ?: item.kind.replace('_', ' ').replaceFirstChar { it.uppercase() },
                    at = item.at,
                )
            }
        }

        // The leaderboard's top three: community standing at a glance.
        if (state.home.leaders.isNotEmpty()) {
            item { SectionHead(title = "Top XP", action = "Leaderboard") { onOpenSection(AppState.Section.WALLET) } }
            items(state.home.leaders, key = { it.accountId.value }) { rank ->
                ActivityLine(title = "#${rank.position}  Level ${rank.level}  ${rank.xp} XP", at = null)
            }
        }

        item { Spacer(modifier = Modifier.height(16.dp)) }
    }
}

/** One compact section heading with its one action, aligned to the baseline. */
@Composable
private fun SectionHead(title: String, action: String, onAction: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 16.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = title,
            style = MaterialTheme.typography.labelMedium,
            color = LocalMigoExtra.current.faint,
            fontWeight = FontWeight.Bold,
            modifier = Modifier.weight(1f),
        )
        TextButton(onClick = onAction) { Text(action) }
    }
}

/** One quick action: a labelled, equal-width door into a section. */
@Composable
private fun androidx.compose.foundation.layout.RowScope.QuickAction(
    label: String,
    weight: Float,
    onClick: () -> Unit,
) {
    Surface(
        modifier = Modifier.weight(weight).clickable(onClick = onClick),
        color = MaterialTheme.colorScheme.surface,
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        shape = MaterialTheme.shapes.medium,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.SemiBold,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(vertical = 18.dp).fillMaxWidth(),
            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
    }
}

/** One conversation row, compact: monogram, title, preview, time. */
@Composable
private fun ConversationRowItem(row: ConversationRow, onOpen: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onOpen).padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Monogram(name = row.title, size = 36.dp)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = row.title,
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
            if (row.preview != null) OneLine(text = row.preview)
        }
        Column(horizontalAlignment = Alignment.End) {
            Text(
                text = clockTime(row.updatedAt),
                style = MaterialTheme.typography.labelSmall,
                color = LocalMigoExtra.current.faint,
            )
            if (row.unread > 0) {
                Spacer(modifier = Modifier.height(2.dp))
                Text(
                    text = row.unread.toString(),
                    style = MaterialTheme.typography.labelSmall,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
        }
    }
}

/** One room row in a digest: name, live online count, and the way in. */
@Composable
fun RoomSummaryRow(room: RoomSummary, onJoin: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Monogram(name = room.name, size = 36.dp)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = room.name,
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
            OneLine(text = "${room.onlineCount} online" + (room.category?.let { " · $it" } ?: ""))
        }
        TextButton(onClick = onJoin) { Text("Join") }
    }
}

/** One person row in a digest: name, handle, an optional note, and the offered action. */
@Composable
fun PersonSummaryRow(
    name: String,
    handle: String,
    note: String?,
    action: String,
    onAction: () -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Monogram(name = name, size = 36.dp)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = name,
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
            OneLine(text = "@" + handle + (note?.let { " · $it" } ?: ""))
        }
        TextButton(onClick = onAction) { Text(action) }
    }
}

/** One line of activity: the headline, and a relative time when it has one. */
@Composable
fun ActivityLine(title: String, at: Long?) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = title,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
            modifier = Modifier.weight(1f),
            maxLines = 2,
        )
        if (at != null) {
            Spacer(modifier = Modifier.width(8.dp))
            Text(
                text = relativeTime(at),
                style = MaterialTheme.typography.labelSmall,
                color = LocalMigoExtra.current.faint,
            )
        }
    }
}

/** A timestamp as a short relative age, matching the web client's wording. */
fun relativeTime(epochMs: Long): String {
    val now = System.currentTimeMillis()
    val age = now - epochMs
    return when {
        age < 45_000L -> "now"
        age < 3_600_000L -> "${age / 60_000L}m"
        age < 86_400_000L -> "${age / 3_600_000L}h"
        age < 7 * 86_400_000L -> "${age / 86_400_000L}d"
        else -> {
            try {
                Instant.ofEpochMilli(epochMs)
                    .atZone(ZoneId.systemDefault())
                    .truncatedTo(ChronoUnit.DAYS)
                    .format(DateTimeFormatter.ofPattern("d MMM"))
            } catch (_: RuntimeException) {
                ""
            }
        }
    }
}
