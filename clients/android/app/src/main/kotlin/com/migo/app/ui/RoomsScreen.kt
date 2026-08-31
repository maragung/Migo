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
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
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
import kotlinx.coroutines.delay

/**
 * The Rooms section: the public directory and the way in.
 *
 * The query is debounced — typing is not yet asking, but a pause is — and the rows state the facts
 * a join decision needs: name, topic, members, live online count. A join hands the conversation
 * back to the caller, which opens the thread exactly as a started chat opens.
 */
@Composable
fun RoomsScreen(
    state: AppState.SignedIn,
    onQuery: (String) -> Unit,
    onJoin: (RoomSummary) -> Unit,
    onCreate: (slug: String, name: String, kind: com.migo.core.protocol.RoomKind, topic: String?) -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var field by rememberSaveable { mutableStateOf(state.rooms.query) }
    var creating by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf(false) }

    // The debounce lives here rather than in the view model so the field's own text is the single
    // source of what is typed; the view model's query is what the wire sees.
    LaunchedEffect(field) {
        delay(300)
        if (field.trim() != state.rooms.query.trim()) onQuery(field)
    }

    Column(modifier = modifier.fillMaxSize().imePadding()) {
        ScreenTitle(title = "Rooms") {
            TextButton(onClick = { creating = true }) { Text("New room") }
            TextButton(onClick = onRefresh, enabled = !state.rooms.loading) { Text("Refresh") }
        }
        OutlinedTextField(
            value = field,
            onValueChange = { field = it },
            placeholder = { Text("Search rooms") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        )

        when {
            state.rooms.loading -> Row(
                modifier = Modifier.fillMaxWidth().padding(24.dp),
                horizontalArrangement = androidx.compose.foundation.layout.Arrangement.Center,
            ) { CircularProgressIndicator() }
            state.rooms.rooms == null -> Unit
            state.rooms.rooms?.isEmpty() == true -> Placeholder(
                text = if (state.rooms.query.isBlank()) "No public rooms yet." else "No rooms matched.",
                modifier = Modifier.weight(1f),
            )
            else -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(state.rooms.rooms.orEmpty(), key = { it.roomId.value }) { room ->
                    DirectoryRow(
                        room = room,
                        joining = state.rooms.joining.contains(room.roomId),
                        onJoin = { onJoin(room) },
                    )
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                }
            }
        }
    }

    if (creating) {
        CreateRoomDialog(
            onCreate = { slug, name, kind, topic ->
                creating = false
                onCreate(slug, name, kind, topic)
            },
            onDismiss = { creating = false },
        )
    }
}

/**
 * The Create Room dialog: a room is named, addressed, and opened in one flow.
 *
 * The slug is the room's permanent address, so it is suggested from the name but stays editable —
 * the name can change and the slug cannot. The suggestion is lowercase, spaces to hyphens,
 * everything else stripped.
 */
@Composable
private fun CreateRoomDialog(
    onCreate: (String, String, com.migo.core.protocol.RoomKind, String?) -> Unit,
    onDismiss: () -> Unit,
) {
    var name by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf("") }
    var slug by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf("") }
    var slugTouched by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf(false) }
    var managed by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf(false) }
    var topic by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf("") }

    // The slug follows the name until the user edits it.
    androidx.compose.runtime.LaunchedEffect(name) {
        if (!slugTouched) {
            slug = name.lowercase().trim().replace(Regex("[^a-z0-9\\s-]"), "").replace(Regex("[\\s-]+"), "-")
        }
    }

    val slugValid = slug.matches(Regex("[a-z0-9][a-z0-9-]*"))
    val kind = if (managed) {
        com.migo.core.protocol.RoomKind.Managed
    } else {
        com.migo.core.protocol.RoomKind.Public
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Create a room") },
        text = {
            Column {
                Row(horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(6.dp)) {
                    FilterChip(
                        selected = !managed,
                        onClick = { managed = false },
                        label = { Text("Public") },
                    )
                    FilterChip(
                        selected = managed,
                        onClick = { managed = true },
                        label = { Text("Managed") },
                    )
                }
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    label = { Text("Name") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                )
                OutlinedTextField(
                    value = slug,
                    onValueChange = {
                        slugTouched = true
                        slug = it.lowercase()
                    },
                    label = { Text("Address (permanent)") },
                    singleLine = true,
                    isError = slug.isNotEmpty() && !slugValid,
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                )
                OutlinedTextField(
                    value = topic,
                    onValueChange = { topic = it },
                    label = { Text("Topic (optional)") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onCreate(slug, name.trim(), kind, topic.trim().ifEmpty { null }) },
                enabled = name.isNotBlank() && slugValid,
            ) { Text("Create") }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("Cancel") }
        },
    )
}

/** One directory row: the facts of a join decision, and the way in. */
@Composable
private fun DirectoryRow(room: RoomSummary, joining: Boolean, onJoin: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Monogram(name = room.name, size = 36.dp)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = room.name,
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
            OneLine(text = "${room.memberCount} members · ${room.onlineCount} online" + (room.category?.let { " · $it" } ?: ""))
            // Read once: the topic is a property of another module, and a smart cast of it is
            // not something the compiler can promise across that boundary.
            val topic = room.topic
            if (!topic.isNullOrBlank()) OneLine(text = topic)
        }
        Button(onClick = onJoin, enabled = !joining) {
            Text(if (joining) "…" else "Join")
        }
    }
}

/** The shared screen title row: the title on the left, the one action on the right. */
@Composable
fun ScreenTitle(title: String, action: @Composable () -> Unit = {}) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = title,
            style = MaterialTheme.typography.titleLarge,
            color = MaterialTheme.colorScheme.onSurface,
            modifier = Modifier.weight(1f),
        )
        action()
    }
    Spacer(modifier = Modifier.height(4.dp))
}
