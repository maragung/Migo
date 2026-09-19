package com.migo.app

import android.content.Intent
import android.os.Bundle
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import com.migo.app.model.AppState
import com.migo.app.ui.CallOverlay
import com.migo.app.ui.GroupCallOverlay
import com.migo.app.ui.LocalCallEglContext
import com.migo.app.ui.LocalGroupEglContext
import com.migo.app.ui.MigoTheme
import com.migo.core.store.ThemeChoice
import com.migo.core.wire.Id
import com.migo.core.wire.IdParseResult
import com.migo.core.wire.tryParseId
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.firstOrNull

/**
 * The chat screen chat-list mode stacks: a conversation, as its own activity.
 *
 * The windowing shell shows a conversation full-bleed beneath the strip because the strip is how
 * that shell navigates. Chat-list mode navigates with a bottom bar instead, so the conversation
 * stands alone here and back returns to the list — but it is the *same* conversation state, read
 * from the same process-held [AppViewModel] the main activity reads: one session, one socket, one
 * set of listeners, one unread count. Opening this activity mints the same window the strip's tab
 * would, through the same [AppViewModel.open]; closing it runs the same [AppViewModel.closeWindow].
 * Nothing about the chat is duplicated, only shown from a second surface.
 *
 * The lifecycle is the whole point of this class, so it is worth stating plainly:
 *
 * - **Creation and recreation.** The open is asked for once per composition, guarded by the open
 *   state it finds, so a rotation or a process restore re-composes the screen without re-fetching
 *   history the session already holds. The intent carries the conversation id and title, and the
 *   id is parsed, not trusted — a malformed extra finishes rather than crashes.
 * - **Going away.** Back closes the window and finishes, in that order, so the list the activity
 *   returns to never shows a chat the model still holds open. A window closed some other way — a
 *   leave, a sign-out, the session ending — finishes the activity too: this screen shows the open
 *   chat or nothing.
 * - **Background and foreground.** The recording interruption pair is observed here exactly as the
 *   main activity observes it; both observers feed the same idempotent model calls, so a
 *   transition that both see pauses and resumes a note once, not twice.
 */
class ChatActivity : CallHostActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        // The same edge-to-edge posture the main activity sets, so the chat draws at the same
        // insets in either shell and the composer's own padding is the only bottom handling.
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        val conversationId = conversationIdFrom(intent)
        val title = intent.getStringExtra(EXTRA_TITLE) ?: "Conversation"
        if (conversationId == null) {
            // An extra this app did not write, or a restore of an intent whose id no longer
            // parses: there is no conversation to show, and finishing is the honest answer.
            finish()
            return
        }
        val model = (application as MigoApplication).appViewModel
        setContent {
            ChatActivityScreen(conversationId = conversationId, title = title, model = model)
        }
    }

    companion object {
        /** The conversation to open, as the id's canonical text form. */
        const val EXTRA_CONVERSATION_ID = "conversation_id"

        /** The title the list row showed, for the open that mints the window. */
        const val EXTRA_TITLE = "title"
    }
}

/** Reads the conversation id extra, or null when it is absent or not a well-formed id. */
private fun conversationIdFrom(intent: Intent): Id? {
    val text = intent.getStringExtra(ChatActivity.EXTRA_CONVERSATION_ID) ?: return null
    return when (val parsed = tryParseId(text)) {
        is IdParseResult.Ok -> parsed.id
        is IdParseResult.Fail -> null
    }
}

/**
 * The chat activity's composition root: theme, the open-once effect, back, and the shared chat
 * surface — the same [ChatPane] the windowing shell composes, over the same model.
 */
@Composable
private fun ChatActivityScreen(conversationId: Id, title: String, model: AppViewModel) {
    val state by model.state.collectAsState()
    // The theme preference is collected here for the same reason the main activity's root
    // collects it: the choice is a device fact the model holds, and the chat should not wear one
    // theme here and another there.
    val preferences by model.preferences.collectAsState()
    val dark = when (preferences.theme) {
        ThemeChoice.System -> isSystemInDarkTheme()
        ThemeChoice.Light -> false
        ThemeChoice.Dark -> true
    }
    val activity = LocalContext.current as? ChatActivity

    // Back returns to the list, and it is registered before the chat is composed so the chat's
    // own deeper handlers — the members sheet's way back — win while their sheets are up, the
    // same order the windowing shell registers in.
    BackHandler {
        val openNow = (model.state.value as? AppState.SignedIn)?.open
        if (openNow != null) model.closeWindow(openNow.conversationId)
        activity?.finish()
    }

    // The interruption rule a recording keeps: the app going to the background — a lock, a
    // switch, a pocket — pauses the note, and coming back resumes it. The main activity observes
    // the same pair for its own lifecycle; both feed idempotent model calls, so a transition one
    // of them misses the other still keeps.
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

    // The one open this activity owes its intent. It waits for a signed-in state rather than
    // assuming one (a restore can compose before the session has resumed), and it asks only when
    // the open chat is not already this conversation — a recreation finds the window where the
    // model left it and re-fetches nothing.
    LaunchedEffect(conversationId, title) {
        model.state.filterIsInstance<AppState.SignedIn>().firstOrNull()?.let { signed ->
            if (signed.open?.conversationId != conversationId) model.open(conversationId, title)
        }
    }

    MigoTheme(dark = dark) {
        Surface(
            modifier = Modifier.fillMaxSize(),
            color = MaterialTheme.colorScheme.background,
        ) {
            Column(modifier = Modifier.fillMaxSize().statusBarsPadding()) {
                when (val current = state) {
                    is AppState.SignedIn -> {
                        val open = current.open
                        // The window this screen shows going away — closed from the model's own
                        // acts (a leave, a sign-out) rather than from back — is this screen's
                        // cue to leave with it. `everOpened` holds the difference between "not
                        // open yet" (the open above is still landing) and "open, then gone".
                        var everOpened by remember { mutableStateOf(false) }
                        LaunchedEffect(open?.conversationId) {
                            if (open != null) {
                                everOpened = true
                            } else if (everOpened) {
                                activity?.finish()
                            }
                        }
                        if (open != null) {
                            ChatPane(
                                state = current,
                                open = open,
                                model = model,
                                modifier = Modifier.weight(1f),
                            )
                        } else {
                            Box(
                                modifier = Modifier.fillMaxSize(),
                                contentAlignment = Alignment.Center,
                            ) {
                                CircularProgressIndicator()
                            }
                        }
                    }

                    // Signed out, or still resuming: not this activity's story. The finish is
                    // the airtight half — a session that ended underneath a stacked chat screen
                    // must not leave a dead surface the person can return to.
                    else -> {
                        LaunchedEffect(Unit) { activity?.finish() }
                        Box(
                            modifier = Modifier.fillMaxSize(),
                            contentAlignment = Alignment.Center,
                        ) {
                            CircularProgressIndicator()
                        }
                    }
                }
            }

            // The call overlays render above whatever the chat is showing, the same rule and the
            // same layer the main activity's root keeps: a call takes the screen from anything.
            // The state is the shared model's, so a call that began on the list is answered here
            // and one that began here is answered on the list — whichever surface is in front.
            SessionOverlays(model = model, state = state)
        }
    }
}

/**
 * The session's call overlays: the 1:1 call screen and the group call, above everything, on
 * whichever activity is in front.
 *
 * The state is the model's own, so this layer renders the same call in every composition that
 * holds it — the main activity's root and the chat activity's — and only the visible one is
 * tappable. The peer's name is resolved here at the composition root, the one place that holds
 * both the call state and the model that knows the names. The video tracks are collected here too
 * — the peer's as state, because it lands mid-call, ours read once per composition — and the
 * session's shared video GL context is provided here so every renderer below the overlay shares
 * the one the call manager minted with its factories.
 */
@Composable
internal fun SessionOverlays(model: AppViewModel, state: AppState) {
    val callState by model.callState.collectAsState()
    val groupCallState by model.groupCallState.collectAsState()
    val groupCallLinks by model.groupCallLinks.collectAsState()
    val groupCallLocalVideo by model.groupCallLocalVideo.collectAsState()
    val callPeerId = callState.incoming?.callerId
        ?: callState.call?.let { if (it.isCaller) it.calleeId else it.callerId }
    val remoteVideo by model.remoteVideo.collectAsState()
    val callOutputs by model.callOutputs.collectAsState()
    val chosenOutput by model.callOutput.collectAsState()
    // The activity drawing this overlay, when it is one that can hold a call: it is the host that
    // knows whether the system is currently showing this screen small, and the only thing that can
    // ask for it. Read from the context rather than passed down, because this layer sits above two
    // shells and the host is whichever activity those shells were composed into.
    val host = LocalContext.current as? CallHostActivity
    CompositionLocalProvider(LocalCallEglContext provides model.callEglContext) {
        CallOverlay(
            state = callState,
            peerName = if (callPeerId != null) model.displayName(callPeerId) else "",
            onAccept = model::acceptCall,
            onDecline = model::declineCall,
            onCancel = model::cancelCall,
            onHangUp = model::hangUpCall,
            onToggleMute = model::toggleCallMute,
            onToggleCamera = model::toggleCallCamera,
            onDismiss = model::dismissCallScreen,
            onRateCall = model::rateCall,
            localVideo = model.localVideo,
            remoteVideo = remoteVideo,
            // Null where the window cannot be drawn at all, so the control is absent rather than
            // present and inert.
            onMinimize = if (host != null && host.canPictureInPicture()) {
                { host.enterCallPip() }
            } else {
                null
            },
            inPictureInPicture = host?.inPictureInPicture?.value == true,
            // Null on a device with one camera, for the same reason: the control is absent rather
            // than present and unable to move.
            onSwitchCamera = if (model.callCanSwitchCamera) {
                { model.switchCallCamera() }
            } else {
                null
            },
            outputs = callOutputs,
            chosenOutput = chosenOutput,
            // Null where the phone has nowhere else to play the call -- one route, or a platform
            // with no honest way to route one -- so the menu is absent rather than a list of one.
            onChooseOutput = if (callOutputs.size > 1) {
                { model.chooseCallOutput(it) }
            } else {
                null
            },
            // The share control is handed over only where the call really has a video track to
            // put a screen on, which is the same fact the state's null already carries: a voice
            // call, or a video call whose camera never opened, gets no control rather than one
            // that cannot send a picture anywhere.
            onStartScreenShare = if (callState.screenSharing != null) {
                { model.startCallScreenShare(it) }
            } else {
                null
            },
            onStopScreenShare = if (callState.screenSharing != null) {
                { model.stopCallScreenShare() }
            } else {
                null
            },
            // The two quality controls are always handed down: the ceiling is the ladder's own
            // rungs and the mode reaches a voice call through its audio, so neither of them needs
            // the call to have a picture to be worth offering.
            onChooseQuality = model::setCallQualityCeiling,
            onToggleLowBandwidth = model::setCallLowBandwidth,
        )
    }

    // The group-call overlay renders above the call overlay — the web layout's own order — and
    // renders nothing at all when this device holds no group-call seat, lost one without a note,
    // or failed to join. The roster's names are resolved here at the composition root, the same
    // place the call overlay resolves its peer's, and the "you" mark is the signed-in account's
    // own id.
    // The group call's renderers need *its own* engine's GL context, which is minted with the
    // plane's factories and only exists once the first link is built: provided here, around the
    // overlay, so a tile drawn for that link is initialized against the context its track lives on.
    CompositionLocalProvider(LocalGroupEglContext provides model.groupCallEglContext) {
        GroupCallOverlay(
            state = groupCallState,
            names = model::displayName,
            meId = (state as? AppState.SignedIn)?.accountId,
            links = groupCallLinks,
            localVideo = groupCallLocalVideo,
            onLeave = model::leaveGroupCall,
            onDismiss = model::dismissGroupCall,
            onToggleMute = model::toggleGroupCallMute,
            onToggleCamera = model::toggleGroupCallCamera,
            // Null where the device has one camera, so the control is absent rather than present
            // and unable to move -- the same rule the one-to-one screen's switch button keeps.
            onSwitchCamera = if (model.callCanSwitchCamera) {
                { model.switchGroupCallCamera() }
            } else {
                null
            },
        )
    }
}
