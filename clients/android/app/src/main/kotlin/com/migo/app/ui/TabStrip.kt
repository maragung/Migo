package com.migo.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.migo.app.model.AppState
import com.migo.app.model.ChatState
import com.migo.app.model.ConversationRow
import com.migo.app.model.WindowTab
import com.migo.core.wire.Id

/**
 * The mobile window strip: one 46dp row at the very top, the whole shell's navigation.
 *
 * The home tabs come first — Friends, Rooms, Feed, in the reference's order — and only Feed carries
 * an X, because Friends and Rooms are the doors everything else opens through; closing Feed parks it
 * in the hidden set until the strip's "+" sheet reopens it. After a 1px divider, one tab per open
 * conversation: tapping it shows that conversation full-bleed below the strip, and its X closes the
 * window outright, decrypted messages and all, per the no-store design. One window is visible at a
 * time; the rest stay parked behind their tabs.
 *
 * The row scrolls horizontally rather than wrapping, exactly as the reference draws it, so the
 * content below never moves when a new tab arrives.
 */
@Composable
fun MobileTabStrip(
    section: AppState.Section,
    windows: List<WindowTab>,
    open: ChatState?,
    hiddenNavs: Set<AppState.Section>,
    conversations: List<ConversationRow>,
    onSelectNav: (AppState.Section) -> Unit,
    onCloseNav: (AppState.Section) -> Unit,
    onReopenNav: (AppState.Section) -> Unit,
    onSelectWindow: (WindowTab) -> Unit,
    onCloseWindow: (Id) -> Unit,
    modifier: Modifier = Modifier,
) {
    val extra = LocalMigoExtra.current
    var reopenOpen by remember { mutableStateOf(false) }
    // Unread per conversation, resolved once per list change: a strip redraws on every scroll frame
    // of the row beneath it, and a lookup per frame is a lookup per frame.
    val unreadBy = remember(conversations) { conversations.associate { it.conversationId to it.unread } }

    Surface(color = extra.nav, modifier = modifier.fillMaxWidth().height(46.dp)) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(end = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            for (nav in navOrder) {
                if (nav in hiddenNavs) continue
                NavChip(
                    label = navLabel(nav),
                    glyph = navGlyph(nav),
                    active = open == null && section == nav,
                    closable = nav == AppState.Section.FEED,
                    onClick = { onSelectNav(nav) },
                    onClose = { onCloseNav(nav) },
                )
            }
            if (hiddenNavs.isNotEmpty()) {
                ReopenChip(onClick = { reopenOpen = true })
            }
            // The divider between the home tabs and the window tabs: 1px of 15% white, the
            // reference's exact separator, tall as two chips so it reads as one seam.
            Box(
                modifier = Modifier
                    .padding(horizontal = 3.dp)
                    .width(1.dp)
                    .height(20.dp)
                    .background(color = Color.White.copy(alpha = 0.15f)),
            )
            for (tab in windows) {
                WindowChip(
                    tab = tab,
                    active = open?.conversationId == tab.conversationId,
                    unread = unreadBy[tab.conversationId] ?: 0L,
                    onClick = { onSelectWindow(tab) },
                    onClose = { onCloseWindow(tab.conversationId) },
                )
            }
        }
    }

    // The "+" sheet: the home tabs the strip no longer shows, each one tap from returning. With
    // only Feed closable it holds one row at most, but it is a list in the reference and a list
    // here, so a second closable tab is a one-line change rather than a redesign.
    if (reopenOpen) {
        MigoSheet(title = "Reopen tab", onDismiss = { reopenOpen = false }) {
            for (nav in navOrder.filter { it in hiddenNavs }) {
                SheetAction(
                    glyph = navSheetGlyph(nav),
                    label = navLabel(nav),
                    onClick = {
                        reopenOpen = false
                        onReopenNav(nav)
                    },
                )
            }
            Spacer(modifier = Modifier.height(8.dp))
        }
    }
}

/** The home tabs in strip order — the reference's MOBILE_NAV_ORDER, verbatim. */
private val navOrder = listOf(AppState.Section.FRIENDS, AppState.Section.ROOMS, AppState.Section.FEED)

private fun navLabel(section: AppState.Section): String = when (section) {
    AppState.Section.FRIENDS -> "Friends"
    AppState.Section.ROOMS -> "Rooms"
    AppState.Section.FEED -> "Feed"
    else -> "Tab"
}

private fun navGlyph(section: AppState.Section): TabGlyph = when (section) {
    AppState.Section.FRIENDS -> TabGlyph.FRIENDS
    AppState.Section.ROOMS -> TabGlyph.ROOMS
    AppState.Section.FEED -> TabGlyph.FEED
    else -> TabGlyph.CHATS
}

/** The reopen sheet's rows carry text glyphs, the sheet's own currency. */
private fun navSheetGlyph(section: AppState.Section): String = when (section) {
    AppState.Section.FRIENDS -> "☺"
    AppState.Section.ROOMS -> "#"
    AppState.Section.FEED -> "✦"
    else -> "•"
}

/**
 * One home tab: the chip style the strip has always drawn — the chosen tab the one solid white
 * pill on the deep teal, carrying the teal-head ink and its short underline — with Feed's X set
 * inside the right edge. Active here means the home view is what is showing, so a parked window
 * never lights a home tab.
 */
@Composable
private fun NavChip(
    label: String,
    glyph: TabGlyph,
    active: Boolean,
    closable: Boolean,
    onClick: () -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val activeInk = Color(0xFF0D6373)
    val idleInk = Color.White.copy(alpha = 0.92f)
    Box(
        modifier = modifier
            .padding(top = 6.dp, start = 6.dp)
            .clickable(onClick = onClick),
    ) {
        Box(
            modifier = Modifier
                .background(
                    color = if (active) LocalMigoExtra.current.navActive else Color.White.copy(alpha = 0.08f),
                    shape = RoundedCornerShape(9.dp),
                )
                .padding(horizontal = 14.dp, vertical = 7.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TabGlyph(kind = glyph, tint = if (active) activeInk else idleInk)
                Spacer(modifier = Modifier.width(7.dp))
                Text(
                    text = label,
                    style = MaterialTheme.typography.labelMedium,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = if (active) activeInk else idleInk,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.widthIn(max = 132.dp),
                )
                if (closable) {
                    Spacer(modifier = Modifier.width(6.dp))
                    CloseGlyph(onClose = onClose, tint = if (active) activeInk else idleInk)
                }
            }
            Box(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = 3.dp)
                    .width(16.dp)
                    .height(2.5.dp)
                    .background(
                        color = if (active) activeInk else Color.Transparent,
                        shape = RoundedCornerShape(999.dp),
                    ),
            )
        }
    }
}

/**
 * One window tab: the conversation's glyph, its title (truncated the reference's 110px way), the
 * unread badge the reference caps at "9+", and the X that closes the window outright. No presence
 * dot — the reference's dot marks which of several desktop windows has focus, and here the active
 * window is already the one the whole strip is lit for, which the white pill carries.
 */
@Composable
private fun WindowChip(
    tab: WindowTab,
    active: Boolean,
    unread: Long,
    onClick: () -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val activeInk = Color(0xFF0D6373)
    val idleInk = Color.White.copy(alpha = 0.92f)
    Box(
        modifier = modifier
            .padding(top = 6.dp, start = 6.dp)
            .clickable(onClick = onClick),
    ) {
        Box(
            modifier = Modifier
                .background(
                    color = if (active) LocalMigoExtra.current.navActive else Color.White.copy(alpha = 0.08f),
                    shape = RoundedCornerShape(9.dp),
                )
                .padding(horizontal = 14.dp, vertical = 7.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TabGlyph(kind = TabGlyph.CHATS, tint = if (active) activeInk else idleInk)
                Spacer(modifier = Modifier.width(7.dp))
                Text(
                    text = tab.title,
                    style = MaterialTheme.typography.labelMedium,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = if (active) activeInk else idleInk,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.widthIn(max = 110.dp),
                )
                if (unread > 0) {
                    Spacer(modifier = Modifier.width(6.dp))
                    StripBadge(count = unread)
                }
                Spacer(modifier = Modifier.width(6.dp))
                CloseGlyph(onClose = onClose, tint = if (active) activeInk else idleInk)
            }
            Box(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = 3.dp)
                    .width(16.dp)
                    .height(2.5.dp)
                    .background(
                        color = if (active) activeInk else Color.Transparent,
                        shape = RoundedCornerShape(999.dp),
                    ),
            )
        }
    }
}

/** The strip's "+", which opens the sheet of closed home tabs. */
@Composable
private fun ReopenChip(onClick: () -> Unit, modifier: Modifier = Modifier) {
    val idleInk = Color.White.copy(alpha = 0.92f)
    Box(
        modifier = modifier
            .padding(top = 6.dp, start = 6.dp)
            .clickable(onClick = onClick),
    ) {
        Box(
            modifier = Modifier
                .background(color = Color.White.copy(alpha = 0.08f), shape = RoundedCornerShape(9.dp))
                .padding(horizontal = 12.dp, vertical = 7.dp),
        ) {
            Text(
                text = "+",
                style = MaterialTheme.typography.labelMedium,
                fontSize = 14.sp,
                fontWeight = FontWeight.Bold,
                color = idleInk,
            )
        }
    }
}

/** The unread badge, capped at "9+" the reference's way. Red on any chip, active or idle. */
@Composable
private fun StripBadge(count: Long) {
    Surface(
        color = Color(0xFFE5503C),
        contentColor = Color.White,
        shape = RoundedCornerShape(999.dp),
    ) {
        Text(
            text = if (count > 9) "9+" else count.toString(),
            style = MaterialTheme.typography.labelMedium,
            fontSize = 8.5.sp,
            fontWeight = FontWeight.Bold,
            maxLines = 1,
            modifier = Modifier.padding(horizontal = 3.5.dp, vertical = 1.dp),
        )
    }
}

/** The X inside a chip: its own touch target, so closing never selects. */
@Composable
private fun CloseGlyph(onClose: () -> Unit, tint: Color) {
    Box(
        modifier = Modifier
            .heightIn(min = 20.dp)
            .clickable(onClick = onClose)
            .padding(horizontal = 2.dp, vertical = 2.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = "✕",
            style = MaterialTheme.typography.labelMedium,
            fontSize = 11.sp,
            color = tint,
        )
    }
}
