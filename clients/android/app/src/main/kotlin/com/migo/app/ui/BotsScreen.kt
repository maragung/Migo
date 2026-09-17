package com.migo.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.app.model.BotReveal
import com.migo.core.domain.BotsDomain
import com.migo.core.protocol.BotView
import com.migo.core.wire.Id

/**
 * The Bots panel: the accounts this person runs.
 *
 * Section 41 asks for a bot surface a developer can build against and this client had none of it:
 * the SDK could register a bot, rotate its token, pause it, and set its permissions, and a person
 * holding the phone could do none of those things. This is that surface, and the web client's Bots
 * window is its twin — the same four controls, the same words for the same permission, because a
 * person who set a bot's permissions on the desk must recognise the list on the phone.
 *
 * Three things here are shaped by the wire rather than by taste:
 *
 *   * **A token is shown once.** Only a keyed tag is stored on the node, so a reply that is lost is
 *     a credential that is lost. The token therefore never enters the list — it appears in one card
 *     above everything, with the copy button and the warning — and rotating again is what a person
 *     does when they missed it, rather than a "show token" that would have to be absent to be
 *     honest.
 *   * **Permissions are replaced, never merged.** The picker holds a whole set and Save sends that
 *     whole set, which is what makes two screens editing one bot agree rather than interleave into
 *     a union neither owner chose.
 *   * **A paused bot is drawn from the flag the node sent.** Both tagged fields may be absent on a
 *     node that predates them, and this panel says "this server did not say" rather than drawing an
 *     active bot holding nothing — the difference between an old node and an off bot is one a
 *     management screen has to keep.
 *
 * Rotation takes two taps, because the old token stops working the moment the node answers and a
 * running bot broken by a mis-tap is a bot nobody can bring back without the new token.
 */
@Composable
fun BotsScreen(
    state: AppState.SignedIn,
    onRegister: (String, String) -> Unit,
    onPause: (Id, Boolean) -> Unit,
    onRotate: (Id) -> Unit,
    onConfirmRotate: (Id?) -> Unit,
    onToggleEditor: (Id) -> Unit,
    onSaveScopes: (Id, List<String>) -> Unit,
    onDismissReveal: () -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val bots = state.bots
    val clipboard = LocalClipboardManager.current

    Column(modifier = modifier.verticalScroll(rememberScrollState())) {
        ScreenTitle(title = "Bots") {
            TextButton(onClick = onRefresh) {
                Text("Refresh")
            }
        }
        Text(
            text = "A bot is an account you run. It signs in with a token, speaks as itself in the " +
                "conversations it joins, and holds exactly the permissions you give it — which " +
                "start at none.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        Spacer(modifier = Modifier.height(12.dp))

        // The one-time reveal, above everything: a token that has to be scrolled to is a token
        // somebody rotates again for no reason.
        bots.reveal?.let { reveal ->
            RevealCard(
                reveal = reveal,
                onCopy = { clipboard.setText(AnnotatedString(reveal.token)) },
                onDismiss = onDismissReveal,
            )
        }

        RegisterBotCard(onRegister = onRegister, busy = bots.loading)

        if (bots.events.isNotEmpty()) {
            Spacer(modifier = Modifier.height(8.dp))
            for (note in bots.events) {
                Text(
                    text = nameOf(bots.bots, note.botId) + ": " + note.event,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 1.dp),
                )
            }
        }

        Spacer(modifier = Modifier.height(8.dp))

        when {
            // Null is "not checked yet", not "nothing": the read's own state draws here so the
            // distinction stays visible — an honest empty beats a confident zero.
            bots.bots == null -> if (bots.loading) {
                LoadingRow()
            } else {
                Text(
                    text = bots.failure ?: "The list is not loaded.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                )
                TextButton(onClick = onRefresh, modifier = Modifier.padding(start = 8.dp)) {
                    Text("Try again")
                }
            }

            bots.bots.isEmpty() -> Placeholder(
                text = "No bots yet. Register one above and give it the permissions it needs — " +
                    "nothing more.",
            )

            else -> for (bot in bots.bots) {
                BotCard(
                    bot = bot,
                    confirming = bots.confirming == bot.botId,
                    editing = bots.editing == bot.botId,
                    onPause = { onPause(bot.botId, bot.paused != true) },
                    onAskRotate = { onConfirmRotate(bot.botId) },
                    onRotate = { onRotate(bot.botId) },
                    onToggleEditor = { onToggleEditor(bot.botId) },
                    onSaveScopes = { scopes -> onSaveScopes(bot.botId, scopes) },
                )
            }
        }

        Spacer(modifier = Modifier.height(16.dp))
    }
}

/**
 * The one-time token card.
 *
 * The loudest thing on the screen on purpose: the value exists once, so the card says so in the
 * same breath as it shows it, and the copy button is the first thing under the token rather than a
 * gesture on the text.
 */
@Composable
private fun RevealCard(reveal: BotReveal, onCopy: () -> Unit, onDismiss: () -> Unit) {
    Surface(
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.primary),
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = (if (reveal.rotated) "New token for " else "Token for ") + reveal.name,
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Spacer(modifier = Modifier.height(4.dp))
            Text(
                text = "This is the only time it is shown. The server stores a tag of it, not the " +
                    "token, so nobody — including this app — can print it again. Put it somewhere " +
                    "safe now; if you lose it, rotate to mint another.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                text = reveal.token,
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
                color = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(modifier = Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Button(onClick = onCopy) {
                    Text("Copy token")
                }
                TextButton(onClick = onDismiss, modifier = Modifier.padding(start = 8.dp)) {
                    Text("I have saved it")
                }
            }
        }
    }
}

/** The register form: a handle and a display name, and nothing else. */
@Composable
private fun RegisterBotCard(onRegister: (String, String) -> Unit, busy: Boolean) {
    var username by remember { mutableStateOf("") }
    var displayName by remember { mutableStateOf("") }
    Surface(
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = "Register a bot",
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Spacer(modifier = Modifier.height(8.dp))
            OutlinedTextField(
                value = username,
                onValueChange = { username = it },
                label = { Text("Handle") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(modifier = Modifier.height(8.dp))
            OutlinedTextField(
                value = displayName,
                onValueChange = { displayName = it },
                label = { Text("Display name") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(modifier = Modifier.height(4.dp))
            Text(
                text = "The handle is the account it signs in with — lowercase letters, digits, " +
                    "dots and underscores, and it has to be free. The display name is what people " +
                    "see beside its messages.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(modifier = Modifier.height(8.dp))
            Button(
                onClick = { onRegister(username, displayName) },
                enabled = !busy && username.isNotBlank() && displayName.isNotBlank(),
            ) {
                Text(if (busy) "Creating…" else "Create bot")
            }
        }
    }
}

/**
 * One bot: what it is called, whether it is paused, what it may do, and the three controls that
 * change any of that. The permission picker opens in place, so the row being edited is the row
 * being read.
 */
@Composable
private fun BotCard(
    bot: BotView,
    confirming: Boolean,
    editing: Boolean,
    onPause: () -> Unit,
    onAskRotate: () -> Unit,
    onRotate: () -> Unit,
    onToggleEditor: () -> Unit,
    onSaveScopes: (List<String>) -> Unit,
) {
    Surface(
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                BotBadge()
                Text(
                    text = bot.name,
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.weight(1f).padding(start = 8.dp),
                )
                Text(
                    text = if (bot.paused == true) "Paused" else "Active",
                    style = MaterialTheme.typography.labelMedium,
                    color = if (bot.paused == true) {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    } else {
                        MaterialTheme.colorScheme.primary
                    },
                )
            }
            Spacer(modifier = Modifier.height(4.dp))
            Text(
                text = when {
                    bot.scopes == null -> "This server did not say what this bot may do."
                    bot.scopes.isEmpty() -> "No permissions."
                    else -> bot.scopes.joinToString(" · ") { scopeLabel(it) }
                },
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(modifier = Modifier.height(4.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = onPause) {
                    Text(if (bot.paused == true) "Resume" else "Pause")
                }
                TextButton(onClick = onToggleEditor) {
                    Text(if (editing) "Close permissions" else "Permissions")
                }
                // The two-tap rotation: the first tap asks, the second does. A single tap here
                // would break whatever is running on a mis-tap.
                if (confirming) {
                    TextButton(onClick = onRotate) {
                        Text("Rotate now — the old token stops working")
                    }
                } else {
                    TextButton(onClick = onAskRotate) {
                        Text("New token")
                    }
                }
            }
            if (editing) {
                ScopePicker(held = bot.scopes, onSave = onSaveScopes)
            }
        }
    }
}

/**
 * The permission picker: a whole set, saved whole.
 *
 * [held] is what the node said the bot holds, and null means the server did not say — in which case
 * nothing is ticked and the hint says so, because a picker that silently ticks nothing under a
 * server that reported nothing is a picker that lies about what the save will replace.
 */
@Composable
private fun ScopePicker(held: List<String>?, onSave: (List<String>) -> Unit) {
    var chosen by remember(held) { mutableStateOf(held.orEmpty().toSet()) }
    Column(modifier = Modifier.fillMaxWidth().padding(top = 4.dp)) {
        if (held == null) {
            Text(
                text = "This server did not report what the bot holds, so nothing is ticked. " +
                    "Saving replaces whatever it holds with exactly what is ticked here.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        for (slug in BotsDomain.BOT_SCOPES) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Checkbox(
                    checked = chosen.contains(slug),
                    onCheckedChange = { ticked ->
                        chosen = if (ticked) chosen + slug else chosen - slug
                    },
                )
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = scopeLabel(slug),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurface,
                    )
                    Text(
                        text = scopeHint(slug),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
        Spacer(modifier = Modifier.height(4.dp))
        Button(
            // In the vocabulary's own order, so what is sent is what the list showed, whatever
            // order the taps happened in.
            onClick = { onSave(BotsDomain.BOT_SCOPES.filter { chosen.contains(it) }) },
            modifier = Modifier.align(Alignment.End),
        ) {
            Text("Save permissions")
        }
    }
}

/**
 * What a permission means, in the words a person deciding would use.
 *
 * The slugs are the wire's vocabulary and never change, but nobody granting authority should have
 * to read `send_announcements` and guess at blast radius: the label is the verb, the hint is what
 * holding it lets a bot do. The web client's picker uses the same two lines.
 */
private fun scopeLabel(slug: String): String = when (slug) {
    "read_messages" -> "Read messages"
    "send_messages" -> "Send messages"
    "moderate" -> "Moderate"
    "manage_games" -> "Manage games"
    "read_members" -> "Read members"
    "send_announcements" -> "Send announcements"
    // A slug this build does not name is still shown as itself rather than dropped: the node is
    // the authority on which slugs exist, and hiding one it accepts would be this app pretending
    // a permission it cannot describe is not there.
    else -> slug
}

private fun scopeHint(slug: String): String = when (slug) {
    "read_messages" -> "See the content of messages it is a member of."
    "send_messages" -> "Post messages as itself, on the ordinary messaging path."
    "moderate" -> "Act on members and content in the rooms it belongs to."
    "manage_games" -> "Start, join, and end games in its conversations."
    "read_members" -> "See who is in its conversations and rooms."
    "send_announcements" -> "Post to a room regardless of who has muted it."
    else -> "A permission this build does not describe."
}

/** A bot's display name from the list in hand, or a neutral stand-in when the list lacks it. */
private fun nameOf(bots: List<BotView>?, botId: Id): String =
    bots?.firstOrNull { it.botId == botId }?.name ?: "A bot"
