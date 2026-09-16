package com.migo.app.ui

import androidx.compose.foundation.clickable
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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.core.protocol.PresenceState
import com.migo.core.protocol.RelationshipEntry
import com.migo.core.protocol.RelationshipKind
import com.migo.core.protocol.SuggestedUser
import kotlinx.coroutines.delay

/**
 * The Friends home view: the relationship graph, its pending requests, its blocks, and the
 * suggestions.
 *
 * The graph is server-owned — every action here asks the server and the view model re-reads the
 * result, so this screen never holds a local mirror. A friend row is tapped along its whole length
 * to open the friend intent sheet, whose primary act is the message and whose other doors end the
 * friendship, quietly or with a block; a request row carries its two answers; a blocked row carries
 * its one; a suggestion carries its one.
 */
@Composable
fun FriendsScreen(
    state: AppState.SignedIn,
    onQuery: (String) -> Unit,
    onRequest: (com.migo.core.wire.Id) -> Unit,
    onRespond: (com.migo.core.wire.Id, Boolean) -> Unit,
    onStartDirect: (com.migo.core.wire.Id) -> Unit,
    onRefresh: () -> Unit,
    /** The display name this shell has learned for an account, or null when it never heard one. */
    nameOf: (com.migo.core.wire.Id) -> String? = { null },
    /** What tapping a friend opens: the friend intent sheet. */
    onOpenIntent: (UserTarget) -> Unit = {},
    /** Opens the new-group sheet over the Friends view. */
    onOpenGroup: () -> Unit = {},
    /** Closes the new-group sheet, keeping what was typed and picked for a re-open. */
    onCloseGroup: () -> Unit = {},
    /** Edits the new group's title text. */
    onGroupTitle: (String) -> Unit = {},
    /** Adds or removes one friend from the new group's picked members. */
    onToggleGroupPick: (com.migo.core.wire.Id) -> Unit = {},
    /** Creates the group from the picked members and opens its thread. */
    onCreateGroup: () -> Unit = {},
    /** Lifts the caller's own block on one account, from the Blocked section's row. */
    onUnblock: (com.migo.core.wire.Id) -> Unit = {},
    /**
     * The session's avatars, by account id — the friends list's pictures, absent keys rendering
     * as the monogram the row already drew.
     */
    avatarBytes: Map<com.migo.core.wire.Id, ByteArray> = emptyMap(),
    modifier: Modifier = Modifier,
) {
    var field by rememberSaveable { mutableStateOf(state.search.query) }

    // The field's visibility: closed by default, opened by the header's search icon, and closed
    // again by the field's own trailing clear once the query is empty. Saved rather than
    // remembered so a rotation keeps a search in progress, and seeded from the shared query so a
    // search this view still holds opens with its field showing rather than an icon sitting over
    // filtered results.
    var searchOpen by rememberSaveable { mutableStateOf(state.search.query.isNotBlank()) }

    // The open owes the person a working field, not just a visible one: the focus and the
    // keyboard arrive with the icon's tap, so the search begins where the finger already is.
    val searchFocus = remember { FocusRequester() }
    val keyboard = LocalSoftwareKeyboardController.current
    LaunchedEffect(searchOpen) {
        if (searchOpen) {
            searchFocus.requestFocus()
            keyboard?.show()
        }
    }

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
    val kindBlocked: Long = RelationshipKind.Block.wire.toLong()
    val entries = state.friends.entries
    val friends = entries.filter { it.kind == kindFriend }
    val incoming = entries.filter { it.kind == kindIncoming }
    val outgoing = entries.filter { it.kind == kindOutgoing }
    val blocked = entries.filter { it.kind == kindBlocked }

    Column(modifier = modifier.fillMaxSize().imePadding()) {
        // The header carries the search behind its own icon, to the left of the new-group
        // control: the Friends list is the one surface whose whole point is finding a person, but
        // a field that is always on screen is a field that competes with the names it filters —
        // so the icon opens it, the focus and the keyboard arrive with it, and the trailing clear
        // folds it away again once the query is emptied and dismissed. The field keeps the
        // debounce it always had — typing is not yet asking, but a pause is.
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = "Friends",
                style = MaterialTheme.typography.titleLarge,
                color = MaterialTheme.colorScheme.onSurface,
            )
            // The incoming count as a badge beside the title, so a waiting invitation is stated at
            // a glance from anywhere in the view — not only once the Requests section has scrolled
            // into sight.
            if (incoming.isNotEmpty()) {
                Spacer(modifier = Modifier.width(8.dp))
                RequestsBadge(count = incoming.size)
            }
            Spacer(modifier = Modifier.width(12.dp))
            if (searchOpen) {
                OutlinedTextField(
                    value = field,
                    onValueChange = { field = it },
                    placeholder = { Text("Search by username") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                    trailingIcon = {
                        // The field's one extra control: a press clears what was typed, and a
                        // press on an already-empty field closes the search back into its icon —
                        // the query's two endings, both stated by the same glyph.
                        TextButton(
                            onClick = {
                                if (field.isNotEmpty()) field = "" else searchOpen = false
                            },
                            modifier = Modifier.semantics {
                                contentDescription = if (field.isNotEmpty()) {
                                    "Clear the search"
                                } else {
                                    "Close the search"
                                }
                            },
                        ) {
                            Text("✕")
                        }
                    },
                    modifier = Modifier
                        .weight(1f)
                        .focusRequester(searchFocus)
                        .onFocusChanged { info ->
                            // Focus leaving an empty field is the search finished the quiet way:
                            // the field folds back into its icon without asking.
                            if (!info.isFocused && field.isBlank()) searchOpen = false
                        },
                )
            } else {
                Spacer(modifier = Modifier.weight(1f))
                TextButton(
                    onClick = { searchOpen = true },
                    modifier = Modifier.semantics { contentDescription = "Search friends" },
                ) {
                    Text("🔍")
                }
            }
            // The new-group entry sits beside the search because a group starts from people, and
            // the Friends view is where the people are.
            TextButton(onClick = onOpenGroup, enabled = !state.friends.loading) { Text("New group") }
            TextButton(onClick = onRefresh, enabled = !state.friends.loading) { Text("Refresh") }
        }
        Spacer(modifier = Modifier.height(4.dp))

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
                        // The friend's display name where one was ever learned, and their direct
                        // conversation's row where one exists — a friend's chat preview and unread
                        // badge belong on their row, and the peer id on the row is what ties them.
                        val name = nameOf(entry.userId) ?: shortId(entry.userId)
                        val direct = state.conversations.firstOrNull { it.peerId == entry.userId }
                        FriendRow(
                            name = name,
                            avatarBytes = avatarBytes[entry.userId],
                            line = direct?.preview ?: "Tap to chat",
                            unread = direct?.unread ?: 0L,
                            presence = state.presence[entry.userId],
                            onClick = { onOpenIntent(UserTarget(userId = entry.userId, name = name, friend = true)) },
                        )
                        HorizontalDivider(color = MaterialTheme.colorScheme.outline)
                    }
                }

                // The blocks, when there are any: the graph's own Block rows, the one surface the
                // block list gets, because a block that could not be seen could not be lifted. The
                // section stands between the friends and the strangers so the list reads people
                // first and edges-last, and shows nothing at all when nothing is blocked — a
                // heading over an empty set is a heading nobody asked for.
                if (blocked.isNotEmpty()) {
                    item { SectionLabel(text = "Blocked") }
                    items(blocked, key = { "blk-" + it.userId.value }) { entry ->
                        BlockedRow(
                            name = nameOf(entry.userId) ?: shortId(entry.userId),
                            busy = state.friends.busy.contains(entry.userId),
                            onUnblock = { onUnblock(entry.userId) },
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

        // The new-group sheet covers the whole view, the same full-surface treatment the chat's
        // member sheet gives a roster: picking people is a list that scrolls.
        if (state.friends.groupOpen) {
            NewGroupSheet(
                friends = friends,
                picked = state.friends.groupPicked,
                title = state.friends.groupTitle,
                busy = state.friends.groupBusy,
                nameOf = nameOf,
                onClose = onCloseGroup,
                onTitle = onGroupTitle,
                onToggle = onToggleGroupPick,
                onCreate = onCreateGroup,
            )
        }
    }
}

/**
 * One friend: the presence-ringed avatar, the name, the line beneath, the unread pill, the chevron
 * — tappable along its whole length to open the friend intent sheet.
 *
 * [presence] is the live stream's word for the friend -- null when no event has arrived, which the
 * ring shows as the not-here grey rather than guessing online. The web client's dot carries the
 * same four states; the ring is this build's shape for the same fact.
 */
@Composable
private fun FriendRow(
    name: String,
    avatarBytes: ByteArray?,
    line: String,
    unread: Long,
    presence: com.migo.core.protocol.PresenceState?,
    onClick: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 58.dp)
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ListRowAvatar(name = name, online = presence != PresenceState.Offline, avatarBytes = avatarBytes)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            ListRowName(text = name)
            ListRowLine(text = line)
        }
        if (unread > 0) {
            UnreadPill(count = unread)
            Spacer(modifier = Modifier.width(8.dp))
        }
        Text(text = "›", fontSize = MigoGlyph.inline, color = LocalMigoExtra.current.faint)
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

/**
 * One blocked account: the person, the fact, and the one way out.
 *
 * The row says what a block is rather than what it might undo — unblocking restores nothing the
 * block tore down, so the row does not offer to. The Unblock button acts on the tap: unlike the
 * friendship's end, a block lifted is reversible by blocking again, and it is the blocker's own
 * edge being returned, so there is nothing here that needs confirming past the press.
 */
@Composable
private fun BlockedRow(
    name: String,
    busy: Boolean,
    onUnblock: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 58.dp)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ListRowAvatar(name = name)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            ListRowName(text = name)
            ListRowLine(text = "Blocked — they cannot reach you")
        }
        TextButton(onClick = onUnblock, enabled = !busy) { Text("Unblock") }
    }
}

/**
 * The pending-requests badge: the incoming count as the me card's mail badge wears unread — the
 * same red pill, capped at "9+" — so a waiting invitation is visible without scrolling to the
 * Requests section. The count is the graph's own incoming set, read where it already lives; no
 * fetch is made for a badge.
 */
@Composable
private fun RequestsBadge(count: Int) {
    Surface(
        color = Color(0xFFE5503C),
        contentColor = Color.White,
        shape = RoundedCornerShape(MigoRadius.pill),
        modifier = Modifier.semantics {
            contentDescription = "$count pending friend requests"
        },
    ) {
        Text(
            text = if (count > 9) "9+" else count.toString(),
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.Bold,
            modifier = Modifier.padding(horizontal = 3.5.dp, vertical = 1.dp),
        )
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

/**
 * The new-group sheet: a title, the friends to pick, and the Create that asks the server for the
 * conversation.
 *
 * The pick order is kept, not just the picked set: the first friend named joins the caller as the
 * group's second founder -- the two of them are the group's memory of who built it -- so the list
 * reads in the order it will mean something.
 */
@Composable
private fun NewGroupSheet(
    friends: List<RelationshipEntry>,
    picked: List<com.migo.core.wire.Id>,
    title: String,
    busy: Boolean,
    nameOf: (com.migo.core.wire.Id) -> String?,
    onClose: () -> Unit,
    onTitle: (String) -> Unit,
    onToggle: (com.migo.core.wire.Id) -> Unit,
    onCreate: () -> Unit,
) {
    Box(modifier = Modifier.fillMaxSize()) {
        Surface(
            color = MaterialTheme.colorScheme.surface,
            modifier = Modifier.fillMaxSize(),
        ) {
            Column(modifier = Modifier.fillMaxSize()) {
                Surface(color = MaterialTheme.colorScheme.surfaceVariant) {
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(end = 16.dp, top = 8.dp, bottom = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        TextButton(onClick = onClose) {
                            Text(text = "<", style = MaterialTheme.typography.titleMedium)
                        }
                        Text(
                            text = "New group",
                            style = MaterialTheme.typography.titleMedium,
                            color = MaterialTheme.colorScheme.onSurface,
                            modifier = Modifier.weight(1f),
                        )
                        // A group needs at least one named member besides the caller, so Create
                        // stays dark until the sheet can build one.
                        Button(
                            onClick = onCreate,
                            enabled = !busy && picked.isNotEmpty(),
                        ) {
                            Text("Create")
                        }
                    }
                }

                OutlinedTextField(
                    value = title,
                    onValueChange = onTitle,
                    placeholder = { Text("Group title (optional)") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                )

                if (friends.isEmpty()) {
                    Placeholder(
                        text = "No friends yet. A group needs someone to be built with.",
                        modifier = Modifier.fillMaxWidth(),
                    )
                } else {
                    LazyColumn(modifier = Modifier.weight(1f).fillMaxWidth()) {
                        item { SectionLabel(text = "Pick members") }
                        items(friends, key = { it.userId.value }) { entry ->
                            val name = nameOf(entry.userId) ?: shortId(entry.userId)
                            val isPicked = entry.userId in picked
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .clickable(enabled = !busy) { onToggle(entry.userId) }
                                    .padding(horizontal = 16.dp, vertical = 8.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Monogram(name = name, size = 32.dp)
                                Spacer(modifier = Modifier.width(12.dp))
                                Column(modifier = Modifier.weight(1f)) {
                                    Text(
                                        text = name,
                                        style = MaterialTheme.typography.bodyLarge,
                                        color = MaterialTheme.colorScheme.onSurface,
                                        maxLines = 1,
                                        overflow = TextOverflow.Ellipsis,
                                    )
                                    // The first pick is called out because it carries a meaning
                                    // the rest do not: that person becomes the second founder.
                                    Text(
                                        text = if (picked.indexOf(entry.userId) == 0) {
                                            "First pick · becomes a founder"
                                        } else if (isPicked) {
                                            "Picked"
                                        } else {
                                            ""
                                        },
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                                if (isPicked) {
                                    TextButton(onClick = { onToggle(entry.userId) }, enabled = !busy) {
                                        Text("Remove")
                                    }
                                }
                            }
                            HorizontalDivider(color = MaterialTheme.colorScheme.outline)
                        }
                        item { Spacer(modifier = Modifier.height(16.dp)) }
                    }
                }
            }
        }
    }
}
