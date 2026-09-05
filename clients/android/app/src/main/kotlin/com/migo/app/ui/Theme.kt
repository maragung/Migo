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
 * The app's colours and type — the Migo flat design language, mapped to Material.
 *
 * Solid colours only: no gradients, no glossy highlights, no inset shadows. Separation comes from
 * 1px borders in the teal line colour and a single soft elevation shadow on floating surfaces.
 * The light scheme is the canonical palette — a teal accent over a soft teal page ground, white
 * cards, the flat orange banner above both — and the dark scheme is the same family carried into
 * deep teal surfaces with a brighter teal accent. Dynamic colour would hand the palette to
 * whatever wallpaper is set, and this app has one place where colour carries meaning rather than
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
 * would not follow the theme. The banner and the login ground hold the same flat values in both
 * themes (the front door does not change with the lights): the banner is the one flat orange, the
 * login ground the one flat turquoise, each stated as three equal stops so the call sites that
 * once painted a gradient now paint a flat field without needing edits of their own.
 */

// The accent — the restyle's teal in light, its brighter twin in dark. Both carry white ink.
private val AccentLight = Color(0xFF1287A0)
private val AccentDark = Color(0xFF1FA5C0)

// The Migo surfaces, straight from the restyle's palette: white cards on the soft teal page
// ground in light; deep teal surfaces over a darker ground in dark.
private val SurfaceLight = Color(0xFFFFFFFF)
private val PageGroundLight = Color(0xFFEEF7FA)
private val SurfaceDark = Color(0xFF0C3A46)
private val PageGroundDark = Color(0xFF072A33)
private val SurfaceVariantDark = Color(0xFF114B5A)

private val LightScheme = lightColorScheme(
    primary = AccentLight,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFCDE9F0),
    onPrimaryContainer = Color(0xFF0D4353),
    secondary = Color(0xFF3FCE6B),
    onSecondary = Color(0xFF06230F),
    secondaryContainer = Color(0xFFD9F5E4),
    onSecondaryContainer = Color(0xFF0B3D1F),
    tertiary = Color(0xFFF5820C),
    onTertiary = Color.White,
    tertiaryContainer = Color(0xFFFDEEE0),
    onTertiaryContainer = Color(0xFF7A3D05),
    background = PageGroundLight,
    onBackground = Color(0xFF134E5E),
    surface = SurfaceLight,
    onSurface = Color(0xFF134E5E),
    surfaceVariant = PageGroundLight,
    onSurfaceVariant = Color(0xFF5F8A99),
    outline = Color(0xFFCFE3EA),
    outlineVariant = Color(0xFFE3F0F4),
    error = Color(0xFFE5503C),
    onError = Color.White,
    errorContainer = Color(0xFFFCE4E0),
    onErrorContainer = Color(0xFF8A2A1E),
)

private val DarkScheme = darkColorScheme(
    primary = AccentDark,
    onPrimary = Color.White,
    primaryContainer = Color(0xFF0E4A58),
    onPrimaryContainer = Color(0xFFCDEEF5),
    secondary = Color(0xFF52DE7E),
    onSecondary = Color(0xFF06230F),
    secondaryContainer = Color(0xFF0B4A26),
    onSecondaryContainer = Color(0xFFC8F7D9),
    tertiary = Color(0xFFF5820C),
    onTertiary = Color.White,
    tertiaryContainer = Color(0xFF5C3A10),
    onTertiaryContainer = Color(0xFFFFE0B8),
    background = PageGroundDark,
    onBackground = Color(0xFFE6F4F8),
    surface = SurfaceDark,
    onSurface = Color(0xFFE6F4F8),
    surfaceVariant = SurfaceVariantDark,
    onSurfaceVariant = Color(0xFFA3C4CD),
    outline = Color(0xFF1A5866),
    outlineVariant = Color(0xFF12414E),
    error = Color(0xFFFF7A68),
    onError = Color(0xFF3B0A05),
    errorContainer = Color(0xFF5C1A12),
    onErrorContainer = Color(0xFFFFD5CE),
)

/**
 * The tokens Material's scheme has no slot for, themed light and dark like the rest.
 *
 * The banner and login grounds hold the same values in both themes, so a screen that paints them
 * never has to ask which theme it is in — the front door is the one surface that ignores the
 * lights. Each is stated as three equal stops because the call sites still paint a three-stop
 * brush: equal stops make the brush flat, which is the restyle's rule.
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
    /** The tab strip's surface: the deep teal bar, the same in both themes. */
    val nav: Color,
    /** The active tab's fill: a solid white pill, carrying the teal-head ink. */
    val navActive: Color,
    /** The banner band, flat orange — the profile banner's three equal stops. */
    val bannerA: Color,
    val bannerB: Color,
    val bannerC: Color,
    /** The ink the banner band carries: white, on every stop. */
    val bannerInk: Color,
    /** The login ground, flat turquoise — the front door's three equal stops. */
    val loginA: Color,
    val loginB: Color,
    val loginC: Color,
)

private val ExtraLight = MigoExtra(
    faint = Color(0xFF8FB0BB),
    gold = Color(0xFFF0A912),
    bubbleIn = PageGroundLight,
    coin = Color(0xFFF0A912),
    nav = Color(0xFF0D4353),
    navActive = Color.White,
    bannerA = Color(0xFFF5820C),
    bannerB = Color(0xFFF5820C),
    bannerC = Color(0xFFF5820C),
    bannerInk = Color.White,
    loginA = Color(0xFF0F96AD),
    loginB = Color(0xFF0F96AD),
    loginC = Color(0xFF0F96AD),
)

private val ExtraDark = MigoExtra(
    faint = Color(0xFF7BA3AD),
    gold = Color(0xFFF0A912),
    bubbleIn = SurfaceVariantDark,
    coin = Color(0xFFF0A912),
    nav = Color(0xFF0D4353),
    navActive = Color.White,
    bannerA = Color(0xFFF5820C),
    bannerB = Color(0xFFF5820C),
    bannerC = Color(0xFFF5820C),
    bannerInk = Color.White,
    loginA = Color(0xFF0F96AD),
    loginB = Color(0xFF0F96AD),
    loginC = Color(0xFF0F96AD),
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
        // Message text: the default body size, with the line height opened up, because a transcript
        // is a narrow measure and tight leading is what makes long messages hard to scan.
        bodyLarge = base.bodyLarge.copy(lineHeight = 22.sp),
        // Timestamps and the sender name on a transcript line.
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
