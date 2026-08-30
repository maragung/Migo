package com.migo.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState

/**
 * The Alerts section: the durable inbox and its read state.
 *
 * The live push stream is droppable by design, so this screen treats it only as a hint to re-read —
 * the rows are the source of truth, and they survive the recipient being offline. A row carries no
 * message content by construction (the server has no plaintext to put there); rendering is kind,
 * actor, and time.
 */
@Composable
fun AlertsScreen(
    state: AppState.SignedIn,
    onMarkAllRead: () -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        ScreenTitle(title = "Alerts") {
            TextButton(
                onClick = onMarkAllRead,
                enabled = !state.alerts.acknowledging && state.alerts.items.isNotEmpty(),
            ) { Text("Mark all read") }
            TextButton(onClick = onRefresh, enabled = !state.alerts.loading) { Text("Refresh") }
        }

        if (state.alerts.items.isEmpty()) {
            Placeholder(
                text = if (state.alerts.loading) "Loading…" else "You are all caught up.",
                modifier = Modifier.weight(1f),
            )
        } else {
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(state.alerts.items, key = { it.id.value }) { item ->
                    ActivityLine(
                        title = (item.title ?: item.kind.replace('_', ' ').replaceFirstChar { it.uppercase() }),
                        at = item.at,
                    )
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                }
                item {
                    Text(
                        text = "Notifications carry no message text: the server never has any to show.",
                        style = MaterialTheme.typography.labelSmall,
                        color = LocalMigoExtra.current.faint,
                        modifier = Modifier.padding(16.dp),
                    )
                }
            }
        }
    }
}
