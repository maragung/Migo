package com.migo.app.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/**
 * The app's colours and type.
 *
 * Two schemes written out rather than derived from one seed. Dynamic colour would hand the palette to
 * whatever wallpaper is set, and this app has one place where colour carries meaning rather than
 * decoration: an outgoing message is the primary colour and an incoming one is the surface, which is
 * how somebody reads a thread at a glance. A wallpaper that flattened that contrast would make the
 * conversation harder to read, so the two are fixed here and checked against each other.
 *
 * Both schemes keep body text on its container above the 4.5:1 contrast ratio, which is the reason the
 * dark scheme's primary is lighter than the light scheme's rather than the same hue dimmed.
 */

// The brand hue: indigo, dark enough in the light scheme to carry white text, light enough in the
// dark scheme to carry near-black text.
private val IndigoLight = Color(0xFF4338CA)
private val IndigoDark = Color(0xFFA5B4FC)

// Teal, used only for the presence dot and the unread badge: one accent, so it means something.
private val TealLight = Color(0xFF0F766E)
private val TealDark = Color(0xFF5EEAD4)

private val LightScheme = lightColorScheme(
    primary = IndigoLight,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFE0E7FF),
    onPrimaryContainer = Color(0xFF1E1B4B),
    secondary = TealLight,
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFCCFBF1),
    onSecondaryContainer = Color(0xFF042F2E),
    background = Color(0xFFFBFBFE),
    onBackground = Color(0xFF15161B),
    surface = Color(0xFFFBFBFE),
    onSurface = Color(0xFF15161B),
    surfaceVariant = Color(0xFFE7E7EF),
    onSurfaceVariant = Color(0xFF44464F),
    outline = Color(0xFF757780),
    outlineVariant = Color(0xFFC6C6D0),
    error = Color(0xFFB3261E),
    onError = Color.White,
    errorContainer = Color(0xFFF9DEDC),
    onErrorContainer = Color(0xFF410E0B),
)

private val DarkScheme = darkColorScheme(
    primary = IndigoDark,
    onPrimary = Color(0xFF1E1B4B),
    primaryContainer = Color(0xFF3730A3),
    onPrimaryContainer = Color(0xFFE0E7FF),
    secondary = TealDark,
    onSecondary = Color(0xFF042F2E),
    secondaryContainer = Color(0xFF115E59),
    onSecondaryContainer = Color(0xFFCCFBF1),
    background = Color(0xFF111318),
    onBackground = Color(0xFFE3E2E9),
    surface = Color(0xFF111318),
    onSurface = Color(0xFFE3E2E9),
    surfaceVariant = Color(0xFF44464F),
    onSurfaceVariant = Color(0xFFC5C6D0),
    outline = Color(0xFF8F909A),
    outlineVariant = Color(0xFF44464F),
    error = Color(0xFFF2B8B5),
    onError = Color(0xFF601410),
    errorContainer = Color(0xFF8C1D18),
    onErrorContainer = Color(0xFFF9DEDC),
)

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
        content = content,
    )
}
