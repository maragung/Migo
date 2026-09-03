package com.migo.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.migo.app.model.ChatMessage
import com.migo.app.model.ChatState
import com.migo.app.model.RoomNotice
import com.migo.app.model.RosterMember
import com.migo.app.model.VoteTally
import com.migo.core.protocol.RoomRole
import com.migo.core.protocol.SanctionAction
import com.migo.core.wire.Id

/**
 * One conversation: its messages, and the field for adding to them.
 *
 * # Why the list is not reversed
 *
 * A reversed `LazyColumn` is the usual way to pin a chat to its newest message, and it is wrong here.
 * This app has no local message store, so a chat opens with the history it just fetched and grows
 * downward from a known end. Scrolling to the last index on change gives the same behaviour and keeps
 * the list in the order [ChatState.messages] is in, so what is drawn matches what is held -- which is
 * the difference between a rendering bug and an ordering bug when the two disagree.
 */
@Composable
fun ChatScreen(
    chat: ChatState,
    onBack: () -> Unit,
    onDraft: (String) -> Unit,
    onSend: () -> Unit,
    /** Leaves the room behind this chat, offered only when the chat is a room this shell knows. */
    onLeave: (() -> Unit)? = null,
    /** Opens the member sheet. Offered only for a room chat; null on a direct chat. */
    onOpenMembers: (() -> Unit)? = null,
    /** Closes the member sheet. */
    onCloseMembers: () -> Unit = {},
    /** Casts this account's voice in a kick vote against the given member. */
    onVoteKick: (Id) -> Unit = {},
    /** Applies a staff action to the given member. */
    onSanction: (Id, SanctionAction) -> Unit = { _, _ -> },
    /** Mutes or unmutes the given account for this device only. */
    onMuteForMe: (Id, Boolean) -> Unit = { _, _ -> },
    /** This account's own id, so the sheet never offers an action against oneself. */
    selfId: Id,
    modifier: Modifier = Modifier,
) {
    val listState = rememberLazyListState()

    // The drawn order is the messages and the room's own notices — joins, leaves, kicks — woven
    // together by time. A direct chat has no notices, so the weave is the message list untouched;
    // only a room pays the sort, and only over the ~150 lines a chat holds. Messages already sit in
    // sequence order, and a stable sort keeps them there among notices minted at the same instant.
    val timeline = remember(chat.messages, chat.notices) {
        if (chat.notices.isEmpty()) {
            chat.messages.map { TimelineItem.Message(it) }
        } else {
            (chat.messages.map { TimelineItem.Message(it) } + chat.notices.map { TimelineItem.Notice(it) })
                .sortedBy { it.at }
        }
    }
    val lastKey = timeline.lastOrNull()?.key

    // Follow the end of the conversation as it grows, and only then. Keyed on the last line rather
    // than the count so an edit or a deletion does not yank the view away from whatever somebody is
    // reading further up.
    LaunchedEffect(lastKey) {
        if (timeline.isNotEmpty()) {
            listState.animateScrollToItem(timeline.lastIndex)
        }
    }

    Box(modifier = modifier.fillMaxSize()) {
        Column(modifier = Modifier.fillMaxSize()) {
            ChatHeader(chat = chat, onBack = onBack, onLeave = onLeave, onOpenMembers = onOpenMembers)

            Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
                when {
                    chat.loading && timeline.isEmpty() -> Box(
                        modifier = Modifier.fillMaxSize(),
                        contentAlignment = Alignment.Center,
                    ) {
                        CircularProgressIndicator()
                    }

                    timeline.isEmpty() -> Placeholder(
                        text = "No messages yet. Anything you send is encrypted on this device first.",
                    )

                    else -> LazyColumn(
                        state = listState,
                        modifier = Modifier.fillMaxSize(),
                    ) {
                        items(timeline, key = { it.key }) { item ->
                            when (item) {
                                is TimelineItem.Message -> MessageBubble(message = item.message)
                                is TimelineItem.Notice -> SystemNotice(text = item.notice.text)
                            }
                        }
                    }
                }
            }

            if (chat.typing.isNotEmpty()) {
                Text(
                    text = if (chat.typing.size == 1) "Typing..." else "Several people are typing...",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(start = 16.dp, bottom = 2.dp),
                )
            }

            Composer(
                draft = chat.draft,
                sending = chat.sending,
                onDraft = onDraft,
                onSend = onSend,
            )
        }

        // The member sheet covers the thread rather than sitting beside it, so back closes the sheet
        // before it closes the chat: this handler is composed deeper than the shell's own, so it wins
        // the press while the sheet is up.
        if (chat.membersOpen && chat.roomId != null) {
            BackHandler(onBack = onCloseMembers)
            MembersSheet(
                chat = chat,
                selfId = selfId,
                onClose = onCloseMembers,
                onVoteKick = onVoteKick,
                onSanction = onSanction,
                onMuteForMe = onMuteForMe,
            )
        }
    }
}

@Composable
private fun ChatHeader(
    chat: ChatState,
    onBack: () -> Unit,
    onLeave: (() -> Unit)?,
    onOpenMembers: (() -> Unit)?,
) {
    Surface(color = MaterialTheme.colorScheme.surfaceVariant) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(end = 16.dp, top = 8.dp, bottom = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // A text arrow rather than an icon: this app declares no icon dependency, and a character
            // scales with the system font size.
            TextButton(onClick = onBack) {
                Text(text = "<", style = MaterialTheme.typography.titleMedium)
            }
            Monogram(name = chat.title, size = 36.dp)
            Spacer(modifier = Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = chat.title,
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                    maxLines = 1,
                )
                Text(
                    text = roomSubtitle(chat),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (chat.roomId != null && onOpenMembers != null) {
                TextButton(onClick = onOpenMembers) {
                    Text("Members")
                }
            }
            if (chat.roomId != null && onLeave != null) {
                TextButton(onClick = onLeave) {
                    Text("Leave", color = MaterialTheme.colorScheme.error)
                }
            }
        }
    }
}

/**
 * The header's second line.
 *
 * A room speaks its live shape -- how many are online now, of a capacity when it declares one, and
 * how many are members in all -- so "2/33 online · 33 members" reads at a glance the way the product
 * asks. The counts come from the room's own event streams, which do not always arrive at once, so a
 * room whose totals are not yet known keeps the plain word rather than showing a confident zero. A
 * direct chat says what it is: encrypted end to end, which a room deliberately is not (§178).
 */
private fun roomSubtitle(chat: ChatState): String {
    if (chat.roomId == null) return "Encrypted end to end"
    val room = chat.room ?: return "Room"
    if (room.memberCount <= 0L) return "Room"
    val members = "${room.memberCount} members"
    return if (room.maxMembers != null && room.maxMembers > 0L) {
        "${room.onlineCount}/${room.maxMembers} online · $members"
    } else {
        "${room.onlineCount} online · $members"
    }
}

/**
 * One message.
 *
 * Outgoing messages sit right in the primary colour, incoming ones sit left on the surface variant.
 * Both keep the corner on the speaker's side square, which is what makes a run of messages from one
 * person read as a run without a name on every line.
 */
@Composable
private fun MessageBubble(message: ChatMessage) {
    val scheme = MaterialTheme.colorScheme
    val shape = if (message.mine) {
        RoundedCornerShape(16.dp, 16.dp, 4.dp, 16.dp)
    } else {
        RoundedCornerShape(16.dp, 16.dp, 16.dp, 4.dp)
    }
    val container = when {
        message.unsupported -> scheme.surfaceVariant
        message.mine -> scheme.primary
        else -> scheme.surfaceVariant
    }
    val content = when {
        message.unsupported -> scheme.onSurfaceVariant
        message.mine -> scheme.onPrimary
        else -> scheme.onSurfaceVariant
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 3.dp),
        horizontalArrangement = if (message.mine) Arrangement.End else Arrangement.Start,
    ) {
        Surface(
            color = container,
            contentColor = content,
            shape = shape,
            modifier = Modifier.widthIn(max = 300.dp),
        ) {
            Column(modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
                if (!message.mine) {
                    Text(
                        text = message.author,
                        style = MaterialTheme.typography.labelSmall,
                        fontWeight = FontWeight.SemiBold,
                        color = scheme.primary,
                    )
                    Spacer(modifier = Modifier.height(2.dp))
                }
                Text(
                    text = message.text,
                    style = MaterialTheme.typography.bodyLarge,
                    fontStyle = if (message.unsupported) FontStyle.Italic else null,
                )
                Spacer(modifier = Modifier.height(2.dp))
                Text(
                    text = if (message.pending) "Sending..." else clockTime(message.at),
                    style = MaterialTheme.typography.labelSmall,
                    textAlign = TextAlign.End,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        }
    }
}

/**
 * The compose field and the send button.
 *
 * `imePadding` and `navigationBarsPadding` together, because this is the one row that has to stay above
 * the keyboard: a composer under the keyboard is a person typing into something they cannot see.
 */
@Composable
private fun Composer(
    draft: String,
    sending: Boolean,
    onDraft: (String) -> Unit,
    onSend: () -> Unit,
) {
    Surface(color = MaterialTheme.colorScheme.surface) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .imePadding()
                .navigationBarsPadding()
                .padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.Bottom,
        ) {
            OutlinedTextField(
                value = draft,
                onValueChange = onDraft,
                placeholder = { Text("Message") },
                maxLines = 5,
                shape = RoundedCornerShape(24.dp),
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.Sentences,
                    imeAction = ImeAction.Send,
                ),
                modifier = Modifier.weight(1f),
            )
            Spacer(modifier = Modifier.width(8.dp))
            FilledIconButton(
                onClick = onSend,
                enabled = draft.isNotBlank() && !sending,
                modifier = Modifier.size(52.dp),
            ) {
                if (sending) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(18.dp),
                        strokeWidth = 2.dp,
                        color = MaterialTheme.colorScheme.onPrimary,
                    )
                } else {
                    // A paper plane in the button's own content ink: the send mark the whole
                    // product draws, without an icon dependency (Canvas strokes, like the bottom
                    // bar's glyphs).
                    val sendInk = MaterialTheme.colorScheme.onPrimary
                    Canvas(modifier = Modifier.size(22.dp)) {
                        drawGlyphSend(sendInk)
                    }
                }
            }
        }
    }
}

/** The composer's paper-plane glyph, drawn on the unit canvas in the given ink. */
private fun androidx.compose.ui.graphics.drawscope.DrawScope.drawGlyphSend(
    color: androidx.compose.ui.graphics.Color,
) {
    val stroke = androidx.compose.ui.graphics.drawscope.Stroke(
        width = 1.75.dp.toPx(),
        cap = androidx.compose.ui.graphics.StrokeCap.Round,
    )
    fun p(x: Float, y: Float) = androidx.compose.ui.geometry.Offset(x * size.width, y * size.height)
    drawLine(color, p(0.26f, 0.52f), p(0.76f, 0.28f), strokeWidth = stroke.width, cap = stroke.cap)
    drawLine(color, p(0.26f, 0.52f), p(0.52f, 0.76f), strokeWidth = stroke.width, cap = stroke.cap)
    drawLine(color, p(0.52f, 0.76f), p(0.44f, 0.56f), strokeWidth = stroke.width, cap = stroke.cap)
    drawLine(color, p(0.44f, 0.56f), p(0.26f, 0.52f), strokeWidth = stroke.width, cap = stroke.cap)
    drawLine(color, p(0.76f, 0.28f), p(0.44f, 0.56f), strokeWidth = stroke.width, cap = stroke.cap)
}

/**
 * One non-message line in a room's timeline: a join, a leave, a disconnect, a reconnect, a kick, a
 * ban. Centred and quiet, because it is context around the conversation rather than part of it.
 */
@Composable
private fun SystemNotice(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 6.dp),
    )
}

/**
 * The member sheet: who is in the room, and what this account may do about them.
 *
 * It covers the thread as a full surface rather than a panel beside it, because a roster is a list
 * that scrolls and a phone has no room for one alongside a chat. Every row but one's own and the
 * Owner's offers a vote-kick -- the one power an ordinary member holds -- and shows the running tally
 * once a vote is open. The staff powers (mute, kick, ban) appear only on a row this account outranks,
 * and only when this account is a Moderator or above; a kick or a ban asks twice, because removing
 * somebody is not a thing a single mis-tap should do. "Mute for me" is a personal choice on every
 * row, and the muted accounts who are not in the room gather in their own list with an Unmute.
 */
@Composable
private fun MembersSheet(
    chat: ChatState,
    selfId: Id,
    onClose: () -> Unit,
    onVoteKick: (Id) -> Unit,
    onSanction: (Id, SanctionAction) -> Unit,
    onMuteForMe: (Id, Boolean) -> Unit,
) {
    val myRole = chat.room?.myRole ?: RoomRole.Unknown
    val roster = chat.roster
    // Names for the muted list: a muted account also in the room is named from the roster, one that
    // is not shows the short id the shell holds -- honest about the one fact it has.
    val rosterNames = roster?.associate { it.userId to it.name } ?: emptyMap()

    Surface(color = MaterialTheme.colorScheme.surface, modifier = Modifier.fillMaxSize()) {
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
                        text = "Members",
                        style = MaterialTheme.typography.titleMedium,
                        color = MaterialTheme.colorScheme.onSurface,
                        modifier = Modifier.weight(1f),
                    )
                }
            }

            Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
                when {
                    chat.rosterLoading && roster == null -> Box(
                        modifier = Modifier.fillMaxSize(),
                        contentAlignment = Alignment.Center,
                    ) {
                        CircularProgressIndicator()
                    }

                    roster == null || roster.isEmpty() -> Placeholder(text = "No members to show.")

                    else -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                        items(roster, key = { "member-" + it.userId.value }) { member ->
                            MemberRow(
                                member = member,
                                isSelf = member.userId == selfId,
                                myRole = myRole,
                                tally = chat.votes[member.userId],
                                muted = member.userId in chat.muted,
                                acting = member.userId in chat.acting,
                                onVoteKick = { onVoteKick(member.userId) },
                                onSanction = { action -> onSanction(member.userId, action) },
                                onMuteForMe = { on -> onMuteForMe(member.userId, on) },
                            )
                        }

                        val mutedOnly = chat.muted.filter { id -> roster.none { it.userId == id } }
                        if (mutedOnly.isNotEmpty()) {
                            item(key = "muted-label") {
                                HorizontalDivider()
                                SectionLabel(text = "Muted")
                            }
                            items(mutedOnly, key = { "muted-" + it.value }) { id ->
                                MutedRow(
                                    name = rosterNames[id] ?: id.value.take(8),
                                    acting = id in chat.acting,
                                    onUnmute = { onMuteForMe(id, false) },
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

/**
 * One roster row: the member, their role, and the actions this account may take on them.
 *
 * The actions sit under the name in up to two lines so a row never runs off a narrow screen: the
 * vote and the personal mute on one, and the staff powers -- shown only to a Moderator-or-above over
 * a strictly lower role -- on the next. One's own row carries no actions at all.
 */
@Composable
private fun MemberRow(
    member: RosterMember,
    isSelf: Boolean,
    myRole: RoomRole,
    tally: VoteTally?,
    muted: Boolean,
    acting: Boolean,
    onVoteKick: () -> Unit,
    onSanction: (SanctionAction) -> Unit,
    onMuteForMe: (Boolean) -> Unit,
) {
    val staff = !isSelf && myRole.wire >= RoomRole.Moderator.wire && member.role.wire < myRole.wire
    val canVote = !isSelf && member.role != RoomRole.Owner

    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Monogram(name = member.name, size = 32.dp)
            Spacer(modifier = Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = if (isSelf) member.name + " (you)" else member.name,
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurface,
                    maxLines = 1,
                )
                Text(
                    text = roleLabel(member.role),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (acting) {
                CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
            }
        }
        if (!isSelf) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (canVote) {
                    TextButton(onClick = onVoteKick, enabled = !acting) {
                        Text(if (tally != null) "Vote kick ${tally.votes}/${tally.needed}" else "Vote kick")
                    }
                }
                TextButton(onClick = { onMuteForMe(!muted) }, enabled = !acting) {
                    Text(if (muted) "Unmute" else "Mute for me")
                }
            }
            if (staff) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    ConfirmTextButton(label = "Kick", enabled = !acting) { onSanction(SanctionAction.Kick) }
                    ConfirmTextButton(label = "Ban", enabled = !acting) { onSanction(SanctionAction.Ban) }
                    TextButton(onClick = { onSanction(SanctionAction.Mute) }, enabled = !acting) {
                        Text("Mute")
                    }
                }
            }
        }
    }
}

/** One account this device has muted who is not in the room, with the control to lift it. */
@Composable
private fun MutedRow(name: String, acting: Boolean, onUnmute: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Monogram(name = name, size = 32.dp)
        Spacer(modifier = Modifier.width(12.dp))
        Text(
            text = name,
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurface,
            maxLines = 1,
            modifier = Modifier.weight(1f),
        )
        TextButton(onClick = onUnmute, enabled = !acting) {
            Text("Unmute")
        }
    }
}

/**
 * A destructive text button that asks once before it acts.
 *
 * The first tap turns the label into "Sure?"; the second, within the same button, is the one that
 * fires. A removal a single mis-tap could cause is not one this sheet should make on a single tap.
 */
@Composable
private fun ConfirmTextButton(label: String, enabled: Boolean, onConfirm: () -> Unit) {
    val armed = remember { mutableStateOf(false) }
    TextButton(
        onClick = {
            if (armed.value) {
                onConfirm()
                armed.value = false
            } else {
                armed.value = true
            }
        },
        enabled = enabled,
    ) {
        Text(
            text = if (armed.value) "Sure?" else label,
            color = MaterialTheme.colorScheme.error,
        )
    }
}

/** A room role as the sheet names it; a role this build cannot name is at least a member. */
private fun roleLabel(role: RoomRole): String = when (role) {
    RoomRole.Owner -> "Owner"
    RoomRole.Manager -> "Manager"
    RoomRole.Admin -> "Admin"
    RoomRole.Moderator -> "Moderator"
    RoomRole.Helper -> "Helper"
    RoomRole.Member -> "Member"
    RoomRole.Unknown -> "Member"
}

/**
 * A line in the woven timeline: either a message bubble or a room notice.
 *
 * Both carry the [at] the weave sorts on and a [key] distinct across the two kinds, so a `LazyColumn`
 * can draw them in one list without a message id and a notice key ever colliding.
 */
private sealed interface TimelineItem {
    val at: Long
    val key: String

    class Message(val message: ChatMessage) : TimelineItem {
        override val at: Long get() = message.at
        override val key: String get() = "m-" + message.messageId.value
    }

    class Notice(val notice: RoomNotice) : TimelineItem {
        override val at: Long get() = notice.at
        override val key: String get() = notice.key
    }
}
