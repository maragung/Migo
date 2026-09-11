package com.migo.app.ui

import android.graphics.BitmapFactory
import android.media.MediaPlayer
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
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
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.migo.app.media.formatBytes
import com.migo.app.media.formatDuration
import com.migo.app.model.Attachment
import com.migo.app.model.AttachmentKind
import com.migo.app.model.ChatMessage
import com.migo.app.model.ChatSafety
import com.migo.app.model.ChatState
import com.migo.app.model.GAME_KIND_GUESS_NUMBER
import com.migo.app.model.GAME_STATUS_OPEN
import com.migo.app.model.GROUP_MUTE_TERMS
import com.migo.app.model.GroupMember
import com.migo.app.model.MediaObject
import com.migo.app.model.QUICK_REACTIONS
import com.migo.app.model.RoomNotice
import com.migo.app.model.RosterMember
import com.migo.app.model.VoteTally
import com.migo.app.model.gameLabelOf
import com.migo.app.model.groupRoleLabel
import com.migo.app.model.guessFeedbackLine
import com.migo.app.model.parseGuessBoard
import com.migo.app.model.playerRangeLabel
import com.migo.core.domain.canFounderAct
import com.migo.core.domain.canVoteKickGroup
import com.migo.core.domain.filterChatSearch
import com.migo.core.protocol.ConversationKind
import com.migo.core.protocol.ConversationRole
import com.migo.core.protocol.GameCatalogueEntry
import com.migo.core.protocol.GameViewWire
import com.migo.core.protocol.RoomRole
import com.migo.core.protocol.SanctionAction
import com.migo.core.wire.Id
import java.io.File
import kotlinx.coroutines.delay

/**
 * One conversation, shown full-bleed beneath the window strip: its messages, and the field for
 * adding to them.
 *
 * The strip is the chat's own way back — the conversation's tab is how the person arrived — so the
 * header carries no back control of its own; the mobile reference's windows have no title bars. The
 * member sheet keeps its own, because a sheet closes to the thread, not to the strip.
 *
 * # The verification surface
 *
 * A direct chat carries the pair's safety numbers (brief sections 47 and 164): a header control
 * opens the sheet that shows them, and a peer's changed identity key is warned about in a banner
 * above the thread rather than accepted silently — the acknowledgment that clears the banner is
 * the person's press, never the read that raised it.
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
    /**
     * Opens the group's member sheet. Offered only for a group chat; the group's sheet is its own
     * surface, because the facts it draws (group roles, founder mutes) are not the room's.
     */
    onOpenGroupMembers: (() -> Unit)? = null,
    /** Closes the group's member sheet. */
    onCloseGroupMembers: () -> Unit = {},
    /** Invites the given account into this group, from the sheet's invite row. */
    onInvite: (Id) -> Unit = {},
    /** Casts this account's voice in a group kick vote against the given member. */
    onGroupVoteKick: (Id) -> Unit = {},
    /** Applies a founder's group mute -- a term, or null to lift one early. */
    onGroupMute: (Id, Long?) -> Unit = { _, _ -> },
    /** Removes the given member outright, no vote -- a founder's call. */
    onGroupKick: (Id) -> Unit = {},
    /** Renames the group to the given title -- a founder's action. */
    onRenameGroup: (String) -> Unit = {},
    /** Opens or closes the rename field; the view model seeds the value. */
    onToggleRename: () -> Unit = {},
    /** Records the rename field's live text. */
    onRenameValue: (String) -> Unit = {},
    /** Leaves the group -- nobody's permission is asked. */
    onLeaveGroup: (() -> Unit)? = null,
    /** The friends this shell knows, for the group sheet's invite quick-pick. */
    groupInvitees: List<GroupInviteCandidate> = emptyList(),
    /** The node's game catalogue, shared with the Games panel; null before the first read. */
    gameCatalogue: List<GameCatalogueEntry>? = null,
    /** True while the shared catalogue read is in flight. */
    gamesLoading: Boolean = false,
    /** Why the shared catalogue read could not answer. */
    gamesFailure: String? = null,
    /** Reads the game catalogue, on the launcher's first open and on a retry. */
    onLoadGames: () -> Unit = {},
    /** Starts a game by catalogue slug in this conversation. */
    onStartGame: (String) -> Unit = {},
    /** Submits a guess for the conversation's active game. */
    onGuess: (Long) -> Unit = {},
    /** This account's own id, so the sheet never offers an action against oneself. */
    selfId: Id,
    /** Acknowledges a changed safety number for this conversation, from the warning itself. */
    onAcknowledgeSafety: () -> Unit = {},
    /**
     * Places a voice call to the direct chat's peer, handed the peer's id. Offered only for a
     * direct chat -- the call is the direct conversation's other half, and a room's audience has
     * no 1:1 to call. Null when the shell cannot place calls.
     */
    onStartCall: ((Id) -> Unit)? = null,
    /**
     * Shares this conversation's transcript as a log, from the header. Null when the shell has no
     * share route; the transcript itself is the model's to build, because the log is the same
     * plaintext the auto-saved snapshots are written from.
     */
    onExportLog: (() -> Unit)? = null,
    /**
     * Opens or closes the thread's search field. Offered in every conversation kind, exactly as on
     * the web — a filter over what the thread holds needs no conversation feature to exist.
     */
    onToggleSearch: () -> Unit = {},
    /** Records the live search query; the filter runs on the messages this device already holds. */
    onSearchQuery: (String) -> Unit = {},
    /**
     * Opens the file picker for an attachment, whose answer is sent into the conversation. Null in
     * a server-readable room — attachments are an end-to-end feature (a room has no key channel to
     * hand the recipients a sealed object's key), mirroring the web composer's own gating.
     */
    onAttach: (() -> Unit)? = null,
    /**
     * Starts a voice note: asks the microphone permission at the moment of use and begins the
     * recording. Offered in every conversation kind, because a voice note is speech and speech is
     * what every conversation is for.
     */
    onVoiceNote: () -> Unit = {},
    /** Finishes the recording and sends it. Offered only while [ChatState.recording] holds. */
    onStopVoiceNote: () -> Unit = {},
    /** Throws the recording away. Offered only while [ChatState.recording] holds. */
    onCancelVoiceNote: () -> Unit = {},
    /** Sets one of the quick reactions on a message, from the long-press bar. */
    onReact: (Id, String) -> Unit = { _, _ -> },
    /**
     * Commits an edit of one of this device's own text lines, handed the message id and the
     * replacement text. Offered only where the shell can seal and send the replacement.
     */
    onEdit: (Id, String) -> Unit = { _, _ -> },
    /** Withdraws one of this device's own messages from everyone's copy, from the bar's Delete. */
    onDelete: (Id) -> Unit = {},
    /** Asks the resolver to fetch and open the given attachment's object. */
    onResolveMedia: (Attachment) -> Unit = {},
    /**
     * Whether an attachment resolves itself on first display. False is the person's own media
     * choice — never, or an unmetered network that turned out metered — and every bubble then
     * offers its bytes as a tap instead of taking them.
     */
    autoFetchMedia: Boolean = true,
    /**
     * Saves the given document, handed the attachment so the shell can stage it and name the
     * destination picker's suggestion after the sender's file name.
     */
    onSaveDocument: (Attachment) -> Unit = {},
    /** The session's resolved media, by media id — what an attachment bubble reads its object from. */
    mediaObjects: Map<Id, MediaObject> = emptyMap(),
    modifier: Modifier = Modifier,
) {
    val listState = rememberLazyListState()

    // The search is a filter over what the thread already holds — the messages this device has
    // decrypted, never a server query, so nothing leaves the device to answer it. A blank query
    // hands back the same list untouched (the filter's own contract), so the remember keys below
    // stay stable until the person actually types. A query matches only text bodies — an
    // attachment's caption and an unsupported body are not text bodies, the web client's
    // ContentType.Text rule — case-blind, per keystroke, with no submit step.
    val shownMessages = filterChatSearch(chat.messages, chat.searchQuery) { message ->
        if (message.attachment == null && !message.unsupported) message.text else null
    }

    // The drawn order is the messages and the room's own notices — joins, leaves, kicks — woven
    // together by time. A direct chat has no notices, so the weave is the message list untouched;
    // only a room pays the sort, and only over the ~150 lines a chat holds. Messages already sit in
    // sequence order, and a stable sort keeps them there among notices minted at the same instant.
    // The notices stay woven and unfiltered while a search runs: they are the thread's own
    // scaffolding, and the web client splices them in unfiltered too.
    val timeline = remember(shownMessages, chat.notices) {
        if (chat.notices.isEmpty()) {
            shownMessages.map { TimelineItem.Message(it) }
        } else {
            (shownMessages.map { TimelineItem.Message(it) } + chat.notices.map { TimelineItem.Notice(it) })
                .sortedBy { it.at }
        }
    }
    // Which lines are run heads: the first message of a consecutive run from one sender. The
    // transcript draws the avatar only on a head, which is what makes a run read as a run without
    // repeating the 22dp disc on every line -- the same job the bubble's square corner once did.
    // A notice breaks the run, because a notice means time passed between the two messages.
    val heads = remember(timeline) {
        val marks = mutableMapOf<String, Boolean>()
        var previous: Pair<Boolean, String>? = null
        for (item in timeline) {
            when (item) {
                is TimelineItem.Message -> {
                    val sender = item.message.mine to item.message.author
                    marks[item.key] = sender != previous
                    previous = sender
                }
                is TimelineItem.Notice -> previous = null
            }
        }
        marks
    }
    val lastKey = timeline.lastOrNull()?.key

    // The game launcher's sheet state, owned here because the sheet is the header's own menu: the
    // button opens it, and the catalogue it lists is the node's, shared with the Games panel.
    val gamesOpen = remember { mutableStateOf(false) }

    // The verification sheet's own state, for the same reason: the header's Safety control opens
    // it, and the warning banner reopens it, so the flag lives where both can reach it.
    val safetyOpen = remember { mutableStateOf(false) }

    // The catalogue loads on first open and stays for the thread's life — a person who never
    // touches the button never pays for its data, and a failure is retried by closing and
    // reopening, which is cheaper to discover than a button that does nothing.
    LaunchedEffect(gamesOpen.value) {
        if (gamesOpen.value && gameCatalogue == null) onLoadGames()
    }

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
            ChatHeader(
                chat = chat,
                onLeave = onLeave,
                onOpenMembers = onOpenMembers,
                onOpenGames = { gamesOpen.value = true },
                onOpenSafety = if (chat.peerId != null) {
                    { safetyOpen.value = true }
                } else {
                    null
                },
                onStartCall = onStartCall,
                onExportLog = onExportLog,
                onToggleSearch = onToggleSearch,
                onOpenGroupMembers = onOpenGroupMembers,
                onLeaveGroup = onLeaveGroup,
            )

            // The change warning (§164) sits between the header and the thread, because it is about
            // the conversation as a whole rather than any one message in it — and because a banner
            // inside the scrolled list is a banner a screenful of history hides.
            if (chat.safety?.changed == true) {
                SafetyWarningBanner(onReview = { safetyOpen.value = true })
            }

            // The search field, under the warning and above the thread — the web client's own
            // placement. The placeholder names the honest scope: this filters the messages this
            // device holds, not the server's whole history, and a field that promised more would
            // be lying in the one place a person reads before typing.
            if (chat.searchOpen) {
                OutlinedTextField(
                    value = chat.searchQuery,
                    onValueChange = onSearchQuery,
                    modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 4.dp),
                    placeholder = { Text("Filter loaded messages") },
                    singleLine = true,
                )
            }

            Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
                when {
                    // Loading is judged on the messages held, not the filtered view: a query that
                    // empties the thread mid-load must show the no-match sentence, not a spinner
                    // that only ends when the history does.
                    chat.loading && chat.messages.isEmpty() -> Box(
                        modifier = Modifier.fillMaxSize(),
                        contentAlignment = Alignment.Center,
                    ) {
                        CircularProgressIndicator()
                    }

                    timeline.isEmpty() && chat.searchQuery.isNotBlank() -> Placeholder(
                        text = "No loaded messages match.",
                    )

                    timeline.isEmpty() -> Placeholder(
                        text = "No messages yet. Anything you send is encrypted on this device first.",
                    )

                    else -> LazyColumn(
                        state = listState,
                        modifier = Modifier.fillMaxSize(),
                    ) {
                        items(timeline, key = { it.key }) { item ->
                            when (item) {
                                is TimelineItem.Message -> MessageLine(
                                    message = item.message,
                                    head = heads[item.key] == true,
                                    onReact = onReact,
                                    onEdit = onEdit,
                                    onDelete = onDelete,
                                    onResolveMedia = onResolveMedia,
                                    onSaveDocument = onSaveDocument,
                                    autoFetchMedia = autoFetchMedia,
                                    mediaObject = mediaObjects[item.message.attachment?.mediaId],
                                )
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

            // The active guessing game's input, above the composer: it is the conversation's one
            // live question, and a card that scrolled away inside the list would be a question the
            // reader has to hunt for. Gated on the view's own yourTurn and open status — another
            // member's solo game is theirs to play, and a finished game has no input left.
            chat.game
                ?.takeIf { it.kind == GAME_KIND_GUESS_NUMBER && it.status == GAME_STATUS_OPEN && it.yourTurn == true }
                ?.let { active -> GuessCard(game = active, busy = chat.gameBusy, onGuess = onGuess) }

            // While a recording runs, the composer is the recording bar: no text can be typed into a
            // moment that is being recorded, and the bar that says so is the same surface that
            // stops or throws it away.
            if (chat.recording) {
                RecordingBar(onStop = onStopVoiceNote, onCancel = onCancelVoiceNote)
            } else {
                Composer(
                    draft = chat.draft,
                    sending = chat.sending,
                    uploading = chat.uploading,
                    onDraft = onDraft,
                    onSend = onSend,
                    onAttach = onAttach,
                    onVoiceNote = onVoiceNote,
                )
            }
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

        // The group's member sheet: the roster with the founder controls, the invite quick-pick,
        // the rename, and the way out. Back closes it, as the room's does.
        if (chat.membersOpen && chat.kind == ConversationKind.Group && chat.roomId == null) {
            BackHandler(onBack = onCloseGroupMembers)
            GroupMembersSheet(
                chat = chat,
                selfId = selfId,
                invitees = groupInvitees,
                onClose = onCloseGroupMembers,
                onInvite = onInvite,
                onVoteKick = onGroupVoteKick,
                onMute = onGroupMute,
                onKick = onGroupKick,
                onRename = onRenameGroup,
                onToggleRename = onToggleRename,
                onRenameValue = onRenameValue,
            )
        }

        // The game launcher: the node's catalogue as the sheet the header's Games control opens.
        // Only single-player games are startable through this build's wire — GAME_START cannot name
        // opponents, so the server refuses a two-player kind outright — and rather than send a
        // request the protocol has already doomed, those entries render disabled with the reason
        // beside them.
        if (gamesOpen.value) {
            GameLauncherSheet(
                catalogue = gameCatalogue,
                loading = gamesLoading,
                failure = gamesFailure,
                busy = chat.gameBusy,
                onDismiss = { gamesOpen.value = false },
                onRetry = onLoadGames,
                onStart = { slug ->
                    gamesOpen.value = false
                    onStartGame(slug)
                },
            )
        }

        // The verification sheet: the conversation's safety numbers, and — when an identity
        // changed — the acknowledgment that clears the warning. Back closes it, for the same
        // reason the member sheet below declares its own handler: the sheet covers the thread.
        if (safetyOpen.value) {
            BackHandler(onBack = { safetyOpen.value = false })
            SafetySheet(
                safety = chat.safety,
                onDismiss = { safetyOpen.value = false },
                onAcknowledge = onAcknowledgeSafety,
            )
        }
    }
}

@Composable
private fun ChatHeader(
    chat: ChatState,
    onLeave: (() -> Unit)?,
    onOpenMembers: (() -> Unit)?,
    onOpenGames: () -> Unit,
    onOpenSafety: (() -> Unit)? = null,
    onStartCall: ((Id) -> Unit)? = null,
    onExportLog: (() -> Unit)? = null,
    onToggleSearch: () -> Unit = {},
    onOpenGroupMembers: (() -> Unit)? = null,
    onLeaveGroup: (() -> Unit)? = null,
) {
    // Games are offered only where a game has an audience: a room or a group conversation, never a
    // direct chat — the web client's own rule, because a game is the room's shared spectacle.
    val supportsGames = chat.kind == ConversationKind.Room || chat.kind == ConversationKind.Group
    Surface(color = MaterialTheme.colorScheme.surfaceVariant) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // No back control: the window strip above is the chat's own way back, the mobile
            // reference's windows having no title bars.
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
            if (supportsGames) {
                TextButton(onClick = onOpenGames) {
                    Text("Games")
                }
            }
            // A direct chat's one header extra: the door to its safety numbers. It is the room
            // chat's Members control in reverse — the room's security surface is who is in it, the
            // direct chat's is who the other side turned out to be. Gated on the peer id rather
            // than the room's absence, because the safety read itself needs that id: a chat with
            // no peer to read offers no door.
            if (chat.peerId != null && onOpenSafety != null) {
                TextButton(onClick = onOpenSafety) {
                    Text("Safety")
                }
            }
            // The direct chat's other extra: the voice call, the conversation's other half. Same
            // peer-id gate as Safety -- the call button dials the peer, and a chat with no peer has
            // no number to dial. The glyph is an emoji character, not an icon font, the app's own
            // rule (and the web client's, whose button this is a port of).
            if (chat.peerId != null && onStartCall != null) {
                val peer = chat.peerId
                TextButton(onClick = { onStartCall(peer) }) {
                    Text("📞")
                }
            }
            // The thread's own search, before the Log control as on the web. Offered in every
            // conversation kind — the filter runs on what the thread already holds, so there is no
            // conversation feature for it to depend on. The label is the toggle's own sentence:
            // tapping it again closes the field, and the toggle clears the query either way.
            TextButton(onClick = onToggleSearch) {
                Text(if (chat.searchOpen) "Close search" else "Search")
            }
            // The conversation's own record: the transcript this device holds, handed to whatever
            // the system shares text with. Offered in every conversation kind — a log is a log
            // whether the room is encrypted or not — and stated as plaintext by the share sheet
            // it opens into.
            if (onExportLog != null) {
                TextButton(onClick = onExportLog) {
                    Text("Log")
                }
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
            // The group's member sheet door: the same word the room's control uses, because the
            // question it answers -- who is in here -- is the same question. Gated on the kind
            // rather than the roster's presence, so a group the sheet has not read yet still
            // offers the door that reads it.
            if (chat.kind == ConversationKind.Group && onOpenGroupMembers != null) {
                TextButton(onClick = onOpenGroupMembers) {
                    Text("Members")
                }
            }
            if (chat.kind == ConversationKind.Group && onLeaveGroup != null) {
                TextButton(onClick = onLeaveGroup) {
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
    if (chat.kind == ConversationKind.Group) {
        // A group speaks its size the way a room speaks its occupancy: how many belong. The count
        // is unknown until a summary or a member event has named it, and the honest word for that
        // is the group itself, not a confident zero.
        val count = chat.group?.memberCount ?: 0L
        return if (count > 0L) "$count members · encrypted end to end" else "Group · encrypted end to end"
    }
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
 * The change warning: the one thing a peer's rotated identity key must never be (§164) — silent.
 *
 * It names the fact and hands over the review door in the same breath, because the two halves
 * separately are either an alarm with no way to answer it or a settings screen nobody opens. The
 * warning does not block the conversation: messages still send and still decrypt, since a changed
 * key is also what an honest reinstall looks like. What it refuses to do is let the change pass
 * unremarked, which is the entire requirement.
 */
@Composable
private fun SafetyWarningBanner(onReview: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.errorContainer, modifier = Modifier.fillMaxWidth()) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(start = 16.dp, top = 4.dp, end = 8.dp, bottom = 4.dp),
        ) {
            Text(
                text = "Your contact's identity key changed. Verify the safety number before " +
                    "trusting this conversation.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onErrorContainer,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = onReview) {
                Text("Review")
            }
        }
    }
}

/**
 * The verification surface for a direct conversation: one safety number per device the peer
 * publishes, and the words that make the number mean something.
 *
 * A safety number is only worth the comparison behind it, so the explanation is the desktop
 * client's own sentence — compare in a call or in person, and a mismatch means stop — rather than
 * a softer paraphrase. The numbers are monospaced so two strings that differ in one digit differ
 * *visibly*, which is the whole reason anyone reads them aloud.
 *
 * The acknowledgment button exists only while a change is unacknowledged: clearing the warning is
 * the person's act, never the read's, and a button that offered to "acknowledge" an unchanged
 * number would be teaching that the word means nothing.
 */
@Composable
private fun SafetySheet(
    safety: ChatSafety?,
    onDismiss: () -> Unit,
    onAcknowledge: () -> Unit,
) {
    MigoSheet(title = "Safety numbers", onDismiss = onDismiss) {
        when {
            // Null is the read in flight — and the honest state, because a number shown before
            // the read lands is a number invented on the spot.
            safety == null -> LoadingRow()

            safety.failure != null -> Text(
                text = safety.failure ?: "",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )

            else -> {
                for (number in safety.numbers) {
                    // One block per device, labelled only when there is more than one to tell
                    // apart: the single-device case is the common one, and "Device ab12cd" over a
                    // lone number is chrome explaining itself.
                    if (safety.numbers.size > 1) {
                        Text(
                            text = "Device ${number.deviceId.value.take(8)}",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(start = 16.dp, top = 8.dp),
                        )
                    }
                    Text(
                        text = number.number,
                        style = MaterialTheme.typography.bodyLarge.copy(fontFamily = FontFamily.Monospace),
                        color = if (number.changed) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.onSurface
                        },
                        modifier = Modifier.padding(horizontal = 16.dp),
                    )
                    if (number.changed) {
                        Text(
                            text = "This device's identity key changed since this conversation " +
                                "last saw it.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(horizontal = 16.dp),
                        )
                    }
                }
                Text(
                    text = "Compare this with the other person, in a call or in person. If it " +
                        "differs, stop and do not trust the conversation.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                )
                if (safety.changed) {
                    Button(
                        onClick = onAcknowledge,
                        modifier = Modifier.padding(start = 8.dp, bottom = 8.dp),
                    ) {
                        Text("I've checked the new number")
                    }
                }
            }
        }
        Spacer(modifier = Modifier.height(8.dp))
    }
}

/**
 * The game launcher sheet: the node's catalogue, one row per kind it can referee.
 *
 * A solo entry starts the game on tap and closes the sheet; a multi-player entry is disabled with
 * its reason on the row, because a button that fails after a round trip the protocol has already
 * doomed is not an affordance, it is a lie with a spinner. The catalogue is the node's own and
 * versionless, so it is read when the sheet first opens rather than cached across sessions.
 */
@Composable
private fun GameLauncherSheet(
    catalogue: List<GameCatalogueEntry>?,
    loading: Boolean,
    failure: String?,
    busy: Boolean,
    onDismiss: () -> Unit,
    onRetry: () -> Unit,
    onStart: (String) -> Unit,
) {
    MigoSheet(title = "Start a game", onDismiss = onDismiss) {
        when {
            catalogue == null -> Column(modifier = Modifier.fillMaxWidth()) {
                if (loading) {
                    LoadingRow()
                } else {
                    Text(
                        text = failure ?: "The catalogue is not loaded.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    )
                    TextButton(onClick = onRetry, modifier = Modifier.padding(start = 8.dp)) {
                        Text("Try again")
                    }
                }
            }

            catalogue.isEmpty() -> Placeholder(text = "No games on this server.")

            else -> {
                for (entry in catalogue) {
                    val solo = entry.minPlayers <= 1L
                    SheetAction(
                        glyph = "🎮",
                        label = gameLabelOf(entry.kind),
                        sub = if (solo) {
                            playerRangeLabel(entry.minPlayers, entry.maxPlayers)
                        } else {
                            "Needs two players — this build cannot pick an opponent"
                        },
                        enabled = solo && !busy,
                        onClick = { onStart(entry.slug) },
                    )
                }
            }
        }
        Spacer(modifier = Modifier.height(8.dp))
    }
}

/**
 * The inline guess input for the active game.
 *
 * Validation is local and lenient about *when* it blocks — the button disables until the field
 * holds an integer inside the board's live range — but the range itself is read from the board,
 * not hard-coded, so a server configured differently is obeyed rather than argued with. The
 * server re-validates regardless; an out-of-range value that slipped through comes back as the
 * shell's own failure banner.
 */
@Composable
private fun GuessCard(
    game: GameViewWire,
    busy: Boolean,
    onGuess: (Long) -> Unit,
) {
    val board = parseGuessBoard(game.board)
    // Without a parsable board the card still offers the protocol's bound: a board this client
    // cannot read is no reason to hide the input the game is waiting on.
    val low = board?.low ?: 1L
    val high = board?.high ?: 100L
    val field = remember(game.gameId) { mutableStateOf("") }
    val value = field.value.trim().toLongOrNull()
    val valid = value != null && value >= low && value <= high
    val feedback = board?.let { guessFeedbackLine(it) }

    Surface(color = MaterialTheme.colorScheme.surfaceVariant, modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(start = 16.dp, top = 8.dp, end = 16.dp, bottom = 8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = "🎯 " + gameLabelOf(game.kind),
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.weight(1f),
                )
                if (board != null) {
                    Text(
                        text = board.low.toString() + "–" + board.high + ", " +
                            board.remaining + (if (board.remaining == 1L) " guess left" else " guesses left"),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            if (feedback != null) {
                Text(
                    text = feedback,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = field.value,
                    onValueChange = { field.value = it },
                    placeholder = { Text("Enter your guess ($low-$high)") },
                    singleLine = true,
                    shape = RoundedCornerShape(14.dp),
                    keyboardOptions = KeyboardOptions(
                        keyboardType = KeyboardType.Number,
                        imeAction = ImeAction.Send,
                    ),
                    modifier = Modifier.weight(1f),
                )
                Spacer(modifier = Modifier.width(8.dp))
                Button(
                    onClick = {
                        val guess = value
                        if (guess != null && valid && !busy) {
                            onGuess(guess)
                            field.value = ""
                        }
                    },
                    enabled = valid && !busy,
                ) {
                    if (busy) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(16.dp),
                            strokeWidth = 2.dp,
                            color = MaterialTheme.colorScheme.onPrimary,
                        )
                    } else {
                        Text("Guess")
                    }
                }
            }
        }
    }
}

/**
 * One message, as the reference's IRC-style line: no bubble, just a reserved 24dp avatar column
 * (the 22dp disc drawn only on a run head) and a single wrapping line whose bold sender name
 * introduces the body. The timestamp and the delivery tick ride at the end of the same line as a
 * quiet 9.5sp run, so a line carries everything it owns in one measure.
 *
 * The sender's name is coloured by a stable hash of the name, which keeps one speaker one colour
 * for the length of a conversation; own messages are the exception, pinned to the teal head so
 * one's own words are always the same colour to oneself.
 *
 * A long-press opens the quick-reaction bar — the reactions affordance, offered on every line
 * (one's own included, because reacting to one's own words is legal speech). A media or voice body
 * draws its [AttachmentBlock] under the line, in the same avatar-indented column the text sits in.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun MessageLine(
    message: ChatMessage,
    head: Boolean,
    onReact: (Id, String) -> Unit,
    onEdit: (Id, String) -> Unit,
    onDelete: (Id) -> Unit,
    onResolveMedia: (Attachment) -> Unit,
    onSaveDocument: (Attachment) -> Unit,
    autoFetchMedia: Boolean,
    mediaObject: MediaObject?,
) {
    val scheme = MaterialTheme.colorScheme
    // The dark scheme's surfaces are too deep for the light ink the reference's name colours were
    // measured against, so the own-message name and the body follow the theme and the hashed
    // palette keeps its values in both -- the reference's own choice, ported as it stands.
    val ownName = if (isSystemInDarkTheme()) Color(0xFF6FD0E6) else Color(0xFF0D6373)
    val stampInk = LocalMigoExtra.current.faint
    val name = if (message.mine) message.author.ifEmpty { "You" } else message.author
    // The reaction bar opens under the line it belongs to, so a press lands on the message the
    // reader was looking at -- the state is local because the bar's life is the press's life.
    val reactionBarOpen = remember { mutableStateOf(false) }
    // The own-line editor: opened from the bar's Edit, closed by Save or Cancel. Local for the
    // same reason -- its life is the edit's life, not the screen's.
    val editing = remember { mutableStateOf(false) }
    // A text line this device sent, and so the one line Edit may be offered on: the server only
    // permits the sender to edit, and only a text body has a text to replace.
    val editable = message.mine && message.attachment == null && !message.unsupported
    val line = buildAnnotatedString {
        withStyle(
            SpanStyle(
                fontWeight = FontWeight.Bold,
                color = if (message.mine) ownName else nameColor(name),
            ),
        ) {
            append(name)
            if (message.mine) append(" (me)")
        }
        withStyle(SpanStyle(color = scheme.onSurface)) { append(": ") }
        withStyle(
            SpanStyle(
                color = scheme.onSurface,
                fontStyle = if (message.unsupported) FontStyle.Italic else null,
            ),
        ) {
            append(message.text)
        }
        // The trailing state: the clock when the server has accepted the line, the word while it
        // has not, the edited stamp when the text has been replaced, and the tick only on one's
        // own messages -- the one delivery mark this build can honestly draw, because it has seen
        // the acceptance.
        val stamp = if (message.pending) "Sending…" else clockTime(message.at)
        if (stamp.isNotEmpty() || message.mine || message.editedAt != null) {
            withStyle(SpanStyle(fontSize = 9.5.sp, color = stampInk)) {
                if (stamp.isNotEmpty()) append("  $stamp")
                if (message.editedAt != null) append("  (edited)")
                if (message.mine && !message.pending) append(" ✓")
            }
        }
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .combinedClickable(
                onClick = { reactionBarOpen.value = !reactionBarOpen.value },
                onLongClick = { reactionBarOpen.value = true },
            )
            .padding(horizontal = 12.dp, vertical = 2.dp),
    ) {
        Box(modifier = Modifier.width(24.dp)) {
            if (head) {
                Monogram(name = name, size = 22.dp, modifier = Modifier.padding(top = 1.dp))
            }
        }
        Column(modifier = Modifier.weight(1f)) {
            if (editing.value) {
                EditLine(
                    initialText = message.text,
                    onSave = { text ->
                        editing.value = false
                        onEdit(message.messageId, text)
                    },
                    onCancel = { editing.value = false },
                )
            } else {
                Text(
                    text = line,
                    fontSize = 12.sp,
                    lineHeight = 17.sp,
                )
                message.attachment?.let { attachment ->
                    AttachmentBlock(
                        attachment = attachment,
                        mediaObject = mediaObject,
                        onResolve = onResolveMedia,
                        onSave = onSaveDocument,
                        autoFetchMedia = autoFetchMedia,
                    )
                }
                if (reactionBarOpen.value) {
                    LineActions(
                        editable = editable,
                        onReact = { emoji ->
                            reactionBarOpen.value = false
                            onReact(message.messageId, emoji)
                        },
                        onEdit = {
                            reactionBarOpen.value = false
                            editing.value = true
                        },
                        onDelete = {
                            reactionBarOpen.value = false
                            onDelete(message.messageId)
                        },
                    )
                }
            }
        }
    }
}

/**
 * The long-press bar's own rows, beneath the quick reactions: the two acts a sender has on their
 * own line. Edit appears only on a text line this device sent (the server refuses anyone else's,
 * and an attachment or an unsupported body has no text to edit); Delete appears on any own line,
 * because withdrawing is not text-shaped -- a photo can be unsent as surely as a sentence.
 */
@Composable
private fun LineActions(
    editable: Boolean,
    onReact: (String) -> Unit,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
) {
    Column(modifier = Modifier.padding(top = 2.dp)) {
        Row {
            for (emoji in QUICK_REACTIONS) {
                TextButton(onClick = { onReact(emoji) }, modifier = Modifier.size(40.dp)) {
                    Text(text = emoji, fontSize = 18.sp)
                }
            }
        }
        Row {
            if (editable) {
                TextButton(onClick = onEdit) { Text("Edit") }
            }
            TextButton(onClick = onDelete) { Text("Delete") }
        }
    }
}

/**
 * The inline editor for one of our own text lines: the line becomes a field with Save and Cancel.
 * The draft starts as the line's current text, Save commits only a change (an untouched draft is a
 * cancel in disguise), and the field gives the composer's own one-line shape, because an edit and
 * a send are the same act at different times.
 */
@Composable
private fun EditLine(
    initialText: String,
    onSave: (String) -> Unit,
    onCancel: () -> Unit,
) {
    var draft by remember { mutableStateOf(initialText) }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        OutlinedTextField(
            value = draft,
            onValueChange = { draft = it },
            modifier = Modifier.weight(1f),
            singleLine = true,
        )
        TextButton(
            onClick = { if (draft.trim().isEmpty() || draft == initialText) onCancel() else onSave(draft) },
        ) { Text("Save") }
        TextButton(onClick = onCancel) { Text("Cancel") }
    }
}

/**
 * One message's media body: the image, the document row, or the voice player, switched on the
 * discriminator decided at decode. Nothing here holds a [com.migo.core.crypto.Content] or a key --
 * the attachment is the decoded mirror, and the bytes come from the resolver's map.
 */
@Composable
private fun AttachmentBlock(
    attachment: Attachment,
    mediaObject: MediaObject?,
    onResolve: (Attachment) -> Unit,
    onSave: (Attachment) -> Unit,
    autoFetchMedia: Boolean,
) {
    when (attachment.kind) {
        AttachmentKind.Image -> ImageBubble(attachment, mediaObject, onResolve, autoFetchMedia)
        AttachmentKind.Document -> DocumentRow(attachment, mediaObject, onResolve, onSave, autoFetchMedia)
        AttachmentKind.Voice -> VoiceBubble(attachment, mediaObject, onResolve, autoFetchMedia)
    }
}

/**
 * An image, inline in the transcript. Resolved on first display (the same moment the reader asks
 * to see it), laid out at the sender's claimed dimensions when it supplied them so the line's
 * height is reserved before a single byte arrives, and capped at 220dp wide -- a photo that
 * commandeered the whole thread width would be the transcript serving the image.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ImageBubble(
    attachment: Attachment,
    mediaObject: MediaObject?,
    onResolve: (Attachment) -> Unit,
    autoFetchMedia: Boolean,
) {
    // Keyed on the choice too: a person who flips the media setting mid-conversation has the
    // bubbles already on screen start fetching — the choice is about the future, not a puzzle
    // about which attachments predate it.
    LaunchedEffect(attachment.mediaId, autoFetchMedia) {
        if (autoFetchMedia) onResolve(attachment)
    }
    val bitmap = (mediaObject as? MediaObject.Ready)?.bytes?.let {
        BitmapFactory.decodeByteArray(it, 0, it.size)
    }
    val shape = RoundedCornerShape(12.dp)
    Column(modifier = Modifier.padding(top = 4.dp)) {
        when {
            bitmap != null -> Image(
                bitmap = bitmap.asImageBitmap(),
                contentDescription = attachment.caption ?: "Photo",
                contentScale = ContentScale.Fit,
                modifier = Modifier
                    .widthIn(max = 220.dp)
                    .let { base ->
                        val w = attachment.width
                        val h = attachment.height
                        if (w != null && h != null && w > 0 && h > 0) {
                            base.aspectRatio(w.toFloat() / h.toFloat())
                        } else {
                            base
                        }
                    }
                    .clip(shape),
            )

            mediaObject is MediaObject.Failed -> Text(
                text = "Could not load the image — tap to retry",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier
                    .clip(shape)
                    .combinedClickable(onClick = { onResolve(attachment) })
                    .padding(horizontal = 8.dp, vertical = 4.dp),
            )

            // Loading, or the resolver has not been asked yet: a reserved block in the claimed
            // shape, so the transcript does not jump when the bytes land. With auto-fetch off the
            // reserved block is the offer — the shape stays reserved, and the tap the reader was
            // going to make anyway is the one that spends their data.
            else -> Surface(
                color = MaterialTheme.colorScheme.surfaceVariant,
                shape = shape,
                modifier = Modifier
                    .widthIn(max = 220.dp)
                    .let { base ->
                        val w = attachment.width
                        val h = attachment.height
                        if (w != null && h != null && w > 0 && h > 0) {
                            base.aspectRatio(w.toFloat() / h.toFloat())
                        } else {
                            base.height(120.dp)
                        }
                    },
            ) {
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .let { base ->
                            if (autoFetchMedia) base else base.combinedClickable(onClick = { onResolve(attachment) })
                        },
                    contentAlignment = Alignment.Center,
                ) {
                    if (autoFetchMedia) {
                        CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                    } else {
                        Text(
                            text = "Tap to load",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
        attachment.caption?.takeIf { it.isNotBlank() }
            ?.let { caption ->
                Text(
                    text = caption,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(start = 4.dp, top = 2.dp),
                )
            }
    }
}

/**
 * The document row: glyph, the sender's file name, the claimed size, and Save. The save writes the
 * resolved bytes, so the button waits for them and says so with a spinner rather than offering a
 * press that would have to fail.
 */
@Composable
private fun DocumentRow(
    attachment: Attachment,
    mediaObject: MediaObject?,
    onResolve: (Attachment) -> Unit,
    onSave: (Attachment) -> Unit,
    autoFetchMedia: Boolean,
) {
    LaunchedEffect(attachment.mediaId, autoFetchMedia) {
        if (autoFetchMedia) onResolve(attachment)
    }
    Surface(
        color = MaterialTheme.colorScheme.surfaceVariant,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier.padding(top = 4.dp).widthIn(max = 280.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
        ) {
            Text(text = "📄", fontSize = 20.sp)
            Spacer(modifier = Modifier.width(8.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = attachment.caption ?: "Attachment",
                    style = MaterialTheme.typography.bodyMedium,
                    color = if (mediaObject is MediaObject.Failed) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurface
                    },
                    maxLines = 1,
                )
                Text(
                    text = when (mediaObject) {
                        is MediaObject.Failed -> "Could not load — tap to retry"
                        else -> formatBytes(attachment.sizeBytes)
                    },
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(modifier = Modifier.width(8.dp))
            when {
                mediaObject is MediaObject.Ready -> TextButton(onClick = { onSave(attachment) }) {
                    Text("Save")
                }
                mediaObject is MediaObject.Failed -> TextButton(onClick = { onResolve(attachment) }) {
                    Text("Retry")
                }
                // A document nobody asked for yet is a row with a price on it: the Load button
                // states the choice the spinner would have hidden, that the bytes are the
                // reader's to spend.
                !autoFetchMedia -> TextButton(onClick = { onResolve(attachment) }) {
                    Text("Load")
                }
                else -> CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
            }
        }
    }
}

/**
 * The voice player: a play/pause glyph, the duration label, and a progress line that walks while
 * it plays. Playback is the platform's [MediaPlayer] over a temp file in this app's own cache --
 * the only handle it accepts -- deleted and the player released the moment the bubble leaves the
 * composition, so an opened note outlives its bubble by no longer than the scroll that took it away.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun VoiceBubble(
    attachment: Attachment,
    mediaObject: MediaObject?,
    onResolve: (Attachment) -> Unit,
    autoFetchMedia: Boolean,
) {
    LaunchedEffect(attachment.mediaId, autoFetchMedia) {
        if (autoFetchMedia) onResolve(attachment)
    }
    val context = LocalContext.current
    var playing by remember { mutableStateOf(false) }
    var positionMs by remember { mutableStateOf(0L) }
    var totalMs by remember { mutableStateOf(attachment.durationMs ?: 0L) }
    var failed by remember { mutableStateOf(false) }
    // The player and its file live for the bubble's composition, not the note's session: two
    // bubbles for one media id (a sender's echo and the received copy) are two players, which is
    // the cheap correct answer -- one shared player would race two play buttons.
    val player = remember { mutableStateOf<MediaPlayer?>(null) }
    val file = remember(attachment.mediaId) {
        File(context.cacheDir, "voice-${attachment.mediaId.value}.play")
    }
    DisposableEffect(attachment.mediaId) {
        onDispose {
            player.value?.release()
            player.value = null
            file.delete()
        }
    }
    // The progress read is a poll rather than a listener because the listener answers in callbacks
    // the composition would have to marshal anyway; a tenth of a second is finer than a progress
    // line can show.
    LaunchedEffect(playing) {
        while (playing) {
            player.value?.let { live ->
                if (live.isPlaying) positionMs = live.currentPosition.toLong()
            }
            delay(100)
        }
    }

    Surface(
        color = MaterialTheme.colorScheme.surfaceVariant,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier.padding(top = 4.dp).widthIn(max = 280.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .combinedClickable(onClick = {
                    val live = player.value
                    when {
                        failed || mediaObject is MediaObject.Failed -> onResolve(attachment)
                        playing -> {
                            live?.pause()
                            playing = false
                        }
                        // Not resolved yet: the first tap on a still-loading note is the reader
                        // telling the resolver to hurry, not a play command to drop on the floor.
                        mediaObject !is MediaObject.Ready -> onResolve(attachment)
                        else -> {
                            val held = live ?: try {
                                file.writeBytes(mediaObject.bytes)
                                MediaPlayer().apply {
                                    setDataSource(file.absolutePath)
                                    prepare()
                                    setOnCompletionListener {
                                        playing = false
                                        positionMs = 0
                                        seekTo(0)
                                    }
                                    totalMs = duration.toLong()
                                }.also { player.value = it }
                            } catch (_: Exception) {
                                // A note the platform's player cannot open is a note this bubble
                                // cannot play; the label says so and the press becomes a retry.
                                failed = true
                                null
                            }
                            if (held != null && !failed) {
                                held.start()
                                playing = true
                            } else {
                                playing = false
                            }
                        }
                    }
                })
                .padding(horizontal = 10.dp, vertical = 6.dp),
        ) {
            Text(
                text = when {
                    failed || mediaObject is MediaObject.Failed -> "⚠️"
                    playing -> "⏸"
                    // An unasked-for note is not a play button until it is loaded: the arrow
                    // says what the tap does, which the row's own tap-to-load already is.
                    mediaObject !is MediaObject.Ready && !autoFetchMedia -> "⬇"
                    else -> "▶"
                },
                fontSize = 16.sp,
            )
            Spacer(modifier = Modifier.width(8.dp))
            Column(modifier = Modifier.weight(1f)) {
                val total = if (totalMs > 0) totalMs else (attachment.durationMs ?: 0L)
                Text(
                    text = when {
                        failed || mediaObject is MediaObject.Failed ->
                            "Could not load the note — tap to retry"
                        else -> "${formatDuration(positionMs)} / ${formatDuration(total)}"
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = if (failed || mediaObject is MediaObject.Failed) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurface
                    },
                    maxLines = 1,
                )
                if (total > 0 && !failed && mediaObject !is MediaObject.Failed) {
                    val played = (positionMs.coerceIn(0, total)).toFloat() / total.toFloat()
                    Surface(
                        color = MaterialTheme.colorScheme.outlineVariant,
                        modifier = Modifier.fillMaxWidth().height(3.dp),
                    ) {
                        Surface(
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.fillMaxWidth(played).height(3.dp),
                        ) {}
                    }
                }
            }
        }
    }
}

/**
 * The hashed name colours for the transcript, ported from the reference as it stands: one of eight
 * mid-saturation hues per name, stable for as long as the name is.
 */
private val NAME_COLORS = listOf(
    0xFF0E7490, 0xFF7C3AED, 0xFF0E9F6E, 0xFFD97706,
    0xFFDB2777, 0xFF2563EB, 0xFFB45309, 0xFF0F766E,
)

private fun nameColor(name: String): Color {
    var h = 0
    for (c in name) h = (h * 31 + c.code)
    return Color(NAME_COLORS[((h.toLong() and 0xFFFFFFFFL) % NAME_COLORS.size).toInt()])
}

/**
 * The compose field and the send button.
 *
 * `imePadding` and `navigationBarsPadding` together, because this is the one row that has to stay above
 * the keyboard: a composer under the keyboard is a person typing into something they cannot see.
 *
 * The attach control is handed in as null for a room (attachments are end-to-end speech, and a
 * server-readable room has no key channel to deliver one through); the microphone is offered in
 * every conversation, because a voice note is speech and speech is what every conversation is for.
 * While an upload runs, the attach slot holds the spinner -- the one honest picture of a file on
 * its way -- and every control stands down until it lands.
 */
@Composable
private fun Composer(
    draft: String,
    sending: Boolean,
    uploading: Boolean,
    onDraft: (String) -> Unit,
    onSend: () -> Unit,
    onAttach: (() -> Unit)?,
    onVoiceNote: () -> Unit,
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
            if (onAttach != null) {
                if (uploading) {
                    Box(
                        modifier = Modifier.size(52.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                    }
                } else {
                    TextButton(onClick = onAttach, enabled = !sending, modifier = Modifier.size(52.dp)) {
                        Text(text = "📎", fontSize = 18.sp)
                    }
                }
            }
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
            TextButton(
                onClick = onVoiceNote,
                enabled = !sending && !uploading,
                modifier = Modifier.size(52.dp),
            ) {
                Text(text = "🎤", fontSize = 18.sp)
            }
            Spacer(modifier = Modifier.width(4.dp))
            FilledIconButton(
                onClick = onSend,
                enabled = draft.isNotBlank() && !sending && !uploading,
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

/**
 * The composer's other face: the bar a running recording replaces it with. A red dot and a timer
 * that counts what is being said, a Cancel that throws it away, and the Send that finishes it --
 * the whole vocabulary of a recording, with nothing to type over it.
 *
 * The timer ticks from this composition's own start, which is the same instant [ChatState.recording]
 * turned true; it is a display clock, not a measurement, so it cannot drift from the recorder's
 * own cap without the cap itself having already stopped the recording.
 */
@Composable
private fun RecordingBar(
    onStop: () -> Unit,
    onCancel: () -> Unit,
) {
    var elapsedMs by remember { mutableStateOf(0L) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(1000)
            elapsedMs += 1000
        }
    }
    Surface(color = MaterialTheme.colorScheme.surface) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .imePadding()
                .navigationBarsPadding()
                .padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(text = "●", color = MaterialTheme.colorScheme.error, fontSize = 14.sp)
            Spacer(modifier = Modifier.width(8.dp))
            Text(
                text = formatDuration(elapsedMs),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Text(
                text = "  recording",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(modifier = Modifier.weight(1f))
            TextButton(onClick = onCancel) {
                Text("Cancel")
            }
            Button(onClick = onStop) {
                Text("Send")
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
 * ban. A quiet italic full-width line, because it is context around the conversation rather than
 * part of it. One grey-teal that holds on both the light ground and the dark surfaces, so the
 * notice never competes with the transcript's coloured names.
 */
@Composable
private fun SystemNotice(text: String) {
    Text(
        text = text,
        fontSize = 11.sp,
        fontStyle = FontStyle.Italic,
        color = Color(0xFF7BA3AD),
        textAlign = TextAlign.Center,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 4.dp),
    )
}

/** One candidate the group sheet's invite quick-pick offers: a friend, by id and name. */
data class GroupInviteCandidate(
    val userId: Id,
    val name: String,
)

/**
 * The group's member sheet: the rename, the invite quick-pick, and the roster with the actions
 * each viewer may take.
 *
 * # Who may do what
 *
 * A group is built by two founders -- the creator and the first person they named -- and the roster
 * is the only statement of who they are, so the sheet reads it before offering anyone a control:
 *
 * - **Invite** is every member's right. The quick-pick lists the friends this shell knows who are
 *   not already seated; a person not in the graph is reached from the Friends screen's search, the
 *   same path a direct chat starts from.
 * - **Rename**, **mute**, and a straight **kick** are the founders' controls. A founder cannot
 *   touch the other founder -- a group built by two cannot be halved by one of them -- and cannot
 *   mute or kick themselves either.
 * - **Vote kick** is the members' own recourse, open to everyone, never against yourself and never
 *   against a founder. Half the group rounded up carries it, and the running tally arrives on the
 *   broadcast vote stream so every member watches the same count climb.
 */
@Composable
private fun GroupMembersSheet(
    chat: ChatState,
    selfId: Id,
    invitees: List<GroupInviteCandidate>,
    onClose: () -> Unit,
    onInvite: (Id) -> Unit,
    onVoteKick: (Id) -> Unit,
    onMute: (Id, Long?) -> Unit,
    onKick: (Id) -> Unit,
    onRename: (String) -> Unit,
    onToggleRename: () -> Unit,
    onRenameValue: (String) -> Unit,
) {
    val myRole = chat.group?.myRole ?: ConversationRole.Member
    val roster = chat.groupRoster
    // The friends who are not already seated, which is the whole point of a quick-pick: a row that
    // offers to invite someone who is already in would be a button that can only fail politely.
    val seated = roster?.filter { !it.departed }?.map { it.userId }?.toSet() ?: emptySet()
    val candidates = invitees.filter { it.userId !in seated && it.userId != selfId }
    val now = System.currentTimeMillis()

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
                    // The rename control: a founder's action, so the sheet says so when it is not
                    // this viewer's to take rather than hiding the group's title from the people
                    // who may read it.
                    TextButton(onClick = onToggleRename, enabled = myRole == ConversationRole.Founder) {
                        Text(if (chat.renameOpen) "Cancel" else "Rename")
                    }
                }
            }

            // The rename field, when a founder opened it: the current title as its starting value,
            // and a Save that cannot double-fire.
            if (chat.renameOpen) {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    OutlinedTextField(
                        value = chat.renameValue,
                        onValueChange = onRenameValue,
                        placeholder = { Text("Group title") },
                        singleLine = true,
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(
                        onClick = { onRename(chat.renameValue) },
                        enabled = !chat.renameBusy && chat.renameValue.trim().isNotEmpty(),
                    ) {
                        Text("Save")
                    }
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

                    else -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                        if (candidates.isNotEmpty()) {
                            item(key = "invite-label") { SectionLabel(text = "Invite a friend") }
                            items(candidates, key = { "invite-" + it.userId.value }) { person ->
                                GroupInviteRow(
                                    name = person.name,
                                    busy = person.userId in chat.acting,
                                    onInvite = { onInvite(person.userId) },
                                )
                            }
                        }

                        item(key = "roster-label") {
                            if (candidates.isNotEmpty()) HorizontalDivider()
                            SectionLabel(text = "In this group")
                        }
                        if (roster == null || roster.isEmpty()) {
                            item(key = "roster-empty") { Placeholder(text = "No members to show.") }
                        } else {
                            items(roster, key = { "member-" + it.userId.value }) { member ->
                                GroupMemberRow(
                                    member = member,
                                    isSelf = member.userId == selfId,
                                    myRole = myRole,
                                    tally = chat.votes[member.userId],
                                    now = now,
                                    acting = member.userId in chat.acting,
                                    onVoteKick = { onVoteKick(member.userId) },
                                    onMute = { term -> onMute(member.userId, term) },
                                    onKick = { onKick(member.userId) },
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
 * One invite candidate: the friend and the single act the row exists for. Busy is the row's own,
 * because an invite is one person's call and no other row should wait for it.
 */
@Composable
private fun GroupInviteRow(name: String, busy: Boolean, onInvite: () -> Unit) {
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
        if (busy) {
            CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
        }
        TextButton(onClick = onInvite, enabled = !busy) {
            Text("Invite")
        }
    }
}

/**
 * One group roster row: the member, their role, any running group mute, and the actions this
 * viewer may take on them -- the vote for everyone, the founder's controls for a founder over a
 * plain member. A departed member reads as "was here" without actions, because history is not
 * deletable and neither is a row that explains it.
 */
@Composable
private fun GroupMemberRow(
    member: GroupMember,
    isSelf: Boolean,
    myRole: ConversationRole,
    tally: VoteTally?,
    now: Long,
    acting: Boolean,
    onVoteKick: () -> Unit,
    onMute: (Long?) -> Unit,
    onKick: () -> Unit,
) {
    val founder = canFounderAct(myRole, member.role, isSelf)
    val canVote = canVoteKickGroup(member.role, isSelf)
    val muted = member.mutedUntil != null && member.mutedUntil > now

    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Monogram(name = member.name, size = 32.dp)
            Spacer(modifier = Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = if (isSelf) member.name + " (you)" else member.name,
                    style = MaterialTheme.typography.bodyLarge,
                    color = if (member.departed) {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    } else {
                        MaterialTheme.colorScheme.onSurface
                    },
                    maxLines = 1,
                )
                val sub = buildString {
                    append(groupRoleLabel(member.role))
                    if (member.departed) append(" · left")
                    if (muted && member.mutedUntil != null) append(" · muted")
                }
                Text(
                    text = sub,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (acting) {
                CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
            }
        }
        if (!isSelf && !member.departed) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (canVote) {
                    TextButton(onClick = onVoteKick, enabled = !acting) {
                        Text(if (tally != null) "Vote kick ${tally.votes}/${tally.needed}" else "Vote kick")
                    }
                }
                if (founder && muted) {
                    TextButton(onClick = { onMute(null) }, enabled = !acting) {
                        Text("Unmute")
                    }
                }
            }
            if (founder && !muted) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    for ((label, term) in GROUP_MUTE_TERMS) {
                        TextButton(onClick = { onMute(term) }, enabled = !acting) {
                            Text("Mute $label")
                        }
                    }
                }
            }
            if (founder) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    ConfirmTextButton(label = "Kick", enabled = !acting, onConfirm = onKick)
                }
                // The tariff's price, stated where the spend is agreed: a kick spends a Kick Point
                // when one is held and a coin when not, and a short wallet refuses rather than
                // half-charging. The vote is every member's free recourse.
                Text(
                    text = "A kick spends 1 KP, else 1 \$MIG. Vote kick is free.",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
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
