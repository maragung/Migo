package com.migo.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.migo.app.model.AppState
import com.migo.app.ui.AlertsScreen
import com.migo.app.ui.ChatScreen
import com.migo.app.ui.ConversationsScreen
import com.migo.app.ui.FriendsScreen
import com.migo.app.ui.HomeScreen
import com.migo.app.ui.MigoTheme
import com.migo.app.ui.ProfileScreen
import com.migo.app.ui.RoomsScreen
import com.migo.app.ui.SearchScreen
import com.migo.app.ui.SignInScreen
import com.migo.app.ui.SpaceScreen
import com.migo.app.ui.WalletScreen

/**
 * The only activity.
 *
 * One activity and a handful of composables rather than a navigation graph. Which screen is showing
 * is already decided by [AppState] -- signed out, signed in, or signed in with a conversation open --
 * and a nav graph would be a second answer to that question, able to disagree with the first. The
 * back gesture is handled where it means something, which is the open chat and the More sheet.
 *
 * The signed-in screen is the shell the design system draws everywhere: a compact header, the
 * section's content, and a five-slot bottom bar -- Home, Chats, Rooms, Space, More -- with More
 * opening the sections the bar cannot carry. Five is the ceiling: a bottom bar that scrolls is a
 * bottom bar that hides. While a chat is open the bar folds away, because a thread plus its composer
 * is the whole screen a phone has.
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

            is AppState.SignedIn -> {
                val open = current.open
                if (open == null) {
                    ShellScreen(state = current, model = model)
                } else {
                    BackHandler(onBack = model::closeChat)
                    ChatScreen(
                        chat = open,
                        onBack = model::closeChat,
                        onDraft = model::setDraft,
                        onSend = model::send,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    }
}

/**
 * The signed-in shell: the section switcher's five slots and the section's screen.
 *
 * More opens a simple sheet of the remaining sections rather than a nested screen: the information
 * architecture is one list, and this is the mobile composition of it.
 */
@Composable
private fun ShellScreen(state: AppState.SignedIn, model: AppViewModel) {
    var moreOpen by rememberSaveable { mutableStateOf(false) }

    Column(modifier = Modifier.fillMaxSize()) {
        when (state.section) {
            AppState.Section.HOME -> HomeScreen(
                state = state,
                onOpenConversation = { model.open(it.conversationId, it.title) },
                onOpenSection = model::selectSection,
                onJoinRoom = model::joinRoom,
                onStartDirect = model::startDirectWith,
                modifier = Modifier.weight(1f),
            )

            AppState.Section.CHATS -> ConversationsScreen(
                state = state,
                onOpen = model::open,
                onRefresh = model::refreshConversations,
                onStartDirect = model::startDirect,
                onSignOut = model::signOut,
                onDismissFailure = model::dismissFailure,
                modifier = Modifier.weight(1f),
            )

            AppState.Section.ROOMS -> RoomsScreen(
                state = state,
                onQuery = model::setRoomsQuery,
                onJoin = model::joinRoom,
                onRefresh = model::loadRooms,
                modifier = Modifier.weight(1f),
            )

            AppState.Section.SPACE -> SpaceScreen(
                state = state,
                onRefresh = model::loadSpace,
                modifier = Modifier.weight(1f),
            )

            AppState.Section.FRIENDS -> FriendsScreen(
                state = state,
                onQuery = model::setSearchQuery,
                onRequest = model::friendRequest,
                onRespond = model::friendRespond,
                onStartDirect = model::startDirectWith,
                onRefresh = model::loadFriends,
                modifier = Modifier.weight(1f),
            )

            AppState.Section.ALERTS -> AlertsScreen(
                state = state,
                onMarkAllRead = model::markAllRead,
                onRefresh = model::loadAlerts,
                modifier = Modifier.weight(1f),
            )

            AppState.Section.SEARCH -> SearchScreen(
                state = state,
                onQuery = model::setSearchQuery,
                onStartDirect = model::startDirectWith,
                onJoinRoom = model::joinRoom,
                onOpenConversation = { model.open(it.conversationId, it.title) },
                modifier = Modifier.weight(1f),
            )

            AppState.Section.WALLET -> WalletScreen(
                state = state,
                onSendGift = model::sendGift,
                onRefresh = model::loadWallet,
                modifier = Modifier.weight(1f),
            )

            AppState.Section.PROFILE -> ProfileScreen(
                state = state,
                onSignOut = model::signOut,
                modifier = Modifier.weight(1f),
            )
        }

        if (moreOpen) {
            MoreSheet(
                state = state,
                onDismiss = { moreOpen = false },
                onSelect = { section ->
                    moreOpen = false
                    model.selectSection(section)
                },
            )
        }

        // The five-slot bottom bar. With a thread open the shell is not composed at all, so the bar
        // folds away with it.
        BottomBar(
            state = state,
            moreOpen = moreOpen,
            onMore = { moreOpen = !moreOpen },
            onSelect = model::selectSection,
        )
    }
}

/**
 * The bar itself: five labelled slots in thumb reach.
 *
 * A hand-built bar rather than Material's `NavigationBar` for the same reason the conversation list
 * builds its own header: the slots are labels (the design system's mobile bar carries icon + label,
 * and this build draws no icon font), the active mark is the accent colour rather than a pill, and
 * the row clears the gesture inset with `navigationBarsPadding` rather than an opt-in API.
 */
@Composable
private fun BottomBar(
    state: AppState.SignedIn,
    moreOpen: Boolean,
    onMore: () -> Unit,
    onSelect: (AppState.Section) -> Unit,
) {
    Column(modifier = Modifier.navigationBarsPadding()) {
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        Surface(color = MaterialTheme.colorScheme.surface) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceEvenly,
            ) {
                BarSlot(
                    label = "Home",
                    active = state.section == AppState.Section.HOME,
                    onClick = { onSelect(AppState.Section.HOME) },
                )
                BarSlot(
                    label = "Chats",
                    active = state.section == AppState.Section.CHATS,
                    onClick = { onSelect(AppState.Section.CHATS) },
                )
                BarSlot(
                    label = "Rooms",
                    active = state.section == AppState.Section.ROOMS,
                    onClick = { onSelect(AppState.Section.ROOMS) },
                )
                BarSlot(
                    label = "Space",
                    active = state.section == AppState.Section.SPACE,
                    onClick = { onSelect(AppState.Section.SPACE) },
                )
                BarSlot(
                    label = "More",
                    active = moreOpen,
                    onClick = onMore,
                )
            }
        }
    }
}

/** One slot: a labelled, thumb-sized door, stated in the accent colour when it is the current one. */
@Composable
private fun BarSlot(label: String, active: Boolean, onClick: () -> Unit) {
    Box(
        modifier = Modifier
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            fontWeight = if (active) FontWeight.Bold else FontWeight.Medium,
            color = if (active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/**
 * The More surface: the sections the five-slot bar cannot carry, as a plain list of doors.
 *
 * Icons would be decoration here -- the labels are the information -- so the rows are text, stated
 * once, in information-architecture order.
 */
@Composable
private fun MoreSheet(
    state: AppState.SignedIn,
    onDismiss: () -> Unit,
    onSelect: (AppState.Section) -> Unit,
) {
    BackHandler(onBack = onDismiss)
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shadowElevation = 8.dp,
    ) {
        Column(modifier = Modifier.fillMaxWidth().navigationBarsPadding()) {
            for ((section, label) in listOf(
                AppState.Section.FRIENDS to "Friends",
                AppState.Section.ALERTS to "Alerts",
                AppState.Section.SEARCH to "Search",
                AppState.Section.WALLET to "Wallet",
                AppState.Section.PROFILE to "Profile",
            )) {
                TextButton(
                    onClick = { onSelect(section) },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(
                        text = label,
                        style = MaterialTheme.typography.bodyLarge,
                        color = if (state.section == section) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.onSurface
                        },
                    )
                }
            }
        }
    }
}
