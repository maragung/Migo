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
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.migo.app.AppViewModel
import com.migo.app.model.AppState
import com.migo.core.ConnectionState
import com.migo.core.domain.ReportSubject
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
    // The friend the remove-friend confirmation is about: the sheet's Remove row hands the person
    // here rather than acting on the tap itself, the same confirm-first shape the log-out and the
    // identity rotation keep — a friendship is ended on purpose or not at all.
    var confirmRemoveFriend by remember { mutableStateOf<UserTarget?>(null) }
    // The session's avatars, for the Friends view's rows. Collected here because the home screen
    // owns the view that draws the most people; the map is the same one every other surface reads.
    val avatarBytes by model.avatarBytes.collectAsState()
    // The report sheet's state, collected here because the home screen owns two of the four doors
    // into it — a friend's intent sheet and a room's — and the sheet is one surface wherever it
    // was opened from. The chat's own two doors are hosted by the chat, which reads the same flow.
    val reportSheet by model.reportSheet.collectAsState()

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
                    onOpenGroup = model::openGroupSheet,
                    onCloseGroup = model::closeGroupSheet,
                    onGroupTitle = model::setGroupTitle,
                    onToggleGroupPick = model::toggleGroupPick,
                    onCreateGroup = model::createGroup,
                    onUnblock = model::unblockUser,
                    avatarBytes = avatarBytes,
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
        presence = intentUser?.let { state.presence[it.userId] },
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
        onRemove = {
            // The sheet folds; the confirmation stands on its own, aimed at the friend it names.
            intentUser = null
            confirmRemoveFriend = it
        },
        onBlock = {
            intentUser = null
            model.blockUser(it.userId)
        },
        // The person's own report door, closed before the sheet opens so the intent sheet is not
        // left standing behind it. The two sheets are modal over the same screen, and a stack of
        // them would leave a person dismissing twice to get back to where they started.
        //
        // The subject is read off the state the sheet was opened with rather than handed in:
        // this door takes no argument of its own, because the sheet already knows who it is
        // about, and the read happens before the dismiss so the id outlives the sheet.
        onReport = {
            val who = intentUser
            intentUser = null
            if (who != null) {
                model.openReport(ReportSubject.User, who.userId, who.name)
            }
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
        // The room's own report door. A directory row for a room this account has never joined is
        // enough to see that the room's name and topic are the problem, so the report is offered
        // before the join rather than behind it. The room is read off the state the sheet was
        // opened with, for the same reason the person above is: this door takes no argument.
        onReport = {
            val room = intentRoom
            intentRoom = null
            if (room != null) {
                model.openReport(ReportSubject.Room, room.roomId, "“${room.name}”")
            }
        },
    )

    // The report sheet, one surface for all four doors into it. Rendered here as well as in the
    // chat because both screens can open it and only one of them is composed at a time: the home
    // screen's doors are a person and a room, the chat's are a message and a person, and the state
    // is the model's either way — so whichever screen is on top draws the same sheet.
    reportSheet?.let { view ->
        ReportSheet(
            view = view,
            onPickReason = model::setReportReason,
            onNote = model::setReportNote,
            onSubmit = model::submitReport,
            onClose = model::closeReport,
        )
    }

    // The remove-friend confirmation: the one confirmation the Friends view owes, because the
    // other acts it offers (a request, an answer, an unblock) are all reversible or mere
    // invitations, where a friendship ends whole and silently.
    confirmRemoveFriend?.let { target ->
        AlertDialog(
            onDismissRequest = { confirmRemoveFriend = null },
            title = { Text("Remove ${target.name} from friends?") },
            text = {
                Text(
                    "The friendship ends on both sides. Nothing is announced — they are not " +
                        "told — and either of you can send a friend request to rebuild it later.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmRemoveFriend = null
                    model.removeFriend(target.userId)
                }) { Text("Remove friend", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = {
                TextButton(onClick = { confirmRemoveFriend = null }) { Text("Cancel") }
            },
        )
    }

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
 * silently mark an away account online. The balance sits above the mail and settings chips, stacked
 * over the pair rather than riding beside them, so the wallet's number is read at a glance above
 * the controls it is spent from. The slot the balance chip once occupied in the row is the
 * connection indicator's now — the session's own word as a dot and a label — and the mail chip
 * carries the unread badge while the settings chip opens the same sheet the avatar does.
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
                        fontSize = MigoType.titleSm,
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
                                .background(Color.White.copy(alpha = 0.26f), RoundedCornerShape(MigoRadius.pill))
                                .padding(horizontal = 10.dp, vertical = 3.dp),
                        ) {
                            BasicTextField(
                                value = draft,
                                onValueChange = { draft = it },
                                textStyle = TextStyle(
                                    color = Color.White,
                                    fontSize = MigoType.meta,
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
                        fontSize = MigoType.meta,
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
            // The connection indicator, in the slot the balance chip once held: the session's own
            // word as a dot and a label, because a colour alone says nothing to anybody who cannot
            // tell this green from this amber. This is the one place the shell states the
            // connection — the chat window does not repeat it, its business being the thread.
            Surface(
                color = Color.White.copy(alpha = 0.2f),
                contentColor = extra.bannerInk,
                shape = RoundedCornerShape(MigoRadius.pill),
                modifier = Modifier.semantics {
                    contentDescription = "Connection: " + connectionLabel(connection)
                },
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
                ) {
                    Box(
                        modifier = Modifier
                            .size(7.dp)
                            .background(connectionColor(connection), CircleShape),
                    )
                    Spacer(modifier = Modifier.width(5.dp))
                    Text(
                        text = connectionLabel(connection),
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.Bold,
                        maxLines = 1,
                    )
                }
            }
            Spacer(modifier = Modifier.width(8.dp))
            // The balance and the two chips it stands above: the wallet's number stacked over the
            // mail and settings pair, so it is read at a glance above the controls it is spent
            // from rather than squeezed between them and the name.
            Column(horizontalAlignment = Alignment.End) {
                if (balance != null) {
                    Surface(
                        color = Color.White.copy(alpha = 0.2f),
                        contentColor = extra.bannerInk,
                        shape = RoundedCornerShape(MigoRadius.pill),
                    ) {
                        Text(
                            text = "$balance \$MIG",
                            style = MaterialTheme.typography.labelMedium,
                            fontWeight = FontWeight.Bold,
                            modifier = Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
                        )
                    }
                    Spacer(modifier = Modifier.height(4.dp))
                }
                Row {
                    // The mail chip, with the unread badge over its corner when anything is unread.
                    Box {
                        Box(
                            modifier = Modifier
                                .size(30.dp)
                                .background(Color(0xFFD2690B), RoundedCornerShape(MigoRadius.md))
                                .clickable(onClick = onOpenMail),
                            contentAlignment = Alignment.Center,
                        ) {
                            Text(
                                text = "✉",
                                fontSize = MigoType.titleSm,
                                color = Color.White,
                                textAlign = TextAlign.Center,
                            )
                        }
                        if (unread > 0) {
                            Surface(
                                color = Color(0xFFE5503C),
                                contentColor = Color.White,
                                shape = RoundedCornerShape(MigoRadius.pill),
                                modifier = Modifier.align(Alignment.TopEnd),
                            ) {
                                Text(
                                    text = if (unread > 9) "9+" else unread.toString(),
                                    fontSize = MigoGlyph.badge,
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
                            .background(Color(0xFFD2690B), RoundedCornerShape(MigoRadius.md))
                            .clickable(onClick = onOpenMe),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            text = "⚙",
                            fontSize = MigoType.titleSm,
                            color = Color.White,
                            textAlign = TextAlign.Center,
                        )
                    }
                }
            }
        }
    }
}

/**
 * The account sheet: the avatar header with presence and the $MIG balance, the presence pills, and
 * the panels the home tabs cannot carry — each a cover-the-screen panel with its own way back.
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
                        fontSize = MigoType.meta,
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
            sub = "The \$MIG wallet, gifts and on-chain AVAX",
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
        SheetAction(
            glyph = "⚙",
            label = "Settings",
            sub = "Chats, privacy, storage and appearance",
            onClick = { onOpenSection(AppState.Section.SETTINGS) },
        )
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
                RoundedCornerShape(MigoRadius.md),
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
                fontSize = MigoType.body,
                fontWeight = FontWeight.SemiBold,
                color = if (selected) scheme.onPrimaryContainer else scheme.onSurfaceVariant,
            )
            if (selected) {
                Spacer(modifier = Modifier.width(6.dp))
                Text(
                    text = "✓",
                    fontSize = MigoType.body,
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

/** The band's dot colour: the connection's own word, worn as a colour — the same three hues
 *  every client wears, green connected, amber connecting or reconnecting, red gone. */
private fun connectionColor(connection: ConnectionState): Color = when (connection) {
    ConnectionState.Online -> Color(0xFF3FCE6B)
    ConnectionState.Connecting -> Color(0xFFF5B83D)
    ConnectionState.Reconnecting -> Color(0xFFF5B83D)
    ConnectionState.Closed -> Color(0xFFE5503C)
}

/** The connection's word, the same labels the panels' [ConnectionBadge] wears. */
private fun connectionLabel(connection: ConnectionState): String = when (connection) {
    ConnectionState.Online -> "Online"
    ConnectionState.Connecting -> "Connecting"
    ConnectionState.Reconnecting -> "Reconnecting"
    ConnectionState.Closed -> "Offline"
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
fun presenceLabel(presence: PresenceState?): String = when (presence) {
    PresenceState.Online -> "Online"
    PresenceState.Away -> "Away"
    PresenceState.Busy -> "Busy"
    PresenceState.Offline -> "Offline"
    PresenceState.Invisible -> "Invisible"
    PresenceState.Unknown, null -> "—"
}
