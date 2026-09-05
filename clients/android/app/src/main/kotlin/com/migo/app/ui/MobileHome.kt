package com.migo.app.ui

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.migo.app.AppViewModel
import com.migo.app.model.AppState
import com.migo.core.ConnectionState
import com.migo.core.protocol.PresenceState
import com.migo.core.protocol.RoomSummary

/**
 * The home screen: the orange me card on top, and the selected home view beneath it.
 *
 * The me card is the reference's — avatar on the left opening the account sheet, the name with its
 * slow-pulsing presence dot, the status line edited in place, and the mail and settings chips on
 * the right — and beneath it whichever home tab the strip has chosen: Friends, Rooms or the Feed.
 * The three intent sheets the card's lists open (friend, room, me) and the log-out confirmation
 * live here too, so the whole home story is one composable and the shell above stays thin.
 */
@Composable
fun MobileHome(
    state: AppState.SignedIn,
    model: AppViewModel,
    modifier: Modifier = Modifier,
) {
    var meOpen by remember { mutableStateOf(false) }
    var intentUser by remember { mutableStateOf<UserTarget?>(null) }
    var intentRoom by remember { mutableStateOf<RoomSummary?>(null) }
    var confirmLogout by remember { mutableStateOf(false) }

    Column(modifier = modifier.fillMaxSize()) {
        MeCard(
            username = state.username,
            connection = state.connection,
            status = state.profileEdit.profile?.customStatus,
            balance = state.wallet.balance,
            unread = state.conversations.sumOf { it.unread },
            onOpenMe = { meOpen = true },
            onOpenMail = { model.selectSection(AppState.Section.ALERTS) },
            onSaveStatus = model::saveCustomStatus,
        )
        Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
            when (state.section) {
                AppState.Section.FRIENDS -> FriendsScreen(
                    state = state,
                    onQuery = model::setSearchQuery,
                    onRequest = model::friendRequest,
                    onRespond = model::friendRespond,
                    onStartDirect = model::startDirectWith,
                    onRefresh = model::loadFriends,
                    nameOf = model::nameOf,
                    onOpenIntent = { intentUser = it },
                    modifier = Modifier.fillMaxSize(),
                )
                AppState.Section.ROOMS -> RoomsScreen(
                    state = state,
                    onQuery = model::setRoomsQuery,
                    onOpenConversation = { model.open(it.conversationId, it.title) },
                    onCreate = model::createRoom,
                    onRefresh = model::loadRooms,
                    liveCounts = model::liveCountsFor,
                    onOpenRoomIntent = { intentRoom = it },
                    modifier = Modifier.fillMaxSize(),
                )
                AppState.Section.FEED -> SpaceScreen(
                    state = state,
                    onRefresh = model::loadSpace,
                    modifier = Modifier.fillMaxSize(),
                )
                // The panels never reach the home screen — they cover the shell from above — and
                // CHATS is the window tabs' own ground, not a home view.
                else -> Unit
            }
        }
    }

    if (meOpen) {
        MeSheet(
            state = state,
            onDismiss = { meOpen = false },
            onPresence = model::setPresence,
            onOpenSection = { section ->
                meOpen = false
                model.selectSection(section)
            },
            onLogOut = {
                meOpen = false
                confirmLogout = true
            },
        )
    }

    UserIntentSheet(
        target = intentUser,
        busy = intentUser?.let { state.friends.busy.contains(it.userId) } == true,
        onDismiss = { intentUser = null },
        onSend = {
            intentUser = null
            model.startDirectWith(it.userId)
        },
        onAdd = {
            intentUser = null
            model.friendRequest(it.userId)
        },
        onBlock = {
            intentUser = null
            model.blockUser(it.userId)
        },
    )

    RoomIntentSheet(
        room = intentRoom,
        live = intentRoom?.let { model.liveCountsFor(it.roomId) },
        joined = intentRoom?.let { room -> state.conversations.any { it.roomId == room.roomId } } == true,
        onDismiss = { intentRoom = null },
        onJoin = {
            intentRoom = null
            model.joinRoom(it)
        },
        onOpen = { room ->
            intentRoom = null
            // A joined room opens the conversation the join made; a directory row for a room the
            // list has not caught up with joins, and the join hands back the conversation id.
            val row = state.conversations.firstOrNull { it.roomId == room.roomId }
            if (row != null) {
                model.open(row.conversationId, row.title)
            } else {
                model.joinRoom(room)
            }
        },
    )

    if (confirmLogout) {
        AlertDialog(
            onDismissRequest = { confirmLogout = false },
            title = { Text("Log out of Migo?") },
            text = { Text("This device's session ends. Nothing is stored here to lose — messages were only ever held in memory.") },
            confirmButton = {
                TextButton(onClick = {
                    confirmLogout = false
                    model.signOut()
                }) { Text("Log out") }
            },
            dismissButton = {
                TextButton(onClick = { confirmLogout = false }) { Text("Stay") }
            },
        )
    }
}

/**
 * The orange "me card": the session's own surface, one flat band the same way in daylight and the
 * dark, because it says who is here.
 *
 * The avatar opens the account sheet; the status line is edited in place — tap it, type, Save —
 * and publishes through presence.set with the current state held, so saving a sentence does not
 * silently mark an away account online. The mail chip carries the unread badge, the settings chip
 * opens the same sheet the avatar does, and the balance rides as a chip while one exists.
 */
@Composable
private fun MeCard(
    username: String,
    connection: ConnectionState,
    status: String?,
    balance: Long?,
    unread: Long,
    onOpenMe: () -> Unit,
    onOpenMail: () -> Unit,
    onSaveStatus: (String) -> Unit,
) {
    val extra = LocalMigoExtra.current
    var editing by remember { mutableStateOf(false) }
    var draft by remember { mutableStateOf("") }

    fun commit() {
        editing = false
        onSaveStatus(draft.trim())
    }

    Box(
        modifier = Modifier
            .fillMaxWidth()
            .background(
                brush = Brush.horizontalGradient(listOf(extra.bannerA, extra.bannerB, extra.bannerC)),
            ),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            MeAvatar(
                name = username,
                modifier = Modifier
                    // The avatar and the settings chip are the account menu's doors; a control that
                    // carries the whole account clears the 48dp touch minimum, not the disc's size.
                    .clickable(onClick = onOpenMe)
                    .padding(3.dp),
            )
            Spacer(modifier = Modifier.width(10.dp))
            Column(modifier = Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    BlinkDot(color = connectionColor(connection))
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        text = username,
                        fontSize = 14.sp,
                        fontWeight = FontWeight.Bold,
                        color = extra.bannerInk,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                if (editing) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(
                            modifier = Modifier
                                .background(Color.White.copy(alpha = 0.26f), RoundedCornerShape(999.dp))
                                .padding(horizontal = 10.dp, vertical = 3.dp),
                        ) {
                            BasicTextField(
                                value = draft,
                                onValueChange = { draft = it },
                                textStyle = TextStyle(
                                    color = Color.White,
                                    fontSize = 11.5.sp,
                                    fontStyle = FontStyle.Italic,
                                ),
                                singleLine = true,
                                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                                keyboardActions = KeyboardActions(onDone = { commit() }),
                                modifier = Modifier.width(150.dp),
                            )
                        }
                        TextButton(onClick = { commit() }) {
                            Text(text = "Save", color = Color.White, fontWeight = FontWeight.Bold)
                        }
                    }
                } else {
                    // The status line: tap to edit. The placeholder is the reference's first-day
                    // wording, shown until the account writes one of its own.
                    Text(
                        text = status?.takeIf { it.isNotBlank() } ?: "New here! Say hi :)",
                        fontSize = 11.5.sp,
                        fontStyle = FontStyle.Italic,
                        color = extra.bannerInk.copy(alpha = 0.95f),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.clickable {
                            draft = status.orEmpty()
                            editing = true
                        },
                    )
                }
            }
            if (balance != null) {
                Surface(
                    color = Color.White.copy(alpha = 0.2f),
                    contentColor = extra.bannerInk,
                    shape = RoundedCornerShape(999.dp),
                ) {
                    Text(
                        text = "$balance \$MIG",
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.Bold,
                        modifier = Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
                    )
                }
                Spacer(modifier = Modifier.width(8.dp))
            }
            // The mail chip, with the unread badge over its corner when anything is unread.
            Box {
                Box(
                    modifier = Modifier
                        .size(30.dp)
                        .background(Color(0xFFD2690B), RoundedCornerShape(9.dp))
                        .clickable(onClick = onOpenMail),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = "✉",
                        fontSize = 14.sp,
                        color = Color.White,
                        textAlign = TextAlign.Center,
                    )
                }
                if (unread > 0) {
                    Surface(
                        color = Color(0xFFE5503C),
                        contentColor = Color.White,
                        shape = RoundedCornerShape(999.dp),
                        modifier = Modifier.align(Alignment.TopEnd),
                    ) {
                        Text(
                            text = if (unread > 9) "9+" else unread.toString(),
                            fontSize = 8.5.sp,
                            fontWeight = FontWeight.Bold,
                            modifier = Modifier.padding(horizontal = 3.5.dp, vertical = 1.dp),
                        )
                    }
                }
            }
            Spacer(modifier = Modifier.width(6.dp))
            // The settings chip, opening the same account sheet the avatar does.
            Box(
                modifier = Modifier
                    .size(30.dp)
                    .background(Color(0xFFD2690B), RoundedCornerShape(9.dp))
                    .clickable(onClick = onOpenMe),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = "⚙",
                    fontSize = 14.sp,
                    color = Color.White,
                    textAlign = TextAlign.Center,
                )
            }
        }
    }
}

/**
 * The account sheet: the avatar header with presence and credits, the presence pills, and the
 * panels the home tabs cannot carry — each a cover-the-screen panel with its own way back.
 *
 * The presence pills publish straight through presence.set, keeping the status line; the log-out
 * row is the danger red and still asks before it acts.
 */
@Composable
private fun MeSheet(
    state: AppState.SignedIn,
    onDismiss: () -> Unit,
    onPresence: (PresenceState) -> Unit,
    onOpenSection: (AppState.Section) -> Unit,
    onLogOut: () -> Unit,
) {
    val profile = state.profileEdit.profile
    val presence = profile?.presence
    val balance = state.wallet.balance

    MigoSheet(title = "My account", onDismiss = onDismiss) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            MeAvatar(name = state.username, modifier = Modifier.size(54.dp))
            Spacer(modifier = Modifier.width(12.dp))
            Column {
                ListRowName(text = state.username)
                ListRowLine(text = "@" + state.username)
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(
                        modifier = Modifier
                            .size(7.dp)
                            .background(presenceColor(presence), CircleShape),
                    )
                    Spacer(modifier = Modifier.width(5.dp))
                    Text(
                        text = presenceLabel(presence) +
                            (if (balance != null) " · $balance \$MIG" else ""),
                        fontSize = 11.5.sp,
                        fontWeight = FontWeight.SemiBold,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
        SectionLabel(text = "Presence")
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
        ) {
            PresencePill(
                label = "Online",
                dot = presenceColor(PresenceState.Online),
                selected = presence == PresenceState.Online,
                onClick = { onPresence(PresenceState.Online) },
                modifier = Modifier.weight(1f),
            )
            Spacer(modifier = Modifier.width(8.dp))
            PresencePill(
                label = "Away",
                dot = presenceColor(PresenceState.Away),
                selected = presence == PresenceState.Away,
                onClick = { onPresence(PresenceState.Away) },
                modifier = Modifier.weight(1f),
            )
        }
        Spacer(modifier = Modifier.height(8.dp))
        Row(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
            PresencePill(
                label = "Busy",
                dot = presenceColor(PresenceState.Busy),
                selected = presence == PresenceState.Busy,
                onClick = { onPresence(PresenceState.Busy) },
                modifier = Modifier.weight(1f),
            )
            Spacer(modifier = Modifier.width(8.dp))
            PresencePill(
                label = "Offline",
                dot = presenceColor(PresenceState.Offline),
                selected = presence == PresenceState.Offline,
                onClick = { onPresence(PresenceState.Offline) },
                modifier = Modifier.weight(1f),
            )
        }
        HorizontalDivider(
            color = MaterialTheme.colorScheme.outlineVariant,
            modifier = Modifier.padding(top = 10.dp, bottom = 4.dp),
        )
        SheetAction(
            glyph = "☺",
            label = "My Profile",
            sub = "Display name, devices, backup and security",
            onClick = { onOpenSection(AppState.Section.PROFILE) },
        )
        SheetAction(
            glyph = "✉",
            label = "Messages & alerts",
            sub = "The inbox and the notifications feed",
            onClick = { onOpenSection(AppState.Section.ALERTS) },
        )
        SheetAction(
            glyph = "🔎",
            label = "Search",
            sub = "People and rooms across the server",
            onClick = { onOpenSection(AppState.Section.SEARCH) },
        )
        SheetAction(
            glyph = "$",
            label = "Store",
            sub = "Credits, gifts and top-up",
            onClick = { onOpenSection(AppState.Section.WALLET) },
        )
        SheetAction(
            glyph = "✶",
            label = "Games",
            sub = "Refereed by the server, played in a conversation",
            onClick = { onOpenSection(AppState.Section.GAMES) },
        )
        // The owner's own entry: the sign-in standing check answers whether to offer it, because
        // the management page's whole point is that its existence is not public information.
        if (state.admins.owner) {
            SheetAction(
                glyph = "★",
                label = "Global Admins",
                onClick = { onOpenSection(AppState.Section.ADMINS) },
            )
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        SheetAction(
            glyph = "✕",
            label = "Log out",
            danger = true,
            onClick = onLogOut,
        )
        Spacer(modifier = Modifier.height(8.dp))
    }
}

/**
 * One presence pill: 42dp tall, 10dp corners, the dot and the word, and the check when it is the
 * state the account is in. A pill publishes on tap — presence is a server fact, not a local one.
 */
@Composable
private fun PresencePill(
    label: String,
    dot: Color,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scheme = MaterialTheme.colorScheme
    Box(
        modifier = modifier
            .heightIn(min = 42.dp)
            .background(
                if (selected) scheme.primaryContainer else scheme.surface,
                RoundedCornerShape(10.dp),
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 10.dp),
        contentAlignment = Alignment.Center,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(modifier = Modifier.size(8.dp).background(dot, CircleShape))
            Spacer(modifier = Modifier.width(8.dp))
            Text(
                text = label,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                color = if (selected) scheme.onPrimaryContainer else scheme.onSurfaceVariant,
            )
            if (selected) {
                Spacer(modifier = Modifier.width(6.dp))
                Text(
                    text = "✓",
                    fontSize = 12.sp,
                    fontWeight = FontWeight.Bold,
                    color = scheme.onPrimaryContainer,
                )
            }
        }
    }
}

/**
 * The me card's avatar: the 51dp halo, the 47dp green ring, the 42dp white disc — three
 * backgrounds and nothing else, drawn rather than stroked so the halo reads as the flat design's
 * cut-out rather than a shadow. Not [Monogram]: the monogram derives a tint from the name, right
 * on a list row and wrong on a surface with a colour of its own.
 */
@Composable
private fun MeAvatar(name: String, modifier: Modifier = Modifier) {
    val letter = name.trim().firstOrNull()?.uppercase() ?: "?"
    Box(
        modifier = modifier
            .size(51.dp)
            .background(Color.White.copy(alpha = 0.85f), CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .size(47.dp)
                .background(Color(0xFF3FCE6B), CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            Box(
                modifier = Modifier
                    .size(42.dp)
                    .background(Color.White, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = letter,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                    color = Color(0xFF0D4353),
                    textAlign = TextAlign.Center,
                )
            }
        }
    }
}

/**
 * The presence dot on the band: a slow pulse rather than a blink fast enough to read as an alarm —
 * it says "here", not "look at me".
 */
@Composable
private fun BlinkDot(color: Color) {
    val pulse = rememberInfiniteTransition(label = "me-card-dot")
    val dotAlpha by pulse.animateFloat(
        initialValue = 1f,
        targetValue = 0.35f,
        animationSpec = infiniteRepeatable(
            animation = tween(1400),
            repeatMode = RepeatMode.Reverse,
        ),
        label = "me-card-dot-alpha",
    )
    Box(
        modifier = Modifier
            .size(8.dp)
            .background(color.copy(alpha = dotAlpha), CircleShape),
    )
}

/** The band's dot colour: the connection's own word, worn as a colour. */
private fun connectionColor(connection: ConnectionState): Color = when (connection) {
    ConnectionState.Online -> Color(0xFF3FCE6B)
    ConnectionState.Connecting -> Color(0xFFF5B83D)
    ConnectionState.Reconnecting -> Color(0xFFE5503C)
    ConnectionState.Closed -> Color(0xFFB9C9D1)
}

/** A presence state's colour, the same marks the web client's pills wear. */
private fun presenceColor(presence: PresenceState?): Color = when (presence) {
    PresenceState.Online -> Color(0xFF3FCE6B)
    PresenceState.Away -> Color(0xFFF5B83D)
    PresenceState.Busy -> Color(0xFFE5503C)
    PresenceState.Offline, PresenceState.Unknown, PresenceState.Invisible -> Color(0xFFB9C9D1)
    null -> Color(0xFFB9C9D1)
}

/** A presence state's word. */
private fun presenceLabel(presence: PresenceState?): String = when (presence) {
    PresenceState.Online -> "Online"
    PresenceState.Away -> "Away"
    PresenceState.Busy -> "Busy"
    PresenceState.Offline -> "Offline"
    PresenceState.Invisible -> "Invisible"
    PresenceState.Unknown, null -> "—"
}
