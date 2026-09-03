package com.migo.app.ui

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.migo.app.model.ActivityCategory
import com.migo.app.model.AppState

/**
 * The Feed tab: the activity stream.
 *
 * The stream is the account's own activity — the notification inbox (durable, server-ordered) and
 * the wallet's statement (gifts, stakes, payouts) merged newest first, with a category filter over
 * the merged rows. Live pushes re-read the stream through the view model; the rows themselves are
 * always the durable record, never a push.
 */
@Composable
fun SpaceScreen(
    state: AppState.SignedIn,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var filter by rememberSaveable { mutableStateOf("all") }

    Column(modifier = modifier.fillMaxSize()) {
        ScreenTitle(title = "Feed") {
            TextButton(onClick = onRefresh, enabled = !state.space.loading) { Text("Refresh") }
        }

        // The filter row scrolls rather than wrapping: five chips do not fit a narrow phone in one
        // fixed row, and a wrapped second row would shift the whole stream down mid-read.
        Row(
            modifier = Modifier.fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            for ((id, label) in listOf(
                "all" to "All",
                "social" to "Social",
                "rooms" to "Rooms",
                "games" to "Games",
                "economy" to "Economy",
            )) {
                FilterChip(
                    selected = filter == id,
                    onClick = { filter = id },
                    label = { Text(label) },
                )
            }
        }

        val rows = state.space.rows.filter { row ->
            filter == "all" || row.category.name.lowercase() == filter
        }

        if (state.space.loading && rows.isEmpty()) {
            // The first read: nothing to show but the honest waiting.
            Placeholder(text = "Loading the stream…", modifier = Modifier.weight(1f))
        } else if (rows.isEmpty()) {
            Placeholder(
                text = if (filter == "all") "No activity yet." else "No ${filter} activity yet.",
                modifier = Modifier.weight(1f),
            )
        } else {
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(rows, key = { it.key }) { row ->
                    ActivityLine(title = row.title, at = row.at)
                }
            }
        }
    }
}

/** The category of an activity row, as the filter's own word. */
fun ActivityCategory.label(): String = when (this) {
    ActivityCategory.SOCIAL -> "Social"
    ActivityCategory.ROOMS -> "Rooms"
    ActivityCategory.GAMES -> "Games"
    ActivityCategory.ECONOMY -> "Economy"
}
