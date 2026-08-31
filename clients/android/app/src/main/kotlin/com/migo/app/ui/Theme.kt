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
 * The canonical source is `shared/design/tokens.json` (v3), mirrored into the web client's CSS
 * variables and the desktop client's `theme.rs`. The values here are that same palette: a teal
 * accent on a cream light surface, a cyan accent on a deep-teal dark surface, and the orange banner
 * and login gradients riding above both. Dynamic colour would hand the palette to whatever
 * wallpaper is set, and this app has one place where colour carries meaning rather than
 * decoration: an outgoing message is the primary colour and an incoming one is the surface, which
 * is how somebody reads a thread at a glance. A wallpaper that flattened that contrast would make
 * the conversation harder to read, so the two schemes are fixed here and checked against each
 * other.
 *
 * # The colours Material has no slot for
 *
 * [MigoExtra] carries the tokens the Material scheme has no role for — the faint ink, the gold the
 * badges and honours use, the tab strip's own surface — through a CompositionLocal, so a composable
 * reads them exactly like a `colorScheme` colour instead of reaching for a hardcoded value that
 * would not follow the theme. The banner and login gradients are the same values in both themes
 * (the front door does not change with the lights), but they live here rather than as loose
 * constants so every client of the palette reads them from one place.
 */

// The accent — the reference's teal in light, its cyan in dark. Both carry their contrast ink
// (white in light, the deep teal #062a30 in dark) above 4.5:1.
private val AccentLight = Color(0xFF00838F)
private val AccentBrightLight = Color(0xFF00ACC1)
private val AccentDark = Color(0xFF00BCD4)
private val AccentBrightDark = Color(0xFF26C6DA)

// The Migo surfaces, straight from the token table: cream and white in light, deep teal in dark.
private val SurfaceLight = Color(0xFFFFFFFF)
private val SurfaceDimLight = Color(0xFFFDFBF7)
private val SurfaceSunkenLight = Color(0xFFF5F1E8)
private val SurfaceDark = Color(0xFF122023)
private val SurfaceDimDark = Color(0xFF0C1517)
private val SurfaceSunkenDark = Color(0xFF1A2C30)

private val LightScheme = lightColorScheme(
    primary = AccentLight,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFCCE9ED),
    onPrimaryContainer = Color(0xFF00363D),
    secondary = Color(0xFF059669),
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFD1F5E5),
    onSecondaryContainer = Color(0xFF00391F),
    tertiary = Color(0xFFD97706),
    onTertiary = Color.White,
    tertiaryContainer = Color(0xFFFBEDD2),
    onTertiaryContainer = Color(0xFF3F2400),
    background = SurfaceDimLight,
    onBackground = Color(0xFF1E2B2E),
    surface = SurfaceLight,
    onSurface = Color(0xFF1E2B2E),
    surfaceVariant = SurfaceSunkenLight,
    onSurfaceVariant = Color(0xFF5C6A6D),
    outline = Color(0xFFD3CAB4),
    outlineVariant = Color(0xFFE8E2D4),
    error = Color(0xFFE03131),
    onError = Color.White,
    errorContainer = Color(0xFFFFE1E1),
    onErrorContainer = Color(0xFFC92A2A),
)

private val DarkScheme = darkColorScheme(
    primary = AccentDark,
    onPrimary = Color(0xFF062A30),
    primaryContainer = Color(0xFF0E4A52),
    onPrimaryContainer = Color(0xFFCCEEF2),
    secondary = Color(0xFF2FCE7E),
    onSecondary = Color(0xFF00391D),
    secondaryContainer = Color(0xFF005028),
    onSecondaryContainer = Color(0xFFBDF5D6),
    tertiary = Color(0xFFFCC419),
    onTertiary = Color(0xFF332800),
    tertiaryContainer = Color(0xFF55430F),
    onTertiaryContainer = Color(0xFFFFE2A6),
    background = SurfaceDimDark,
    onBackground = Color(0xFFE9F4F5),
    surface = SurfaceDark,
    onSurface = Color(0xFFE9F4F5),
    surfaceVariant = SurfaceSunkenDark,
    onSurfaceVariant = Color(0xFF9DB4B8),
    outline = Color(0xFF355158),
    outlineVariant = Color(0xFF24393E),
    error = Color(0xFFFF5C7A),
    onError = Color(0xFF2B060D),
    errorContainer = Color(0xFF5C1120),
    onErrorContainer = Color(0xFFFFC2CF),
)

/**
 * The tokens Material's scheme has no slot for, themed light and dark like the rest.
 *
 * The banner and login gradients hold the same values in both themes, so a screen that paints them
 * never has to ask which theme it is in — the front door is the one surface that ignores the lights.
 */
data class MigoExtra(
    /** The tertiary ink: hints, placeholders, timestamps' fainter sibling. */
    val faint: Color,
    /** The gold of badges and honours — tertiary's own colour, stated as a plain value. */
    val gold: Color,
    /** The bubble an incoming message sits in: the sunken surface. */
    val bubbleIn: Color,
    /** The $MIG coin accent on the wallet's cards. */
    val coin: Color,
    /** The tab strip's surface: the reference's teal bar in light, its deepened twin in dark. */
    val nav: Color,
    /** The active tab's fill: the accent-bright token, the cyan an active chip wears. */
    val navActive: Color,
    /** The banner gradient, orange into amber — the profile banner's three stops. */
    val bannerA: Color,
    val bannerB: Color,
    val bannerC: Color,
    /** The ink the banner gradient carries: white, on every stop. */
    val bannerInk: Color,
    /** The login gradient, the front door's three cyan stops. */
    val loginA: Color,
    val loginB: Color,
    val loginC: Color,
)

private val ExtraLight = MigoExtra(
    faint = Color(0xFF9AA5A7),
    gold = Color(0xFFD97706),
    bubbleIn = SurfaceSunkenLight,
    coin = Color(0xFFD97706),
    nav = Color(0xFF00838F),
    navActive = AccentBrightLight,
    bannerA = Color(0xFFEA580C),
    bannerB = Color(0xFFF97316),
    bannerC = Color(0xFFF59E0B),
    bannerInk = Color.White,
    loginA = Color(0xFF0093AF),
    loginB = Color(0xFF00ACC1),
    loginC = Color(0xFF00838F),
)

private val ExtraDark = MigoExtra(
    faint = Color(0xFF64808A),
    gold = Color(0xFFFCC419),
    bubbleIn = SurfaceSunkenDark,
    coin = Color(0xFFFCC419),
    nav = Color(0xFF0F3A40),
    navActive = AccentBrightDark,
    bannerA = Color(0xFFEA580C),
    bannerB = Color(0xFFF97316),
    bannerC = Color(0xFFF59E0B),
    bannerInk = Color.White,
    loginA = Color(0xFF0093AF),
    loginB = Color(0xFF00ACC1),
    loginC = Color(0xFF00838F),
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
