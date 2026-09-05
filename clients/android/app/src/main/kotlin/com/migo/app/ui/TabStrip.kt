package com.migo.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
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
import androidx.compose.ui.unit.sp
import com.migo.app.model.AppState

/**
 * The left panel's tab strip: the new-ui-02 model's top navigation, as every client draws it.
 *
 * Five system tabs — Chats, Friends, Rooms, Games, Feed — the lists and streams a messenger
 * lives in. Chats leads because it is the product's centre: the conversation list is the
 * tab the others ultimately feed. A conversation is never a chip here, exactly as the
 * reference draws it: in the new model a chat covers the screen the way a menu panel does,
 * carrying its own way back, so the strip is the lists and nothing else — which is also why
 * it never has to stand down for a thread again.
 *
 * The strip is the deep teal bar, one 46dp row that scrolls horizontally rather than wrapping:
 * the reference's strip is one row on every screen size, and a second row would push the banner
 * down by the height of a tab.
 */
@Composable
fun TabStrip(
    section: AppState.Section,
    onSelect: (AppState.Section) -> Unit,
    modifier: Modifier = Modifier,
) {
    val extra = LocalMigoExtra.current
    Surface(color = extra.nav, modifier = modifier.fillMaxWidth().height(46.dp)) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(end = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            StripChip(
                label = "Chats",
                glyph = TabGlyph.CHATS,
                active = section == AppState.Section.CHATS,
                onClick = { onSelect(AppState.Section.CHATS) },
            )
            StripChip(
                label = "Friends",
                glyph = TabGlyph.FRIENDS,
                active = section == AppState.Section.FRIENDS,
                onClick = { onSelect(AppState.Section.FRIENDS) },
            )
            StripChip(
                label = "Rooms",
                glyph = TabGlyph.ROOMS,
                active = section == AppState.Section.ROOMS,
                onClick = { onSelect(AppState.Section.ROOMS) },
            )
            StripChip(
                label = "Games",
                glyph = TabGlyph.GAMES,
                active = section == AppState.Section.GAMES,
                onClick = { onSelect(AppState.Section.GAMES) },
            )
            StripChip(
                label = "Feed",
                glyph = TabGlyph.FEED,
                active = section == AppState.Section.FEED,
                onClick = { onSelect(AppState.Section.FEED) },
            )
        }
    }
}

/**
 * One chip: a 9dp-radius pill. The chosen tab is the one solid white fill in the design, carrying
 * the teal-head ink and a short rounded underline set just above the pill's bottom edge; an idle
 * chip is a faint white wash on the strip's own surface. Flat both ways — the fill is the mark, and
 * there is no gradient or glow anywhere in it.
 */
@Composable
private fun StripChip(
    label: String,
    glyph: TabGlyph,
    active: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // The active pill's ink and underline: the teal head the reference puts on the white fill. The
    // strip's own bar is the same colour in both themes, so these marks are too.
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
            }
            // The active tab's underline: a centred 16x2.5dp rounded bar in the teal head, sitting
            // 3dp above the pill's bottom edge. Drawn (transparently) on every chip so the pills
            // keep one height whether or not the chip is the chosen one.
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
