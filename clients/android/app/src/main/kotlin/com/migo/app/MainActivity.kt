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
import com.migo.app.ui.ChatScreen
import com.migo.app.ui.ConversationsScreen
import com.migo.app.ui.MigoTheme
import com.migo.app.ui.SignInScreen

/**
 * The only activity.
 *
 * One activity and three composables rather than a navigation graph. Which screen is showing is
 * already decided by [AppState] -- signed out, signed in, or signed in with a conversation open -- and
 * a nav graph would be a second answer to that question, able to disagree with the first. The back
 * gesture is handled where it means something, which is the open chat.
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
                onServerUrl = model::setServerUrl,
                onIdentifier = model::setIdentifier,
                onSubmit = model::signIn,
                onDismissFailure = model::dismissFailure,
            )

            is AppState.SignedIn -> {
                val open = current.open
                if (open == null) {
                    ConversationsScreen(
                        state = current,
                        onOpen = model::open,
                        onRefresh = model::refreshConversations,
                        onStartDirect = model::startDirect,
                        onSignOut = model::signOut,
                        onDismissFailure = model::dismissFailure,
                    )
                } else {
                    BackHandler(onBack = model::closeChat)
                    ChatScreen(
                        chat = open,
                        onBack = model::closeChat,
                        onDraft = model::setDraft,
                        onSend = model::send,
                    )
                }
            }
        }
    }
}
