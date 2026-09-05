package com.migo.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.migo.app.model.AppState
import com.migo.app.model.ConversationRow
import com.migo.app.model.RoomLiveInfo
import com.migo.core.protocol.RoomKind
import com.migo.core.protocol.RoomSummary
import com.migo.core.wire.Id
import kotlinx.coroutines.delay

/**
 * The Rooms home view: the rooms this account is in, and the public directory below them.
 *
 * The query is debounced — typing is not yet asking, but a pause is — and the directory rows state
 * the facts a join decision needs: name, topic, members, live online count, and how full the room is
 * against its ceiling. A row is tapped rather than buttoned, mobile-reference style: one of this
 * account's own rooms walks straight into the conversation the join made, and a directory room
 * opens the room intent sheet — occupancy in hand — whose orange primary is the join itself. The
 * create dialog states the capacity rule the server enforces — a public room's 33 seats are fixed
 * by the kind, so the form says so rather than letting the user discover it by asking for more.
 */
@Composable
fun RoomsScreen(
    state: AppState.SignedIn,
    onQuery: (String) -> Unit,
    onOpenConversation: (ConversationRow) -> Unit,
    onCreate: (slug: String, name: String, kind: RoomKind, topic: String?) -> Unit,
    onRefresh: () -> Unit,
    /** The live record a room's event streams keep current, for the rows to count from. */
    liveCounts: (Id) -> RoomLiveInfo? = { null },
    /** What tapping a directory room opens: the room intent sheet. */
    onOpenRoomIntent: (RoomSummary) -> Unit = {},
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
                        // The same live record the directory reads, so the account's own rooms
                        // count online members here too — the row a returning user reads first.
                        val roomId = row.roomId
                        val live = if (roomId == null) null else liveCounts(roomId)
                        YourRoomRow(row = row, live = live, onOpen = { onOpenConversation(row) })
                        HorizontalDivider(color = MaterialTheme.colorScheme.outline)
                    }
                }
                if (state.rooms.rooms.orEmpty().isNotEmpty()) {
                    item(key = "directory-label") { SectionLabel(text = "Public directory") }
                    items(
                        state.rooms.rooms.orEmpty().filter { row -> mineByRoom[row.roomId] == null },
                        key = { "dir-${it.roomId.value}" },
                    ) { room ->
                        // The page's counts are a snapshot from the moment of the read; the live
                        // record is the one the member and state events keep current. A row whose
                        // room the account is in counts in front of the user instead of waiting
                        // for a refresh. Rooms not watched keep their page counts: the deltas only
                        // arrive on the room's own topic, which the shell subscribes on join.
                        val live = liveCounts(room.roomId)
                        val counted = if (live == null) room else room.copy(
                            memberCount = live.memberCount,
                            onlineCount = live.onlineCount,
                            maxMembers = live.maxMembers,
                        )
                        DirectoryRow(
                            room = counted,
                            onOpenIntent = { onOpenRoomIntent(room) },
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outline)
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

/**
 * One of this account's own rooms: the 66dp room row — the ringed avatar, the bold name, the last
 * line, the occupancy pill and its bar — tappable along its whole length to walk back in. Unread
 * rides as the red pill rather than as the second line's words, the same mark the conversation list
 * gives it.
 */
@Composable
private fun YourRoomRow(row: ConversationRow, live: RoomLiveInfo?, onOpen: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 66.dp)
            .clickable(onClick = onOpen)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ListRowAvatar(name = row.title)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            ListRowName(text = row.title)
            // The preview line keeps its place; the live online count rides beside it, the same
            // "N online" the chat's own header states, so the list and the thread never disagree.
            ListRowLine(
                text = when {
                    live != null && live.memberCount > 0L ->
                        (row.preview ?: "Open the room") + " · ${live.onlineCount} online"
                    else -> row.preview ?: "Open the room"
                },
            )
            if (live?.maxMembers != null && live.maxMembers > 0L) {
                OccupancyBar(current = live.onlineCount, capacity = live.maxMembers)
            }
        }
        if (row.unread > 0) {
            UnreadPill(count = row.unread)
            Spacer(modifier = Modifier.width(8.dp))
        }
        Text(text = "›", fontSize = 18.sp, color = LocalMigoExtra.current.faint)
    }
}

/**
 * The "bold users / capacity" pill a room row carries when the room declares a ceiling, and the
 * 3.5dp occupancy bar under it. The two flip to the orange family once the room is 85% full: a
 * room about to be full is the fact a join decision wants shouted, not whispered in teal.
 *
 * Not private: the room intent sheet draws the same pill and bar over the same numbers.
 */
@Composable
fun OccupancyBar(current: Long, capacity: Long) {
    val nearFull = capacity > 0L && current >= (capacity * 85L) / 100L
    Row(verticalAlignment = Alignment.CenterVertically) {
        Surface(
            color = if (nearFull) Color(0xFFFDEEE0) else Color(0xFFEEF7FA),
            contentColor = if (nearFull) Color(0xFFD95F07) else Color(0xFF157A92),
            border = BorderStroke(1.dp, if (nearFull) Color(0xFFF6D4B4) else Color(0xFFCFE3EA)),
            shape = RoundedCornerShape(999.dp),
        ) {
            Text(
                text = "$current / $capacity",
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.padding(horizontal = 8.dp, vertical = 1.dp),
            )
        }
        Spacer(modifier = Modifier.width(6.dp))
        Box(
            modifier = Modifier
                .width(64.dp)
                .height(3.5.dp)
                .background(Color(0xFFE4F1F5), RoundedCornerShape(999.dp)),
        ) {
            if (capacity > 0L) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth(current.toFloat() / capacity.toFloat())
                        .height(3.5.dp)
                        .background(
                            if (nearFull) Color(0xFFF5820C) else Color(0xFF1993AB),
                            RoundedCornerShape(999.dp),
                        ),
                )
            }
        }
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

/**
 * One directory row: the 66dp room row — the facts of a join decision, the capacity pill and its
 * occupancy bar — tappable along its whole length to open the room intent sheet.
 */
@Composable
private fun DirectoryRow(room: RoomSummary, onOpenIntent: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 66.dp)
            .clickable(onClick = onOpenIntent)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ListRowAvatar(name = room.name)
        Spacer(modifier = Modifier.width(10.dp))
        // Read once: the topic is a property of another module, and a smart cast of it is
        // not something the compiler can promise across that boundary. The ceiling is read
        // beside it because the Column's occupancy bar below asks for it.
        val topic = room.topic
        val max = room.maxMembers
        Column(modifier = Modifier.weight(1f)) {
            ListRowName(text = room.name)
            ListRowLine(text = "${room.memberCount} members · ${room.onlineCount} online" + (room.category?.let { " · $it" } ?: ""))
            if (!topic.isNullOrBlank()) ListRowLine(text = topic)
            // The occupancy pill and bar, under the counts: live online count over the room's
            // ceiling, "2/33", the product's own shorthand for how full a room is. A room
            // that declares no ceiling simply shows neither, and the count line above still
            // carries the raw numbers.
            if (max != null && max > 0L) {
                OccupancyBar(current = room.onlineCount, capacity = max)
            }
        }
        // A full room's pill is already orange, and the sheet the row opens names the fact on
        // its primary button rather than letting the server refuse the tap.
        Text(text = "›", fontSize = 18.sp, color = LocalMigoExtra.current.faint)
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
