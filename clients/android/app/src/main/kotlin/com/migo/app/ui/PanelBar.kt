package com.migo.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState

/**
 * The menu panel's bar: the new-ui-02 right pane's header, as a phone wears it.
 *
 * On a PC the right pane is a second column with its own tab buttons; on a phone there is no
 * second column, so a menu panel (Alerts, Search, Wallet, Profile — opened from the banner's
 * avatar menu) covers the whole screen and carries its own way back. The bar keeps the model's
 * shape: the teal strip, the cyan "‹ Menu Panel" control, and the title naming what is showing.
 *
 * Text characters rather than icons, for the same reason every glyph in this app is drawn: no
 * icon dependency, and they scale with the system font size.
 */
@Composable
fun PanelBar(
    title: String,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val extra = LocalMigoExtra.current
    Surface(color = extra.nav, modifier = modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 6.dp, vertical = 5.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                modifier = Modifier
                    .background(color = extra.navActive, shape = RoundedCornerShape(8.dp))
                    .clickable(onClick = onBack)
                    .padding(horizontal = 8.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "‹",
                    style = MaterialTheme.typography.labelMedium,
                    fontWeight = FontWeight.Bold,
                    color = extra.bannerInk,
                )
                Spacer(modifier = Modifier.width(3.dp))
                Text(
                    text = "Menu Panel",
                    style = MaterialTheme.typography.labelMedium,
                    fontWeight = FontWeight.Bold,
                    color = extra.bannerInk,
                )
            }
            Spacer(modifier = Modifier.width(10.dp))
            Text(
                text = "✦",
                style = MaterialTheme.typography.labelMedium,
                color = extra.gold,
            )
            Spacer(modifier = Modifier.width(4.dp))
            Text(
                text = "Panel: $title",
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.Bold,
                color = extra.bannerInk,
            )
        }
    }
}

/** The panels' names, as the bar's title spells them — the web client's right pane uses the same. */
fun panelTitle(section: AppState.Section): String = when (section) {
    AppState.Section.ALERTS -> "Alerts"
    AppState.Section.SEARCH -> "Search"
    AppState.Section.WALLET -> "TopUp"
    AppState.Section.PROFILE -> "Profile"
    else -> "Panel"
}
