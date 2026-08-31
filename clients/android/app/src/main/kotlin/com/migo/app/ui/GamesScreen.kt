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
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/**
 * The Games tab: the catalogue the server referees, as a destination of its own.
 *
 * The reference's Games tab is an arcade with a dice table; this build's wire honestly offers
 * something narrower. Games are room-scoped and server-authoritative: they are *started inside a
 * conversation*, and the play happens in the thread. The list is the games domain's own fixed
 * numbering — the same three every other client's launcher offers — so the tab never names a game
 * the server cannot referee.
 */
@Composable
fun GamesScreen(modifier: Modifier = Modifier) {
    Column(modifier = modifier.verticalScroll(rememberScrollState())) {
        ScreenTitle(title = "Games")
        Text(
            text = "Refereed by the server, played inside a conversation.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        Spacer(modifier = Modifier.height(12.dp))

        for ((name, players) in listOf(
            "Tic-tac-toe" to "2 players",
            "Rock paper scissors" to "2 players",
            "Guess the number" to "1 player",
        )) {
            GameCard(name = name, players = players)
        }

        Spacer(modifier = Modifier.height(12.dp))
        Text(
            text = "Open a conversation from the Chats tab and start one from its header — " +
                "the game plays out in the thread.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        Spacer(modifier = Modifier.height(16.dp))
    }
}

/** One catalogue card: the game's name and the player range the server allows. */
@Composable
private fun GameCard(name: String, players: String) {
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
                text = players,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
