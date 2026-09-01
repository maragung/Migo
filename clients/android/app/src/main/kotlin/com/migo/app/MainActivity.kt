package com.migo.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
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
import com.migo.app.ui.ConversationsScreen
import com.migo.app.ui.FriendsScreen
import com.migo.app.ui.GamesScreen
import com.migo.app.ui.MigoTheme
import com.migo.app.ui.ProfileBanner
import com.migo.app.ui.ProfileScreen
import com.migo.app.ui.RoomsScreen
import com.migo.app.ui.SearchScreen
import com.migo.app.ui.SignInScreen
import com.migo.app.ui.SpaceScreen
import com.migo.app.ui.TabStrip
import com.migo.app.ui.WalletScreen

/**
 * The only activity.
 *
 * One activity and a handful of composables rather than a navigation graph. Which screen is showing
 * is already decided by [AppState] -- signed out, or signed in -- and a nav graph would be a second
 * answer to that question, able to disagree with the first. The back gesture is handled where it
 * means something, which is the open chat.
 *
 * The signed-in screen is the shell every client now draws: a tab strip (Friends, Chats, Rooms,
 * Games, Feed, plus the conversation being read as its own closable chip) above an orange profile
 * banner whose avatar menu carries the panels and the way out. The strip stays on screen while a
 * thread is open -- closing the thread is what its chip's close mark and the back gesture are for
 * -- so the shell's navigation is never taken away by reading a message.
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
                onDismissFailure = model::dismissFailure,
            )

            is AppState.SignedIn -> ShellScreen(state = current, model = model)
        }
    }
}

/**
 * The signed-in shell: the tab strip, the banner, and whichever tab is active.
 *
 * A chat is a tab like the reference composes it: while one is open it renders in place of the
 * section screens, under the same strip, and everything that leaves it -- a system tab, the chip's
 * close mark, the back gesture -- closes it.
 */
@Composable
private fun ShellScreen(state: AppState.SignedIn, model: AppViewModel) {
    val open = state.open
    BackHandler(enabled = open != null, onBack = model::closeChat)

    Column(modifier = Modifier.fillMaxSize()) {
        TabStrip(
            section = state.section,
            openChatTitle = open?.title,
            unread = state.conversations.sumOf { it.unread },
            onSelect = { section ->
                // A system tab takes over from the thread: choosing a destination is leaving the
                // conversation, exactly as closing its chip is.
                if (open != null) model.closeChat()
                model.selectSection(section)
            },
            onCloseChat = model::closeChat,
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

        if (open != null) {
            ChatScreen(
                chat = open,
                onBack = model::closeChat,
                onDraft = model::setDraft,
                onSend = model::send,
                onLeave = open.roomId?.let { roomId ->
                    { model.leaveRoom(open.conversationId, roomId) }
                },
                modifier = Modifier.weight(1f),
            )
        } else {
            SectionScreen(state = state, model = model, modifier = Modifier.weight(1f))
        }
    }
}

/** The section screens, as the tab strip's five destinations and the banner's panels. */
@Composable
private fun SectionScreen(state: AppState.SignedIn, model: AppViewModel, modifier: Modifier = Modifier) {
    when (state.section) {
        AppState.Section.FRIENDS -> FriendsScreen(
            state = state,
            onQuery = model::setSearchQuery,
            onRequest = model::friendRequest,
            onRespond = model::friendRespond,
            onStartDirect = model::startDirectWith,
            onRefresh = model::loadFriends,
            modifier = modifier,
        )

        AppState.Section.CHATS -> ConversationsScreen(
            state = state,
            onOpen = model::open,
            onRefresh = model::refreshConversations,
            onStartDirect = model::startDirect,
            onSignOut = model::signOut,
            onDismissFailure = model::dismissFailure,
            modifier = modifier,
        )

        AppState.Section.ROOMS -> RoomsScreen(
            state = state,
            onQuery = model::setRoomsQuery,
            onJoin = model::joinRoom,
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
            modifier = modifier,
        )
    }
}
