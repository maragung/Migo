package com.migo.app.ui

import androidx.compose.foundation.Canvas
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
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalDensity
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
        color = LocalMigoExtra.current.faint,
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

/** The five bottom-bar glyphs, in bar order. */
enum class BarGlyph { HOME, CHATS, ROOMS, SPACE, MORE }

/**
 * A bottom-bar glyph, drawn with the Canvas rather than typed or imported.
 *
 * The design system draws its icons as strokes — one weight, round caps — and this app declares
 * no icon font (the Material icon artefacts are transitive, and depending on a version nobody
 * chose is how builds break). Each glyph is a 20dp box of geometric strokes, the same shapes the
 * web client's SVG family and the desktop client's painted icons carry, because it is one
 * product.
 */
@Composable
fun BarGlyph(kind: BarGlyph, active: Boolean, modifier: Modifier = Modifier) {
    val color = if (active) {
        MaterialTheme.colorScheme.primary
    } else {
        MaterialTheme.colorScheme.onSurfaceVariant
    }
    val stroke = with(LocalDensity.current) { 1.75.dp.toPx() }
    Canvas(modifier = modifier.size(20.dp)) {
        drawGlyph(kind, color, stroke)
    }
}

/** The glyph shapes, on a unit box scaled to the drawing size. */
private fun DrawScope.drawGlyph(kind: BarGlyph, color: Color, stroke: Float) {
    fun p(x: Float, y: Float) = Offset(x * size.width, y * size.height)
    val cap = StrokeCap.Round
    when (kind) {
        BarGlyph.HOME -> {
            // A roof over a box with a door.
            drawLine(color, p(0.1f, 0.5f), p(0.5f, 0.12f), stroke, cap)
            drawLine(color, p(0.5f, 0.12f), p(0.9f, 0.5f), stroke, cap)
            drawLine(color, p(0.2f, 0.42f), p(0.2f, 0.9f), stroke, cap)
            drawLine(color, p(0.8f, 0.42f), p(0.8f, 0.9f), stroke, cap)
            drawLine(color, p(0.2f, 0.9f), p(0.8f, 0.9f), stroke, cap)
            drawLine(color, p(0.42f, 0.9f), p(0.42f, 0.62f), stroke, cap)
            drawLine(color, p(0.42f, 0.62f), p(0.58f, 0.62f), stroke, cap)
            drawLine(color, p(0.58f, 0.62f), p(0.58f, 0.9f), stroke, cap)
        }

        BarGlyph.CHATS -> {
            // A speech bubble with a tail.
            drawRoundRect(
                color = color,
                topLeft = p(0.08f, 0.15f),
                size = androidx.compose.ui.geometry.Size(size.width * 0.84f, size.height * 0.57f),
                cornerRadius = androidx.compose.ui.geometry.CornerRadius(4.dp.toPx()),
                style = Stroke(width = stroke, cap = cap),
            )
            drawLine(color, p(0.3f, 0.72f), p(0.22f, 0.9f), stroke, cap)
            drawLine(color, p(0.22f, 0.9f), p(0.48f, 0.72f), stroke, cap)
        }

        BarGlyph.ROOMS -> {
            // A hash: the glyph the whole product marks rooms with.
            drawLine(color, p(0.32f, 0.08f), p(0.32f, 0.92f), stroke, cap)
            drawLine(color, p(0.68f, 0.08f), p(0.68f, 0.92f), stroke, cap)
            drawLine(color, p(0.08f, 0.32f), p(0.92f, 0.32f), stroke, cap)
            drawLine(color, p(0.08f, 0.68f), p(0.92f, 0.68f), stroke, cap)
        }

        BarGlyph.SPACE -> {
            // A pulse: activity as a heartbeat line.
            drawLine(color, p(0.05f, 0.55f), p(0.3f, 0.55f), stroke, cap)
            drawLine(color, p(0.3f, 0.55f), p(0.4f, 0.2f), stroke, cap)
            drawLine(color, p(0.4f, 0.2f), p(0.55f, 0.85f), stroke, cap)
            drawLine(color, p(0.55f, 0.85f), p(0.65f, 0.55f), stroke, cap)
            drawLine(color, p(0.65f, 0.55f), p(0.95f, 0.55f), stroke, cap)
        }

        BarGlyph.MORE -> {
            // A hamburger: three even lines.
            drawLine(color, p(0.15f, 0.3f), p(0.85f, 0.3f), stroke, cap)
            drawLine(color, p(0.15f, 0.5f), p(0.85f, 0.5f), stroke, cap)
            drawLine(color, p(0.15f, 0.7f), p(0.85f, 0.7f), stroke, cap)
        }
    }
}
