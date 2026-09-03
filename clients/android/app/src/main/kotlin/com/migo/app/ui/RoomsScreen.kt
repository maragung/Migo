package com.migo.app.ui

import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.app.model.ConversationRow
import com.migo.core.protocol.RoomKind
import com.migo.core.protocol.RoomSummary
import com.migo.core.wire.Id
import kotlinx.coroutines.delay

/**
 * The Rooms section: the rooms this account is in, and the public directory below them.
 *
 * The query is debounced — typing is not yet asking, but a pause is — and the directory rows state
 * the facts a join decision needs: name, topic, members, live online count, and how full the room is
 * against its ceiling. A room already joined never offers Join again: it offers Open, which walks
 * straight into the conversation the join made, the same way a started chat opens. The create dialog
 * states the capacity rule the server enforces — a public room's 33 seats are fixed by the kind, so
 * the form says so rather than letting the user discover it by asking for more.
 */
@Composable
fun RoomsScreen(
    state: AppState.SignedIn,
    onQuery: (String) -> Unit,
    onJoin: (RoomSummary) -> Unit,
    onOpenConversation: (ConversationRow) -> Unit,
    onCreate: (slug: String, name: String, kind: RoomKind, topic: String?) -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var field by rememberSaveable { mutableStateOf(state.rooms.query) }
    var creating by remember { mutableStateOf(false) }

    // The debounce lives here rather than in the view model so the field's own text is the single
    // source of what is typed; the view model's query is what the wire sees.
    LaunchedEffect(field) {
        delay(300)
        if (field.trim() != state.rooms.query.trim()) onQuery(field)
    }

    // The rooms this account is in, newest activity first, by the conversations the joins made. A
    // joined room whose conversation has not loaded yet (the list is still syncing) keeps its
    // directory row with Join below — the server re-admits a member harmlessly, and the join
    // hands back the conversation id that opens the thread.
    val mine = remember(state.conversations) {
        state.conversations.filter { it.roomId != null }.sortedByDescending { it.updatedAt }
    }
    // The conversation a joined room opens, by room id: what turns a directory row's Join into Open.
    val mineByRoom = remember(state.conversations) {
        state.conversations.byRoomId()
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
            state.rooms.loading -> LoadingRow()
            state.rooms.rooms == null && mine.isEmpty() -> Unit
            mine.isEmpty() && state.rooms.rooms?.isEmpty() == true -> Placeholder(
                text = if (state.rooms.query.isBlank()) "No public rooms yet." else "No rooms matched.",
                modifier = Modifier.weight(1f),
            )
            else -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                if (mine.isNotEmpty()) {
                    item(key = "mine-label") { SectionLabel(text = "Your rooms") }
                    items(mine, key = { "mine-${it.conversationId.value}" }) { row ->
                        YourRoomRow(row = row, onOpen = { onOpenConversation(row) })
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    }
                }
                if (state.rooms.rooms.orEmpty().isNotEmpty()) {
                    item(key = "directory-label") { SectionLabel(text = "Public directory") }
                    items(
                        state.rooms.rooms.orEmpty().filter { row -> mineByRoom[row.roomId] == null },
                        key = { "dir-${it.roomId.value}" },
                    ) { room ->
                        DirectoryRow(
                            room = room,
                            joining = state.rooms.joining.contains(room.roomId),
                            onJoin = { onJoin(room) },
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    }
                }
                item(key = "tail") { Spacer(modifier = Modifier.height(16.dp)) }
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

/** Maps each joined room to the conversation that opens it, keyed by room id. */
private fun List<ConversationRow>.byRoomId(): Map<Id, ConversationRow> =
    mapNotNull { row -> row.roomId?.let { it to row } }.toMap()

/** One of this account's own rooms: the name, the last line, and the way back in. */
@Composable
private fun YourRoomRow(row: ConversationRow, onOpen: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Monogram(name = row.title, size = 36.dp)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = row.title,
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
            if (row.unread > 0) {
                OneLine(text = "${row.unread} unread")
            } else {
                OneLine(text = row.preview ?: "Open the room")
            }
        }
        Button(onClick = onOpen) { Text("Open") }
    }
}

/**
 * The Create Room dialog: a room is named, addressed, and opened in one flow.
 *
 * The slug is the room's permanent address, so it is suggested from the name but stays editable —
 * the name can change and the slug cannot. The suggestion is lowercase, spaces to hyphens,
 * everything else stripped. Validation mirrors the server's own rule (3–32 characters, lowercase
 * letters and digits, single interior hyphens) so a bad address is caught here, with the rule
 * spelled out beside the field, instead of as a refusal after Create is pressed.
 */
@Composable
private fun CreateRoomDialog(
    onCreate: (String, String, RoomKind, String?) -> Unit,
    onDismiss: () -> Unit,
) {
    var name by remember { mutableStateOf("") }
    var slug by remember { mutableStateOf("") }
    var slugTouched by remember { mutableStateOf(false) }
    var managed by remember { mutableStateOf(false) }
    var topic by remember { mutableStateOf("") }

    // The slug follows the name until the user edits it.
    LaunchedEffect(name) {
        if (!slugTouched) {
            slug = name.lowercase().trim().replace(Regex("[^a-z0-9\\s-]"), "").replace(Regex("[\\s-]+"), "-")
        }
    }

    val slugValid = slugIsValid(slug)
    val kind = if (managed) RoomKind.Managed else RoomKind.Public

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Create a room") },
        text = {
            // The form scrolls: on a small phone with the keyboard up, the stack of chips, help
            // line, and three fields is taller than the dialog's share of the screen, and a field
            // that cannot be scrolled to is a field that cannot be filled.
            Column(
                modifier = Modifier
                    .verticalScroll(rememberScrollState())
                    .heightIn(max = 420.dp),
            ) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    FilterChip(
                        selected = !managed,
                        onClick = { managed = false },
                        label = { Text("Public · 33 seats") },
                    )
                    FilterChip(
                        selected = managed,
                        onClick = { managed = true },
                        label = { Text("Managed · 5–50") },
                    )
                }
                // The capacity is not a field because it is not a choice: the kind fixes it. Saying
                // so here is cheaper than a user asking for more and being quietly seated at 33.
                OneLine(
                    text = if (managed) {
                        "Managed rooms start at 5 seats and grow with your friendships, up to 50."
                    } else {
                        "Public rooms seat 33 — the capacity is fixed by the kind."
                    },
                    modifier = Modifier.padding(top = 4.dp),
                )
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
                    supportingText = {
                        Text(
                            if (slug.isEmpty() || slugValid) {
                                "3–32 lowercase letters, digits, single hyphens"
                            } else {
                                "3–32 lowercase letters and digits, with single hyphens inside"
                            },
                        )
                    },
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

/**
 * The server's own slug rule, mirrored so the dialog refuses locally what the wire would refuse
 * anyway: 3 to 32 characters, lowercase letters and digits only, hyphens single and interior.
 * (The server also refuses a slug that parses as a ULID; that arm stays server-side — no honest
 * person names a room one, and the refusal is one round trip away.)
 */
private fun slugIsValid(slug: String): Boolean {
    if (slug.length !in 3..32) return false
    if (slug.startsWith("-") || slug.endsWith("-") || slug.contains("--")) return false
    return slug.all { it in 'a'..'z' || it in '0'..'9' || it == '-' }
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
        // The capacity badge: live online count over the room's ceiling, "2/33" — the product's own
        // shorthand for how full a room is. Read once, for the same cross-module smart-cast reason as
        // the topic; a room that declares no ceiling simply shows no badge, and the count line above
        // still carries the raw numbers.
        val max = room.maxMembers
        val full = max != null && max > 0L && room.memberCount >= max
        if (max != null && max > 0L) {
            CapacityBadge(online = room.onlineCount, max = max, full = full)
            Spacer(modifier = Modifier.width(10.dp))
        }
        // A full room is not joinable, and saying so here is kinder than letting the server refuse
        // the tap: the badge above is already red, and the button names the fact.
        Button(onClick = onJoin, enabled = !joining && !full) {
            Text(when {
                joining -> "…"
                full -> "Full"
                else -> "Join"
            })
        }
    }
}

/** The "online/max" pill a directory row wears when its room declares a ceiling. */
@Composable
private fun CapacityBadge(online: Long, max: Long, full: Boolean) {
    Surface(
        color = if (full) {
            MaterialTheme.colorScheme.errorContainer
        } else {
            MaterialTheme.colorScheme.secondaryContainer
        },
        contentColor = if (full) {
            MaterialTheme.colorScheme.onErrorContainer
        } else {
            MaterialTheme.colorScheme.onSecondaryContainer
        },
        shape = RoundedCornerShape(percent = 50),
    ) {
        Text(
            text = if (full) "$online/$max full" else "$online/$max",
            style = MaterialTheme.typography.labelMedium,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
        )
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
