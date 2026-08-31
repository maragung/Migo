package com.migo.app.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
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
import com.migo.core.protocol.RoomSummary
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.time.temporal.ChronoUnit

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
 * teal-anchored family and status hues, and every entry carries white text.
 */
private fun tintFor(name: String): Color {
    val palette = listOf(
        Color(0xFF00838F), Color(0xFF059669), Color(0xFFD97706), Color(0xFF9D174D),
        Color(0xFF00ACC1), Color(0xFF15803D), Color(0xFF7E22CE), Color(0xFFC2410C),
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

/** The tab strip's glyphs, in strip order. */
enum class TabGlyph { FRIENDS, CHATS, ROOMS, GAMES, FEED }

/**
 * A tab-strip glyph, drawn with the Canvas rather than typed or imported.
 *
 * The design system draws its icons as strokes — one weight, round caps — and this app declares
 * no icon font (the Material icon artefacts are transitive, and depending on a version nobody
 * chose is how builds break). Each glyph is a 20dp box of geometric strokes, the same shapes the
 * web client's SVG family and the desktop client's painted icons carry, because it is one
 * product.
 */
@Composable
fun TabGlyph(kind: TabGlyph, tint: Color, modifier: Modifier = Modifier) {
    val stroke = with(LocalDensity.current) { 1.75.dp.toPx() }
    Canvas(modifier = modifier.size(20.dp)) {
        drawGlyph(kind, tint, stroke)
    }
}

/** The glyph shapes, on a unit box scaled to the drawing size. */
private fun DrawScope.drawGlyph(kind: TabGlyph, color: Color, stroke: Float) {
    fun p(x: Float, y: Float) = Offset(x * size.width, y * size.height)
    val cap = StrokeCap.Round
    when (kind) {
        TabGlyph.FRIENDS -> {
            // Two people: a front figure and the one half a step behind.
            drawCircle(color = color, radius = size.width * 0.14f, center = p(0.36f, 0.3f), style = Stroke(width = stroke))
            drawLine(color, p(0.14f, 0.88f), p(0.36f, 0.52f), stroke, cap)
            drawLine(color, p(0.36f, 0.52f), p(0.58f, 0.88f), stroke, cap)
            drawCircle(color = color, radius = size.width * 0.11f, center = p(0.72f, 0.36f), style = Stroke(width = stroke))
            drawLine(color, p(0.56f, 0.88f), p(0.74f, 0.62f), stroke, cap)
            drawLine(color, p(0.74f, 0.62f), p(0.92f, 0.88f), stroke, cap)
        }

        TabGlyph.CHATS -> {
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

        TabGlyph.ROOMS -> {
            // A hash: the glyph the whole product marks rooms with.
            drawLine(color, p(0.32f, 0.08f), p(0.32f, 0.92f), stroke, cap)
            drawLine(color, p(0.68f, 0.08f), p(0.68f, 0.92f), stroke, cap)
            drawLine(color, p(0.08f, 0.32f), p(0.92f, 0.32f), stroke, cap)
            drawLine(color, p(0.08f, 0.68f), p(0.92f, 0.68f), stroke, cap)
        }

        TabGlyph.GAMES -> {
            // A d-pad: the cross plus the two buttons a controller carries.
            drawLine(color, p(0.5f, 0.14f), p(0.5f, 0.86f), stroke, cap)
            drawLine(color, p(0.14f, 0.5f), p(0.86f, 0.5f), stroke, cap)
            drawCircle(color = color, radius = size.width * 0.07f, center = p(0.5f, 0.5f))
            drawCircle(color = color, radius = size.width * 0.07f, center = p(0.14f, 0.14f))
            drawCircle(color = color, radius = size.width * 0.07f, center = p(0.86f, 0.86f))
        }

        TabGlyph.FEED -> {
            // A pulse: activity as a heartbeat line.
            drawLine(color, p(0.05f, 0.55f), p(0.3f, 0.55f), stroke, cap)
            drawLine(color, p(0.3f, 0.55f), p(0.4f, 0.2f), stroke, cap)
            drawLine(color, p(0.4f, 0.2f), p(0.55f, 0.85f), stroke, cap)
            drawLine(color, p(0.55f, 0.85f), p(0.65f, 0.55f), stroke, cap)
            drawLine(color, p(0.65f, 0.55f), p(0.95f, 0.55f), stroke, cap)
        }
    }
}
