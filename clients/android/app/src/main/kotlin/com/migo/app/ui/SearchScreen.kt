package com.migo.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.core.protocol.RoomSummary
import com.migo.core.protocol.SuggestedUser
import kotlinx.coroutines.delay

/**
 * The Search section: one box, everything it can honestly find.
 *
 * People and rooms answer on the wire (username prefixes and room names); conversations answer
 * locally, from the list the session already holds. The query debounces — a pause is the question,
 * a keystroke is not — and the answers group under the headings a user thinks in.
 */
@Composable
fun SearchScreen(
    state: AppState.SignedIn,
    onQuery: (String) -> Unit,
    onStartDirect: (com.migo.core.wire.Id) -> Unit,
    onJoinRoom: (RoomSummary) -> Unit,
    onOpenConversation: (com.migo.app.model.ConversationRow) -> Unit,
    modifier: Modifier = Modifier,
) {
    var field by rememberSaveable { mutableStateOf(state.search.query) }

    LaunchedEffect(field) {
        delay(300)
        if (field.trim() != state.search.query.trim()) onQuery(field)
    }

    val query = state.search.query.trim()
    val chats = if (query.isEmpty()) {
        emptyList()
    } else {
        state.conversations.filter { it.title.contains(query, ignoreCase = true) }
    }

    Column(modifier = modifier.fillMaxSize().imePadding()) {
        ScreenTitle(title = "Search")
        OutlinedTextField(
            value = field,
            onValueChange = { field = it },
            placeholder = { Text("Search people, rooms, chats") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        )

        if (state.search.loading) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(24.dp),
                horizontalArrangement = androidx.compose.foundation.layout.Arrangement.Center,
            ) { CircularProgressIndicator() }
        } else if (query.isEmpty()) {
            Placeholder(text = "Find people by username, rooms by name,\nand your chats by title.", modifier = Modifier.weight(1f))
        } else if (
            state.search.people?.isEmpty() == true &&
            state.search.rooms?.isEmpty() == true &&
            chats.isEmpty()
        ) {
            Placeholder(text = "Nothing found for \"$query\".", modifier = Modifier.weight(1f))
        } else {
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                if (chats.isNotEmpty()) {
                    item { SectionLabel(text = "Chats") }
                    items(chats, key = { it.conversationId.value }) { row ->
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Monogram(name = row.title, size = 36.dp)
                            Spacer(modifier = Modifier.width(10.dp))
                            Column(modifier = Modifier.weight(1f)) {
                                Text(row.title, style = MaterialTheme.typography.titleMedium, maxLines = 1)
                                OneLine(text = row.preview ?: "Open conversation")
                            }
                            TextButton(onClick = { onOpenConversation(row) }) { Text("Open") }
                        }
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    }
                }
                val people: List<SuggestedUser> = state.search.people.orEmpty()
                if (people.isNotEmpty()) {
                    item { SectionLabel(text = "People") }
                    items(people, key = { it.accountId.value }) { person ->
                        PersonSummaryRow(
                            name = person.displayName,
                            handle = person.username,
                            note = if (person.mutualFriends > 0) "${person.mutualFriends} mutual" else null,
                            action = "Message",
                            onAction = { onStartDirect(person.accountId) },
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    }
                }
                val rooms: List<RoomSummary> = state.search.rooms.orEmpty()
                if (rooms.isNotEmpty()) {
                    item { SectionLabel(text = "Rooms") }
                    items(rooms, key = { it.roomId.value }) { room ->
                        RoomSummaryRow(room = room, onJoin = { onJoinRoom(room) })
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    }
                }
                item { Spacer(modifier = Modifier.height(16.dp)) }
            }
        }
    }
}
