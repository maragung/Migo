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
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
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
import com.migo.core.net.DeviceSummary

/**
 * The Profile section: the account, in the owner's own words.
 *
 * The facts are the ones the session already holds — the username, the account id, the server, the
 * connection — plus the account's devices and the sign-out. Profile facts are not editable yet
 * (profile editing rides the profile opcodes, which this build reads but does not write), so the
 * screen states what is true rather than offering controls that cannot work. The device list is
 * the exception: it is the account-root security view (§16-§18), and removing a device is a
 * control that works — which is exactly why it asks for confirmation first.
 */
@Composable
fun ProfileScreen(
    state: AppState.SignedIn,
    onSignOut: () -> Unit,
    onRefreshDevices: () -> Unit,
    onRemoveDevice: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    var showId by rememberSaveable { mutableStateOf(false) }
    var confirmRemove by rememberSaveable { mutableStateOf<String?>(null) }

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
                        color = LocalMigoExtra.current.faint,
                    )
                }
            }
        }

        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

        SectionLabel(text = "Devices")
        DevicesSection(
            devicesState = state.devices,
            onRefresh = onRefreshDevices,
            onRemove = { confirmRemove = it },
        )

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

    // The confirmation is the whole point (§70): removing a device signs it out of the account
    // everywhere, so the person names the phone before the server acts.
    val deviceId = confirmRemove
    if (deviceId != null) {
        val named = state.devices.devices
            ?.firstOrNull { it.deviceId == deviceId }
            ?.let { "“${it.displayName}”" }
            ?: "this device"
        AlertDialog(
            onDismissRequest = { confirmRemove = null },
            title = { Text("Remove $named?") },
            text = {
                Text(
                    "Every session on it will be signed out, and it will not be able to sign in " +
                        "with its credential again.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmRemove = null
                    onRemoveDevice(deviceId)
                }) { Text("Remove", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = {
                TextButton(onClick = { confirmRemove = null }) { Text("Cancel") }
            },
        )
    }
}

/**
 * The account's devices: one row per device, the current one marked, revoked ones still listed.
 *
 * A null list is the honest "not checked yet" state — a panel that showed an empty list before
 * the read landed would be saying "you have one device", which is the most reassuring answer a
 * security screen can wrongly give.
 */
@Composable
private fun DevicesSection(
    devicesState: AppState.DevicesState,
    onRefresh: () -> Unit,
    onRemove: (String) -> Unit,
) {
    if (devicesState.failure != null) {
        Text(
            text = devicesState.failure,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
    }
    if (devicesState.notice != null) {
        Text(
            text = devicesState.notice,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.secondary,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
    }

    when (val rows = devicesState.devices) {
        null -> Text(
            text = "Not checked yet.",
            style = MaterialTheme.typography.bodySmall,
            color = LocalMigoExtra.current.faint,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
        else -> {
            if (rows.isEmpty()) {
                Text(
                    text = "No devices are registered.",
                    style = MaterialTheme.typography.bodySmall,
                    color = LocalMigoExtra.current.faint,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }
            for (row in rows) {
                DeviceRowView(
                    row = row,
                    busy = devicesState.removing.contains(row.deviceId),
                    onRemove = onRemove,
                )
            }
        }
    }

    TextButton(
        onClick = onRefresh,
        enabled = !devicesState.loading,
        modifier = Modifier.padding(horizontal = 8.dp),
    ) {
        if (devicesState.loading) {
            CircularProgressIndicator(
                modifier = Modifier.width(16.dp).padding(end = 8.dp),
                strokeWidth = 2.dp,
            )
        }
        Text("Refresh devices")
    }
}

/** One device row: name, platform, the credential mark, and the removal a lost device wants. */
@Composable
private fun DeviceRowView(row: DeviceSummary, busy: Boolean, onRemove: (String) -> Unit) {
    val revoked = row.status == "revoked"
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = row.displayName + when {
                    row.isCurrent -> "  (this device)"
                    revoked -> "  (revoked)"
                    else -> ""
                },
                style = MaterialTheme.typography.bodyMedium,
                color = if (revoked) LocalMigoExtra.current.faint else MaterialTheme.colorScheme.onSurface,
            )
            Text(
                text = buildString {
                    append(row.platform)
                    if (row.hasCredential) append(" · holds a sign-in credential")
                },
                style = MaterialTheme.typography.bodySmall,
                color = LocalMigoExtra.current.faint,
            )
        }
        if (!row.isCurrent && !revoked) {
            TextButton(onClick = { onRemove(row.deviceId) }, enabled = !busy) {
                Text(if (busy) "Removing…" else "Remove")
            }
        }
    }
}
