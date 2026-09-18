package com.migo.app

import android.os.Bundle
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import com.migo.app.model.AppState
import com.migo.app.ui.AdminsScreen
import com.migo.app.ui.AlertsScreen
import com.migo.app.ui.BotsScreen
import com.migo.app.ui.CallsScreen
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
import com.migo.core.protocol.RelationshipKind
import com.migo.core.store.NavigationMode
import com.migo.core.store.ThemeChoice

/**
 * The main activity: the app's front door, and the shell of whichever navigation mode is chosen.
 *
 * One activity and a handful of composables rather than a navigation graph. Which screen is showing
 * is already decided by [AppState] -- signed out, or signed in -- and a nav graph would be a second
 * answer to that question, able to disagree with the first. The one other activity is
 * [ChatActivity], which chat-list mode stacks for a conversation tapped in the list; it reads the
 * same process-held view model this activity reads, so the two are two windows onto one session,
 * never two sessions.
 *
 * The signed-in screen is the mobile reference's windowing shell (the default, unchanged): a 46dp
 * tab strip at the very top carrying the home tabs (Friends, Rooms, Feed) and one tab per open
 * conversation, with the selected view showing full-bleed beneath it — a home view (the orange me
 * card and its list), a conversation, or a panel the me card's sheet opened. The back gesture is
 * handled where it means something: back closes the visible window's tab, backs a panel out of the
 * way, and never exits the app while a window or a panel is showing. The chat-list mode swaps the
 * strip for a bottom bar and Main's conversation list, in [ChatListShell].
 */
class MainActivity : CallHostActivity() {
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
            // The session view model is the process's one instance, held by the application so
            // the chat activity reads the same session this shell reads.
            MigoApp(model = (application as MigoApplication).appViewModel)
        }
    }

    override fun onDestroy() {
        // The main activity finishing is the app leaving, and that is the moment the session's
        // store lets the view model go — the same teardown an activity-scoped store always ran,
        // at the same moment. A rebuild (a rotation, a resize) finishes nothing and keeps the
        // session; a stacked chat activity never takes the session with it, because the main
        // activity below it is not finishing while it is in front.
        if (isFinishing) {
            (application as MigoApplication).releaseSession()
        }
        super.onDestroy()
    }
}

/**
 * Routes the current state to a screen, and the navigation mode to a shell.
 *
 * The view model is handed in by the activity — the process-held instance — so the whole tree
 * below reads the one instance the chat activity also reads, and survives a configuration change
 * with it.
 */
@Composable
private fun MigoApp(model: AppViewModel) {
    val state by model.state.collectAsState()
    // The theme preference is collected here — the composition root, the one place that both
    // holds the view model and wraps everything the theme colours. "System" is the system's own
    // dark fact; the other two choices are the person's word over it.
    val preferences by model.preferences.collectAsState()
    val dark = when (preferences.theme) {
        ThemeChoice.System -> isSystemInDarkTheme()
        ThemeChoice.Light -> false
        ThemeChoice.Dark -> true
    }

    // The interruption rule a recording keeps: the app going to the background — a lock, a
    // switch, a pocket — pauses the note, and coming back resumes it. The lifecycle observer is
    // the shell's own ears; audio focus covers the interruptions the lifecycle cannot hear (a
    // call, another app's playback) on the model's side.
    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner, model) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_STOP -> model.recordingWentToBackground()
                Lifecycle.Event.ON_START -> model.recordingReturnedToForeground()
                else -> {}
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
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
                        servers = model.serverChoices,
                        onServerEndpoint = model::setServerEndpoint,
                        onServerMode = model::setServerMode,
                        onIdentifier = model::setIdentifier,
                        onSubmit = model::signIn,
                        onRestore = model::restoreFromBackup,
                        onRefreshCaptcha = model::refreshCaptcha,
                        onDismissFailure = model::dismissFailure,
                    )

                    // The two navigation modes are two shells over one session: the windowing
                    // shell the app has always drawn, and the chat list. The preference is read
                    // here so a change in the settings panel is a recomposition away, and both
                    // shells read the same state — switching between them keeps every window,
                    // every unread, and the open chat exactly where it was.
                    is AppState.SignedIn -> if (preferences.navigationMode == NavigationMode.ChatList) {
                        ChatListShell(state = current, model = model)
                    } else {
                        ShellScreen(state = current, model = model)
                    }
                }
            }

            // The call overlays render above whatever the shell is showing — a call takes the
            // screen from anything, the same rule the web client's overlay keeps — and render
            // nothing at all when no call is ringing, live, just ended, or failed to start. The
            // chat activity holds this same layer over this same state, so a call is answered
            // from whichever surface is in front.
            SessionOverlays(model = model, state = state)
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
 *
 * The conversation itself is [ChatPane]'s to draw — the same surface the chat activity composes —
 * so the chat is wired once and shown by both shells.
 */
@Composable
private fun ShellScreen(state: AppState.SignedIn, model: AppViewModel) {
    val open = state.open
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
                // The Friends tab wears the graph's own incoming count; the number is read from
                // the state the shell already holds, never fetched for the badge.
                friendRequests = state.friends.entries.count {
                    it.kind == RelationshipKind.PendingIncoming.wire.toLong()
                }.toLong(),
            )
            // The banner rides on top of the chat too: a failure raised while reading (a send that
            // did not go, a room event the server refused) is news the reader should get where
            // they are, not after they back out.
            ErrorBanner(message = state.failure, onDismiss = model::dismissFailure)
            if (open != null) {
                ChatPane(
                    state = state,
                    open = open,
                    model = model,
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
    // showing — the person presses Save where they are, not in a settings panel they have yet to
    // find. A dialog rather than a sheet because it interrupts: there is no conversation to
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
 *
 * Not private: the chat-list shell routes its panels through the same router, so a panel is wired
 * once and reached the same way from either shell.
 */
@Composable
internal fun SectionScreen(state: AppState.SignedIn, model: AppViewModel, modifier: Modifier = Modifier) {
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

        AppState.Section.BOTS -> BotsScreen(
            state = state,
            onRegister = model::registerBot,
            onPause = model::setBotPaused,
            onRotate = model::rotateBot,
            onConfirmRotate = model::confirmBotRotate,
            onToggleEditor = model::toggleBotEditor,
            onSaveScopes = model::setBotScopes,
            onDismissReveal = model::dismissBotReveal,
            onRefresh = model::loadBots,
            modifier = modifier,
        )

        AppState.Section.CALLS -> CallsScreen(
            state = state,
            onRefresh = model::loadCalls,
            onLoadOlder = model::loadOlderCalls,
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
            onBuyKickPoints = model::buyKickPoints,
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
                onNavigationMode = model::setNavigationMode,
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
