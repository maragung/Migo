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
 * The left panel's tab strip: the new-ui-02 model's top navigation, as every client draws it.
 *
 * Four system tabs — Main (friends), Rooms, Games, Feed — the lists and streams a messenger
 * lives in. A conversation is never a chip here, exactly as the reference draws it: in the new
 * model a chat covers the screen the way a menu panel does, carrying its own way back, so the
 * strip is the lists and nothing else — which is also why it never has to stand down for a
 * thread again.
 *
 * The strip scrolls horizontally rather than wrapping: the reference's strip is one row on every
 * screen size, and a second row would push the banner down by the height of a tab.
 */
@Composable
fun TabStrip(
    section: AppState.Section,
    onSelect: (AppState.Section) -> Unit,
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
            StripChip(
                label = "Main",
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
 * One chip: a glyph, a label, the active fill and underline. The rounded fill and the orange
 * underline are the two marks the reference puts on the chosen tab; an idle chip is the strip's
 * own ink on the strip's own surface.
 */
@Composable
private fun StripChip(
    label: String,
    glyph: TabGlyph,
    active: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
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
