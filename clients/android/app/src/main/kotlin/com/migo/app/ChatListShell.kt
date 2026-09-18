package com.migo.app

import android.content.Intent
import android.text.format.DateUtils
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.app.model.ConversationRow
import com.migo.app.ui.ErrorBanner
import com.migo.app.ui.ListRowAvatar
import com.migo.app.ui.ListRowLine
import com.migo.app.ui.ListRowName
import com.migo.app.ui.LoadingRow
import com.migo.app.ui.LocalMigoExtra
import com.migo.app.ui.MigoType
import com.migo.app.ui.MobileHome
import com.migo.app.ui.PanelBar
import com.migo.app.ui.Placeholder
import com.migo.app.ui.SaveAccountFileDialog
import com.migo.app.ui.ScreenTitle
import com.migo.app.ui.TabGlyph
import com.migo.app.ui.UnreadPill
import com.migo.app.ui.panelTitle

/**
 * The chat-list shell: the second navigation mode, and the windowing shell's counterpart.
 *
 * The screen itself is the conversation list — every conversation this session knows, with its
 * preview and unread — the way a phone's messenger wears its home, and tapping a row stacks
 * [ChatActivity] for that conversation; back from it returns here. A bottom bar of Main, Friends,
 * Rooms, Feed instead of the top strip carries the list's own way home plus the views the list
 * cannot be, drawn by the same [MobileHome], because the mode changes how a person moves between
 * those views, not what the views are. Main is the bar's first tab and always present: it returns
 * to — or stays on — the list itself, so the screen a person spends their day on is named by the
 * bar rather than reached only through the system's back gesture.
 *
 * What the bar cannot carry is still the model's: a conversation opened from inside Friends or
 * Rooms (a new direct chat, a room's open) or minted by an arriving message opens in place, the
 * same full-bleed chat the windowing shell shows beneath its strip, with back closing it the same
 * way. The session's facts — the conversations, their unread, the one open chat — are identical
 * in both modes, so switching between them mid-session keeps everything where it was.
 */
@Composable
internal fun ChatListShell(state: AppState.SignedIn, model: AppViewModel, modifier: Modifier = Modifier) {
    val open = state.open
    val context = LocalContext.current

    // Back means "close this, not the app", in the same order the windowing shell keeps: the
    // chat showing in place, then a panel, then a home view the bar chose. Registered before the
    // chat below is composed so the chat's own deeper handlers — the members sheet's way back —
    // win while their sheets are up.
    BackHandler(enabled = open != null, onBack = {
        if (open != null) model.closeWindow(open.conversationId)
    })
    BackHandler(
        enabled = open == null && state.section.isPanel,
        onBack = { model.selectSection(state.stripSection) },
    )
    // Back and the bar's Main tab are the same step from any of the bar's other views: home is
    // both the system's own gesture and a tab the bar names, so a person who never learned the
    // gesture still finds the way back. The bar's views are the only sections this covers: the
    // list itself is the resting state back stands on, and a panel's handler above has already
    // taken its turn.
    BackHandler(
        enabled = open == null && !state.section.isPanel && state.section != AppState.Section.CHATS,
        onBack = { model.selectSection(AppState.Section.CHATS) },
    )

    // A session that chose the chat list lands on it. Friends is where a session starts, so that
    // is the one section read as "not yet navigated"; anything else is a place the person (or a
    // restore) put themselves, and is kept. The flag is saved rather than remembered so a
    // rotation does not read as a second landing.
    var landed by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(Unit) {
        if (!landed) {
            landed = true
            if (state.section == AppState.Section.FRIENDS) {
                model.selectSection(AppState.Section.CHATS)
            }
        }
    }

    // The conversation the list stacked [ChatActivity] for, as its id's text form so the guard
    // survives a rotation. While that activity is in front this shell is stopped behind it, and
    // the chat it shows is already composed there — composing it a second time here would be a
    // screen nobody sees doing work nobody asked for, so the in-place chat below stands down for
    // that one conversation. It is cleared when the window closes, so a conversation minted open
    // later (a message arriving for it) still shows in place as it would have.
    var stacked by rememberSaveable { mutableStateOf<String?>(null) }
    LaunchedEffect(open?.conversationId) {
        if (open == null) stacked = null
    }

    when {
        // A menu panel covers the whole shell, exactly as it covers the strip: its own bar is its
        // way back, and the error banner comes too — a panel that swallows the failure message
        // hides it from the only person who caused it.
        state.section.isPanel -> Column(modifier = modifier.fillMaxSize()) {
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

        else -> Column(modifier = modifier.fillMaxSize()) {
            // The banner rides on top of whatever the bar's tabs are showing: a failure raised
            // while reading is news the reader should get where they are.
            ErrorBanner(message = state.failure, onDismiss = model::dismissFailure)
            Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
                when {
                    // A conversation opened from inside the home views, or minted by an arriving
                    // message: the same full-bleed chat the strip's shell shows, closing on back
                    // the same way. The one the list itself opened lives in the activity it
                    // stacked — `stacked` holds it, and this branch stands down for it while
                    // that activity shows it in front.
                    open != null && open.conversationId.value != stacked -> ChatPane(
                        state = state,
                        open = open,
                        model = model,
                        modifier = Modifier.fillMaxSize(),
                    )

                    // The list itself: the screen the shell is, reached by the bar's Main tab,
                    // by back from its other views, and landed on at the session's start.
                    open == null && state.section == AppState.Section.CHATS -> ChatListScreen(
                        state = state,
                        onOpen = { row ->
                            stacked = row.conversationId.value
                            context.startActivity(
                                Intent(context, ChatActivity::class.java)
                                    .putExtra(ChatActivity.EXTRA_CONVERSATION_ID, row.conversationId.value)
                                    .putExtra(ChatActivity.EXTRA_TITLE, row.title),
                            )
                        },
                        modifier = Modifier.fillMaxSize(),
                    )

                    // Friends, Rooms, Feed: the home views themselves, unchanged.
                    open == null -> MobileHome(
                        state = state,
                        model = model,
                        modifier = Modifier.fillMaxSize(),
                    )

                    // The stacked conversation's activity is in front; this shell is stopped
                    // behind it, and shows nothing until back closes the window.
                    else -> Unit
                }
            }
            ChatListBottomNav(
                section = state.section,
                onSelect = model::selectSection,
            )
        }
    }

    // The registration offer is the shell's to show in either mode: the person presses Save
    // where they are, not in a panel they have yet to find.
    if (state.accountFileOffer) {
        SaveAccountFileDialog(
            username = state.username,
            onSave = model::saveAccountFile,
            onDecline = model::declineAccountFile,
        )
    }
}

/**
 * The list itself: every conversation this session knows, newest activity first, each row carrying
 * the preview and the unread the model already keeps. Tapping a row hands the conversation to
 * [ChatActivity] — the list never opens a chat itself, so there is exactly one path from a row to
 * a screen, and back from that screen always means this list.
 */
@Composable
private fun ChatListScreen(
    state: AppState.SignedIn,
    onOpen: (ConversationRow) -> Unit,
    modifier: Modifier = Modifier,
) {
    // Newest activity first, resolved once per list change: a scroll frame is not a sort.
    val sorted = remember(state.conversations) {
        state.conversations.sortedByDescending { it.updatedAt }
    }
    Column(modifier = modifier.fillMaxSize()) {
        ScreenTitle(title = "Chats")
        when {
            state.loading && sorted.isEmpty() -> LoadingRow()
            sorted.isEmpty() -> Placeholder(
                text = "No conversations yet. A room you join, a group you are added to, " +
                    "or a friend you message will land here.",
                modifier = Modifier.weight(1f),
            )
            else -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(sorted, key = { it.conversationId.value }) { row ->
                    ChatListRow(row = row, onOpen = { onOpen(row) })
                    HorizontalDivider(color = MaterialTheme.colorScheme.outline)
                }
                item(key = "tail") { Spacer(modifier = Modifier.height(16.dp)) }
            }
        }
    }
}

/**
 * One conversation row, in the list rows' own shape: the avatar, the name, the last message, the
 * unread pill, and the activity's own words for when it happened.
 */
@Composable
private fun ChatListRow(row: ConversationRow, onOpen: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 66.dp)
            .clickable(onClick = onOpen)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ListRowAvatar(name = row.title)
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            ListRowName(text = row.title)
            ListRowLine(text = row.preview ?: "No messages yet")
        }
        if (row.unread > 0) {
            UnreadPill(count = row.unread)
            Spacer(modifier = Modifier.width(8.dp))
        }
        // The timestamp is the activity's, stated the system's own way; a conversation with no
        // activity yet states none rather than dating the epoch.
        if (row.updatedAt > 0) {
            Text(
                text = DateUtils.getRelativeTimeSpanString(row.updatedAt).toString(),
                fontSize = MigoType.bodySm,
                color = LocalMigoExtra.current.faint,
            )
        }
    }
}

/**
 * The bottom bar: Main, Friends, Rooms, Feed, in the shape a phone's messenger wears its home.
 *
 * The four are equal shares of the width, each an icon over its own name, the one in front the
 * only one at full brightness — the arrangement WhatsApp settled on for a phone's thumb, and the
 * one this bar is asked to keep. What it replaced was a row of pills: a pill is a tab, and a tab
 * is something you close, whereas these four are the phone's home itself. The shape says so
 * before anything is read.
 *
 * Main is the first tab and the list's own: it lands on the conversation list the shell is, so the
 * home the person stands on is one tap from anywhere the bar can take them, and stays put when it
 * is pressed where it already is. The list's unread stays in the rows' own pills rather than a
 * badge on the tab, because the conversations themselves — not the list — are what is unread.
 */
@Composable
private fun ChatListBottomNav(
    section: AppState.Section,
    onSelect: (AppState.Section) -> Unit,
    modifier: Modifier = Modifier,
) {
    val extra = LocalMigoExtra.current
    Column(modifier = modifier.fillMaxWidth()) {
        // The hairline is what separates the bar from the list it sits under on a bright screen;
        // without it the two grounds meet with nothing between them.
        HorizontalDivider(color = Color.Black.copy(alpha = 0.18f))
        Surface(color = extra.nav, modifier = Modifier.fillMaxWidth()) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .navigationBarsPadding()
                    .heightIn(min = 58.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                for (tab in listNavOrder) {
                    ListNavItem(
                        label = tab.first,
                        glyph = tab.second,
                        active = section == tab.second,
                        onClick = { onSelect(tab.second) },
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    }
}

/** The bar's tabs in order, as label to section. Main leads and is the list itself, so the
 *  shell's home is a tab the bar always offers rather than a place only back reaches. */
private val listNavOrder = listOf(
    "Main" to AppState.Section.CHATS,
    "Friends" to AppState.Section.FRIENDS,
    "Rooms" to AppState.Section.ROOMS,
    "Feed" to AppState.Section.FEED,
)

/**
 * One tab of the bottom bar: its glyph over its name, centred in its share of the width.
 *
 * The whole column is the target rather than the glyph alone, because on a phone the label is
 * most of what a thumb aims at. The one in front carries the full white and the extra weight;
 * the rest step back to a fraction of it, which is the same "where am I" the pill used to say
 * without drawing a box around it.
 */
@Composable
private fun ListNavItem(
    label: String,
    glyph: AppState.Section,
    active: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val ink = if (active) Color.White else Color.White.copy(alpha = 0.62f)
    Column(
        modifier = modifier
            .fillMaxHeight()
            .clickable(onClick = onClick)
            .padding(vertical = 7.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        TabGlyph(kind = glyphKind(glyph), tint = ink)
        Spacer(modifier = Modifier.height(3.dp))
        Text(
            text = label,
            fontSize = MigoType.meta,
            fontWeight = if (active) FontWeight.Bold else FontWeight.Medium,
            color = ink,
            maxLines = 1,
        )
    }
}

/** The bar's glyph for a section, the strip's own set. */
private fun glyphKind(section: AppState.Section): TabGlyph = when (section) {
    AppState.Section.CHATS -> TabGlyph.CHATS
    AppState.Section.FRIENDS -> TabGlyph.FRIENDS
    AppState.Section.ROOMS -> TabGlyph.ROOMS
    AppState.Section.FEED -> TabGlyph.FEED
    else -> TabGlyph.CHATS
}
