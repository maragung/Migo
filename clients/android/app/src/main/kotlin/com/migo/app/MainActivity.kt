package com.migo.app

import android.Manifest
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import androidx.lifecycle.viewmodel.compose.viewModel
import com.migo.app.model.AppState
import com.migo.app.ui.AdminsScreen
import com.migo.app.ui.AlertsScreen
import com.migo.app.ui.CallOverlay
import com.migo.app.ui.ChatScreen
import com.migo.app.ui.ErrorBanner
import com.migo.app.ui.GamesScreen
import com.migo.app.ui.MigoTheme
import com.migo.app.ui.MobileHome
import com.migo.app.ui.MobileTabStrip
import com.migo.app.ui.PanelBar
import com.migo.app.ui.ProfileScreen
import com.migo.app.ui.SaveAccountFileDialog
import com.migo.app.ui.SearchScreen
import com.migo.app.ui.SettingsScreen
import com.migo.app.ui.SignInScreen
import com.migo.app.ui.WalletScreen
import com.migo.app.ui.panelTitle
import com.migo.core.protocol.ConversationKind
import com.migo.core.store.MediaAutoDownload
import com.migo.core.store.ThemeChoice
import com.migo.core.wire.Id

/**
 * The only activity.
 *
 * One activity and a handful of composables rather than a navigation graph. Which screen is showing
 * is already decided by [AppState] -- signed out, or signed in -- and a nav graph would be a second
 * answer to that question, able to disagree with the first.
 *
 * The signed-in screen is the mobile reference's windowing shell: a 46dp tab strip at the very top
 * carrying the home tabs (Friends, Rooms, Feed) and one tab per open conversation, with the
 * selected view showing full-bleed beneath it — a home view (the orange me card and its list), a
 * conversation, or a panel the me card's sheet opened. The back gesture is handled where it means
 * something: back closes the visible window's tab, backs a panel out of the way, and never exits
 * the app while a window or a panel is showing.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        // Android 15 draws behind the system bars whether an app asks or not, at this target level.
        // Calling it explicitly makes older versions behave the same, so the insets handled below are
        // handled on every version rather than only the newest.
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        // The theme is the composition's own business, not the activity's: the preference it
        // answers to is a device fact the view model already holds, and wrapping the tree here
        // would leave the activity choosing a theme the settings panel then cannot change.
        setContent {
            MigoApp()
        }
    }
}

/**
 * Routes the current state to a screen.
 *
 * The view model is obtained here rather than by the activity, so the whole tree below reads one
 * instance and survives a configuration change with it.
 */
@Composable
private fun MigoApp(model: AppViewModel = viewModel()) {
    val state by model.state.collectAsState()
    val callState by model.callState.collectAsState()
    // The theme preference is collected here — the composition root, the one place that both
    // holds the view model and wraps everything the theme colours. "System" is the system's own
    // dark fact; the other two choices are the person's word over it.
    val preferences by model.preferences.collectAsState()
    val dark = when (preferences.theme) {
        ThemeChoice.System -> isSystemInDarkTheme()
        ThemeChoice.Light -> false
        ThemeChoice.Dark -> true
    }

    // The microphone permission is asked for at the moment of use, at the call button: a prompt
    // at first launch teaches nothing (the user has not called anybody yet), and the call that
    // needs the microphone is the one that explains why. The launcher lives here — the shell's
    // only composition root — so the button deep in a chat header can reach it without threading
    // an activity through the screens, and the model's staged call is what carries the intent
    // across the permission dialog's asynchronous answer.
    val context = LocalContext.current
    val microphone = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted -> model.microphonePermission(granted) }
    val requestVoiceCall: (Id, Id) -> Unit = { conversationId, peerId ->
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            model.startVoiceCall(conversationId, peerId)
        } else {
            model.stageVoiceCall(conversationId, peerId)
            microphone.launch(Manifest.permission.RECORD_AUDIO)
        }
    }
    // The same moment-of-use asking for the composer's microphone: the mic button is the one
    // control that explains why the permission exists, and the model's staged note is what carries
    // the intent across the dialog's answer. The permission itself is shared with the call -- one
    // microphone, one question.
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

    MigoTheme(dark = dark) {
        Surface(
            modifier = Modifier.fillMaxSize(),
            color = MaterialTheme.colorScheme.background,
        ) {
            Column(modifier = Modifier.fillMaxSize().statusBarsPadding()) {
                when (val current = state) {
                    AppState.Starting -> Box(
                        modifier = Modifier.fillMaxSize(),
                        contentAlignment = Alignment.Center,
                    ) {
                        CircularProgressIndicator()
                    }

                    is AppState.SignedOut -> SignInScreen(
                        form = current,
                        onServerEndpoint = model::setServerEndpoint,
                        onIdentifier = model::setIdentifier,
                        onSubmit = model::signIn,
                        onRestore = model::restoreFromBackup,
                        onRefreshCaptcha = model::refreshCaptcha,
                        onDismissFailure = model::dismissFailure,
                    )

                    is AppState.SignedIn -> ShellScreen(
                        state = current,
                        model = model,
                        onRequestVoiceCall = requestVoiceCall,
                        onRequestVoiceNote = requestVoiceNote,
                    )
                }
            }

            // The call overlay renders above whatever the shell is showing — a call takes the screen
            // from anything, the same rule the web client's overlay keeps — and renders nothing at all
            // when no call is ringing, live, just ended, or failed to start. The peer's name is
            // resolved here at the composition root, the one place that holds both the call state and
            // the model that knows the names.
            val callPeerId = callState.incoming?.callerId
                ?: callState.call?.let { if (it.isCaller) it.calleeId else it.callerId }
            CallOverlay(
                state = callState,
                peerName = if (callPeerId != null) model.displayName(callPeerId) else "",
                onAccept = model::acceptCall,
                onDecline = model::declineCall,
                onCancel = model::cancelCall,
                onHangUp = model::hangUpCall,
                onToggleMute = model::toggleCallMute,
                onDismiss = model::dismissCallScreen,
            )
        }
    }
}

/**
 * The signed-in shell: the mobile reference's windowing model, as a phone wears it.
 *
 * The strip is always at the top — home tabs and a tab per open conversation — so the shell's
 * navigation is never taken away by reading a message: a conversation shows full-bleed beneath the
 * strip, with no title bar of its own, its tab being its way back. Selecting a home tab parks the
 * visible window; the window's tab stays. A panel (Alerts, Search, Wallet, Profile, Games, Admins)
 * is the one thing that covers the strip, carrying its own "‹ Menu Panel" bar back to the tab the
 * strip still shows, held in [AppState.SignedIn.stripSection].
 */
@Composable
private fun ShellScreen(
    state: AppState.SignedIn,
    model: AppViewModel,
    onRequestVoiceCall: (Id, Id) -> Unit,
    onRequestVoiceNote: () -> Unit,
) {
    val open = state.open
    // The attachment picker and the document destination picker, both the system's own sheets: a
    // GetContent for anything a person might attach (the image/document split is the model's, by
    // the picked file's own MIME type), and a CreateDocument for the save a document row asks for
    // -- no storage permission either way, because the person's own pick is the person's own
    // grant. The staged document is what carries the Save press across the picker's answer.
    val pickAttachment = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent(),
    ) { uri ->
        if (uri != null) model.sendAttachment(uri)
    }
    val saveDocument = rememberLauncherForActivityResult(
        ActivityResultContracts.CreateDocument("application/octet-stream"),
    ) { uri ->
        if (uri != null) model.saveDocumentTo(uri)
    }
    val mediaObjects by model.mediaObjects.collectAsState()
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
    // Back means "close this, not the app", in the order a person reads the screen: the members
    // sheet (handled inside the chat, composed deeper so it wins while it is up), then the visible
    // window's tab, then a panel. The strip and the home screen are the resting state back stands
    // on.
    BackHandler(enabled = open != null, onBack = {
        if (open != null) model.closeWindow(open.conversationId)
    })
    BackHandler(
        enabled = open == null && state.section.isPanel,
        onBack = { model.selectSection(state.stripSection) },
    )

    when {
        // A menu panel covers the whole shell, with the model's own bar as its way back. The error
        // banner comes too — a panel that swallows the failure message hides it from the only
        // person who caused it.
        state.section.isPanel -> Column(modifier = Modifier.fillMaxSize()) {
            PanelBar(
                title = panelTitle(state.section),
                onBack = { model.selectSection(state.stripSection) },
            )
            ErrorBanner(message = state.failure, onDismiss = model::dismissFailure)
            SectionScreen(
                state = state,
                model = model,
                modifier = Modifier.weight(1f).navigationBarsPadding(),
            )
        }

        else -> Column(modifier = Modifier.fillMaxSize()) {
            MobileTabStrip(
                section = state.section,
                windows = state.windows,
                open = state.open,
                hiddenNavs = state.hiddenNavs,
                conversations = state.conversations,
                onSelectNav = model::selectSection,
                onCloseNav = model::closeNav,
                onReopenNav = model::reopenNav,
                onSelectWindow = { model.open(it.conversationId, it.title) },
                onCloseWindow = model::closeWindow,
            )
            // The banner rides on top of the chat too: a failure raised while reading (a send that
            // did not go, a room event the server refused) is news the reader should get where
            // they are, not after they back out.
            ErrorBanner(message = state.failure, onDismiss = model::dismissFailure)
            if (open != null) {
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
                    gameCatalogue = state.games.catalogue,
                    gamesLoading = state.games.loading,
                    gamesFailure = state.games.failure,
                    onLoadGames = model::loadGameCatalogue,
                    onStartGame = { slug -> model.startGame(open.conversationId, slug) },
                    onGuess = { value -> model.submitGuess(open.conversationId, value) },
                    selfId = state.accountId,
                    onAcknowledgeSafety = model::acknowledgeSafetyChange,
                    onStartCall = { peerId -> onRequestVoiceCall(open.conversationId, peerId) },
                    onExportLog = { model.shareChatLog(open.conversationId) },
                    onToggleSearch = model::toggleChatSearch,
                    onSearchQuery = model::setChatSearchQuery,
                    // Attachments are an end-to-end feature: the control is offered only where the
                    // conversation has a key channel to hand the recipients the object's key --
                    // every direct chat and group, never a server-readable room.
                    onAttach = if (open.kind != ConversationKind.Room) {
                        { pickAttachment.launch("*/*") }
                    } else {
                        null
                    },
                    onVoiceNote = onRequestVoiceNote,
                    onStopVoiceNote = model::stopVoiceNote,
                    onCancelVoiceNote = model::cancelVoiceNote,
                    onReact = model::react,
                    onResolveMedia = model::resolveMedia,
                    autoFetchMedia = autoFetchMedia,
                    onSaveDocument = { attachment ->
                        model.stageDocumentSave(attachment)
                        saveDocument.launch(attachment.caption ?: "document")
                    },
                    mediaObjects = mediaObjects,
                    modifier = Modifier.weight(1f),
                )
            } else {
                // The home screen: the me card and the selected home view. The gesture bar draws
                // over the list's last row unless the content stands above it — only the chat
                // manages its own insets (its composer does), so the home screen pads here.
                MobileHome(
                    state = state,
                    model = model,
                    modifier = Modifier.weight(1f).navigationBarsPadding(),
                )
            }
        }
    }

    // A registration ends with the account file offer: the `.migo` container the session layer
    // sealed from the root that just registered, offered once, over whatever the shell is
    // showing — the person presses Save where they are, not in a settings panel they have yet
    // to find. A dialog rather than a sheet because it interrupts: there is no conversation to
    // read underneath an account that has not been backed up yet.
    if (state.accountFileOffer) {
        SaveAccountFileDialog(
            username = state.username,
            onSave = model::saveAccountFile,
            onDecline = model::declineAccountFile,
        )
    }
}

/**
 * The panels the me sheet opens, each covering the screen with its own way back. The home views
 * (Friends, Rooms, Feed) live in [MobileHome], and the conversation list is the window strip's own
 * ground — so the router here is the panels, and the home sections stand down.
 */
@Composable
private fun SectionScreen(state: AppState.SignedIn, model: AppViewModel, modifier: Modifier = Modifier) {
    when (state.section) {
        // The home views are [MobileHome]'s to draw; the router stands down here so there is one
        // place each screen is wired.
        AppState.Section.CHATS,
        AppState.Section.FRIENDS,
        AppState.Section.ROOMS,
        AppState.Section.FEED,
        -> Unit

        AppState.Section.GAMES -> GamesScreen(
            state = state,
            onRefresh = model::loadGameCatalogue,
            modifier = modifier,
        )

        AppState.Section.ALERTS -> AlertsScreen(
            state = state,
            onMarkAllRead = model::markAllRead,
            onRefresh = model::loadAlerts,
            modifier = modifier,
        )

        AppState.Section.SEARCH -> SearchScreen(
            state = state,
            onQuery = model::setSearchQuery,
            onStartDirect = model::startDirectWith,
            onJoinRoom = model::joinRoom,
            onOpenConversation = { model.open(it.conversationId, it.title) },
            modifier = modifier,
        )

        AppState.Section.WALLET -> WalletScreen(
            state = state,
            onSendGift = model::sendGift,
            onRefresh = model::loadWallet,
            onArchiveWallet = model::archiveWallet,
            onChainNetwork = model::selectChainNetwork,
            onChainBalance = model::refreshChainBalance,
            onChainPrepare = model::prepareChainSend,
            onChainAcknowledged = model::setChainAcknowledged,
            onChainCancel = model::cancelChainPrepare,
            onChainSend = model::confirmChainSend,
            modifier = modifier,
        )

        AppState.Section.PROFILE -> ProfileScreen(
            state = state,
            onSignOut = model::signOut,
            onRefreshDevices = model::loadDevices,
            onRemoveDevice = model::revokeDevice,
            onExport = model::exportBackup,
            onLoadProfile = model::loadOwnProfile,
            onSaveProfile = model::saveProfile,
            onSaveStatus = model::saveCustomStatus,
            onChangePassphrase = model::changePassphrase,
            onSaveContact = model::saveContact,
            onRotateIdentity = model::rotateIdentity,
            onChangeAvatar = model::changeAvatar,
            modifier = modifier,
        )

        AppState.Section.ADMINS -> AdminsScreen(
            state = state,
            onGrant = model::grantAdmin,
            onRevoke = model::revokeAdmin,
            onRefresh = model::loadAdmins,
            modifier = modifier,
        )

        AppState.Section.SETTINGS -> {
            val preferences by model.preferences.collectAsState()
            SettingsScreen(
                state = state,
                preferences = preferences,
                onTheme = model::setTheme,
                onSendReadReceipts = model::setSendReadReceipts,
                onSendTypingIndicators = model::setSendTypingIndicators,
                onMediaAutoDownload = model::setMediaAutoDownload,
                onAutoSaveChatLogs = model::setAutoSaveChatLogs,
                onSaveAllChats = model::saveAllChatsTo,
                onRefreshStorage = model::refreshStorage,
                onClearCaches = model::clearCaches,
                onSignOut = model::signOut,
                modifier = modifier,
            )
        }
    }
}
