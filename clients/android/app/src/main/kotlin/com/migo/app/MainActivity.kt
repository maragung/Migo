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
import com.migo.app.ui.AdminsScreen
import com.migo.app.ui.AlertsScreen
import com.migo.app.ui.ChatScreen
import com.migo.app.ui.ErrorBanner
import com.migo.app.ui.GamesScreen
import com.migo.app.ui.MigoTheme
import com.migo.app.ui.MobileHome
import com.migo.app.ui.MobileTabStrip
import com.migo.app.ui.PanelBar
import com.migo.app.ui.ProfileScreen
import com.migo.app.ui.SearchScreen
import com.migo.app.ui.SignInScreen
import com.migo.app.ui.WalletScreen
import com.migo.app.ui.panelTitle

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
                    selfId = state.accountId,
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

        AppState.Section.GAMES -> GamesScreen(modifier = modifier)

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
    }
}
