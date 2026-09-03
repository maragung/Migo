package com.migo.app.ui

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.migo.app.model.AppState
import com.migo.app.model.BackupState
import com.migo.app.model.DevicesState
import com.migo.core.net.DeviceSummary

/**
 * The Profile section: the account, in the owner's own words.
 *
 * The facts are the ones the session already holds — the username, the account id, the server, the
 * connection — plus the account's devices, the `.migo` backup, and the sign-out. Profile facts are
 * not editable yet (profile editing rides the profile opcodes, which this build reads but does not
 * write), so the screen states what is true rather than offering controls that cannot work. The
 * device list is the exception: it is the account-root security view (§16-§18), and removing a
 * device is a control that works — which is exactly why it asks for confirmation first. The backup
 * is the other exception: sealing the account into a container the person can carry to another
 * device is a control that works, and its recovery credential lives in the form that uses it, never
 * on the state object.
 */
@Composable
fun ProfileScreen(
    state: AppState.SignedIn,
    onSignOut: () -> Unit,
    onRefreshDevices: () -> Unit,
    onRemoveDevice: (String) -> Unit,
    onExport: (container: Uri, credential: String) -> Unit,
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

        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

        SectionLabel(text = "Backup")
        BackupSection(
            backup = state.backup,
            accountName = state.username,
            onExport = onExport,
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
 * The account's devices: one row per device, the current one marked. (A revoked
 * device no longer appears — the server's list only holds devices that may still
 * authenticate — so a row is always live.)
 *
 * A null list is the honest "not checked yet" state — a panel that showed an empty list before
 * the read landed would be saying "you have one device", which is the most reassuring answer a
 * security screen can wrongly give.
 */
@Composable
private fun DevicesSection(
    devicesState: DevicesState,
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

/**
 * Seals the account into a `.migo` container: one credential field, one file the picker names.
 *
 * The credential is for the backup, not the account — a container sealed under the passphrase is a
 * backup one passphrase breach opens — and it lives in this form's `remember`, never on the state
 * object, for exactly as long as the form is on screen. The honest limit is stated where the
 * person presses: a device that does not hold the root cannot seal one, and the view model's
 * answer says so rather than offering a control that cannot work.
 */
@Composable
private fun BackupSection(
    backup: BackupState,
    accountName: String,
    onExport: (container: Uri, credential: String) -> Unit,
) {
    // A `remember` rather than `rememberSaveable`: the recovery credential follows the passphrase
    // rule and stays out of the saved-state bundle.
    var credential by remember { mutableStateOf("") }
    val pickDestination = rememberLauncherForActivityResult(
        ActivityResultContracts.CreateDocument("application/octet-stream"),
    ) { chosen ->
        if (chosen != null) onExport(chosen, credential)
    }

    Text(
        text = "A backup is a .migo file carrying the account root, sealed under a recovery " +
            "credential of its own. Restoring it on another device joins the account from there.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
    )

    OutlinedTextField(
        value = credential,
        onValueChange = { credential = it },
        label = { Text("Recovery credential") },
        singleLine = true,
        enabled = !backup.sealing,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    )
    Button(
        onClick = { pickDestination.launch("$accountName.migo") },
        enabled = !backup.sealing && credential.isNotEmpty(),
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        if (backup.sealing) {
            CircularProgressIndicator(
                modifier = Modifier.width(16.dp),
                strokeWidth = 2.dp,
            )
            Spacer(modifier = Modifier.width(8.dp))
        }
        Text(if (backup.sealing) "Sealing…" else "Seal a backup")
    }

    if (backup.failure != null) {
        Text(
            text = backup.failure,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            textAlign = TextAlign.Start,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
    }
    if (backup.notice != null) {
        Text(
            text = backup.notice,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.secondary,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
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
