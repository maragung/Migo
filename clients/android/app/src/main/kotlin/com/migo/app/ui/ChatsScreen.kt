package com.migo.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.app.model.ConversationRow

/**
 * The Chats tab: every conversation the session holds, in one place.
 *
 * Until this tab existed the list was only ever seen filtered — rooms under Rooms, matches under
 * Search — and a direct chat had no home at all once closed: the only ways back were a friend's
 * Message button or typing the thread's title into Search. The list is the messenger's spine, so
 * it leads the strip; the newest activity sorts to the top, the same order every other list of
 * these rows already agreed on.
 */
@Composable
fun ChatsScreen(
    state: AppState.SignedIn,
    onOpenConversation: (ConversationRow) -> Unit,
    modifier: Modifier = Modifier,
) {
    // Newest activity first — the same ordering Rooms gives "Your rooms", so a conversation never
    // changes places between the two lists that show it.
    val rows = remember(state.conversations) {
        state.conversations.sortedByDescending { it.updatedAt }
    }

    Column(modifier = modifier.fillMaxSize().imePadding()) {
        ScreenTitle(title = "Chats")
        if (rows.isEmpty()) {
            Placeholder(
                text = "No conversations yet.\nStart one from Friends, or join a room.",
                modifier = Modifier.weight(1f),
            )
        } else {
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(rows, key = { it.conversationId.value }) { row ->
                    ChatListRow(row = row, onOpen = { onOpenConversation(row) })
                    HorizontalDivider(color = MaterialTheme.colorScheme.outline)
                }
                item { Spacer(modifier = Modifier.height(16.dp)) }
            }
        }
    }
}

/**
 * One conversation row, in the reference list row's anatomy: the ringed avatar, the bold teal-head
 * name, the last line under it, and — when there is one — the red unread pill on the end. Unread
 * count outranks the preview as colour rather than as text now: a number the user is hunting for
 * beats the words they have not read yet, and red is what they spot first.
 */
@Composable
private fun ChatListRow(row: ConversationRow, onOpen: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 58.dp)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ListRowAvatar(name = row.title)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            ListRowName(text = row.title)
            ListRowLine(text = row.preview ?: "Open the conversation")
        }
        if (row.unread > 0) {
            UnreadPill(count = row.unread)
            Spacer(modifier = Modifier.width(8.dp))
        }
        Button(onClick = onOpen) { Text("Open") }
    }
}
