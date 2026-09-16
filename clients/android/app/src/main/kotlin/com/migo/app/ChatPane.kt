package com.migo.app

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import com.migo.app.model.AppState
import com.migo.app.model.AttachSource
import com.migo.app.model.ChatState
import com.migo.app.ui.ChatScreen
import com.migo.app.ui.GroupInviteCandidate
import com.migo.core.protocol.ConversationKind
import com.migo.core.protocol.RelationshipKind
import com.migo.core.store.MediaAutoDownload
import com.migo.core.wire.Id
import java.io.File

/**
 * The chat surface both shells share: the windowing shell's full-bleed conversation and the chat
 * activity's whole screen are the same screen, so it lives once here and each shell composes it
 * with the modifier its own layout owes.
 *
 * Everything the chat needs that must be asked of the system -- the permission dialogs, the
 * attachment pickers, the document save -- is wired here rather than in either shell, because a
 * control that exists in two copies is a control that drifts. The model stays the one source of
 * state; this composable owns nothing but launchers and the reads the chat's rows want.
 */
@Composable
internal fun ChatPane(
    state: AppState.SignedIn,
    open: ChatState,
    model: AppViewModel,
    modifier: Modifier = Modifier,
) {
    // The call and voice-note permission doors: the moment-of-use asking, shared with the chat
    // activity, that the header's call buttons and the composer's mic button go through.
    val controls = rememberCallControls(model)

    // The composer's four attachment doors and the document destination picker, all the system's
    // own sheets: three GetContent reads (anything, a video, an image) and one TakePicture write
    // for the camera, plus the CreateDocument for the save a document row asks for -- no storage
    // permission either way, because the person's own pick is the person's own grant, and the
    // camera's output is the app's own cache through a FileProvider the manifest names. The image
    // or document split is still the model's, by the picked file's own MIME type; the staged
    // document is what carries the Save press across the picker's answer.
    val pickFile = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent(),
    ) { uri ->
        if (uri != null) model.sendAttachment(uri)
    }
    val pickVideo = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent(),
    ) { uri ->
        if (uri != null) model.sendAttachment(uri)
    }
    val pickImage = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent(),
    ) { uri ->
        if (uri != null) model.sendAttachment(uri)
    }
    // The camera's destination, remembered across the shot itself: TakePicture answers only
    // whether the camera wrote the file, so the uri the launch was handed has to be the one the
    // answer reads. Saved rather than remembered, because a rotation mid-shot must not orphan a
    // photo the camera did take.
    var cameraOutput by rememberSaveable { mutableStateOf<String?>(null) }
    val takePhoto = rememberLauncherForActivityResult(
        ActivityResultContracts.TakePicture(),
    ) { saved ->
        val path = cameraOutput
        cameraOutput = null
        if (saved && path != null) model.sendAttachment(Uri.parse(path))
    }
    val saveDocument = rememberLauncherForActivityResult(
        ActivityResultContracts.CreateDocument("application/octet-stream"),
    ) { uri ->
        if (uri != null) model.saveDocumentTo(uri)
    }
    val mediaObjects by model.mediaObjects.collectAsState()
    val avatarBytes by model.avatarBytes.collectAsState()
    // The receiver-local listened marks, beside the media objects they describe: the set is the
    // model's (it survives the chat's recompositions and the restart), while the dim and the
    // toggle that read it belong to the row.
    val listenedVoiceNotes by model.listenedVoiceNotes.collectAsState()
    // The member profile sheet's state, collected here for the same reason the avatars are: the
    // read is the model's, while the surface belongs to whichever member sheet named the person.
    val memberProfile by model.memberProfile.collectAsState()
    // The report sheet's state, collected here for the same reason the member profile's is: the
    // picks are the model's, while the surface is drawn by whichever screen is on top. The chat's
    // two report doors open it, and the sheet outlives the card it was opened from — a person
    // reports somebody and then closes their card, and the report is still being written.
    val reportSheet by model.reportSheet.collectAsState()
    // The group-call affordance's own facts, beside the overlays' state it shares a flow with:
    // which conversations have a call running that this device holds no seat in. The header's call
    // button is the reader, and the entry it names must be as live as the chat it sits in.
    val groupCallState by model.groupCallState.collectAsState()
    // The account's owned pack SKUs, for the composer's emoticon/sticker picker: null while the
    // one-per-session read is in flight, which the picker renders as its own wait.
    val ownedPacks by model.ownedPacks.collectAsState()
    // The media choice, answered as one fact for every bubble on screen: "Wi-Fi only" reads the
    // connection's own metered state, which is the network's word rather than the app's guess.
    // Read per composition rather than remembered — a settings change must reach the next
    // recomposition without a key to invalidate on, and the read is a system-service lookup this
    // screen makes at most a few times a second.
    val context = LocalContext.current
    val preferences by model.preferences.collectAsState()
    val autoFetchMedia = when (preferences.mediaAutoDownload) {
        MediaAutoDownload.Always -> true
        MediaAutoDownload.Never -> false
        // A missing manager or no active network answers "metered": the safe reading of a
        // Wi-Fi-only choice is the one that spends nothing.
        MediaAutoDownload.Unmetered ->
            context.getSystemService(ConnectivityManager::class.java)?.isActiveNetworkMetered == false
    }

    ChatScreen(
        chat = open,
        onDraft = model::setDraft,
        onSend = model::send,
        onLeave = open.roomId?.let { roomId ->
            { model.leaveRoom(open.conversationId, roomId) }
        },
        onOpenMembers = open.roomId?.let { roomId ->
            { model.openMembers(open.conversationId, roomId) }
        },
        onCloseMembers = { model.closeMembers(open.conversationId) },
        onVoteKick = { target ->
            open.roomId?.let { roomId -> model.voteKick(open.conversationId, roomId, target) }
        },
        onSanction = { target, action ->
            open.roomId?.let { roomId -> model.sanction(open.conversationId, roomId, target, action) }
        },
        onMuteForMe = { userId, on -> model.muteForMe(open.conversationId, userId, on) },
        // The group lifecycle rides the same chat surface: the member sheet, the rename, the
        // invite quick-pick, and the founder-vs-vote controls all read from the conversation id
        // alone, where the room controls above need a room.
        onOpenGroupMembers = if (open.roomId == null) {
            { model.openGroupMembers(open.conversationId) }
        } else {
            null
        },
        onCloseGroupMembers = { model.closeMembers(open.conversationId) },
        onInvite = { userIds -> model.inviteToGroup(open.conversationId, userIds) },
        onGroupVoteKick = { target -> model.groupVoteKick(open.conversationId, target) },
        onGroupMute = { target, term -> model.groupMute(open.conversationId, target, term) },
        onGroupKick = { target -> model.groupKick(open.conversationId, target) },
        onRenameGroup = { title -> model.renameGroup(open.conversationId, title) },
        onToggleRename = model::toggleGroupRename,
        onRenameValue = model::setGroupRename,
        onLeaveGroup = if (open.roomId == null) {
            { model.leaveGroup(open.conversationId) }
        } else {
            null
        },
        // The invite quick-pick lists the friends this shell can name: a row that offers
        // to invite someone has to say who it is offering.
        groupInvitees = state.friends.entries
            .filter { it.kind == RelationshipKind.Friend.wire.toLong() }
            .mapNotNull { entry ->
                val name = model.nameOf(entry.userId) ?: return@mapNotNull null
                GroupInviteCandidate(entry.userId, name)
            }
            .sortedBy { it.name },
        gameCatalogue = state.games.catalogue,
        gamesLoading = state.games.loading,
        gamesFailure = state.games.failure,
        onLoadGames = model::loadGameCatalogue,
        onStartGame = { slug -> model.startGame(open.conversationId, slug) },
        onGuess = { value -> model.submitGuess(open.conversationId, value) },
        selfId = state.accountId,
        onAcknowledgeSafety = model::acknowledgeSafetyChange,
        onStartCall = { peerId, video -> controls.requestCall(open.conversationId, peerId, video) },
        // The group-call join rides the same header: offered only for a group, the web client's
        // own gate — a direct chat has the 1:1 buttons and a room has no group call to join. The
        // running call's id is read at tap time rather than composed in, because the affordance
        // may have appeared or retired since the header was last drawn: the join must name the
        // call that is running now, and fall through to starting one only when none is.
        onJoinGroupCall = if (open.kind == ConversationKind.Group) {
            {
                val running = groupCallState.inProgress[open.conversationId]
                model.joinGroupCall(open.conversationId, running?.callId)
            }
        } else {
            null
        },
        groupCallInProgress = groupCallState.inProgress[open.conversationId],
        onExportLog = { model.shareChatLog(open.conversationId) },
        onToggleSearch = model::toggleChatSearch,
        onSearchQuery = model::setChatSearchQuery,
        // Attachments are an end-to-end feature: the control is offered only where the
        // conversation has a key channel to hand the recipients the object's key -- every direct
        // chat and group, never a server-readable room. The four menu picks all funnel into the
        // model's one attachment send path; the choice only decides which of the system's pickers
        // opens.
        onPickAttachment = if (open.kind != ConversationKind.Room) {
            { source ->
                when (source) {
                    AttachSource.File -> pickFile.launch("*/*")
                    AttachSource.Photo -> {
                        val output = newCameraOutput(context)
                        cameraOutput = output.toString()
                        takePhoto.launch(output)
                    }
                    AttachSource.Video -> pickVideo.launch("video/*")
                    AttachSource.Image -> pickImage.launch("image/*")
                }
            }
        } else {
            null
        },
        onVoiceNote = controls.requestVoiceNote,
        onPauseVoiceNote = model::pauseVoiceNote,
        onResumeVoiceNote = model::resumeVoiceNote,
        onStopVoiceNote = model::stopVoiceNote,
        onSendVoiceNote = model::sendVoiceNote,
        onDeleteVoiceNote = model::deleteVoiceNoteDraft,
        onUndoVoiceNoteDiscard = model::undoVoiceNoteDiscard,
        onCancelVoiceNote = model::cancelVoiceNote,
        onReact = model::react,
        // The sender's own two acts on their line, from the long-press bar: the edit seals a
        // replacement through the same chain the send used, and the delete is the tombstone every
        // member's copy drops when the server broadcasts it.
        onEdit = model::editMessage,
        onDelete = model::deleteMessage,
        onResolveMedia = model::resolveMedia,
        autoFetchMedia = autoFetchMedia,
        onSaveDocument = { attachment ->
            model.stageDocumentSave(attachment)
            saveDocument.launch(attachment.caption ?: "document")
        },
        mediaObjects = mediaObjects,
        avatarBytes = avatarBytes,
        // The voice-note player's two receiver-side facts: which notes this account has heard,
        // and the rate the next note plays at. Both are local — no mark and no rate ever rides
        // the wire — so both are handed straight from the model's own state.
        listenedVoiceNotes = listenedVoiceNotes,
        onSetVoiceNoteListened = model::setVoiceNoteListened,
        voiceNoteSpeed = preferences.voiceNoteSpeed,
        onVoiceNoteSpeed = model::setVoiceNoteSpeed,
        // The member menu's two doors: the profile sheet's read and the gift picker's send, both
        // the model's because both are round trips the sheet cannot make for itself. The gift
        // catalogue is the wallet's own — the session loads it at sign-in for the banner's
        // balance — so the picker never has to wait on a read.
        onViewMember = { userId, name -> model.openMemberProfile(userId, name) },
        memberProfile = memberProfile,
        onCloseMemberProfile = model::closeMemberProfile,
        giftCatalogue = state.wallet.catalogue,
        onSendGift = { sku, recipient, clientKey -> model.sendGift(sku, recipient, clientKey) },
        // The header gift's recipient list, the composer's emoticon picker's owned read, and the
        // profile card's two friend acts: all the model's round trips, handed in because a sheet
        // cannot make them for itself. The owned pack set is collected here like the avatars are
        // -- one read per session, kept by the model, read by whichever surface needs it.
        onLoadGiftRecipients = model::loadGiftRecipients,
        ownedPacks = ownedPacks,
        onLoadOwnedPacks = model::loadOwnedPacks,
        onMemberFriendRequest = model::memberFriendRequest,
        onMemberFriendRespond = { accept -> model.memberFriendRespond(accept) },
        // The chat's two report doors — a line's own menu, and the card of the person who sent
        // it — both open the one sheet, which lives on the model because it is the model that
        // files. Handed in as the open, not the file: the sheet collects a reason and a note
        // before anything is priced, and that collection is what the model's flow holds.
        onReport = model::openReport,
        modifier = modifier,
    )

    // The report sheet, drawn by the pane rather than the chat because the pane is what holds the
    // model: the chat is a function of its arguments, and the sheet's state is a flow the model
    // owns. It sits outside [ChatScreen] so a sheet opened from a line's menu and a sheet opened
    // from a profile card are the same surface with the same state, and so closing the profile
    // card does not close the sheet its Report button just opened.
    reportSheet?.let { view ->
        ReportSheet(
            view = view,
            onPickReason = model::setReportReason,
            onNote = model::setReportNote,
            onSubmit = model::submitReport,
            onClose = model::closeReport,
        )
    }
}

/**
 * The call and voice-note doors a chat's controls go through, as the two lambdas the chat passes
 * on to its buttons.
 *
 * The microphone permission is asked for at the moment of use, at the call button: a prompt at
 * first launch teaches nothing (the user has not called anybody yet), and the call that needs the
 * microphone is the one that explains why. The launcher lives with the chat — the surface whose
 * controls reach for it — so the button deep in a chat header can reach it without threading an
 * activity through the screens, and the model's staged call is what carries the intent across the
 * permission dialog's asynchronous answer. A video call asks for the camera too, in the same one
 * dialog: the two permissions serve one gesture, and two questions for one tap is the dialog the
 * user has already learned to distrust.
 */
@Composable
internal fun rememberCallControls(model: AppViewModel): CallControls {
    val context = LocalContext.current
    val microphone = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted -> model.microphonePermission(granted) }
    val callPermissions = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        // The microphone is the call itself; the camera is the video half. A grant of the mic
        // alone on a video call still places the video call — the manager's answer path is where
        // a missing camera falls back, and a refused mic is the one refusal that is stated.
        val microphoneGranted =
            grants[Manifest.permission.RECORD_AUDIO] == true
        val cameraGranted =
            grants[Manifest.permission.CAMERA] == true
        model.callPermissions(microphoneGranted, cameraGranted)
    }
    val requestCall: (Id, Id, Boolean) -> Unit = { conversationId, peerId, video ->
        val microphoneNeeded = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.RECORD_AUDIO,
        ) != PackageManager.PERMISSION_GRANTED
        val cameraNeeded = video && ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.CAMERA,
        ) != PackageManager.PERMISSION_GRANTED
        when {
            !microphoneNeeded && !cameraNeeded ->
                if (video) {
                    model.startVideoCall(conversationId, peerId)
                } else {
                    model.startVoiceCall(conversationId, peerId)
                }

            video -> {
                // One dialog, both permissions: a camera without a microphone is a silent
                // camera, and a microphone without a camera is the voice call nobody asked
                // for. Both are always requested together even when one is already granted --
                // the system asks only for what is missing, and the answer map then holds both
                // keys, which is what the model's answer path reads.
                model.stageVideoCall(conversationId, peerId)
                callPermissions.launch(
                    arrayOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA),
                )
            }

            else -> {
                model.stageVoiceCall(conversationId, peerId)
                microphone.launch(Manifest.permission.RECORD_AUDIO)
            }
        }
    }
    // The same moment-of-use asking for the composer's microphone: the mic button is the one
    // control that explains why the permission exists, and the model's staged note is what
    // carries the intent across the dialog's answer. The permission itself is shared with the
    // call -- one microphone, one question.
    val requestVoiceNote: () -> Unit = {
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            model.startVoiceNote()
        } else {
            model.stageVoiceNote()
            microphone.launch(Manifest.permission.RECORD_AUDIO)
        }
    }
    return CallControls(requestCall = requestCall, requestVoiceNote = requestVoiceNote)
}

/** The two doors [rememberCallControls] hands the chat: place a call, start a voice note. */
internal class CallControls(
    val requestCall: (Id, Id, Boolean) -> Unit,
    val requestVoiceNote: () -> Unit,
)

/**
 * Mints the camera shot's destination: a fresh file in the app's own cache, handed to the camera
 * through the FileProvider the manifest names.
 *
 * The app's cache rather than shared storage, because the shot is this app's own intermediate —
 * the attachment send reads the bytes and the file's MIME straight from the provider's uri, and
 * nothing outside the app ever needs the path. A fresh file per shot, because the camera's answer
 * is only "written" or "not": reusing a name would leave a refused shot showing the photo before
 * it, which is a lie a fresh temp file cannot tell.
 */
private fun newCameraOutput(context: Context): Uri {
    val dir = File(context.cacheDir, "camera")
    dir.mkdirs()
    val file = File.createTempFile("shot", ".jpg", dir)
    return FileProvider.getUriForFile(context, context.packageName + ".filepicker", file)
}
