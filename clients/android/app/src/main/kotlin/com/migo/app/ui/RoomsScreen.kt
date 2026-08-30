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
import androidx.compose.material3.Button
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
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var field by rememberSaveable { mutableStateOf(state.rooms.query) }

    // The debounce lives here rather than in the view model so the field's own text is the single
    // source of what is typed; the view model's query is what the wire sees.
    LaunchedEffect(field) {
        delay(300)
        if (field.trim() != state.rooms.query.trim()) onQuery(field)
    }

    Column(modifier = modifier.fillMaxSize().imePadding()) {
        ScreenTitle(title = "Rooms") {
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
            if (!room.topic.isNullOrBlank()) OneLine(text = room.topic)
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
