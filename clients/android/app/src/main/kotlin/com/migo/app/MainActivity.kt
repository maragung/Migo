package com.migo.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
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
import androidx.lifecycle.viewmodel.compose.viewModel
import com.migo.app.model.AppState
import com.migo.app.ui.AlertsScreen
import com.migo.app.ui.BannerAction
import com.migo.app.ui.ChatScreen
import com.migo.app.ui.ChatsScreen
import com.migo.app.ui.ErrorBanner
import com.migo.app.ui.FriendsScreen
import com.migo.app.ui.GamesScreen
import com.migo.app.ui.MigoTheme
import com.migo.app.ui.PanelBar
import com.migo.app.ui.ProfileBanner
import com.migo.app.ui.ProfileScreen
import com.migo.app.ui.RoomsScreen
import com.migo.app.ui.SearchScreen
import com.migo.app.ui.SignInScreen
import com.migo.app.ui.SpaceScreen
import com.migo.app.ui.TabStrip
import com.migo.app.ui.WalletScreen
import com.migo.app.ui.panelTitle

/**
 * The only activity.
 *
 * One activity and a handful of composables rather than a navigation graph. Which screen is showing
 * is already decided by [AppState] -- signed out, or signed in -- and a nav graph would be a second
 * answer to that question, able to disagree with the first. The back gesture is handled where it
 * means something, which is the open chat.
 *
 * The signed-in screen is the new-ui-02 left panel: a tab strip (Main, Rooms, Games, Feed) above
 * an orange profile banner whose avatar menu carries the panels and the way out. A
 * conversation and a menu panel both cover this shell rather than joining it -- on a PC they
 * would be the right pane -- and each carries its own way back, so the strip's navigation is
 * never taken away by reading a message.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        // Android 15 draws behind the system bars whether an app asks or not, at this target level.
        // Calling it explicitly makes older versions behave the same, so the insets handled below are
        // handled on every version rather than only the newest.
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        setContent {
            MigoTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    MigoApp()
                }
            }
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
                onDismissFailure = model::dismissFailure,
            )

            is AppState.SignedIn -> ShellScreen(state = current, model = model)
        }
    }
}

/**
 * The signed-in shell: the new-ui-02 model, as a phone wears it.
 *
 * The left panel is the app: the tab strip (Main, Rooms, Games, Feed) above the orange profile
 * banner whose avatar menu carries the panels and the way out. A conversation and a menu
 * panel (Alerts, Search, Wallet, Profile) both COVER the screen rather than joining the strip —
 * on a PC they would be the right pane — and each carries its own way back: the thread's header
 * and back gesture, the panel's "‹ Menu Panel" bar. Covering the shell never disturbs the strip:
 * a panel's back returns to the tab the strip still shows, held in [AppState.SignedIn.stripSection].
 */
@Composable
private fun ShellScreen(state: AppState.SignedIn, model: AppViewModel) {
    val open = state.open
    BackHandler(enabled = open != null, onBack = model::closeChat)
    BackHandler(
        enabled = open == null && state.section.isPanel,
        onBack = { model.selectSection(state.stripSection) },
    )

    when {
        // The banner rides on top of the chat too: a failure raised while reading (a send that did
        // not go, a room event the server refused) is news the reader should get where they are,
        // not after they back out and the strip's copy of the banner finally appears.
        open != null -> Column(modifier = Modifier.fillMaxSize()) {
            ErrorBanner(message = state.failure, onDismiss = model::dismissFailure)
            ChatScreen(
                chat = open,
                onBack = model::closeChat,
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
                selfId = state.accountId,
                modifier = Modifier.weight(1f),
            )
        }

        // A menu panel covers the screen, with the model's own bar as its way back. The banner
        // comes too — a panel that swallows the failure message hides it from the only person
        // who caused it.
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
            TabStrip(
                section = state.section,
                onSelect = model::selectSection,
            )
            ProfileBanner(
                username = state.username,
                connection = state.connection,
                balance = state.wallet.balance,
                onAction = { action ->
                    when (action) {
                        BannerAction.PROFILE -> model.selectSection(AppState.Section.PROFILE)
                        BannerAction.WALLET -> model.selectSection(AppState.Section.WALLET)
                        BannerAction.ALERTS -> model.selectSection(AppState.Section.ALERTS)
                        BannerAction.SEARCH -> model.selectSection(AppState.Section.SEARCH)
                        BannerAction.SIGN_OUT -> model.signOut()
                    }
                },
            )
            ErrorBanner(message = state.failure, onDismiss = model::dismissFailure)
            // The gesture bar draws over the list's last row unless the section content stands
            // above it — only the chat manages its own insets (its composer does), so every
            // other destination pads here, once, at the edge that needs it.
            SectionScreen(
                state = state,
                model = model,
                modifier = Modifier.weight(1f).navigationBarsPadding(),
            )
        }
    }
}

/** The section screens, as the tab strip's five destinations and the banner's panels. */
@Composable
private fun SectionScreen(state: AppState.SignedIn, model: AppViewModel, modifier: Modifier = Modifier) {
    when (state.section) {
        AppState.Section.CHATS -> ChatsScreen(
            state = state,
            onOpenConversation = { model.open(it.conversationId, it.title) },
            modifier = modifier,
        )

        AppState.Section.FRIENDS -> FriendsScreen(
            state = state,
            onQuery = model::setSearchQuery,
            onRequest = model::friendRequest,
            onRespond = model::friendRespond,
            onStartDirect = model::startDirectWith,
            onRefresh = model::loadFriends,
            modifier = modifier,
        )

        AppState.Section.ROOMS -> RoomsScreen(
            state = state,
            onQuery = model::setRoomsQuery,
            onJoin = model::joinRoom,
            onOpenConversation = { model.open(it.conversationId, it.title) },
            onCreate = model::createRoom,
            onRefresh = model::loadRooms,
            modifier = modifier,
        )

        AppState.Section.GAMES -> GamesScreen(modifier = modifier)

        AppState.Section.FEED -> SpaceScreen(
            state = state,
            onRefresh = model::loadSpace,
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
            modifier = modifier,
        )
    }
}
