package com.migo.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.app.model.gameLabelOf
import com.migo.app.model.playerRangeLabel

/**
 * The Games panel: the node's own catalogue, as a destination of its own.
 *
 * Games are room-scoped and server-authoritative: they are *started inside a conversation* — a
 * room or a group, from the chat header's Games control — and the play happens in the thread, as
 * lines in the transcript and the guessing game's card above the composer. This panel is the
 * browsing half: what this server referees, and the player counts that decide which games can
 * even be started. The list is read from the server rather than fixed in the app, so a node that
 * adds or retires a kind is obeyed, never mis-named.
 */
@Composable
fun GamesScreen(
    state: AppState.SignedIn,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val games = state.games
    Column(modifier = modifier.verticalScroll(rememberScrollState())) {
        ScreenTitle(title = "Games") {
            TextButton(onClick = onRefresh) {
                Text("Refresh")
            }
        }
        Text(
            text = "Refereed by the server, played inside a conversation.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        Spacer(modifier = Modifier.height(12.dp))

        when {
            // Null is "not checked yet", not "nothing": the read's own state draws here so the
            // distinction stays visible — an honest empty beats a confident zero.
            games.catalogue == null -> if (games.loading) {
                LoadingRow()
            } else {
                Text(
                    text = games.failure ?: "The catalogue is not loaded.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                )
                TextButton(onClick = onRefresh, modifier = Modifier.padding(start = 8.dp)) {
                    Text("Try again")
                }
            }

            games.catalogue.isEmpty() -> Placeholder(text = "This server referees no games.")

            else -> for (entry in games.catalogue) {
                GameCard(
                    name = gameLabelOf(entry.kind),
                    players = playerRangeLabel(entry.minPlayers, entry.maxPlayers),
                    startable = entry.minPlayers <= 1L,
                )
            }
        }

        Spacer(modifier = Modifier.height(12.dp))
        Text(
            text = "Open a room's conversation from the Rooms tab and press Games in its header — " +
                "the game plays out in the thread. Only single-player games can be started in this " +
                "build: the wire cannot name an opponent.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        Spacer(modifier = Modifier.height(16.dp))
    }
}

/**
 * One catalogue card: the game's name, the player range the server allows, and — for the
 * multi-player kinds this build cannot start — the reason the header's launcher will not offer it,
 * said here too so the panel and the launcher never disagree.
 */
@Composable
private fun GameCard(name: String, players: String, startable: Boolean) {
    Surface(
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = name,
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Spacer(modifier = Modifier.height(2.dp))
            Text(
                text = if (startable) players else "$players · needs an opponent picker this build lacks",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
