package com.migo.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState

/**
 * The tab strip: the reference's top navigation, as every client now draws it.
 *
 * Five system tabs — Friends, Chats, Rooms, Games, Feed — plus one chip for the conversation being
 * read, with its own close mark. The strip sits above the banner and stays on screen while a thread
 * is open, which is the whole difference from the bottom bar it replaces: a thread no longer takes
 * the shell's navigation away, because closing the thread is what the chip's ✕ is for.
 *
 * The strip scrolls horizontally rather than wrapping: the reference's strip is one row on every
 * screen size, and a second row would push the banner down by the height of a tab.
 */
@Composable
fun TabStrip(
    section: AppState.Section,
    openChatTitle: String?,
    unread: Long,
    onSelect: (AppState.Section) -> Unit,
    onCloseChat: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val extra = LocalMigoExtra.current
    Surface(color = extra.nav, modifier = modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(end = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // While a chat chip is showing, the system tabs stand down: the thread is the active
            // tab, exactly as the reference's mobile view composes it.
            val chatOpen = openChatTitle != null
            StripChip(
                label = "Friends",
                glyph = TabGlyph.FRIENDS,
                active = !chatOpen && section == AppState.Section.FRIENDS,
                onClick = { onSelect(AppState.Section.FRIENDS) },
            )
            StripChip(
                label = "Chats",
                glyph = TabGlyph.CHATS,
                active = !chatOpen && section == AppState.Section.CHATS,
                badge = unread,
                onClick = { onSelect(AppState.Section.CHATS) },
            )
            StripChip(
                label = "Rooms",
                glyph = TabGlyph.ROOMS,
                active = !chatOpen && section == AppState.Section.ROOMS,
                onClick = { onSelect(AppState.Section.ROOMS) },
            )
            StripChip(
                label = "Games",
                glyph = TabGlyph.GAMES,
                active = !chatOpen && section == AppState.Section.GAMES,
                onClick = { onSelect(AppState.Section.GAMES) },
            )
            StripChip(
                label = "Feed",
                glyph = TabGlyph.FEED,
                active = !chatOpen && section == AppState.Section.FEED,
                onClick = { onSelect(AppState.Section.FEED) },
            )
            if (openChatTitle != null) {
                StripChip(
                    label = openChatTitle,
                    glyph = TabGlyph.CHATS,
                    active = true,
                    onClick = { },
                    onClose = onCloseChat,
                )
            }
        }
    }
}

/**
 * One chip: a glyph, a label, the active fill and underline, and — for a conversation — its own
 * close mark. The rounded fill and the orange underline are the two marks the reference puts on the
 * chosen tab; an idle chip is the strip's own ink on the strip's own surface.
 */
@Composable
private fun StripChip(
    label: String,
    glyph: TabGlyph,
    active: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    badge: Long = 0L,
    onClose: (() -> Unit)? = null,
) {
    val extra = LocalMigoExtra.current
    Column(
        modifier = modifier
            .padding(top = 6.dp, start = 6.dp)
            .clickable(onClick = onClick),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Row(
            modifier = Modifier
                .background(
                    color = if (active) extra.navActive else Color.Transparent,
                    shape = RoundedCornerShape(12.dp),
                )
                .padding(horizontal = 14.dp, vertical = 7.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TabGlyph(kind = glyph, tint = extra.bannerInk)
            Spacer(modifier = Modifier.width(7.dp))
            Text(
                text = label,
                style = MaterialTheme.typography.labelMedium,
                fontWeight = if (active) FontWeight.Bold else FontWeight.Medium,
                color = extra.bannerInk,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 132.dp),
            )
            if (badge > 0) {
                Spacer(modifier = Modifier.width(6.dp))
                Surface(color = extra.bannerB, shape = RoundedCornerShape(999.dp)) {
                    Text(
                        text = badge.coerceAtMost(99).toString(),
                        style = MaterialTheme.typography.labelSmall,
                        color = extra.bannerInk,
                        modifier = Modifier.padding(horizontal = 5.dp, vertical = 1.dp),
                    )
                }
            }
            if (onClose != null) {
                Spacer(modifier = Modifier.width(7.dp))
                // A character rather than an icon, for the same reason every glyph here is drawn:
                // no icon dependency, and it scales with the system font size.
                Text(
                    text = "✕",
                    style = MaterialTheme.typography.labelMedium,
                    color = extra.bannerInk.copy(alpha = 0.85f),
                    modifier = Modifier
                        .clickable(onClick = onClose)
                        .padding(2.dp),
                )
            }
        }
        // The active tab's orange underline. Drawn (transparently) on every chip so the row's
        // baselines stay level whether or not the chip is the chosen one.
        Box(
            modifier = Modifier
                .width(26.dp)
                .height(3.dp)
                .background(
                    color = if (active) extra.bannerB else Color.Transparent,
                    shape = RoundedCornerShape(topStart = 2.dp, topEnd = 2.dp),
                ),
        )
    }
}
