package com.migo.app.ui

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.migo.core.ConnectionState

/** What the banner's avatar menu was asked for. */
enum class BannerAction { PROFILE, WALLET, ALERTS, SEARCH, ADMINS, SIGN_OUT }

/**
 * The profile banner: the flat orange "me card" that carries who is signed in.
 *
 * The band is the reference's one flat orange and ignores the theme, because the banner is the
 * session's own surface — it says who is here and what they have, the same way in daylight and in
 * the dark. The avatar opens the menu the five tabs cannot carry: the profile, the wallet, the
 * panels, and the way out.
 *
 * [owner] gates the Admins entry, which the sign-in standing check answers: the management
 * page's whole point is that its existence is not public information, and the server refuses
 * every read and write there for anybody else anyway — so a non-owner never sees the word.
 */
@Composable
fun ProfileBanner(
    username: String,
    connection: ConnectionState,
    balance: Long?,
    owner: Boolean,
    onAction: (BannerAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val extra = LocalMigoExtra.current
    var menuOpen by remember { mutableStateOf(false) }
    // The away toggle is the banner's own: a local choice this build's state has no field for yet,
    // so it lives here rather than being wired to anything the wire would have to agree with.
    var away by remember { mutableStateOf(false) }

    Box(
        modifier = modifier
            .fillMaxWidth()
            .height(71.dp)
            .background(
                brush = Brush.horizontalGradient(
                    listOf(extra.bannerA, extra.bannerB, extra.bannerC),
                ),
            ),
        contentAlignment = Alignment.Center,
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box {
                // The avatar's clickable is padded past the disc's own 51dp: this disc is the only
                // entry to Profile, Wallet, Alerts, Search and sign-out, and a control that carries
                // the whole account menu clears the 48dp touch minimum rather than the avatar's
                // visual size.
                BannerAvatar(
                    name = username,
                    modifier = Modifier
                        .clickable { menuOpen = true }
                        .padding(3.dp),
                )
                DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                    // Built as a list of (action, label) pairs, then the owner's own entry
                    // appended before the way out — a conditional inside a loop would draw it
                    // in place, but this reads as the menu's shape: five entries for
                    // everybody, a sixth for the one account that owns the deployment.
                    val entries = buildList {
                        add(BannerAction.PROFILE to "My Profile")
                        add(BannerAction.WALLET to "My Credits & TopUp")
                        add(BannerAction.ALERTS to "Alerts")
                        add(BannerAction.SEARCH to "Search")
                        if (owner) add(BannerAction.ADMINS to "Global Admins")
                        add(BannerAction.SIGN_OUT to "Exit / Logout")
                    }
                    for ((action, label) in entries) {
                        DropdownMenuItem(
                            text = {
                                Text(
                                    text = label,
                                    color = if (action == BannerAction.SIGN_OUT) {
                                        MaterialTheme.colorScheme.error
                                    } else {
                                        MaterialTheme.colorScheme.onSurface
                                    },
                                )
                            },
                            onClick = {
                                menuOpen = false
                                onAction(action)
                            },
                        )
                    }
                }
            }
            Spacer(modifier = Modifier.width(10.dp))
            Column(modifier = Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    // The live dot: a slow pulse rather than a blink fast enough to read as an
                    // alarm. It says "here", not "look at me".
                    val pulse = rememberInfiniteTransition(label = "banner-dot")
                    val dotAlpha by pulse.animateFloat(
                        initialValue = 1f,
                        targetValue = 0.35f,
                        animationSpec = infiniteRepeatable(
                            animation = tween(1400),
                            repeatMode = RepeatMode.Reverse,
                        ),
                        label = "banner-dot-alpha",
                    )
                    Box(
                        modifier = Modifier
                            .size(8.dp)
                            .background(Color(0xFF3FCE6B).copy(alpha = dotAlpha), CircleShape),
                    )
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        text = username,
                        fontSize = 14.sp,
                        fontWeight = FontWeight.Bold,
                        color = extra.bannerInk,
                        maxLines = 1,
                    )
                }
                Text(
                    text = "@$username",
                    fontSize = 11.sp,
                    fontStyle = FontStyle.Italic,
                    color = extra.bannerInk.copy(alpha = 0.85f),
                    maxLines = 1,
                )
                // The status line. This build's banner has no status of its own to show yet, so
                // the line keeps its place with the reference's first-day wording; tapping it
                // opens the profile panel, where the status the profile screen edits lives.
                Text(
                    text = "New here! Say hi :)",
                    fontSize = 11.5.sp,
                    fontStyle = FontStyle.Italic,
                    color = extra.bannerInk.copy(alpha = 0.95f),
                    maxLines = 1,
                    modifier = Modifier.clickable { onAction(BannerAction.PROFILE) },
                )
            }
            if (balance != null) {
                Surface(
                    color = Color.White.copy(alpha = 0.2f),
                    contentColor = extra.bannerInk,
                    shape = RoundedCornerShape(999.dp),
                ) {
                    Text(
                        text = "$balance \$MIG",
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.Bold,
                        modifier = Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
                    )
                }
                Spacer(modifier = Modifier.width(8.dp))
            }
            // The presence chip: the connection state as one word and a caret, on the band's own
            // darker orange. A glance at the chip is the same glance the dot and the word were.
            val label = when (connection) {
                ConnectionState.Online -> "Online"
                ConnectionState.Connecting -> "Connecting"
                ConnectionState.Reconnecting -> "Reconnecting"
                ConnectionState.Closed -> "Offline"
            }
            Surface(
                color = Color(0xFFD2690B),
                contentColor = extra.bannerInk,
                shape = RoundedCornerShape(999.dp),
            ) {
                Text(
                    text = "$label ▾",
                    fontSize = 11.5.sp,
                    fontWeight = FontWeight.Bold,
                    modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                )
            }
            Spacer(modifier = Modifier.width(8.dp))
            // The away toggle: a 28dp moon, faint when off and the white pill with the orange
            // glyph when on.
            Box(
                modifier = Modifier
                    .size(28.dp)
                    .background(
                        color = if (away) Color.White else Color.White.copy(alpha = 0.22f),
                        shape = RoundedCornerShape(8.dp),
                    )
                    .clickable { away = !away },
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = "☾",
                    fontSize = 14.sp,
                    color = if (away) Color(0xFFF5820C) else Color.White.copy(alpha = 0.8f),
                    textAlign = TextAlign.Center,
                )
            }
        }
    }
}

/**
 * The banner's avatar: the 42dp disc, the green ring around it, and the white halo outside that —
 * three backgrounds and nothing else, drawn that way rather than with strokes so the halo reads as
 * the flat design's cut-out rather than a shadow. Not [Monogram] — the monogram derives a solid
 * tint from the name, which is right on a list row and wrong on a surface that already has its
 * own strong colour.
 */
@Composable
private fun BannerAvatar(name: String, modifier: Modifier = Modifier) {
    val letter = name.trim().firstOrNull()?.uppercase() ?: "?"
    Box(
        modifier = modifier
            .size(51.dp)
            .background(Color.White.copy(alpha = 0.85f), CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .size(47.dp)
                .background(Color(0xFF3FCE6B), CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            Box(
                modifier = Modifier
                    .size(42.dp)
                    .background(Color.White, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = letter,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                    color = Color(0xFF0D4353),
                    textAlign = TextAlign.Center,
                )
            }
        }
    }
}
