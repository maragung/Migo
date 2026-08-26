package com.migo.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.app.model.ConversationRow
import com.migo.core.protocol.ConversationKind
import com.migo.core.wire.Id

/**
 * The conversation list: the screen this app opens on once somebody is signed in.
 *
 * # A hand-built header rather than `TopAppBar`
 *
 * Material's app bar is still behind an experimental opt-in, and this one has to hold three things
 * that are not a title -- the account name, the live connection state, and sign-out. A `Surface` and a
 * `Row` express that without an opt-in that could change under the app on a library upgrade.
 */
@Composable
fun ConversationsScreen(
    state: AppState.SignedIn,
    onOpen: (Id, String) -> Unit,
    onRefresh: () -> Unit,
    onStartDirect: (String) -> Unit,
    onSignOut: () -> Unit,
    onDismissFailure: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var composing by rememberSaveable { mutableStateOf(false) }
    var peer by rememberSaveable { mutableStateOf("") }

    Column(modifier = modifier.fillMaxSize()) {
        ListHeader(state = state, onSignOut = onSignOut)
        ErrorBanner(message = state.failure, onDismiss = onDismissFailure)
        if (state.loading) {
            LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
        }

        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(onClick = { composing = !composing }) {
                Text(if (composing) "Cancel" else "New chat")
            }
            Spacer(modifier = Modifier.weight(1f))
            TextButton(onClick = onRefresh, enabled = !state.loading) {
                Text("Refresh")
            }
        }

        if (composing) {
            NewChatRow(
                peer = peer,
                onPeer = { peer = it },
                onStart = {
                    onStartDirect(peer)
                    peer = ""
                    composing = false
                },
            )
        }

        if (state.conversations.isEmpty() && !state.loading) {
            Placeholder(
                text = "No conversations yet.\nStart one with somebody's account id.",
                modifier = Modifier.weight(1f),
            )
        } else {
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(state.conversations, key = { it.conversationId.value }) { row ->
                    ConversationItem(row = row, onOpen = { onOpen(row.conversationId, row.title) })
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                }
            }
        }
    }
}

@Composable
private fun ListHeader(state: AppState.SignedIn, onSignOut: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surfaceVariant) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Monogram(name = state.username, size = 40.dp)
            Spacer(modifier = Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = state.username,
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                ConnectionBadge(state = state.connection)
            }
            TextButton(onClick = onSignOut) {
                Text("Sign out", color = MaterialTheme.colorScheme.error)
            }
        }
    }
}

/**
 * The field for starting a conversation.
 *
 * An account id, not a name, and the hint says so. There is no directory endpoint in this protocol
 * version -- the social opcodes are still spec -- so a field that took a username would be a field
 * that always failed, which is worse than one that asks for something awkward and works.
 */
@Composable
private fun NewChatRow(peer: String, onPeer: (String) -> Unit, onStart: () -> Unit) {
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp).imePadding()) {
        OutlinedTextField(
            value = peer,
            onValueChange = onPeer,
            label = { Text("Account id") },
            placeholder = { Text("00000000-0000-0000-0000-000000000000") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Done,
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(modifier = Modifier.height(8.dp))
        EndRow {
            Button(onClick = onStart, enabled = peer.isNotBlank()) {
                Text("Start")
            }
        }
        Spacer(modifier = Modifier.height(8.dp))
    }
}

@Composable
private fun ConversationItem(row: ConversationRow, onOpen: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onOpen)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Monogram(name = row.title)
        Spacer(modifier = Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = row.title,
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.weight(1f, fill = false),
                )
                if (row.kind != ConversationKind.Direct) {
                    Spacer(modifier = Modifier.width(6.dp))
                    KindTag(kind = row.kind)
                }
            }
            row.preview?.let {
                Spacer(modifier = Modifier.height(2.dp))
                OneLine(text = it)
            }
        }
        Spacer(modifier = Modifier.width(8.dp))
        Column(horizontalAlignment = Alignment.End) {
            Text(
                text = clockTime(row.updatedAt),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (row.unread > 0) {
                Spacer(modifier = Modifier.height(4.dp))
                UnreadBadge(count = row.unread)
            }
        }
    }
}

/** The word "Group" or "Room", for a conversation that is not one other person. */
@Composable
private fun KindTag(kind: ConversationKind) {
    val label = when (kind) {
        ConversationKind.Group -> "Group"
        ConversationKind.Room -> "Room"
        else -> return
    }
    Surface(
        color = MaterialTheme.colorScheme.primaryContainer,
        contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
        shape = RoundedCornerShape(4.dp),
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(horizontal = 5.dp, vertical = 1.dp),
        )
    }
}

/**
 * The unread count.
 *
 * Capped at "99+" because the badge is a circle and a four-digit number in it is unreadable. The real
 * count is still the number the state holds; this only limits what is drawn.
 */
@Composable
private fun UnreadBadge(count: Long) {
    Box(
        modifier = Modifier
            .size(20.dp)
            .background(MaterialTheme.colorScheme.secondary, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = if (count > 99) "99+" else count.toString(),
            style = MaterialTheme.typography.labelSmall,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onSecondary,
        )
    }
}
