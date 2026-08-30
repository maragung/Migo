package com.migo.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState

/**
 * The Profile section: the account, in the owner's own words.
 *
 * The facts are the ones the session already holds — the username, the account id, the server, the
 * connection — plus the sign-out. Nothing here is editable yet (profile editing rides the profile
 * opcodes, which this build reads but does not write), so the screen states what is true rather
 * than offering controls that cannot work.
 */
@Composable
fun ProfileScreen(
    state: AppState.SignedIn,
    onSignOut: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var showId by rememberSaveable { mutableStateOf(false) }

    Column(modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        ScreenTitle(title = "Profile")

        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
        ) {
            Monogram(name = state.username, size = 56.dp)
            Spacer(modifier = Modifier.width(16.dp))
            Column {
                Text(
                    text = state.username,
                    style = MaterialTheme.typography.titleLarge,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                ConnectionBadge(state = state.connection)
                Spacer(modifier = Modifier.padding(2.dp))
                TextButton(onClick = { showId = !showId }) {
                    Text(if (showId) "Hide account id" else "Show account id")
                }
                if (showId) {
                    Text(
                        text = state.accountId.value,
                        style = MaterialTheme.typography.labelSmall,
                        color = MigoExtra.current.faint,
                    )
                }
            }
        }

        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

        SectionLabel(text = "About")
        Text(
            text = "Migo — compact, social, realtime. One design system across every screen size.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )

        SectionLabel(text = "Account")
        TextButton(onClick = onSignOut, modifier = Modifier.padding(horizontal = 8.dp)) {
            Text("Sign out", color = MaterialTheme.colorScheme.error)
        }
        Spacer(modifier = Modifier.padding(8.dp))
    }
}
