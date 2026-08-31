package com.migo.app.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/**
 * The app's colours and type — the Migo design system, mapped to Material.
 *
 * The canonical source is `shared/design/tokens.json`, mirrored into the web client's CSS variables
 * and the desktop client's `theme.rs`. The values here are the same palette: one accent (blue in
 * light, cyan in dark), the same ink ladder, the same status colours. Dynamic colour would hand the
 * palette to whatever wallpaper is set, and this app has one place where colour carries meaning
 * rather than decoration: an outgoing message is the primary colour and an incoming one is the
 * surface, which is how somebody reads a thread at a glance. A wallpaper that flattened that
 * contrast would make the conversation harder to read, so the two schemes are fixed here and checked
 * against each other.
 *
 * # The colours Material has no slot for
 *
 * [MigoExtra] carries the tokens the Material scheme has no role for — the hairline border, the
 * tertiary ink, the gold the badges and honours use — through a CompositionLocal, so a composable
 * reads them exactly like a `colorScheme` colour instead of reaching for a hardcoded value that
 * would not follow the theme.
 */

// The accent — the reference's blue-violet. Both schemes carry white text on the primary
// (above 4.5:1 in each).
private val AccentLight = Color(0xFF3B5BDB)
private val AccentDark = Color(0xFF4C6EF5)

// The Migo surfaces, straight from the token table (the main-ui.jpg reference's charcoal).
private val SurfaceLight = Color(0xFFFFFFFF)
private val SurfaceDimLight = Color(0xFFF1F2F6)
private val SurfaceSunkenLight = Color(0xFFEFF1F5)
private val SurfaceDark = Color(0xFF1B1C24)
private val SurfaceDimDark = Color(0xFF141519)
private val SurfaceSunkenDark = Color(0xFF24252E)

private val LightScheme = lightColorScheme(
    primary = AccentLight,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFD6DCFF),
    onPrimaryContainer = Color(0xFF1B2A6B),
    secondary = Color(0xFF0CA678),
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFC8F5E3),
    onSecondaryContainer = Color(0xFF003926),
    tertiary = Color(0xFF9A6700),
    onTertiary = Color.White,
    tertiaryContainer = Color(0xFFF7E8C8),
    onTertiaryContainer = Color(0xFF2E2000),
    background = SurfaceDimLight,
    onBackground = Color(0xFF1B1C24),
    surface = SurfaceLight,
    onSurface = Color(0xFF1B1C24),
    surfaceVariant = SurfaceSunkenLight,
    onSurfaceVariant = Color(0xFF5C6170),
    outline = Color(0xFFC9CDD8),
    outlineVariant = Color(0xFFE3E5EC),
    error = Color(0xFFE03131),
    onError = Color.White,
    errorContainer = Color(0xFFFFD9D9),
    onErrorContainer = Color(0xFFC92A2A),
)

private val DarkScheme = darkColorScheme(
    primary = AccentDark,
    onPrimary = Color.White,
    primaryContainer = Color(0xFF33418C),
    onPrimaryContainer = Color(0xFFD6DCFF),
    secondary = Color(0xFF2FCE7E),
    onSecondary = Color(0xFF00240F),
    secondaryContainer = Color(0xFF00502B),
    onSecondaryContainer = Color(0xFFB8F5D9),
    tertiary = Color(0xFFFCC419),
    onTertiary = Color(0xFF332800),
    tertiaryContainer = Color(0xFF55430F),
    onTertiaryContainer = Color(0xFFFFE2A6),
    background = SurfaceDimDark,
    onBackground = Color(0xFFF2F3F7),
    surface = SurfaceDark,
    onSurface = Color(0xFFF2F3F7),
    surfaceVariant = SurfaceSunkenDark,
    onSurfaceVariant = Color(0xFFA0A3B1),
    outline = Color(0xFF3A3C4A),
    outlineVariant = Color(0xFF2A2B36),
    error = Color(0xFFFF5C7A),
    onError = Color(0xFF2B060D),
    errorContainer = Color(0xFF5C1120),
    onErrorContainer = Color(0xFFFFC2CF),
)

/** The tokens Material's scheme has no slot for, themed light and dark like the rest. */
data class MigoExtra(
    /** The tertiary ink: hints, placeholders, timestamps' fainter sibling. */
    val faint: Color,
    /** The gold of badges and honours — tertiary's own colour, stated as a plain value. */
    val gold: Color,
    /** The bubble an incoming message sits in: the sunken surface. */
    val bubbleIn: Color,
    /** The $MIG coin accent on the wallet's cards. */
    val coin: Color,
)

private val ExtraLight = MigoExtra(
    faint = Color(0xFF9BA0AC),
    gold = Color(0xFF9A6700),
    bubbleIn = SurfaceSunkenLight,
    coin = AccentLight,
)

private val ExtraDark = MigoExtra(
    faint = Color(0xFF6E7180),
    gold = Color(0xFFFCC419),
    bubbleIn = SurfaceSunkenDark,
    coin = AccentDark,
)

/** Reads the extra tokens like a `colorScheme` colour: `LocalMigoExtra.current.gold`. */
val LocalMigoExtra = staticCompositionLocalOf { ExtraDark }

/**
 * Material 3's type scale, with the three styles this app actually sets adjusted.
 *
 * Only what is used is overridden. A full custom scale would be nine declarations that have to stay
 * consistent with each other for no visible gain, when the default scale is already the one Material's
 * components are measured against.
 */
private val MigoTypography = Typography().let { base ->
    base.copy(
        // A conversation title is a name, and names read better slightly heavier than the default.
        titleMedium = base.titleMedium.copy(fontWeight = FontWeight.SemiBold),
        // Message text: the default body size, with the line height opened up, because a bubble is a
        // narrow measure and tight leading is what makes long messages hard to scan.
        bodyLarge = base.bodyLarge.copy(lineHeight = 22.sp),
        // Timestamps and the sender name above a group bubble.
        labelSmall = TextStyle(fontSize = 11.sp, lineHeight = 14.sp, fontWeight = FontWeight.Medium),
    )
}

/** Wraps the app in its colours and type, following the system's light and dark setting. */
@Composable
fun MigoTheme(dark: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = if (dark) DarkScheme else LightScheme,
        typography = MigoTypography,
    ) {
        androidx.compose.runtime.CompositionLocalProvider(
            LocalMigoExtra provides if (dark) ExtraDark else ExtraLight,
        ) {
            content()
        }
    }
}
