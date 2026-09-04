package com.migo.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.core.net.AdminView

/**
 * The Admins section: the Owner/CEO's management page over the global admins.
 *
 * The surface is closed by construction, twice. The banner menu only offers the section after
 * the sign-in standing check said the viewer is the owner, and the server refuses every read
 * and write here for anybody else — so a stale client that kept the section open after the
 * owner designation moved draws the refusal, not a silent blank. `closed` is a fact, not a
 * failure: the honest answer for an account that holds neither role, drawn as a sentence the
 * same way the web client draws it.
 *
 * A revoke is a two-step action, always: the row's first click turns its button into a confirm
 * that names the account, and only the second click acts. An accidental click on a destructive
 * action must never move moderation away from a person in one step, and the armed confirm holds
 * the row's id rather than its position — the list can refresh between the two clicks, and a
 * confirm keyed to a position would act on whatever moved into it.
 */
@Composable
fun AdminsScreen(
    state: AppState.SignedIn,
    onGrant: (username: String) -> Unit,
    onRevoke: (accountId: String) -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val admins = state.admins
    var draft by rememberSaveable { mutableStateOf("") }
    var asked by rememberSaveable { mutableStateOf<String?>(null) }

    // The asking state dies with its notice: once a revoke lands (or is refused), the row's
    // button goes back to "Revoke", because the armed confirm was an answer to a question that
    // has been answered.
    LaunchedEffect(admins.notice, admins.failure) {
        if (admins.notice != null || admins.failure != null) asked = null
    }

    Column(modifier = modifier.fillMaxSize()) {
        ScreenTitle(title = "Admins") {
            TextButton(onClick = onRefresh, enabled = !admins.loading) { Text("Refresh") }
        }

        if (admins.failure != null) {
            NoticeText(
                text = admins.failure,
                color = MaterialTheme.colorScheme.error,
            )
        }
        if (admins.notice != null) {
            NoticeText(
                text = admins.notice,
                color = MaterialTheme.colorScheme.secondary,
            )
        }

        when {
            admins.loading -> LoadingRow()
            admins.closed -> Placeholder(
                text = "This page belongs to the Migo Owner/CEO. Your account cannot open it.",
                modifier = Modifier.weight(1f),
            )
            admins.admins == null -> Placeholder(
                text = "Not checked yet.",
                modifier = Modifier.weight(1f),
            )
            else -> {
                // --- the grant form -----------------------------------------------------------
                SectionLabel(text = "Appoint")
                Text(
                    text = "Global admins moderate every public room. Appointing and revoking " +
                        "is the Owner/CEO's alone.",
                    style = MaterialTheme.typography.labelSmall,
                    color = LocalMigoExtra.current.faint,
                    modifier = Modifier.padding(horizontal = 16.dp),
                )
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    OutlinedTextField(
                        value = draft,
                        onValueChange = { draft = it },
                        placeholder = { Text("username to appoint") },
                        singleLine = true,
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Button(
                        onClick = {
                            onGrant(draft)
                            draft = ""
                        },
                        enabled = draft.trim().isNotEmpty() && !admins.busy,
                    ) { Text("Appoint") }
                }

                // --- the list -----------------------------------------------------------------
                SectionLabel(text = "Current admins")
                if (admins.admins.isEmpty()) {
                    Placeholder(
                        text = "No global admins yet.",
                        modifier = Modifier.weight(1f),
                    )
                } else {
                    LazyColumn(modifier = Modifier.fillMaxSize()) {
                        items(admins.admins, key = { it.accountId }) { admin ->
                            AdminRowView(
                                admin = admin,
                                asking = asked == admin.accountId,
                                busy = admin.accountId in admins.revoking,
                                onAsk = { asked = admin.accountId },
                                onConfirm = {
                                    asked = null
                                    onRevoke(admin.accountId)
                                },
                            )
                            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                        }
                    }
                }
            }
        }
    }
}

/**
 * One appointed admin, as the owner's list renders it.
 *
 * Presentational over plain data, the same contract every extracted row in this app follows:
 * the rules (the confirm that names the account, the busy gate) are visible here, testable
 * without a live client.
 */
@Composable
fun AdminRowView(
    admin: AdminView,
    asking: Boolean,
    busy: Boolean,
    onAsk: () -> Unit,
    onConfirm: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = admin.username,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
            Text(
                text = "global admin · appointed ${relativeTime(admin.grantedAtMs)}",
                style = MaterialTheme.typography.labelSmall,
                color = LocalMigoExtra.current.faint,
            )
        }
        TextButton(
            onClick = if (asking) onConfirm else onAsk,
            enabled = !busy,
            contentPadding = PaddingValues(horizontal = 12.dp),
        ) {
            Text(
                text = when {
                    busy -> "…"
                    asking -> "Confirm — ${admin.username}"
                    else -> "Revoke"
                },
                color = if (asking) {
                    MaterialTheme.colorScheme.error
                } else {
                    MaterialTheme.colorScheme.primary
                },
                fontWeight = if (asking) FontWeight.Bold else FontWeight.Normal,
                maxLines = 1,
            )
        }
    }
}

/** One sentence the panel owes the person who acted, or the reason the server refused. */
@Composable
private fun NoticeText(text: String?, color: Color) {
    if (text == null) return
    Text(
        text = text,
        style = MaterialTheme.typography.bodySmall,
        color = color,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
    )
}
