package com.migo.app.ui

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
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
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.migo.app.model.AccountSecurityState
import com.migo.app.model.AppState
import com.migo.app.model.BackupState
import com.migo.app.model.DevicesState
import com.migo.app.model.ProfileEditState
import com.migo.core.net.DeviceSummary

/**
 * The Profile section: the account, in the owner's own words.
 *
 * The facts are the ones the session already holds — the username, the account id, the server, the
 * connection — plus the profile the person can now edit (display name, bio, custom status, birth
 * year, search visibility, the three privacy choices — the same form the web client's Profile
 * panel carries, absent-means-unchanged on every privacy control), the passphrase change and the
 * recovery contact (the web Account panel's two account-level controls), the account's devices,
 * the `.migo` backup, and the sign-out. The custom status rides the presence wire rather than the
 * profile patch, exactly as on web, so saving a sentence never flips the presence state.
 *
 * The device list is the account-root security view (§16-§18), and removing a device is a control
 * that works — which is exactly why it asks for confirmation first. The backup is the other
 * exception: sealing the account into a container the person can carry to another device is a
 * control that works, and its recovery credential lives in the form that uses it, never on the
 * state object.
 */
@Composable
fun ProfileScreen(
    state: AppState.SignedIn,
    onSignOut: () -> Unit,
    onRefreshDevices: () -> Unit,
    onRemoveDevice: (String) -> Unit,
    onExport: (container: Uri, credential: String) -> Unit,
    onLoadProfile: () -> Unit,
    onSaveProfile: (
        displayName: String,
        bio: String,
        birthYear: String,
        searchable: Boolean?,
        showLastSeen: Long?,
        whoCanMessage: Long?,
        whoCanAdd: Long?,
    ) -> Unit,
    onSaveStatus: (String) -> Unit,
    onChangePassphrase: (current: String, next: String) -> Unit,
    onSaveContact: (contact: String) -> Unit,
    onChangeAvatar: (image: Uri, contentType: String) -> Unit,
    modifier: Modifier = Modifier,
) {
    var showId by rememberSaveable { mutableStateOf(false) }
    var confirmRemove by rememberSaveable { mutableStateOf<String?>(null) }

    // The image picker: one choice, and the avatar's own upload starts from it. The type is
    // image/* because the server's sniffer accepts exactly the picture formats — anything else
    // the picker could hand over is a file this flow would only ever be refused on.
    val context = LocalContext.current
    val pickAvatar = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent(),
    ) { chosen ->
        if (chosen != null) {
            val claimed = context.contentResolver.getType(chosen) ?: "application/octet-stream"
            onChangeAvatar(chosen, claimed)
        }
    }

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

        // The avatar's own control, in the identity block it belongs to: not a form field — it
        // acts the moment it is pressed, uploading the picked image and pointing the profile at
        // it in one action, exactly the contract the web panel's change-photo button holds. The
        // server judges the bytes, not the name, so the picker's only job is to offer pictures.
        TextButton(
            onClick = { pickAvatar.launch("image/*") },
            enabled = !state.profileEdit.busy,
            modifier = Modifier.padding(horizontal = 16.dp),
        ) {
            Text(
                text = if (state.profileEdit.busy) {
                    "Changing photo…"
                } else {
                    "Change photo"
                },
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
            )
        }
        Text(
            text = "A PNG, JPEG, WebP, GIF or AVIF image, up to 2 MiB. The server judges the " +
                "bytes, not the name.",
            style = MaterialTheme.typography.labelSmall,
            color = LocalMigoExtra.current.faint,
            modifier = Modifier.padding(horizontal = 16.dp),
        )

        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

        SectionLabel(text = "Edit profile")
        ProfileEditSection(
            edit = state.profileEdit,
            onLoad = onLoadProfile,
            onSave = onSaveProfile,
            onSaveStatus = onSaveStatus,
        )

        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

        SectionLabel(text = "Account security")
        AccountSecuritySection(
            security = state.accountSecurity,
            onChangePassphrase = onChangePassphrase,
            onSaveContact = onSaveContact,
        )

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
 * The editable half of the profile, primed from the caller's own profile read.
 *
 * The privacy controls are "leave as-is" by default and join the save only once touched: the
 * server never sends current privacy values back (they are private), so a form that assumed a
 * default would overwrite a deliberate choice with a guess. The same rule holds for the search
 * switch and the birth year — an untouched field sends nothing.
 *
 * The custom status is its own field with its own save, because it rides the presence wire: saving
 * it beside the profile patch would be saving it on the wrong opcode.
 */
@Composable
private fun ProfileEditSection(
    edit: ProfileEditState,
    onLoad: () -> Unit,
    onSave: (
        displayName: String,
        bio: String,
        birthYear: String,
        searchable: Boolean?,
        showLastSeen: Long?,
        whoCanMessage: Long?,
        whoCanAdd: Long?,
    ) -> Unit,
    onSaveStatus: (String) -> Unit,
) {
    LaunchedEffect(Unit) { onLoad() }

    if (edit.failure != null) {
        Text(
            text = edit.failure,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
    }
    if (edit.notice != null) {
        Text(
            text = edit.notice,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.secondary,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
    }

    val profile = edit.profile
    if (profile == null) {
        Text(
            text = if (edit.busy) "Loading profile…" else "Profile not loaded yet.",
            style = MaterialTheme.typography.bodySmall,
            color = LocalMigoExtra.current.faint,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
        TextButton(onClick = onLoad, enabled = !edit.busy) { Text("Load profile") }
        return
    }

    // The form's own text, primed from the read and re-primed when a new read lands. `remember`
    // keyed on the profile: a save's answer replaces the profile object, so the fields follow the
    // server's version of what was just written. The birth year joins this priming now that the
    // wire carries it back — an untouched field still sends nothing, but "untouched" is measured
    // against the year the server holds rather than always starting blank.
    var displayName by remember(profile) { mutableStateOf(profile.displayName) }
    var bio by remember(profile) { mutableStateOf(profile.bio ?: "") }
    var birthYear by remember(profile) { mutableStateOf(profile.birthYear?.toString() ?: "") }
    var status by remember(profile) { mutableStateOf(profile.customStatus ?: "") }
    // The privacy selections: null is "leave as-is"; a picked index maps to the wire value.
    var showLastSeenChoice by remember(profile) { mutableStateOf(-1) }
    var whoCanMessageChoice by remember(profile) { mutableStateOf(-1) }
    var whoCanAddChoice by remember(profile) { mutableStateOf(-1) }
    var searchableChoice by remember(profile) { mutableStateOf(-1) }

    OutlinedTextField(
        value = displayName,
        onValueChange = { displayName = it },
        label = { Text("Display name") },
        singleLine = true,
        enabled = !edit.busy,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    )
    OutlinedTextField(
        value = bio,
        onValueChange = { bio = it },
        label = { Text("Bio") },
        enabled = !edit.busy,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    )
    OutlinedTextField(
        value = status,
        onValueChange = { status = it },
        label = { Text("Custom status") },
        supportingText = { Text("Shown beside your presence, everywhere your name appears.") },
        singleLine = true,
        enabled = !edit.busy,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    )
    OutlinedTextField(
        value = birthYear,
        onValueChange = { birthYear = it.filter { ch -> ch.isDigit() }.take(4) },
        label = { Text("Birth year (optional)") },
        supportingText = { Text("Not public; visible only on your own profile.") },
        singleLine = true,
        enabled = !edit.busy,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    )

    PrivacyDropdown(
        label = "Who sees your last seen",
        choice = showLastSeenChoice,
        onChoice = { showLastSeenChoice = it },
        enabled = !edit.busy,
    )
    PrivacyDropdown(
        label = "Who can message you",
        choice = whoCanMessageChoice,
        onChoice = { whoCanMessageChoice = it },
        enabled = !edit.busy,
    )
    PrivacyDropdown(
        label = "Who can add you as a friend",
        choice = whoCanAddChoice,
        onChoice = { whoCanAddChoice = it },
        enabled = !edit.busy,
    )
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = "Appear in username search",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Spacer(modifier = Modifier.width(8.dp))
        TriStateSwitch(
            choice = searchableChoice,
            onChoice = { searchableChoice = it },
            enabled = !edit.busy,
        )
    }
    Text(
        text = "Your current setting is private; the switch joins the save only once you flip it.",
        style = MaterialTheme.typography.labelSmall,
        color = LocalMigoExtra.current.faint,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
    )

    Button(
        onClick = {
            onSave(
                displayName.trim(),
                bio.trim(),
                birthYear,
                when (searchableChoice) { -1 -> null; 0 -> false; else -> true },
                VISIBILITY_VALUES.getOrNull(showLastSeenChoice),
                VISIBILITY_VALUES.getOrNull(whoCanMessageChoice),
                VISIBILITY_VALUES.getOrNull(whoCanAddChoice),
            )
            // The status saves on its own wire, so it does not wait for this button.
            onSaveStatus(status.trim())
        },
        enabled = !edit.busy,
        modifier = Modifier.padding(start = 16.dp, top = 8.dp, bottom = 8.dp),
    ) {
        Text(if (edit.busy) "Saving…" else "Save profile")
    }
}

/** The privacy wire values, in the order the dropdowns list them. */
private val VISIBILITY_VALUES = listOf(0L, 1L, 2L)

/**
 * One privacy control: a label plus a dropdown whose first entry is "leave as-is".
 *
 * The server never returns current privacy values (they are private), so "leave as-is" is not a
 * cop-out — it is the only honest default. A picked entry is the wire value; index -1 is untouched.
 */
@Composable
private fun PrivacyDropdown(
    label: String,
    choice: Int,
    onChoice: (Int) -> Unit,
    enabled: Boolean,
) {
    val options = listOf("Leave as-is", "Everyone", "Friends only", "Nobody")
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Box {
            var open by remember { mutableStateOf(false) }
            OutlinedButton(onClick = { open = true }, enabled = enabled) {
                Text(options[choice.coerceIn(0, options.lastIndex)])
            }
            DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                options.forEachIndexed { index, option ->
                    DropdownMenuItem(
                        text = { Text(option) },
                        onClick = {
                            onChoice(index)
                            open = false
                        },
                    )
                }
            }
        }
    }
}

/**
 * The search switch: three states — untouched, on, off.
 *
 * A plain toggle would read the current value from the profile, which the server never sends; this
 * control says what it is doing instead. Untouched (index -1) sends nothing.
 */
@Composable
private fun TriStateSwitch(choice: Int, onChoice: (Int) -> Unit, enabled: Boolean) {
    val label = when (choice) {
        -1 -> "leave as-is"
        0 -> "off"
        else -> "on"
    }
    OutlinedButton(onClick = { onChoice(if (choice <= 0) 1 else 0) }, enabled = enabled) {
        Text(label)
    }
}

/**
 * The account-security half of the Profile panel: the passphrase-change form and the
 * recovery-contact form.
 *
 * The two passphrase secrets and the contact string live in the composable's own state — not on
 * the [AccountSecurityState] object — because state objects survive recomposition and get logged
 * in bug reports, and a secret's only safe home is the field it is typed into, wiped the moment
 * the save takes it. They wipe on the success notice rather than on the click: a refused change
 * keeps its typed text (the person is mid-edit), a successful one starts fresh.
 */
@Composable
private fun AccountSecuritySection(
    security: AccountSecurityState,
    onChangePassphrase: (current: String, next: String) -> Unit,
    onSaveContact: (contact: String) -> Unit,
) {
    var current by rememberSaveable { mutableStateOf("") }
    var next by rememberSaveable { mutableStateOf("") }
    var confirm by rememberSaveable { mutableStateOf("") }
    var contact by rememberSaveable { mutableStateOf("") }

    // A success notice is the one event that means the typed secrets are spent: the server has
    // taken them and the vault is re-sealed. Cleared here rather than in the click so a refusal
    // keeps the person's typing.
    LaunchedEffect(security.notice) {
        if (security.notice != null) {
            current = ""
            next = ""
            confirm = ""
            contact = ""
        }
    }

    // Whether the two new-passphrase fields disagree, once both have something to compare.
    // Named once: the field's error styling and its supporting line must agree, and a repeated
    // condition is a second place they can drift apart.
    val mismatched = confirm.isNotEmpty() && next != confirm

    Text(
        text = "Changing the passphrase signs out every other session, on every device; this " +
            "one stays signed in.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
    )
    OutlinedTextField(
        value = current,
        onValueChange = { current = it },
        label = { Text("Current passphrase") },
        singleLine = true,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
        enabled = !security.busy,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    )
    OutlinedTextField(
        value = next,
        onValueChange = { next = it },
        label = { Text("New passphrase") },
        singleLine = true,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
        enabled = !security.busy,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    )
    OutlinedTextField(
        value = confirm,
        onValueChange = { confirm = it },
        label = { Text("Confirm new passphrase") },
        singleLine = true,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
        enabled = !security.busy,
        isError = mismatched,
        supportingText = if (mismatched) {
            { Text("The new passphrases do not match.") }
        } else {
            null
        },
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    )
    if (security.failure != null) {
        Text(
            text = security.failure ?: "",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
        )
    }
    if (security.notice != null) {
        Text(
            text = security.notice ?: "",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.secondary,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
        )
    }
    Button(
        onClick = { onChangePassphrase(current, next) },
        enabled = !security.busy && current.isNotEmpty() && next.isNotEmpty() && next == confirm,
        modifier = Modifier.padding(start = 16.dp, top = 4.dp, bottom = 8.dp),
    ) {
        Text(if (security.busy) "Working…" else "Change passphrase")
    }

    Spacer(modifier = Modifier.padding(8.dp))

    Text(
        text = "A recovery contact — an email or a phone — is where a recovery starts. The " +
            "account keeps one; saving replaces it.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
    )
    OutlinedTextField(
        value = contact,
        onValueChange = { contact = it },
        label = { Text("Email or phone") },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Email),
        enabled = !security.busy,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
    )
    Button(
        onClick = { onSaveContact(contact) },
        enabled = !security.busy && contact.isNotBlank(),
        modifier = Modifier.padding(start = 16.dp, top = 4.dp, bottom = 8.dp),
    ) {
        Text("Save contact")
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
