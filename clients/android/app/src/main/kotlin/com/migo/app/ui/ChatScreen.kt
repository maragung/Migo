package com.migo.app.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.graphics.BitmapFactory
import android.media.MediaPlayer
import android.os.SystemClock
import android.text.format.DateUtils
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.LocalContentColor
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
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
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
import androidx.compose.ui.window.Dialog
import com.migo.app.media.formatBytes
import com.migo.app.media.formatDuration
import com.migo.app.model.AttachSource
import com.migo.app.model.Attachment
import com.migo.app.model.AttachmentKind
import com.migo.app.model.ChatMessage
import com.migo.app.model.ChatSafety
import com.migo.app.model.ChatState
import com.migo.app.model.FREE_EMOTICONS
import com.migo.app.model.GAME_KIND_GUESS_NUMBER
import com.migo.app.model.GAME_STATUS_OPEN
import com.migo.app.model.GROUP_MUTE_TERMS
import com.migo.app.model.GroupMember
import com.migo.app.model.MediaObject
import com.migo.app.model.MemberProfileView
import com.migo.app.model.QUICK_REACTIONS
import com.migo.app.model.RoomNotice
import com.migo.app.model.RosterMember
import com.migo.app.model.VoiceNotePreview
import com.migo.app.model.VoteTally
import com.migo.app.model.gameLabelOf
import com.migo.app.model.groupRoleLabel
import com.migo.app.model.guessFeedbackLine
import com.migo.app.model.ownedEmoticons
import com.migo.app.model.ownedStickerPacks
import com.migo.app.model.parseGuessBoard
import com.migo.app.model.playerRangeLabel
import com.migo.core.domain.canFounderAct
import com.migo.core.domain.canVoteKickGroup
import com.migo.core.domain.filterChatSearch
import com.migo.core.protocol.ConversationKind
import com.migo.core.protocol.ConversationRole
import com.migo.core.protocol.GameCatalogueEntry
import com.migo.core.protocol.GameViewWire
import com.migo.core.protocol.GiftListing
import com.migo.core.protocol.PresenceState
import com.migo.core.protocol.RelationshipKind
import com.migo.core.protocol.RoomRole
import com.migo.core.protocol.SanctionAction
import com.migo.core.store.VoiceNoteSpeed
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
     * Places a call to the direct chat's peer, handed the peer's id and whether the call asked
     * for video. Offered only for a direct chat -- the call is the direct conversation's other
     * half, and a room's audience has no 1:1 to call. Null when the shell cannot place calls.
     */
    onStartCall: ((Id, Boolean) -> Unit)? = null,
    /**
     * Joins the group call of this conversation: the roster overlay comes up and the seat is
     * requested with a sealed placeholder offer. Offered only for a group -- the web client's own
     * gate, its single button's -- and null when the shell cannot join group calls.
     */
    onJoinGroupCall: (() -> Unit)? = null,
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
     * Picks what the next message carries, in one of the four ways the attach menu names — any
     * file, a photo from the camera, a video, an image — whose answer is sent into the
     * conversation. Null in a server-readable room: attachments are an end-to-end feature (a room
     * has no key channel to hand the recipients a sealed object's key), mirroring the web
     * composer's own gating.
     */
    onPickAttachment: ((AttachSource) -> Unit)? = null,
    /**
     * Starts a voice note: asks the microphone permission at the moment of use and begins the
     * recording. Offered in every conversation kind, because a voice note is speech and speech is
     * what every conversation is for.
     */
    onVoiceNote: () -> Unit = {},
    /** Pauses the running recording — the speaker's own Pause, lifted by [onResumeVoiceNote]. */
    onPauseVoiceNote: () -> Unit = {},
    /** Resumes a recording paused by its speaker. */
    onResumeVoiceNote: () -> Unit = {},
    /**
     * Stops the recording into its preview — the two-step mode's second tap. The note waits on
     * the preview row's Send and Delete rather than leaving immediately.
     */
    onStopVoiceNote: () -> Unit = {},
    /** Sends the held note: the preview's Send, and the hold mode's release-to-send. */
    onSendVoiceNote: () -> Unit = {},
    /** Deletes the previewed note — through the undo window, like every discard of a recording. */
    onDeleteVoiceNote: () -> Unit = {},
    /** Restores the note the undo window is holding, back into its preview. */
    onUndoVoiceNoteDiscard: () -> Unit = {},
    /** Cancels the recording — the hold mode's slide-to-cancel, and the recording bar's Cancel. */
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
    /**
     * The session's avatars, by account id — what a run head and the sheets read their pictures
     * from. Absent keys render as the monogram, which is the avatar's loading state.
     */
    avatarBytes: Map<Id, ByteArray> = emptyMap(),
    /**
     * The voice-note messages this account has marked heard, by message id — the receiver-local
     * state brief section 179 names. A bubble reads it to dim what has been heard, and nothing
     * about it ever rides the wire: the sender is not told, because the wire has no status to
     * tell them with.
     */
    listenedVoiceNotes: Set<Id> = emptySet(),
    /** Marks one voice-note message heard or unheard on this device; never sends anything. */
    onSetVoiceNoteListened: (Id, Boolean) -> Unit = { _, _ -> },
    /**
     * The rate the voice-note player plays at — a local preference, so the note already on this
     * device is retuned rather than refetched when it changes (brief section 167).
     */
    voiceNoteSpeed: VoiceNoteSpeed = VoiceNoteSpeed.Speed1x,
    /** Sets the playback rate, from the player's own speed control. */
    onVoiceNoteSpeed: (VoiceNoteSpeed) -> Unit = {},
    /**
     * Opens the member profile sheet for the given account, handed the id and the name the roster
     * row already knew, from a member menu's View profile.
     */
    onViewMember: (Id, String) -> Unit = { _, _ -> },
    /** The member profile sheet's state; held by the shell because the profile read is the model's. */
    memberProfile: MemberProfileView? = null,
    /** Closes the member profile sheet. */
    onCloseMemberProfile: () -> Unit = {},
    /**
     * The gift shop's catalogue, the same list the Wallet panel draws; a member menu's Gift opens
     * the picker over it, pre-aimed at the member the menu was opened for.
     */
    giftCatalogue: List<GiftListing> = emptyList(),
    /**
     * Sends a gift from the member menu's picker, handed the sku, the member it is aimed at, and
     * the intent's idempotency key minted with the pick — a retry of the same pick is the first
     * send again server-side, not a second charge.
     */
    onSendGift: (sku: String, recipient: Id, clientKey: String) -> Unit = { _, _, _ -> },
    /**
     * Reads the conversation's roster for the header gift's recipient list. The header's Gift aims
     * at a person, and in a room or a group the person is chosen from the roster — which the member
     * sheet reads on its own open, so the first gift from a cold thread asks for the same read.
     */
    onLoadGiftRecipients: () -> Unit = {},
    /**
     * The account's owned pack SKUs, as the composer's emoticon/sticker picker reads them; null
     * while the read is in flight, which the picker renders as its own wait.
     */
    ownedPacks: Set<String>? = null,
    /** Reads the owned pack SKUs, on the picker's first open and on a retry. */
    onLoadOwnedPacks: () -> Unit = {},
    /**
     * Sends a friend request to the account the member profile sheet is about — the profile card's
     * social line's own act, kept separate from the Friends screen's because the sheet's line
     * re-reads only its edge.
     */
    onMemberFriendRequest: () -> Unit = {},
    /**
     * Answers a pending incoming friend request from the account the member profile sheet is
     * about, either way.
     */
    onMemberFriendRespond: (Boolean) -> Unit = { },
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

    // The member the gift picker is aimed at, opened from a member menu's Gift or the header's.
    // Local because the picker's life is the pick's life: it closes on the send, and the member it
    // names is the one the menu was opened for, pre-aimed the way the web client's own picker is.
    var giftTarget by remember { mutableStateOf<GiftTarget?>(null) }

    // The header gift's recipient selection, open when a room or a group gift has more than one
    // person it could go to (a direct chat aims at its peer without asking). False once a choice
    // is made or the sheet is dismissed; the roster it lists is the chat's own.
    var giftPicking by remember { mutableStateOf(false) }

    // The composer's two pickers: the emoticon/sticker sheet and the attach menu. Both are the
    // row's own controls, so both flags live here, beside the gift state they share the bottom of
    // the thread with.
    var emoticonOpen by remember { mutableStateOf(false) }
    var attachOpen by remember { mutableStateOf(false) }

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
                avatarBytes = avatarBytes,
                onLeave = onLeave,
                onOpenMembers = onOpenMembers,
                onOpenGames = { gamesOpen.value = true },
                onOpenGift = {
                    // A direct chat has exactly one person a gift could go to, so the header aims
                    // at the peer without asking; a room or a group offers the roster to choose
                    // from, reading it first if the member sheet never has.
                    val peer = chat.peerId
                    if (peer != null) {
                        giftTarget = GiftTarget(peer, chat.title)
                    } else {
                        onLoadGiftRecipients()
                        giftPicking = true
                    }
                },
                onOpenSafety = if (chat.peerId != null) {
                    { safetyOpen.value = true }
                } else {
                    null
                },
                onStartCall = onStartCall,
                onJoinGroupCall = onJoinGroupCall,
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
                                    avatarBytes = item.message.senderId?.let { avatarBytes[it] },
                                    onReact = onReact,
                                    onEdit = onEdit,
                                    onDelete = onDelete,
                                    onResolveMedia = onResolveMedia,
                                    onSaveDocument = onSaveDocument,
                                    autoFetchMedia = autoFetchMedia,
                                    mediaObject = mediaObjects[item.message.attachment?.mediaId],
                                    voiceListened = item.message.messageId in listenedVoiceNotes,
                                    onSetVoiceNoteListened = onSetVoiceNoteListened,
                                    voiceNoteSpeed = voiceNoteSpeed,
                                    onVoiceNoteSpeed = onVoiceNoteSpeed,
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

            // The composer's four faces, one per state a voice note can leave it in. While a
            // recording runs *unheld* — the two-step mode, or a hold that slid up into its lock —
            // the composer is the recording bar, with the whole vocabulary of a recording. While
            // a recording runs *held* (the finger still pressing the mic), the composer keeps its
            // own shape — the gesture owns the mic button, and the row between the attach button
            // and it becomes the timer and the slide hints. A finished note waiting on the word
            // is the preview row, and a note just cancelled is the undo window's chip.
            var micHeld by remember { mutableStateOf(false) }
            if (chat.recording && !micHeld) {
                RecordingBar(
                    paused = chat.recordingPaused,
                    elapsedMs = chat.recordingElapsedMs,
                    amplitudes = chat.recordingAmplitudes,
                    onPause = onPauseVoiceNote,
                    onResume = onResumeVoiceNote,
                    onStop = onStopVoiceNote,
                    onCancel = onCancelVoiceNote,
                )
            } else if (chat.notePreview != null) {
                PreviewBar(
                    preview = chat.notePreview,
                    onSend = onSendVoiceNote,
                    onDelete = onDeleteVoiceNote,
                    uploading = chat.uploading,
                )
            } else {
                if (chat.noteDiscardUndo) {
                    DiscardUndoBar(onUndo = onUndoVoiceNoteDiscard)
                }
                Composer(
                    draft = chat.draft,
                    sending = chat.sending,
                    uploading = chat.uploading,
                    onDraft = onDraft,
                    onSend = onSend,
                    onToggleEmoticons = {
                        if (!emoticonOpen && ownedPacks == null) onLoadOwnedPacks()
                        emoticonOpen = !emoticonOpen
                    },
                    emoticonsOpen = emoticonOpen,
                    onOpenAttach = { attachOpen = true },
                    onPickAttachment = onPickAttachment,
                    onVoiceNote = onVoiceNote,
                    recordingHeld = chat.recording,
                    heldElapsedMs = chat.recordingElapsedMs,
                    heldAmplitudes = chat.recordingAmplitudes,
                    heldPaused = chat.recordingPaused,
                    onMicHeld = { held -> micHeld = held },
                    onMicReleaseSend = { micHeld = false; onSendVoiceNote() },
                    onMicLock = { micHeld = false },
                    onMicCancel = { micHeld = false; onCancelVoiceNote() },
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
                avatarBytes = avatarBytes,
                onClose = onCloseMembers,
                onViewProfile = onViewMember,
                onGift = { userId, name -> giftTarget = GiftTarget(userId, name) },
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
                avatarBytes = avatarBytes,
                onClose = onCloseGroupMembers,
                onViewProfile = onViewMember,
                onGift = { userId, name -> giftTarget = GiftTarget(userId, name) },
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

        // The gift picker a member menu's Gift or the header's opens, aimed at the member it was
        // opened for. The price rides on every row before the send — the same rule the Wallet
        // panel's shop keeps, because a gift is a spend and a spend is agreed on its price, not
        // surprised by it.
        giftTarget?.let { target ->
            GiftSheet(
                target = target,
                catalogue = giftCatalogue,
                onDismiss = { giftTarget = null },
                onSend = { sku ->
                    giftTarget = null
                    onSendGift(sku, target.userId, java.util.UUID.randomUUID().toString())
                },
            )
        }

        // The header gift's recipient selection for a room or a group: the roster the chat holds,
        // read on open when the member sheet never has. A direct chat never gets here — its gift
        // is aimed at the peer the moment the header is pressed.
        if (giftPicking) {
            GiftRecipientSheet(
                chat = chat,
                selfId = selfId,
                avatarBytes = avatarBytes,
                onDismiss = { giftPicking = false },
                onPick = { userId, name ->
                    giftPicking = false
                    giftTarget = GiftTarget(userId, name)
                },
            )
        }

        // The attach menu the composer's file control opens: four ways to pick what the next
        // message carries, each naming its promise honestly before the system picker opens. The
        // disappearing toggle the web client folds in here has no Android counterpart to fold —
        // this client's composer never carried one — so the menu keeps exactly its four picks.
        if (attachOpen && onPickAttachment != null) {
            val pick = onPickAttachment
            AttachSheet(
                onDismiss = { attachOpen = false },
                onPick = { source ->
                    attachOpen = false
                    pick(source)
                },
            )
        }

        // The emoticon/sticker picker the composer's smile control opens: two tabs, one free
        // baseline, every pack owned. Everything in either tab inserts as text — the glyphs are
        // Unicode and the conversation is E2EE, a sticker riding out as ordinary message text the
        // way an emoticon does.
        if (emoticonOpen) {
            EmoticonSheet(
                owned = ownedPacks,
                onDismiss = { emoticonOpen = false },
                onInsert = { glyph -> onDraft(chat.draft + glyph) },
            )
        }

        // The member profile sheet a member menu's View profile opens. The mute-for-me control
        // rides here rather than in the menu, mirroring the web client's own profile card: it is
        // a personal choice about the person, and the person's card is where the web client
        // puts it. Offered only for a room chat, the one conversation the mute is a fact of.
        memberProfile?.let { view ->
            MemberProfileSheet(
                view = view,
                avatarBytes = avatarBytes[view.userId],
                selfId = selfId,
                canMuteForMe = chat.roomId != null,
                muted = view.userId in chat.muted,
                onClose = onCloseMemberProfile,
                onMuteForMe = { on -> onMuteForMe(view.userId, on) },
                onFriendRequest = onMemberFriendRequest,
                onFriendRespond = onMemberFriendRespond,
            )
        }
    }
}

@Composable
private fun ChatHeader(
    chat: ChatState,
    avatarBytes: Map<Id, ByteArray>,
    onLeave: (() -> Unit)?,
    onOpenMembers: (() -> Unit)?,
    onOpenGames: () -> Unit,
    onOpenGift: () -> Unit,
    onOpenSafety: (() -> Unit)? = null,
    onStartCall: ((Id, Boolean) -> Unit)? = null,
    onJoinGroupCall: (() -> Unit)? = null,
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
            // reference's windows having no title bars. No title either, and no second line: the
            // strip's tab already names the thread, and the counts and the encryption label the
            // header once carried were facts a person could not act on from where they read them.
            // What stays is the avatar — a direct chat's picture of the peer, a room's own mark —
            // and the controls, every one the composer's send button's own measure.
            Avatar(name = chat.title, bytes = chat.peerId?.let { avatarBytes[it] }, size = 36.dp)
            Spacer(modifier = Modifier.weight(1f))
            // Gifting is a thread-level act — it picks a person and spends balance — so its control
            // lives with the thread's other actions in the header, immediately left of games, not
            // in the row the composer keeps exclusively for chat. Offered in every conversation
            // kind, exactly as the web client's own header button is.
            HeaderGlyphButton(glyph = "🎁", description = "Send a gift", onClick = onOpenGift)
            if (supportsGames) {
                HeaderGlyphButton(glyph = "🎮", description = "Games", onClick = onOpenGames)
            }
            // A direct chat's one header extra: the door to its safety numbers. It is the room
            // chat's Members control in reverse — the room's security surface is who is in it, the
            // direct chat's is who the other side turned out to be. Gated on the peer id rather
            // than the room's absence, because the safety read itself needs that id: a chat with
            // no peer to read offers no door.
            if (chat.peerId != null && onOpenSafety != null) {
                HeaderGlyphButton(glyph = "🛡", description = "Safety numbers", onClick = onOpenSafety)
            }
            // The direct chat's other extras: the two calls, the conversation's other halves.
            // Same peer-id gate as Safety -- the call buttons dial the peer, and a chat with no
            // peer has no number to dial. Voice and video are two buttons, as on the web client,
            // because the two calls are two intents -- a person who means to talk is not asked to
            // confirm a camera they never wanted. The glyphs are emoji characters, not an icon
            // font, the app's own rule (and the web client's, whose buttons these are a port of).
            if (chat.peerId != null && onStartCall != null) {
                val peer = chat.peerId
                HeaderGlyphButton(
                    glyph = "📞",
                    description = "Voice call",
                    onClick = { onStartCall(peer, false) },
                )
                HeaderGlyphButton(
                    glyph = "🎥",
                    description = "Video call",
                    onClick = { onStartCall(peer, true) },
                )
            }
            // The thread's own search, before the Log control as on the web. Offered in every
            // conversation kind — the filter runs on what the thread already holds, so there is no
            // conversation feature for it to depend on. The description is the toggle's own
            // sentence: tapping it again closes the field, and the toggle clears the query either
            // way.
            HeaderGlyphButton(
                glyph = "🔍",
                description = if (chat.searchOpen) "Close search" else "Search",
                onClick = onToggleSearch,
            )
            // The conversation's own record: the transcript this device holds, handed to whatever
            // the system shares text with. Offered in every conversation kind — a log is a log
            // whether the room is encrypted or not — and stated as plaintext by the share sheet
            // it opens into.
            if (onExportLog != null) {
                HeaderGlyphButton(glyph = "⬇", description = "Log", onClick = onExportLog)
            }
            if (chat.roomId != null && onOpenMembers != null) {
                HeaderGlyphButton(glyph = "👥", description = "Members", onClick = onOpenMembers)
            }
            // The group's call door: one button, joining the roster, exactly the web client's own
            // single control. The glyph is the direct chat's own phone -- the web button's glyph --
            // and the kind gate keeps the two controls from ever sharing a header, because the
            // direct chat dials a person and the group joins a conversation.
            if (chat.kind == ConversationKind.Group && onJoinGroupCall != null) {
                HeaderGlyphButton(
                    glyph = "📞",
                    description = "Join group call",
                    onClick = onJoinGroupCall,
                )
            }
            // The group's member-sheet door: the same glyph the room's control uses, because the
            // question it answers -- who is in here -- is the same question. Gated on the kind
            // rather than the roster's presence, so a group the sheet has not read yet still
            // offers the door that reads it.
            if (chat.kind == ConversationKind.Group && onOpenGroupMembers != null) {
                HeaderGlyphButton(
                    glyph = "👥",
                    description = "Members",
                    onClick = onOpenGroupMembers,
                )
            }
            // Leave stays a word rather than a glyph, and the danger red: it is the one control in
            // the row that ends the conversation rather than using it, and a word that says so is
            // cheaper to read than a glyph that would have to be guessed.
            if (chat.roomId != null && onLeave != null) {
                TextButton(onClick = onLeave) {
                    Text("Leave", color = MaterialTheme.colorScheme.error)
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
 * One header control, drawn to the composer's send button's own measure.
 *
 * The send button is a fixed 52dp square, so every control beside it is too: a row of controls
 * that each took the width of their own label would be a row that breathes as the labels
 * translate, and a header that changes shape between conversations is a header a person has to
 * re-find. The glyphs are emoji characters — the app's own icon convention, the same rule the
 * composer's attach and mic buttons keep — and the description carries the word the label once
 * did, because a control that only a picture names is a control a screen reader cannot name.
 */
@Composable
private fun HeaderGlyphButton(
    glyph: String,
    description: String,
    onClick: () -> Unit,
) {
    TextButton(
        onClick = onClick,
        modifier = Modifier
            .size(52.dp)
            .semantics { contentDescription = description },
    ) {
        Text(text = glyph, fontSize = 18.sp)
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
    avatarBytes: ByteArray?,
    onReact: (Id, String) -> Unit,
    onEdit: (Id, String) -> Unit,
    onDelete: (Id) -> Unit,
    onResolveMedia: (Attachment) -> Unit,
    onSaveDocument: (Attachment) -> Unit,
    autoFetchMedia: Boolean,
    mediaObject: MediaObject?,
    /** Whether this message's voice note is marked heard on this device; receiver-local only. */
    voiceListened: Boolean,
    /** Marks this message's voice note heard or unheard on this device; never sends anything. */
    onSetVoiceNoteListened: (Id, Boolean) -> Unit,
    /** The rate the voice player plays at, if this line carries a voice note. */
    voiceNoteSpeed: VoiceNoteSpeed,
    /** Sets the playback rate, from the player's own speed control. */
    onVoiceNoteSpeed: (VoiceNoteSpeed) -> Unit,
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
    // A voice note somebody else sent, the one line the heard-mark toggle may be offered on.
    // Listened is the *receiver's* state (brief section 179), so the toggle is not offered on
    // this device's own notes: a sender has no mark to keep about hearing their own words.
    val receivedVoiceNote = message.attachment?.kind == AttachmentKind.Voice && !message.mine
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
                Avatar(name = name, bytes = avatarBytes, size = 22.dp, modifier = Modifier.padding(top = 1.dp))
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
                        onMarkListened = { heard -> onSetVoiceNoteListened(message.messageId, heard) },
                        listened = voiceListened,
                        speed = voiceNoteSpeed,
                        onSpeedChange = onVoiceNoteSpeed,
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
                        onToggleVoiceListened = if (receivedVoiceNote) {
                            {
                                reactionBarOpen.value = false
                                onSetVoiceNoteListened(message.messageId, !voiceListened)
                            }
                        } else {
                            null
                        },
                        voiceListened = voiceListened,
                    )
                }
            }
        }
    }
}

/**
 * The long-press bar's own rows, beneath the quick reactions: the acts a sender has on their own
 * line, and the one act a receiver has on somebody else's voice note. Edit appears only on a text
 * line this device sent (the server refuses anyone else's, and an attachment or an unsupported
 * body has no text to edit); Delete appears on any own line, because withdrawing is not
 * text-shaped -- a photo can be unsent as surely as a sentence.
 *
 * The heard toggle appears only on a received voice note, because listened is the receiver's own
 * state (brief section 179): it marks this device's dim, it is never sent, and unmarking cancels
 * no receipt that already left -- which is why the button is a plain word rather than anything
 * that reads like a delivery status.
 */
@Composable
private fun LineActions(
    editable: Boolean,
    onReact: (String) -> Unit,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
    onToggleVoiceListened: (() -> Unit)? = null,
    voiceListened: Boolean = false,
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
            if (onToggleVoiceListened != null) {
                TextButton(onClick = onToggleVoiceListened) {
                    Text(if (voiceListened) "Mark as unlistened" else "Mark as listened")
                }
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
    onMarkListened: (Boolean) -> Unit = {},
    listened: Boolean = false,
    speed: VoiceNoteSpeed = VoiceNoteSpeed.Speed1x,
    onSpeedChange: (VoiceNoteSpeed) -> Unit = {},
) {
    when (attachment.kind) {
        AttachmentKind.Image -> ImageBubble(attachment, mediaObject, onResolve, autoFetchMedia)
        AttachmentKind.Document -> DocumentRow(attachment, mediaObject, onResolve, onSave, autoFetchMedia)
        AttachmentKind.Voice -> VoiceBubble(
            attachment = attachment,
            mediaObject = mediaObject,
            onResolve = onResolve,
            autoFetchMedia = autoFetchMedia,
            onListenedChange = onMarkListened,
            listened = listened,
            speed = speed,
            onSpeedChange = onSpeedChange,
        )
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
    // The lightbox: a tap on the rendered image opens it fullscreen, the same second look the web
    // client's overlay gives. A dialog rather than a navigation so back closes only the picture.
    var lightboxOpen by remember { mutableStateOf(false) }
    if (lightboxOpen && bitmap != null) {
        Dialog(onDismissRequest = { lightboxOpen = false }) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(Color.Black.copy(alpha = 0.9f))
                    .combinedClickable(onClick = { lightboxOpen = false }),
                contentAlignment = Alignment.Center,
            ) {
                Image(
                    bitmap = bitmap.asImageBitmap(),
                    contentDescription = attachment.caption ?: "Photo",
                    contentScale = ContentScale.Fit,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        }
    }
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
                    .clip(shape)
                    .combinedClickable(onClick = { lightboxOpen = true }),
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
 * The voice player: a play/pause glyph, the duration label, a progress line that walks while it
 * plays, and the speed control beside both. Playback is the platform's [MediaPlayer] over a temp
 * file in this app's own cache -- the only handle it accepts -- deleted and the player released
 * the moment the bubble leaves the composition, so an opened note outlives its bubble by no longer
 * than the scroll that took it away.
 *
 * # The speed control
 *
 * The three rates of brief section 167 are applied to the player this bubble already holds: no
 * rate asks for the media again, and the position the note is at is the position it stays at,
 * because a playback-params change retunes the live player rather than restarting it. The rate is
 * a preference rather than bubble state, so the choice made on one note is the rate the next one
 * starts at.
 *
 * # The heard mark
 *
 * Listened is this device's own fact about the note (brief section 179): a note played to near
 * its end marks itself heard, the row's long-press bar can mark it either way, and a heard note
 * draws dimmed so the unheard ones stand out. Nothing about the mark is sent -- the wire has no
 * Played status, and a sender shown one would be shown a status that never existed.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun VoiceBubble(
    attachment: Attachment,
    mediaObject: MediaObject?,
    onResolve: (Attachment) -> Unit,
    autoFetchMedia: Boolean,
    onListenedChange: (Boolean) -> Unit,
    listened: Boolean,
    speed: VoiceNoteSpeed,
    onSpeedChange: (VoiceNoteSpeed) -> Unit,
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
    // The mark's own state, read through the indirection because the poll loop below outlives the
    // recompositions that change it: a note marked unlistened by hand while it plays must not be
    // re-marked by a threshold check still holding the older value.
    val heardNow by rememberUpdatedState(listened)
    val markHeardNow by rememberUpdatedState(onListenedChange)
    // The progress read is a poll rather than a listener because the listener answers in callbacks
    // the composition would have to marshal anyway; a tenth of a second is finer than a progress
    // line can show. The same tick carries the automatic mark: a note heard to nine tenths of its
    // length is heard -- the last tenth is usually the tail of the last word, and a mark that
    // waited for the completion event would never fire on a note the reader paused at the end of.
    LaunchedEffect(playing) {
        while (playing) {
            player.value?.let { live ->
                if (live.isPlaying) {
                    positionMs = live.currentPosition.toLong()
                    val total = totalMs
                    if (!heardNow && total > 0 && positionMs * 10 >= total * 9) markHeardNow(true)
                }
            }
            delay(100)
        }
    }
    // The rate reaches the player whenever the preference names a new one, so a change made on
    // one bubble retunes a note already playing on another, and a player created before the
    // preference settled picks the rate up the moment it exists.
    LaunchedEffect(attachment.mediaId, speed) {
        player.value?.let { live -> applyPlaybackSpeed(live, speed) }
    }

    // The dim is the heard mark made visible: everything that names the note -- its glyph, its
    // clock -- goes faint once it has been heard, so the notes still unheard keep the full ink
    // and stand out in a run of them. A note being replayed drops the dim while it plays, because
    // the reader is looking at *this* one now.
    val heardInk = LocalMigoExtra.current.faint
    val heard = listened && !playing
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
                                        // The note ran to its end, whatever it was marked: heard
                                        // in full is the one mark completion can set by itself.
                                        onListenedChange(true)
                                        seekTo(0)
                                    }
                                    // The persisted rate is the note's rate from its first
                                    // moment, so a 2x listener hears 2x without a second press.
                                    playbackParams = playbackParams.setSpeed(speed.rate)
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
                color = if (heard) heardInk else LocalContentColor.current,
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
                    color = when {
                        failed || mediaObject is MediaObject.Failed -> MaterialTheme.colorScheme.error
                        heard -> heardInk
                        else -> MaterialTheme.colorScheme.onSurface
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
                            color = if (heard) {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            } else {
                                MaterialTheme.colorScheme.primary
                            },
                            modifier = Modifier.fillMaxWidth(played).height(3.dp),
                        ) {}
                    }
                }
            }
            // The speed control: one chip that steps through the three rates, naming the one it
            // holds. Its press reaches the player through the preference, not the click handler,
            // so the rate change and the mark of it are the same fact in one place.
            Spacer(modifier = Modifier.width(4.dp))
            Text(
                text = speed.label,
                style = MaterialTheme.typography.labelSmall,
                color = if (heard) heardInk else MaterialTheme.colorScheme.primary,
                modifier = Modifier
                    .clip(RoundedCornerShape(8.dp))
                    .clickable { onSpeedChange(speed.next()) }
                    .padding(horizontal = 6.dp, vertical = 4.dp)
                    .semantics { contentDescription = "Playback speed ${speed.label}" },
            )
        }
    }
}

/**
 * Retunes a live player to the given rate without moving the note: the position the player is at
 * is the position it keeps, which is the mid-playback rule the speed control owes. The
 * was-playing guard is for the platform's own quirk -- on some versions a paused player treats a
 * playback-params change as a resume -- so a rate chosen while paused leaves the note paused.
 */
private fun applyPlaybackSpeed(player: MediaPlayer, speed: VoiceNoteSpeed) {
    val wasPlaying = player.isPlaying
    player.playbackParams = player.playbackParams.setSpeed(speed.rate)
    if (!wasPlaying && player.isPlaying) player.pause()
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
 * The hold mode's release window: a press shorter than this is the two-step mode's first tap (the
 * recording continues, the bar with its Stop takes over), while anything longer is a hold whose
 * release sends. Two modes on one button, told apart by the only thing that distinguishes them —
 * how long the finger stayed.
 */
private const val MIC_QUICK_TAP_MS = 400L

/** How far left a held mic must be dragged before the release becomes a cancel. */
private val MIC_CANCEL_SLIDE = 96.dp

/** How far up a held mic must be dragged to lock the recording. */
private val MIC_LOCK_SLIDE = 72.dp

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
 *
 * The microphone carries the hold mode's whole vocabulary in one press: the field's place is taken
 * by the recording it started, and the release decides between send, cancel, and the two-step
 * mode's handoff to the recording bar.
 */
@Composable
private fun Composer(
    draft: String,
    sending: Boolean,
    uploading: Boolean,
    onDraft: (String) -> Unit,
    onSend: () -> Unit,
    onToggleEmoticons: () -> Unit,
    emoticonsOpen: Boolean,
    onOpenAttach: () -> Unit,
    onPickAttachment: ((AttachSource) -> Unit)? = null,
    onVoiceNote: () -> Unit,
    recordingHeld: Boolean,
    heldElapsedMs: Long,
    heldAmplitudes: List<Int>,
    heldPaused: Boolean,
    onMicHeld: (Boolean) -> Unit,
    onMicReleaseSend: () -> Unit,
    onMicLock: () -> Unit,
    onMicCancel: () -> Unit,
) {
    var cancelSlide by remember { mutableStateOf(false) }
    Surface(color = MaterialTheme.colorScheme.surface) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .imePadding()
                .navigationBarsPadding()
                .padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.Bottom,
        ) {
            // The row is chat and only chat: the two picker controls (emoticons, attachments) sit
            // left of the input, the mic and the send right of it. Everything else a thread offers
            // -- gifting, games -- lives in the header, where actions about the conversation
            // belong rather than acts of writing in it.
            //
            // The emoticon control opens the picker's two tabs (emoticons, then stickers) and is
            // offered in every conversation kind, exactly as on the web client: a glyph is speech,
            // and speech is what every conversation is for.
            TextButton(
                onClick = onToggleEmoticons,
                enabled = !sending && !uploading,
                modifier = Modifier
                    .size(52.dp)
                    .semantics {
                        contentDescription = if (emoticonsOpen) {
                            "Close emoticon picker"
                        } else {
                            "Open emoticon picker"
                        }
                    },
            ) {
                Text(text = if (emoticonsOpen) "😀" else "😊", fontSize = 18.sp)
            }
            // The attach control stands for four ways to pick what the next message carries, each
            // named in the menu it opens; the picking itself is the shell's, because the system
            // picker is an activity result only the shell can launch. While an upload runs, the
            // control becomes the wait it caused — the row's one honest spinner.
            if (onPickAttachment != null) {
                if (uploading) {
                    Box(
                        modifier = Modifier.size(52.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                    }
                } else {
                    TextButton(
                        onClick = onOpenAttach,
                        enabled = !sending,
                        modifier = Modifier
                            .size(52.dp)
                            .semantics { contentDescription = "Attach a file, photo, video, or image" },
                    ) {
                        Text(text = "📎", fontSize = 18.sp)
                    }
                }
            }
            // While the mic is held, the field's place is the recording it started: the clock, the
            // live waveform, and the two ways the hold can end. The mic button itself stays — the
            // gesture owns it, and removing it mid-press would end the very hold it shows.
            if (recordingHeld) {
                Row(
                    modifier = Modifier
                        .weight(1f)
                        .height(52.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = if (heldPaused) "⏸" else "●",
                        color = MaterialTheme.colorScheme.error,
                        fontSize = 14.sp,
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Text(
                        text = formatDuration(heldElapsedMs),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurface,
                    )
                    Spacer(modifier = Modifier.width(10.dp))
                    WaveformBars(
                        amplitudes = heldAmplitudes,
                        modifier = Modifier
                            .weight(1f)
                            .height(24.dp),
                    )
                    Spacer(modifier = Modifier.width(10.dp))
                    Text(
                        text = if (cancelSlide) {
                            "release to cancel"
                        } else {
                            "release to send · slide ⇧ to lock"
                        },
                        style = MaterialTheme.typography.labelSmall,
                        color = if (cancelSlide) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                }
            } else {
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
            }
            // The mic is the whole hold-mode vocabulary under one button: press to talk, release to
            // send, slide left to cancel, slide up to lock — and a quick tap is the two-step mode's
            // start, the recording continuing into the bar that owns it from there. The gesture
            // runs in the Initial pass and consumes every change, so the button's own clickable
            // never sees an unconsumed press and cannot fire a second start; the onClick remains
            // for an accessibility click, which arrives as performClick and no pointer events.
            TextButton(
                onClick = onVoiceNote,
                enabled = !sending && !uploading,
                modifier = Modifier
                    .size(52.dp)
                    .pointerInput(sending, uploading) {
                        awaitEachGesture {
                            val down = awaitFirstDown(pass = PointerEventPass.Initial)
                            if (sending || uploading) {
                                return@awaitEachGesture
                            }
                            down.consume()
                            onVoiceNote()
                            onMicHeld(true)
                            cancelSlide = false
                            // The hold's length decides the release's meaning, so the wall clock
                            // starts the moment the press does. The event's own timestamps are a
                            // newer API than this compose version carries; the platform's uptime
                            // clock is the same measure on every version the app runs on.
                            val downAt = SystemClock.uptimeMillis()
                            val cancelPx = MIC_CANCEL_SLIDE.toPx()
                            val lockPx = MIC_LOCK_SLIDE.toPx()
                            while (true) {
                                val event = awaitPointerEvent(PointerEventPass.Initial)
                                event.changes.forEach { it.consume() }
                                val pressed = event.changes.any { it.pressed }
                                val change = event.changes.firstOrNull { it.id == down.id }
                                if (change != null) {
                                    val dx = change.position.x - down.position.x
                                    val dy = change.position.y - down.position.y
                                    if (dy < -lockPx) {
                                        // Locked: the hold ends and the recording keeps going, the
                                        // bar with its Pause and Cancel and Stop taking over.
                                        onMicLock()
                                        break
                                    }
                                    if (dx < -cancelPx) {
                                        cancelSlide = true
                                    } else if (dx > -cancelPx / 2) {
                                        cancelSlide = false
                                    }
                                }
                                if (!pressed) {
                                    if (cancelSlide) {
                                        onMicCancel()
                                    } else if (SystemClock.uptimeMillis() - downAt >= MIC_QUICK_TAP_MS) {
                                        onMicReleaseSend()
                                    } else {
                                        // A quick tap: the two-step mode's start. The recording
                                        // continues and the bar takes it from here; the release has
                                        // nothing left to decide.
                                        onMicLock()
                                    }
                                    break
                                }
                            }
                        }
                    },
            ) {
                Text(
                    text = if (recordingHeld) "⏺" else "🎤",
                    fontSize = 18.sp,
                    color = if (recordingHeld) MaterialTheme.colorScheme.error else Color.Unspecified,
                )
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
 * The composer's other face while a recording runs unheld: the two-step mode and the lock share
 * it, because from the moment the finger is gone the vocabulary is the same — a clock that counts
 * what is being said, a live waveform, a Pause that lifts, a Cancel that keeps the undo window,
 * and a Stop that hands the note to the preview.
 */
@Composable
private fun RecordingBar(
    paused: Boolean,
    elapsedMs: Long,
    amplitudes: List<Int>,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onStop: () -> Unit,
    onCancel: () -> Unit,
) {
    Surface(color = MaterialTheme.colorScheme.surface) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .imePadding()
                .navigationBarsPadding()
                .padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(text = if (paused) "⏸" else "●", color = MaterialTheme.colorScheme.error, fontSize = 14.sp)
            Spacer(modifier = Modifier.width(8.dp))
            // The clock the model drives: the state's own elapsed, which stands still through a
            // pause exactly as the recording does.
            Text(
                text = formatDuration(elapsedMs),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Spacer(modifier = Modifier.width(10.dp))
            WaveformBars(
                amplitudes = amplitudes,
                modifier = Modifier
                    .weight(1f)
                    .height(24.dp),
            )
            Spacer(modifier = Modifier.width(10.dp))
            TextButton(onClick = if (paused) onResume else onPause) {
                Text(if (paused) "Resume" else "Pause")
            }
            TextButton(onClick = onCancel) {
                Text("Cancel")
            }
            Button(onClick = onStop) {
                Text("Stop")
            }
        }
    }
}

/**
 * A finished note waiting on the composer's word: the two-step mode's preview, an undone cancel,
 * or a draft recovered from an app death. Its length and its waveform are stated up front — the
 * shape of what was said — with Send and Delete as the two things left to do with it.
 */
@Composable
private fun PreviewBar(
    preview: VoiceNotePreview,
    onSend: () -> Unit,
    onDelete: () -> Unit,
    uploading: Boolean,
) {
    Surface(color = MaterialTheme.colorScheme.surface) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .imePadding()
                .navigationBarsPadding()
                .padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(text = "♪", color = MaterialTheme.colorScheme.primary, fontSize = 16.sp)
            Spacer(modifier = Modifier.width(8.dp))
            Text(
                text = formatDuration(preview.durationMs),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Spacer(modifier = Modifier.width(10.dp))
            WaveformBars(
                amplitudes = preview.waveform?.map { it.toInt() and 0xFF } ?: emptyList(),
                modifier = Modifier
                    .weight(1f)
                    .height(24.dp),
            )
            Spacer(modifier = Modifier.width(10.dp))
            if (uploading) {
                CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
            } else {
                TextButton(onClick = onDelete) {
                    Text("Delete")
                }
                Button(onClick = onSend) {
                    Text("Send")
                }
            }
        }
    }
}

/**
 * The undo window's chip: a cancelled note is not gone yet, and the few seconds this row shows are
 * the whole of brief 179's rule that a slide nobody meant must be undoable.
 */
@Composable
private fun DiscardUndoBar(onUndo: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surface) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .padding(horizontal = 12.dp, vertical = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = "Recording discarded",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(modifier = Modifier.weight(1f))
            TextButton(onClick = onUndo) {
                Text("Undo")
            }
        }
    }
}

/**
 * The bars a recording samples, drawn right to left as they arrived — the newest sample at the
 * tail, the way the eye expects a live meter to move. Bars are the amplitude bytes themselves:
 * 0 is silence and 255 a full-scale swing, drawn against the row's height.
 */
@Composable
private fun WaveformBars(amplitudes: List<Int>, modifier: Modifier = Modifier) {
    Canvas(modifier = modifier) {
        if (amplitudes.isEmpty()) {
            return@Canvas
        }
        val barWidth = 3.dp.toPx()
        val gap = 2.dp.toPx()
        val barHeight = 2.dp.toPx()
        val count = minOf(amplitudes.size, (size.width / (barWidth + gap)).toInt().coerceAtLeast(1))
        // The newest bars keep their place at the right edge; the rest of the row is the silence
        // that has not been spoken yet.
        var x = size.width - count * (barWidth + gap)
        for (i in amplitudes.size - count until amplitudes.size) {
            val amplitude = amplitudes[i].coerceIn(0, 255) / 255f
            val height = (barHeight + amplitude * (size.height - barHeight)).coerceAtMost(size.height)
            drawRoundRect(
                color = Color(0xFF7BA3AD),
                topLeft = androidx.compose.ui.geometry.Offset(x, size.height - height),
                size = androidx.compose.ui.geometry.Size(barWidth, height),
                cornerRadius = androidx.compose.ui.geometry.CornerRadius(barWidth / 2),
            )
            x += barWidth + gap
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
 * The group's member sheet: the rename, the invite quick-pick, and the roster whose rows open the
 * member menu.
 *
 * # Who may do what
 *
 * A group is built by two founders -- the creator and the first person they named -- and the roster
 * is the only statement of who they are, so the sheet reads it before offering anyone a control:
 *
 * - **Invite** is every member's right. The quick-pick lists the friends this shell knows who are
 *   not already seated; a person not in the graph is reached from the Friends screen's search, the
 *   same path a direct chat starts from.
 * - **Rename** is the founders' control, and the roster's **mute terms** and a straight **kick**
 *   are theirs too, offered in the member menu a row's tap opens. A founder cannot touch the other
 *   founder -- a group built by two cannot be halved by one of them -- and cannot mute or kick
 *   themselves either.
 * - **Vote kick** is the members' own recourse, open to everyone, never against yourself and never
 *   against a founder. Half the group rounded up carries it, and the running tally arrives on the
 *   broadcast vote stream so every member watches the same count climb.
 */
@Composable
private fun GroupMembersSheet(
    chat: ChatState,
    selfId: Id,
    invitees: List<GroupInviteCandidate>,
    avatarBytes: Map<Id, ByteArray>,
    onClose: () -> Unit,
    onViewProfile: (Id, String) -> Unit,
    onGift: (Id, String) -> Unit,
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
                                    avatarBytes = avatarBytes[person.userId],
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
                                    avatarBytes = avatarBytes[member.userId],
                                    isSelf = member.userId == selfId,
                                    myRole = myRole,
                                    tally = chat.votes[member.userId],
                                    now = now,
                                    acting = member.userId in chat.acting,
                                    onViewProfile = { onViewProfile(member.userId, member.name) },
                                    onGift = if (member.userId != selfId && !member.departed) {
                                        { onGift(member.userId, member.name) }
                                    } else {
                                        null
                                    },
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
private fun GroupInviteRow(name: String, avatarBytes: ByteArray?, busy: Boolean, onInvite: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Avatar(name = name, bytes = avatarBytes, size = 32.dp)
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
 * One group roster row: the member, their role, any running group mute — and, on a tap, the member
 * menu the room roster's rows open too. The gating is the pair of predicates the sheet has always
 * kept: the vote for everyone but oneself and a founder, the founder's controls for a founder over
 * a plain member. A departed member reads as "was here" and opens nothing, because history is not
 * deletable and neither is a row that explains it.
 */
@Composable
private fun GroupMemberRow(
    member: GroupMember,
    avatarBytes: ByteArray?,
    isSelf: Boolean,
    myRole: ConversationRole,
    tally: VoteTally?,
    now: Long,
    acting: Boolean,
    onViewProfile: () -> Unit,
    onGift: (() -> Unit)?,
    onVoteKick: () -> Unit,
    onMute: (Long?) -> Unit,
    onKick: () -> Unit,
) {
    val founder = canFounderAct(myRole, member.role, isSelf)
    val canVote = canVoteKickGroup(member.role, isSelf)
    val muted = member.mutedUntil != null && member.mutedUntil > now
    val menuOpen = remember { mutableStateOf(false) }
    val sub = buildString {
        append(groupRoleLabel(member.role))
        if (member.departed) append(" · left")
        if (muted && member.mutedUntil != null) append(" · muted")
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = !member.departed) { menuOpen.value = true }
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Avatar(name = member.name, bytes = avatarBytes, size = 32.dp)
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
            Text(
                text = sub,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (acting) {
            CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
        }
        if (!member.departed) {
            Text(text = "›", fontSize = 18.sp, color = LocalMigoExtra.current.faint)
        }
    }

    if (menuOpen.value && !member.departed) {
        MemberMenuSheet(
            title = member.name,
            avatarName = member.name,
            avatarBytes = avatarBytes,
            sub = sub,
            acting = acting,
            onDismiss = { menuOpen.value = false },
            onViewProfile = onViewProfile,
            onGift = onGift,
        ) {
            if (canVote) {
                SheetAction(
                    glyph = "🗳",
                    label = if (tally != null) "Vote kick ${tally.votes}/${tally.needed}" else "Vote kick",
                    sub = "When half the group agrees, they are kicked",
                    enabled = !acting,
                    onClick = {
                        menuOpen.value = false
                        onVoteKick()
                    },
                )
            }
            if (founder && muted) {
                SheetAction(
                    glyph = "🔊",
                    label = "Unmute",
                    sub = "Lift this group mute now",
                    enabled = !acting,
                    onClick = {
                        menuOpen.value = false
                        onMute(null)
                    },
                )
            }
            if (founder && !muted) {
                for ((label, term) in GROUP_MUTE_TERMS) {
                    SheetAction(
                        glyph = "🔇",
                        label = "Mute $label",
                        sub = "Silenced for the whole group; every other right kept",
                        enabled = !acting,
                        onClick = {
                            menuOpen.value = false
                            onMute(term)
                        },
                    )
                }
            }
            if (founder) {
                ConfirmSheetAction(
                    glyph = "⛔",
                    label = "Kick",
                    sub = "A kick spends 1 KP, else 1 \$MIG. Vote kick is free.",
                    enabled = !acting,
                    onConfirm = {
                        menuOpen.value = false
                        onKick()
                    },
                )
            }
        }
    }
}


/**
 * The member sheet: who is in the room, and what this account may do about them.
 *
 * It covers the thread as a full surface rather than a panel beside it, because a roster is a list
 * that scrolls and a phone has no room for one alongside a chat. A row is no longer a line of
 * buttons: tapping it opens the member menu — the profile, the gift, the vote, and the staff
 * powers where rank admits them — so the roster reads as a list of people first and a set of
 * controls second, the same shape the web client's roster now keeps. The vote is the one power an
 * ordinary member holds, offered on every row but one's own and the Owner's, with the running
 * tally once a vote is open; the staff powers (silence, kick, ban) appear in the menu only on a
 * row this account outranks, and only when this account is a Moderator or above, with a kick or a
 * ban asking twice because removing somebody is not a thing a single mis-tap should do. "Mute for
 * me" is a personal choice that lives on the profile the menu opens, and the muted accounts who
 * are not in the room gather in their own list with an Unmute.
 */
@Composable
private fun MembersSheet(
    chat: ChatState,
    selfId: Id,
    avatarBytes: Map<Id, ByteArray>,
    onClose: () -> Unit,
    onViewProfile: (Id, String) -> Unit,
    onGift: (Id, String) -> Unit,
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
                                avatarBytes = avatarBytes[member.userId],
                                isSelf = member.userId == selfId,
                                myRole = myRole,
                                tally = chat.votes[member.userId],
                                acting = member.userId in chat.acting,
                                onViewProfile = { onViewProfile(member.userId, member.name) },
                                onGift = if (member.userId != selfId) {
                                    { onGift(member.userId, member.name) }
                                } else {
                                    null
                                },
                                onVoteKick = { onVoteKick(member.userId) },
                                onSanction = { action -> onSanction(member.userId, action) },
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
                                    avatarBytes = avatarBytes[id],
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
 * One roster row: the member, their role, and — on a tap — the member menu.
 *
 * The row itself is the entry: tapping it opens the actions this viewer may take against the
 * member (the profile, the gift, the vote, and the staff sanctions when rank admits them) instead
 * of laying them out beside every name. One's own row still opens, carrying the profile alone —
 * a person may read their own card — and the gating predicates are the ones the row always kept:
 * the vote for everyone but oneself and the Owner, the staff powers for a Moderator-or-above over
 * a strictly lower role.
 */
@Composable
private fun MemberRow(
    member: RosterMember,
    avatarBytes: ByteArray?,
    isSelf: Boolean,
    myRole: RoomRole,
    tally: VoteTally?,
    acting: Boolean,
    onViewProfile: () -> Unit,
    onGift: (() -> Unit)?,
    onVoteKick: () -> Unit,
    onSanction: (SanctionAction) -> Unit,
) {
    val staff = !isSelf && myRole.wire >= RoomRole.Moderator.wire && member.role.wire < myRole.wire
    val canVote = !isSelf && member.role != RoomRole.Owner
    val menuOpen = remember { mutableStateOf(false) }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = { menuOpen.value = true })
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Avatar(name = member.name, bytes = avatarBytes, size = 32.dp)
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
        Text(text = "›", fontSize = 18.sp, color = LocalMigoExtra.current.faint)
    }

    if (menuOpen.value) {
        MemberMenuSheet(
            title = member.name,
            avatarName = member.name,
            avatarBytes = avatarBytes,
            sub = roleLabel(member.role),
            acting = acting,
            onDismiss = { menuOpen.value = false },
            onViewProfile = onViewProfile,
            onGift = onGift,
        ) {
            if (canVote) {
                SheetAction(
                    glyph = "🗳",
                    label = if (tally != null) "Vote kick ${tally.votes}/${tally.needed}" else "Vote kick",
                    sub = "When half the room agrees, they are kicked",
                    enabled = !acting,
                    onClick = {
                        menuOpen.value = false
                        onVoteKick()
                    },
                )
            }
            if (staff) {
                SheetAction(
                    glyph = "🔇",
                    label = "Silence in room",
                    sub = "For everyone — the server sets the term",
                    enabled = !acting,
                    onClick = {
                        menuOpen.value = false
                        onSanction(SanctionAction.Mute)
                    },
                )
                ConfirmSheetAction(
                    glyph = "⛔",
                    label = "Kick",
                    sub = "Removed from the room; they can come back",
                    enabled = !acting,
                    onConfirm = {
                        menuOpen.value = false
                        onSanction(SanctionAction.Kick)
                    },
                )
                ConfirmSheetAction(
                    glyph = "🚫",
                    label = "Ban",
                    sub = "Removed and barred from returning",
                    enabled = !acting,
                    onConfirm = {
                        menuOpen.value = false
                        onSanction(SanctionAction.Ban)
                    },
                )
            }
        }
    }
}

/** One account this device has muted who is not in the room, with the control to lift it. */
@Composable
private fun MutedRow(name: String, avatarBytes: ByteArray?, acting: Boolean, onUnmute: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Avatar(name = name, bytes = avatarBytes, size = 32.dp)
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
 * A destructive member-menu action that asks once before it acts.
 *
 * The first tap turns the label into "Sure?"; the second, within the same row, is the one that
 * fires. A removal a single mis-tap could cause is not one this menu should make on a single tap —
 * the same rule the roster's inline buttons kept, carried into the menu that replaced them.
 */
@Composable
private fun ConfirmSheetAction(
    glyph: String,
    label: String,
    sub: String?,
    enabled: Boolean,
    onConfirm: () -> Unit,
) {
    val armed = remember { mutableStateOf(false) }
    SheetAction(
        glyph = glyph,
        label = if (armed.value) "Sure?" else label,
        sub = sub,
        danger = true,
        enabled = enabled,
        onClick = {
            if (armed.value) {
                onConfirm()
            } else {
                armed.value = true
            }
        },
    )
}

/**
 * The member menu a roster row's tap opens: the person's head, the two doors every menu carries,
 * and whatever actions the roster's own rules admit beneath them.
 *
 * The head is the member as the row already drew them — the avatar, the name, the role line — so
 * the menu never has to say who it is about in any other words. View profile and Gift come first,
 * the same order the web client's menu keeps, because they are the two acts that are *for* the
 * person rather than against them; the sanctions and the vote follow in the caller's [actions],
 * gated by the predicates the roster has always kept.
 */
@Composable
private fun MemberMenuSheet(
    title: String,
    avatarName: String,
    avatarBytes: ByteArray?,
    sub: String,
    acting: Boolean,
    onDismiss: () -> Unit,
    onViewProfile: () -> Unit,
    onGift: (() -> Unit)?,
    actions: @Composable () -> Unit,
) {
    MigoSheet(title = title, onDismiss = onDismiss) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Avatar(name = avatarName, bytes = avatarBytes, size = 44.dp)
            Spacer(modifier = Modifier.width(12.dp))
            Column {
                ListRowName(text = avatarName)
                ListRowLine(text = sub)
            }
        }
        SheetAction(
            glyph = "☺",
            label = "View profile",
            onClick = {
                onDismiss()
                onViewProfile()
            },
        )
        if (onGift != null) {
            SheetAction(
                glyph = "🎁",
                label = "Gift",
                sub = "From the gift shop",
                enabled = !acting,
                onClick = {
                    onDismiss()
                    onGift()
                },
            )
        }
        actions()
        Spacer(modifier = Modifier.height(8.dp))
    }
}

/** The member the gift picker is aimed at: who they are, by id and the name the roster knew. */
private data class GiftTarget(
    val userId: Id,
    val name: String,
)

/**
 * The gift picker a member menu's Gift opens, pre-aimed at the member the menu was for.
 *
 * The web client's own picker states the price on every card before the send, because a gift is a
 * spend and the spend is agreed on its price; the rows here keep the same rule — the price rides
 * under the name, and the tap that picks the gift is the tap that sends it. One tap is enough
 * because the picker closes on it: a second gift costs a deliberate reopen, not a jittery thumb.
 */
@Composable
private fun GiftSheet(
    target: GiftTarget,
    catalogue: List<GiftListing>,
    onDismiss: () -> Unit,
    onSend: (sku: String) -> Unit,
) {
    MigoSheet(title = "Send a gift", onDismiss = onDismiss) {
        ListRowLine(
            text = "To " + target.name,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
        )
        if (catalogue.isEmpty()) {
            Placeholder(text = "The gift shop is empty on this server.")
        } else {
            for (gift in catalogue) {
                SheetAction(
                    glyph = "🎁",
                    label = gift.name,
                    sub = "${gift.price} \$MIG · ${gift.category}",
                    onClick = { onSend(gift.sku) },
                )
            }
        }
        Spacer(modifier = Modifier.height(8.dp))
    }
}

// The relationship kinds the profile card files its social line under, as plain numbers the wire
// may extend past the enum's names — the same guard the Friends screen keeps.
private val KIND_FRIEND: Long = RelationshipKind.Friend.wire.toLong()
private val KIND_PENDING_INCOMING: Long = RelationshipKind.PendingIncoming.wire.toLong()
private val KIND_PENDING_OUTGOING: Long = RelationshipKind.PendingOutgoing.wire.toLong()
private val KIND_BLOCK: Long = RelationshipKind.Block.wire.toLong()

/**
 * Another member's profile, as the member menu's View profile opens it.
 *
 * The facts are the ones the web client's own profile card carries: the profile service's public
 * answer (the name with its verified mark, the handle, the presence word, the custom status in
 * quotes, the bio, the country, the language), the standing the economy service answers for anyone
 * (the level, the total XP, the level's progress bar, the badges with the dates they were earned),
 * the XP-board rank when the person holds one on the board's first page, and the social line read
 * from the one graph walk — ✓ Friends, Request sent, or the incoming request's Accept and Decline,
 * with Add friend on every other known edge. The shareable public id carries its own copy button,
 * the one identifier a person can hand out freely.
 *
 * Every standing fact degrades to a missing line rather than a broken card, and a withheld profile
 * answer (the wire serves nothing rather than explaining why) is its own sentence, not a spinner
 * that never ends and not a failure colour. The personal mute rides here rather than in the member
 * menu, mirroring the web client's own profile card: a choice about the person, made where the
 * person is being read.
 */
@Composable
private fun MemberProfileSheet(
    view: MemberProfileView,
    avatarBytes: ByteArray?,
    selfId: Id,
    canMuteForMe: Boolean,
    muted: Boolean,
    onClose: () -> Unit,
    onMuteForMe: (Boolean) -> Unit,
    onFriendRequest: () -> Unit,
    onFriendRespond: (Boolean) -> Unit,
) {
    // The clipboard the copy-id control writes to, and the mark that says it did: the platform's
    // own service, fetched once, with the copied state outliving the tap that set it the same way
    // the web client's does (until the sheet is reopened).
    val clipboard = LocalContext.current.getSystemService(ClipboardManager::class.java)
    var idCopied by remember { mutableStateOf(false) }
    MigoSheet(title = view.name, onDismiss = onClose) {
        when {
            view.profile == null && !view.settled -> LoadingRow()

            view.profile == null && view.failure != null -> Text(
                text = view.failure ?: "",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )

            // Settled with no profile is the wire's withheld rule: the server served nothing for
            // the id, and the honest sentence is the one that states that without guessing why.
            view.profile == null -> Placeholder(
                text = "This account's profile is not available.",
            )

            else -> {
                val profile = view.profile
                if (profile != null) {
                    // A friend act's failure is stated without losing the card it was made from —
                    // the profile has already landed, and the person is still worth reading while
                    // the act that failed is being retried.
                    if (view.failure != null) {
                        Text(
                            text = view.failure ?: "",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                        )
                    }
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Avatar(
                            name = profile.displayName.ifBlank { view.name },
                            bytes = avatarBytes,
                            size = 56.dp,
                        )
                        Spacer(modifier = Modifier.width(12.dp))
                        Column {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                ListRowName(text = profile.displayName.ifBlank { view.name })
                                if (profile.verified == true) {
                                    Spacer(modifier = Modifier.width(6.dp))
                                    // The verified mark, the same glyph the web card carries.
                                    Text(
                                        text = "✔",
                                        style = MaterialTheme.typography.titleSmall,
                                        color = MaterialTheme.colorScheme.primary,
                                    )
                                }
                            }
                            if (profile.username.isNotBlank()) {
                                ListRowLine(text = "@" + profile.username)
                            }
                            // The presence word only when the wire named a state: "—" is the
                            // unknown's own answer, and an unknown is a line that is not there.
                            val presence = profile.presence
                            if (presence != null && presence != PresenceState.Unknown) {
                                ListRowLine(text = presenceLabel(presence))
                            }
                            view.progression?.let { standing ->
                                ListRowLine(text = "Level " + standing.level)
                            }
                            profile.customStatus?.takeIf { it.isNotBlank() }?.let { status ->
                                ListRowLine(text = "“$status”")
                            }
                        }
                    }
                    if (!profile.bio.isNullOrBlank()) {
                        Text(
                            text = profile.bio ?: "",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurface,
                            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                        )
                    }
                    // The facts, each one its own absence when its read did not answer: a card
                    // without its country or its XP is still a card, exactly as on the web client.
                    val facts = buildList {
                        profile.country?.let { add("🌍 $it") }
                        profile.language?.let { add("🗣 $it") }
                        view.progression?.let { standing -> add("⭐ ${standing.xp} XP") }
                        view.rank?.let { held -> add("🏆 #$held on the XP board") }
                    }
                    if (facts.isNotEmpty()) {
                        ListRowLine(
                            text = facts.joinToString(" · "),
                            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                        )
                    }
                    // The shareable public id, with the copy that makes sharing it a tap: the one
                    // identifier the account can hand out freely, so the button beside it is the
                    // card's one piece of its own machinery.
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        ListRowLine(
                            text = "🪪 " + profile.publicId,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(
                            onClick = {
                                clipboard?.setPrimaryClip(
                                    ClipData.newPlainText("public id", profile.publicId),
                                )
                                idCopied = true
                            },
                            modifier = Modifier.semantics {
                                contentDescription = if (idCopied) {
                                    "Copied"
                                } else {
                                    "Copy the shareable id"
                                }
                            },
                        ) {
                            Text(
                                text = if (idCopied) "✓ Copied" else "📋 Copy",
                                style = MaterialTheme.typography.labelMedium,
                            )
                        }
                    }
                    // The level's progress bar, only when the wire named a span: a bar without its
                    // denominator is a picture that promises a number it does not have.
                    view.progression?.let { standing ->
                        if (standing.xpForNextLevel > 0) {
                            Column(
                                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
                            ) {
                                LinearProgressIndicator(
                                    progress = {
                                        (standing.xpIntoLevel.toDouble() / standing.xpForNextLevel.toDouble())
                                            .toFloat()
                                            .coerceIn(0f, 1f)
                                    },
                                    modifier = Modifier.fillMaxWidth(),
                                )
                                ListRowLine(
                                    text = "${standing.xpIntoLevel} / ${standing.xpForNextLevel} XP " +
                                        "to level ${standing.level + 1}",
                                    modifier = Modifier.padding(top = 4.dp),
                                )
                            }
                        }
                    }
                    // The badges with the dates they were earned — the honours the economy service
                    // answers for anyone, unread (null) being no row at all and read-empty being
                    // the same honest nothing.
                    val badges = view.badges
                    if (!badges.isNullOrEmpty()) {
                        for (badge in badges) {
                            ListRowLine(
                                text = "🏅 ${badge.badgeCode.replace('_', ' ')} · Earned " +
                                    DateUtils.getRelativeTimeSpanString(badge.awardedAt),
                                modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
                            )
                        }
                    }
                    // The social line: what this account is to the person, and the one act that
                    // state admits. Never for one's own card — a relationship to oneself is not a
                    // fact the graph holds — and never for an unknown edge or a block, which the
                    // wire states its own ways. Each act re-reads the edge, so the line says what
                    // the wire says, not what the button hoped.
                    if (view.userId != selfId) {
                        val edge = view.relationship
                        when (edge) {
                            null, KIND_BLOCK -> Unit

                            KIND_FRIEND -> ListRowLine(
                                text = "✓ Friends",
                                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                            )

                            KIND_PENDING_OUTGOING -> ListRowLine(
                                text = "Request sent",
                                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                            )

                            KIND_PENDING_INCOMING -> Column(
                                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
                            ) {
                                ListRowLine(text = "wants to be your friend")
                                Row {
                                    TextButton(
                                        onClick = { onFriendRespond(true) },
                                        enabled = !view.friendBusy,
                                    ) { Text("Accept") }
                                    TextButton(
                                        onClick = { onFriendRespond(false) },
                                        enabled = !view.friendBusy,
                                    ) { Text("Decline", color = MaterialTheme.colorScheme.error) }
                                }
                            }

                            else -> TextButton(
                                onClick = onFriendRequest,
                                enabled = !view.friendBusy,
                                modifier = Modifier.padding(horizontal = 12.dp),
                            ) { Text("Add friend") }
                        }
                    }
                }
                if (canMuteForMe) {
                    SheetAction(
                        glyph = if (muted) "🔊" else "🔇",
                        label = if (muted) "Unmute" else "Mute for me",
                        sub = "Hides this person's messages in this room for you",
                        onClick = { onMuteForMe(!muted) },
                    )
                }
            }
        }
        Spacer(modifier = Modifier.height(8.dp))
    }
}

/**
 * The header gift's recipient picker for a room or a group: the people the conversation holds,
 * each one a tap away from becoming the gift's aim.
 *
 * A direct chat never opens this sheet — its gift is aimed at the peer the moment the header's
 * control is pressed — so the rows here are the conversation's own roster, read on open when the
 * member sheet never has. One's own row is not offered (a gift to oneself is not a thing the shop
 * sells), and a group's departed members are skipped for the same reason the invite quick-pick
 * skips them: a row that aimed a gift at someone no longer in the room would be a button that can
 * only fail politely.
 */
@Composable
private fun GiftRecipientSheet(
    chat: ChatState,
    selfId: Id,
    avatarBytes: Map<Id, ByteArray>,
    onDismiss: () -> Unit,
    onPick: (userId: Id, name: String) -> Unit,
) {
    MigoSheet(title = "Send a gift", onDismiss = onDismiss) {
        when {
            chat.rosterLoading && chat.roster == null && chat.groupRoster == null -> LoadingRow()

            else -> {
                val recipients = if (chat.kind == ConversationKind.Group) {
                    chat.groupRoster
                        ?.filter { !it.departed && it.userId != selfId }
                        ?.map { it.userId to it.name }
                        .orEmpty()
                } else {
                    chat.roster
                        ?.filter { it.userId != selfId }
                        ?.map { it.userId to it.name }
                        .orEmpty()
                }
                if (recipients.isEmpty()) {
                    Placeholder(text = "No one else is in this conversation.")
                } else {
                    for ((userId, name) in recipients) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { onPick(userId, name) }
                                .padding(horizontal = 16.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Avatar(name = name, bytes = avatarBytes[userId], size = 44.dp)
                            Spacer(modifier = Modifier.width(12.dp))
                            ListRowName(text = name)
                        }
                    }
                }
            }
        }
        Spacer(modifier = Modifier.height(8.dp))
    }
}

/**
 * The composer's attach menu: the four ways a message's payload can be chosen, each naming its
 * promise before the system's own picker opens.
 *
 * A port of the web composer's own menu, minus the one row that has no Android counterpart to
 * fold in — the disappearing-messages toggle, which this client's composer never carried, so the
 * menu keeps exactly its four picks and nothing invented to fill the web's shape. All four funnel
 * into the same attachment send path; the choice only decides which picker the platform opens and
 * which promise the row made.
 */
@Composable
private fun AttachSheet(
    onDismiss: () -> Unit,
    onPick: (AttachSource) -> Unit,
) {
    MigoSheet(title = "Attach", onDismiss = onDismiss) {
        SheetAction(
            glyph = "📄",
            label = "Pick a file",
            sub = "Anything the picker offers",
            onClick = { onPick(AttachSource.File) },
        )
        SheetAction(
            glyph = "📷",
            label = "Take a photo",
            sub = "From the camera",
            onClick = { onPick(AttachSource.Photo) },
        )
        SheetAction(
            glyph = "🎬",
            label = "Pick a video",
            sub = "From the gallery",
            onClick = { onPick(AttachSource.Video) },
        )
        SheetAction(
            glyph = "🖼",
            label = "Pick an image",
            sub = "From the gallery",
            onClick = { onPick(AttachSource.Image) },
        )
        Spacer(modifier = Modifier.height(8.dp))
    }
}

/**
 * The composer's emoticon picker: two tabs, the same shape the web client's picker keeps.
 *
 * The first tab is the emoticons — the free baseline every account can use plus whatever emoticon
 * packs the account owns — and the second is the stickers, each owned pack under its own name.
 * Everything inserts as text: the glyphs are Unicode, and a sticker rides out as ordinary message
 * text the same way an emoticon does. The owned set arrives as null while the entitlements read is
 * in flight, and the picker waits for it rather than guessing — the free baseline is not a
 * substitute for an answer about what was paid for.
 */
@Composable
private fun EmoticonSheet(
    owned: Set<String>?,
    onDismiss: () -> Unit,
    onInsert: (glyph: String) -> Unit,
) {
    var stickersTab by remember { mutableStateOf(false) }
    MigoSheet(title = "Emoticons", onDismiss = onDismiss) {
        Row(modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp)) {
            TextButton(
                onClick = { stickersTab = false },
                modifier = Modifier.weight(1f),
            ) {
                Text(
                    text = "Emoticons",
                    style = MaterialTheme.typography.labelLarge,
                    color = if (stickersTab) {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    } else {
                        MaterialTheme.colorScheme.primary
                    },
                )
            }
            TextButton(
                onClick = { stickersTab = true },
                modifier = Modifier.weight(1f),
            ) {
                Text(
                    text = "Stickers",
                    style = MaterialTheme.typography.labelLarge,
                    color = if (stickersTab) {
                        MaterialTheme.colorScheme.primary
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                )
            }
        }
        when {
            owned == null -> LoadingRow()

            !stickersTab -> {
                val glyphs = FREE_EMOTICONS + ownedEmoticons(owned)
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(max = 320.dp)
                        .verticalScroll(rememberScrollState())
                        .padding(horizontal = 12.dp),
                ) {
                    for (row in glyphs.chunked(8)) {
                        Row(modifier = Modifier.fillMaxWidth()) {
                            for (glyph in row) {
                                TextButton(
                                    onClick = { onInsert(glyph) },
                                    modifier = Modifier
                                        .weight(1f)
                                        .semantics { contentDescription = "Insert $glyph" },
                                ) {
                                    Text(text = glyph, style = MaterialTheme.typography.titleLarge)
                                }
                            }
                            repeat(8 - row.size) { Spacer(modifier = Modifier.weight(1f)) }
                        }
                    }
                }
            }

            else -> {
                val packs = ownedStickerPacks(owned)
                if (packs.isEmpty()) {
                    Placeholder(text = "You do not own any sticker packs yet.")
                } else {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(max = 320.dp)
                            .verticalScroll(rememberScrollState())
                            .padding(horizontal = 12.dp),
                    ) {
                        for (pack in packs) {
                            SectionLabel(text = pack.name)
                            for (row in pack.items.chunked(4)) {
                                Row(modifier = Modifier.fillMaxWidth()) {
                                    for (glyph in row) {
                                        TextButton(
                                            onClick = { onInsert(glyph) },
                                            modifier = Modifier
                                                .weight(1f)
                                                .semantics { contentDescription = "Insert $glyph" },
                                        ) {
                                            Text(
                                                text = glyph,
                                                style = MaterialTheme.typography.headlineMedium,
                                            )
                                        }
                                    }
                                    repeat(4 - row.size) { Spacer(modifier = Modifier.weight(1f)) }
                                }
                            }
                        }
                    }
                }
            }
        }
        Spacer(modifier = Modifier.height(8.dp))
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
