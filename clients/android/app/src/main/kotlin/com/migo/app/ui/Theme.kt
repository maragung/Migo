package com.migo.app.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
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
    /**
     * The list rows' name ink: the teal head the reference puts on a row's first line.
     *
     * A token rather than a value read from the system's dark setting, which is what the row
     * helpers used to do: with the colours here, a screen rendered under an explicit
     * `MigoTheme(dark = false)` keeps light ink even on a dark device, instead of the rows
     * disagreeing with every other surface around them.
     */
    val rowName: Color,
    /** The list rows' second line: the quieter ink under a name. */
    val rowLine: Color,
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
    rowName = Color(0xFF0D6373),
    rowLine = Color(0xFF5F8A99),
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
    rowName = Color(0xFF9ADCE8),
    rowLine = Color(0xFFA3C4CD),
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
 * The corner radii, named after the web client's own tokens so the two clients round the same
 * things by the same amount.
 *
 * The web stylesheet declares exactly three radii — `--radius-sm: 4px`, `--radius: 6px`, and
 * `--radius-lg: 12px` — plus a fully-rounded pill, and this object is those four by their web
 * names. The reason a named scale exists at all is that the screens had eight different corner
 * values between them (8, 9, 10, 12, 14, 16, 24 and the pill, as bare literals), which is not a
 * design decision anybody made: it is what happens when every screen picks its own number.
 *
 * [pill] is 999 rather than 50% because Compose clamps a corner radius to half the shorter side, so
 * the two spell the same shape while this one needs no measurement to write.
 */
object MigoRadius {
    /** Inputs, buttons and the small chips — the web's `--radius-sm`. */
    val sm = 4.dp

    /** The default surface corner — the web's `--radius`. */
    val md = 6.dp

    /** Cards, sheets, dialogs and bubbles — the web's `--radius-lg`. */
    val lg = 12.dp

    /** Badges, avatars and anything that should read as a pill. */
    val pill = 999.dp
}

/**
 * The Material shape slots, pointed at [MigoRadius].
 *
 * This is the one place where not saying anything was itself a visible choice: with no `shapes`
 * handed to [MaterialTheme], every Material 3 component falls back to the library's own scale
 * (4/8/12/16/28dp), and that scale is far rounder than this product's. A filled `Button` drew at a
 * 12dp corner while every hand-built chip beside it was drawn at 8 or 9 — so a screen holding both
 * showed two corner languages at once, and the Material half was the one that did not look like the
 * web client, whose buttons are 4px. Wiring the slots here is what makes a `Button`, a `Card`, a
 * `TextField` and an `AlertDialog` round like the rest of Migo instead of like Material's demo.
 */
private val MigoShapes = Shapes(
    extraSmall = RoundedCornerShape(MigoRadius.sm),
    small = RoundedCornerShape(MigoRadius.sm),
    medium = RoundedCornerShape(MigoRadius.md),
    large = RoundedCornerShape(MigoRadius.lg),
    extraLarge = RoundedCornerShape(MigoRadius.lg),
)

/**
 * The type scale, as the web client names it.
 *
 * The web stylesheet declares seven steps — micro 10.5, meta 11.5, bodySm 11, body 12, titleSm 14,
 * title 16, display 20 — and the screens here had drifted into thirteen sizes of their own
 * (8.5, 9.5, 11, 11.5, 12, 13, 13.5, 14, 15, 16, 18, 20, 26) with no rule saying which was which.
 * This is the web's seven plus one: [label], which the front door's form captions need and the web
 * solves with its own 13px literal. A size is now a name a reader can look up rather than a number
 * each screen re-decided.
 */
object MigoType {
    /** Micro-labels: the smallest step, for counts and dense chrome. */
    val micro = 10.5.sp

    /** Metadata: timestamps, quiet second lines. */
    val meta = 11.5.sp

    /** The small body step, for dense secondary text. */
    val bodySm = 11.sp

    /** The body step: message text and ordinary copy. */
    val body = 12.sp

    /** A form label: the bold caption above an input, a step up from body for weight's sake. */
    val label = 13.sp

    /** A small title: list-row names and section headers. */
    val titleSm = 14.sp

    /** A title: conversation headers and panel titles. */
    val title = 16.sp

    /** The display step: screen and empty-state headings. */
    val display = 20.sp
}

/**
 * The sizes for characters used *as icons* — an emoji, a chevron, a check mark, the "✕" on a tab.
 *
 * These are not type steps and forcing them onto [MigoType] would be a mistake: a chevron is sized
 * to sit optically beside a line of text, not to be read as text, and the two scales move for
 * different reasons. They are collected here because they had the same problem the type scale did —
 * the same glyph written at 15, 18 and 26sp across a dozen files, every one of them a bare number.
 */
object MigoGlyph {
    /** A mark inside a list row or a chip: the check on a picked row, a sheet's row glyph. */
    val small = 15.sp

    /** The workhorse: chevrons, emoji and the glyphs that lead into a row. */
    val inline = 18.sp

    /** A call-control button's glyph, sized for a thumb rather than a line of text. */
    val control = 26.sp

    /** A count inside a badge — the strip's "9+", matching the web client's own 8.5px badge. */
    val badge = 8.5.sp
}

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
        shapes = MigoShapes,
    ) {
        androidx.compose.runtime.CompositionLocalProvider(
            LocalMigoExtra provides if (dark) ExtraDark else ExtraLight,
        ) {
            content()
        }
    }
}
