package com.migo.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.core.domain.CallDirection
import com.migo.core.domain.CallKind
import com.migo.core.domain.CallOutcome
import com.migo.core.protocol.CallHistoryEntry

/**
 * The Calls section: what this account's calls came to.
 *
 * The one call surface that reads the past, and the row it draws is the server's own: whether a
 * call was answered is the answer the node recorded when the callee picked up, never a claim either
 * party made afterwards, so a call this account missed reads as missed here no matter what was said
 * on the wire. Everything this screen shows is that row, rendered — it computes no fact of its own.
 *
 * The name comes from the conversation the row belongs to rather than from a profile read, because
 * the shell has already resolved that title (a direct conversation borrows its peer's name when the
 * row is built) and a lookup per row at draw time is a lookup on every scroll frame.
 */
@Composable
fun CallsScreen(
    state: AppState.SignedIn,
    onRefresh: () -> Unit,
    onLoadOlder: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        ScreenTitle(title = "Calls") {
            TextButton(onClick = onRefresh, enabled = !state.calls.loading) { Text("Refresh") }
        }

        if (state.calls.rows.isEmpty()) {
            // Two different sentences, and the difference matters: nothing read yet is not the
            // same fact as nothing there.
            Placeholder(
                text = if (!state.calls.loaded || state.calls.loading) "Loading…" else "No calls yet.",
                modifier = Modifier.weight(1f),
            )
        } else {
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(state.calls.rows, key = { it.callId.value }) { row ->
                    CallHistoryLine(
                        row = row,
                        // The conversation's own title when the shell knows it, which for a direct
                        // call is the peer's name and for a group call is the group's.
                        name = state.conversations
                            .firstOrNull { it.conversationId == row.conversationId }
                            ?.title,
                    )
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                }
                if (!state.calls.complete) {
                    item {
                        TextButton(onClick = onLoadOlder, enabled = !state.calls.loading) {
                            Text(if (state.calls.loading) "Loading…" else "Load older calls")
                        }
                    }
                }
            }
        }
    }
}

/** One history row: the direction, who, how it ended, and — when it connected — how long it ran. */
@Composable
private fun CallHistoryLine(row: CallHistoryEntry, name: String?) {
    // Narrowed once each at the boundary, the app's own rule for the wire's bare numbers: what is
    // drawn below is decided from the enums, not from integers that happen to line up.
    val direction = CallDirection.fromWire(row.direction)
    val outcome = CallOutcome.fromWire(row.outcome)
    val outgoing = direction == CallDirection.Outgoing
    val group = CallKind.fromWire(row.kind) == CallKind.Group
    val answered = outcome == CallOutcome.Answered

    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = if (outgoing) "↗" else "↙",
            style = MaterialTheme.typography.bodyMedium,
            color = LocalMigoExtra.current.faint,
        )
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            // The name when the shell knows it, and otherwise what the row can say on its own. A
            // group row is never "Unknown": it is a call with a roster, whether or not this
            // session ever saw the conversation it happened in.
            ListRowName(
                text = name?.takeIf { it.isNotBlank() }
                    ?: if (group) "Group call" else "Unknown",
            )
            ListRowLine(text = callSentence(row, outcome, outgoing, group, answered))
        }
        Spacer(modifier = Modifier.width(8.dp))
        Text(
            text = relativeTime(row.endedAt),
            style = MaterialTheme.typography.labelSmall,
            color = LocalMigoExtra.current.faint,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/**
 * What the row reads as, from facts the row carries.
 *
 * Direction is what makes it honest: an outgoing call that rang out was not missed by this account,
 * it went unanswered by the other party, and one word for both would be telling the user they
 * missed a call they placed.
 */
private fun callSentence(
    row: CallHistoryEntry,
    outcome: CallOutcome?,
    outgoing: Boolean,
    group: Boolean,
    answered: Boolean,
): String {
    val how = when (outcome) {
        CallOutcome.Answered -> if (outgoing) "Outgoing" else "Incoming"
        CallOutcome.Missed -> if (outgoing) "No answer" else "Missed"
        CallOutcome.Declined -> "Declined"
        CallOutcome.Busy -> if (outgoing) "Busy" else "Missed on another call"
        CallOutcome.Cancelled -> "Cancelled"
        CallOutcome.Failed -> "Failed"
        // A number this build does not know is a server ahead of it; the row is still a call, so
        // it is drawn as one rather than dropped.
        null -> "Call"
    }
    // A duration only when the call connected: an unanswered ring has no length to report, and a
    // zero would read as a call that lasted no time rather than one that never happened.
    val lasted = if (answered) row.answeredAt?.let { formatCallDuration(row.endedAt - it) } else null
    val seats = if (group) row.participantCount?.let { "$it people" } else null
    return listOfNotNull(how, lasted, seats).joinToString(" · ")
}

/** A call's length as a person reads it: seconds under a minute, then minutes, then hours. */
private fun formatCallDuration(ms: Long): String {
    val seconds = (ms.coerceAtLeast(0L)) / 1000
    if (seconds < 60) return "${seconds}s"
    val minutes = seconds / 60
    if (minutes < 60) return "${minutes}m ${seconds % 60}s"
    return "${minutes / 60}h ${minutes % 60}m"
}
