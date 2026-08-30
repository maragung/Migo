package com.migo.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.migo.core.ConnectionState
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/**
 * The pieces more than one screen needs.
 *
 * # No icon font
 *
 * Every glyph here is a character, not a vector from `material-icons`. That library is not a declared
 * dependency: it is available transitively, and depending on something transitively is depending on a
 * version nobody chose. Text also scales with the user's font size setting for free, which a fixed-size
 * icon does not.
 */

/**
 * The error banner, shown above whatever it applies to.
 *
 * Dismissable rather than timed. A message that disappears on its own is a message somebody misses
 * while they are looking at the keyboard, and every failure this app reports is one a person may want
 * to read twice -- a wrong password, a server that cannot be reached, a send that did not go.
 */
@Composable
fun ErrorBanner(message: String?, onDismiss: () -> Unit, modifier: Modifier = Modifier) {
    if (message == null) return
    Surface(
        modifier = modifier.fillMaxWidth(),
        color = MaterialTheme.colorScheme.errorContainer,
        contentColor = MaterialTheme.colorScheme.onErrorContainer,
    ) {
        Row(
            modifier = Modifier.padding(start = 16.dp, top = 4.dp, bottom = 4.dp, end = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = message,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = onDismiss) {
                Text(text = "Dismiss", color = MaterialTheme.colorScheme.onErrorContainer)
            }
        }
    }
}

/**
 * The connection state, as a dot and a word.
 *
 * Both, not one. The dot is what someone glances at and the word is what they read when the dot is not
 * green, and a colour on its own would say nothing to anybody who cannot tell this green from this
 * amber.
 */
@Composable
fun ConnectionBadge(state: ConnectionState, modifier: Modifier = Modifier) {
    val scheme = MaterialTheme.colorScheme
    val (label, tint) = when (state) {
        ConnectionState.Online -> Pair("Online", scheme.secondary)
        ConnectionState.Connecting -> Pair("Connecting", scheme.outline)
        ConnectionState.Reconnecting -> Pair("Reconnecting", scheme.error)
        ConnectionState.Closed -> Pair("Offline", scheme.outline)
    }
    Row(modifier = modifier, verticalAlignment = Alignment.CenterVertically) {
        Box(modifier = Modifier.size(8.dp).background(tint, CircleShape))
        Spacer(modifier = Modifier.width(6.dp))
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = scheme.onSurfaceVariant,
        )
    }
}

/**
 * A circle with the first character of a name in it.
 *
 * A stand-in for an avatar this build cannot fetch: media is section 168 and still spec, and the
 * server is explicitly never a byte proxy for it, so there is no URL here to load. The colour is
 * derived from the name so the same person keeps the same circle between launches without anything
 * being stored.
 */
@Composable
fun Monogram(name: String, size: Dp = 44.dp, modifier: Modifier = Modifier) {
    val letter = name.trim().firstOrNull()?.uppercase() ?: "?"
    Box(
        modifier = modifier.size(size).background(tintFor(name), CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = letter,
            style = MaterialTheme.typography.titleMedium,
            color = Color.White,
            textAlign = TextAlign.Center,
        )
    }
}

/**
 * A stable colour for a name.
 *
 * `hashCode` rather than anything cryptographic: this picks one of eight colours for a monogram, and
 * nothing about it needs to be unpredictable. The palette is drawn from the Migo design system's
 * accent family and status hues, and every entry carries white text.
 */
private fun tintFor(name: String): Color {
    val palette = listOf(
        Color(0xFF005CB8), Color(0xFF00875A), Color(0xFF9A6700), Color(0xFF9D174D),
        Color(0xFF1D4ED8), Color(0xFF15803D), Color(0xFF7E22CE), Color(0xFFC2410C),
    )
    val index = (name.hashCode().toLong() and 0xffffffffL).mod(palette.size.toLong()).toInt()
    return palette[index]
}

/** A compact section label: the micro type step, uppercase, in the tertiary ink. */
@Composable
fun SectionLabel(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelMedium,
        color = MigoExtra.current.faint,
        fontWeight = androidx.compose.ui.text.font.FontWeight.Bold,
        modifier = modifier.padding(start = 16.dp, top = 12.dp, bottom = 4.dp),
    )
}

/**
 * A row of secondary text that never wraps to a second line.
 *
 * For previews and status lines, where a second line would shift everything below it as messages
 * arrive.
 */
@Composable
fun OneLine(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier,
    )
}

/** Centres a short message in whatever space is left, for an empty list or a first run. */
@Composable
fun Placeholder(text: String, modifier: Modifier = Modifier) {
    Box(modifier = modifier.fillMaxWidth().padding(32.dp), contentAlignment = Alignment.Center) {
        Text(
            text = text,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
    }
}

/** A right-aligned row, for the one-button footers. */
@Composable
fun EndRow(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Row(
        modifier = modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.End,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        content()
    }
}

/**
 * A server timestamp as a local clock time.
 *
 * The server's milliseconds are UTC and the formatter is the device's, so the same message reads
 * correctly in whatever zone the phone is in. Localised rather than a fixed `HH:mm`, because a
 * twelve-hour clock is what half the world expects to see.
 */
fun clockTime(millis: Long): String {
    if (millis <= 0L) return ""
    return try {
        Instant.ofEpochMilli(millis)
            .atZone(ZoneId.systemDefault())
            .format(DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT))
    } catch (_: RuntimeException) {
        // A timestamp from a future protocol version, or one outside the supported range. A blank
        // label is better than a crash in a message list.
        ""
    }
}
