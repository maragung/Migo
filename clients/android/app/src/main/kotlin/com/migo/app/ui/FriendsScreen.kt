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
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
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
import com.migo.core.protocol.RelationshipEntry
import com.migo.core.protocol.RelationshipKind
import com.migo.core.protocol.SuggestedUser
import kotlinx.coroutines.delay

/**
 * The Friends section: the relationship graph, its pending requests, and the suggestions.
 *
 * The graph is server-owned — every action here asks the server and the view model re-reads the
 * result, so this screen never holds a local mirror. A friend row is a door into a chat; a request
 * row carries its two answers; a suggestion carries its one.
 */
@Composable
fun FriendsScreen(
    state: AppState.SignedIn,
    onQuery: (String) -> Unit,
    onRequest: (com.migo.core.wire.Id) -> Unit,
    onRespond: (com.migo.core.wire.Id, Boolean) -> Unit,
    onStartDirect: (com.migo.core.wire.Id) -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var field by rememberSaveable { mutableStateOf(state.search.query) }

    // The search field debounces into the shared search state; the people results below it are the
    // same answer the Search section shows.
    LaunchedEffect(field) {
        delay(300)
        if (field.trim() != state.search.query.trim()) onQuery(field)
    }

    // The wire carries the kind as a number; the enum's own values are read into numbers once and
    // compared number-to-number, the same discipline the web client keeps.
    val kindFriend: Long = RelationshipKind.Friend.wire.toLong()
    val kindIncoming: Long = RelationshipKind.PendingIncoming.wire.toLong()
    val kindOutgoing: Long = RelationshipKind.PendingOutgoing.wire.toLong()
    val entries = state.friends.entries
    val friends = entries.filter { it.kind == kindFriend }
    val incoming = entries.filter { it.kind == kindIncoming }
    val outgoing = entries.filter { it.kind == kindOutgoing }

    Column(modifier = modifier.fillMaxSize().imePadding()) {
        ScreenTitle(title = "Friends") {
            TextButton(onClick = onRefresh, enabled = !state.friends.loading) { Text("Refresh") }
        }
        OutlinedTextField(
            value = field,
            onValueChange = { field = it },
            placeholder = { Text("Search by username") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        )

        if (state.friends.loading && entries.isEmpty()) {
            LoadingRow()
        } else {
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                if (incoming.isNotEmpty() || outgoing.isNotEmpty()) {
                    item { SectionLabel(text = "Requests") }
                    items(incoming, key = { "in-" + it.userId.value }) { entry ->
                        RequestRow(
                            userId = entry.userId,
                            note = "wants to be friends",
                            busy = state.friends.busy.contains(entry.userId),
                            onAccept = { onRespond(entry.userId, true) },
                            onDecline = { onRespond(entry.userId, false) },
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outline)
                    }
                    items(outgoing, key = { "out-" + it.userId.value }) { entry ->
                        PersonSummaryRow(
                            name = shortName(entry),
                            handle = shortName(entry),
                            note = "request sent",
                            action = "Message",
                            onAction = { onStartDirect(entry.userId) },
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outline)
                    }
                }

                item { SectionLabel(text = "Friends") }
                if (friends.isEmpty()) {
                    item { Placeholder(text = "No friends yet. Add someone below.", modifier = Modifier.fillMaxWidth()) }
                } else {
                    items(friends, key = { it.userId.value }) { entry ->
                        PersonSummaryRow(
                            name = shortName(entry),
                            handle = shortName(entry),
                            note = null,
                            action = "Message",
                            onAction = { onStartDirect(entry.userId) },
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outline)
                    }
                }

                // The search's people answers, when a query is held.
                val found = state.search.people
                if (found != null && found.isNotEmpty()) {
                    item { SectionLabel(text = "People found") }
                    items(found, key = { "found-" + it.accountId.value }) { person ->
                        SuggestionRow(
                            person = person,
                            busy = state.friends.busy.contains(person.accountId),
                            onAdd = { onRequest(person.accountId) },
                            onStartDirect = { onStartDirect(person.accountId) },
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outline)
                    }
                }

                if (state.friends.suggestions.isNotEmpty() && field.isBlank()) {
                    item { SectionLabel(text = "Suggestions") }
                    items(state.friends.suggestions, key = { "sug-" + it.accountId.value }) { person ->
                        SuggestionRow(
                            person = person,
                            busy = state.friends.busy.contains(person.accountId),
                            onAdd = { onRequest(person.accountId) },
                            onStartDirect = { onStartDirect(person.accountId) },
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outline)
                    }
                }

                item { Spacer(modifier = Modifier.height(16.dp)) }
            }
        }
    }
}

/** A pending incoming request: the person and their two answers. */
@Composable
private fun RequestRow(
    userId: com.migo.core.wire.Id,
    note: String,
    busy: Boolean,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 58.dp)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ListRowAvatar(name = userId.value)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            ListRowName(text = shortId(userId))
            ListRowLine(text = note)
        }
        TextButton(onClick = onDecline, enabled = !busy) { Text("Decline") }
        Button(onClick = onAccept, enabled = !busy) { Text("Accept") }
    }
}

/** One suggested or found person: the two doors a stranger is offered. */
@Composable
private fun SuggestionRow(
    person: SuggestedUser,
    busy: Boolean,
    onAdd: () -> Unit,
    onStartDirect: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 58.dp)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ListRowAvatar(name = person.displayName)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            ListRowName(text = person.displayName)
            ListRowLine(
                text = "@" + person.username +
                    (if (person.mutualFriends > 0) " · ${person.mutualFriends} mutual" else ""),
            )
        }
        TextButton(onClick = onStartDirect) { Text("Message") }
        Button(onClick = onAdd, enabled = !busy) { Text("Add") }
    }
}

/** A relationship's display name: the id's short form, until a profile says better. */
private fun shortName(entry: RelationshipEntry): String = shortId(entry.userId)

/** An id as the short, readable form the rest of this build uses. */
private fun shortId(id: com.migo.core.wire.Id): String = id.value.take(8)
