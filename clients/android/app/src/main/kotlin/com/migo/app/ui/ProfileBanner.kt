package com.migo.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.migo.core.ConnectionState

/** What the banner's avatar menu was asked for. */
enum class BannerAction { PROFILE, WALLET, ALERTS, SEARCH, SIGN_OUT }

/**
 * The profile banner: the orange strip that carries who is signed in.
 *
 * The gradient is the reference's (orange into amber) and ignores the theme, because the banner is
 * the session's own surface — it says who is here and what they have, the same way in daylight and
 * in the dark. The avatar opens the menu the five tabs cannot carry: the profile, the wallet, the
 * panels, and the way out.
 */
@Composable
fun ProfileBanner(
    username: String,
    connection: ConnectionState,
    balance: Long?,
    onAction: (BannerAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val extra = LocalMigoExtra.current
    var menuOpen by remember { mutableStateOf(false) }

    Box(
        modifier = modifier
            .fillMaxWidth()
            .background(
                brush = Brush.horizontalGradient(
                    listOf(extra.bannerA, extra.bannerB, extra.bannerC),
                ),
            ),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box {
                BannerAvatar(name = username, modifier = Modifier.clickable { menuOpen = true })
                DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                    for ((action, label) in listOf(
                        BannerAction.PROFILE to "My Profile",
                        BannerAction.WALLET to "My Credits & TopUp",
                        BannerAction.ALERTS to "Alerts",
                        BannerAction.SEARCH to "Search",
                        BannerAction.SIGN_OUT to "Exit / Logout",
                    )) {
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
            Spacer(modifier = Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = username,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                    color = extra.bannerInk,
                    maxLines = 1,
                )
                // The connection state in the banner's own ink: a dot and a word, as everywhere
                // else, recoloured here because the surface under it is orange.
                val (label, dot) = when (connection) {
                    ConnectionState.Online -> "Online" to Color(0xFFB9F6CA)
                    ConnectionState.Connecting -> "Connecting" to Color(0xFFFFF3B0)
                    ConnectionState.Reconnecting -> "Reconnecting" to Color(0xFFFFD5D8)
                    ConnectionState.Closed -> "Offline" to Color(0x66FFFFFF)
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(modifier = Modifier.size(7.dp).background(dot, CircleShape))
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        text = label,
                        style = MaterialTheme.typography.labelSmall,
                        color = extra.bannerInk.copy(alpha = 0.9f),
                    )
                }
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
            }
        }
    }
}

/**
 * The banner's avatar: a translucent disc with a white ring and the initial, as the reference
 * draws it. Not [Monogram] — the monogram derives a solid tint from the name, which is right on a
 * list row and wrong on a surface that already has its own strong colour.
 */
@Composable
private fun BannerAvatar(name: String, modifier: Modifier = Modifier) {
    val extra = LocalMigoExtra.current
    val letter = name.trim().firstOrNull()?.uppercase() ?: "?"
    Box(
        modifier = modifier
            .size(40.dp)
            .background(Color.White.copy(alpha = 0.35f), CircleShape)
            .border(
                border = BorderStroke(1.5.dp, Color.White),
                shape = CircleShape,
            ),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = letter,
            style = MaterialTheme.typography.titleMedium,
            fontWeight = FontWeight.Bold,
            color = extra.bannerInk,
            textAlign = TextAlign.Center,
        )
    }
}
