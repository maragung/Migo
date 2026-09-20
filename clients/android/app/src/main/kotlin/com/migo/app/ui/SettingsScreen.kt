package com.migo.app.ui

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.migo.app.call.CallInput
import com.migo.app.model.AppState
import com.migo.core.domain.CallInputDevice
import com.migo.core.domain.CallInputResolution
import com.migo.core.domain.resolveCallInput
import com.migo.core.store.AppSettings
import com.migo.core.store.MediaAutoDownload
import com.migo.core.store.NavigationMode
import com.migo.core.store.ThemeChoice

/**
 * The Settings panel: the device-level choices this app actually honours, grouped by what they
 * touch.
 *
 * Every row here is wired to a real behaviour — the read-receipt and typing switches gate the
 * sends themselves, the media choice gates the bubbles' automatic fetch, the theme reaches the
 * composition's own [MigoTheme], and the auto-save switch gates the transcript snapshot a
 * closed conversation leaves behind. Nothing is offered that the app merely stores and ignores,
 * because a settings screen full of dead switches teaches that none of them work. The one group
 * the headings rule would allow that this app cannot honour — notifications — is absent rather
 * than stubbed: this build posts no notifications, so it offers nothing to configure.
 *
 * The chat-log group carries the feature's one unavoidable sentence out loud: a saved log is
 * plaintext on this device, and the switch that writes it is the same switch that owns that
 * fact.
 *
 * The call group's microphone list is the one row here whose offer is a *preference* rather than a
 * command, because that is the whole of what the platform provides: the chosen device is handed to
 * the recorder as a preference, and a phone that cannot honour it -- the device has gone, another
 * app holds it -- falls back to its own choice. The sentence under the list says so, and the list
 * itself is read from the phone rather than remembered, so a headset plugged in while the pane is
 * open is a row that appears.
 */
@Composable
fun SettingsScreen(
    state: AppState.SignedIn,
    preferences: AppSettings,
    /** The microphones the phone is offering, read when the pane opened. */
    callInputs: List<CallInput> = emptyList(),
    onTheme: (ThemeChoice) -> Unit,
    onNavigationMode: (NavigationMode) -> Unit,
    onSendReadReceipts: (Boolean) -> Unit,
    onSendTypingIndicators: (Boolean) -> Unit,
    onMediaAutoDownload: (MediaAutoDownload) -> Unit,
    /** The microphone calls should record from, or null for the phone's own choice. */
    onCallInputDevice: (CallInputDevice?) -> Unit,
    onAutoSaveChatLogs: (Boolean) -> Unit,
    onSaveAllChats: (Uri) -> Unit,
    onRefreshStorage: () -> Unit,
    onRefreshCallInputs: () -> Unit,
    onClearCaches: () -> Unit,
    onSignOut: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // The all-chats export goes through the system's own destination picker, the same grant the
    // document save uses: the person's own choice of file is the person's own permission, and no
    // storage permission is ever asked for.
    val exportLogs = rememberLauncherForActivityResult(
        ActivityResultContracts.CreateDocument("text/plain"),
    ) { uri ->
        if (uri != null) onSaveAllChats(uri)
    }

    // The storage group's numbers are null until a walk lands, and a walk only happens on entry:
    // the sizes are facts of the moment the panel was opened, not live figures anybody needs
    // watched.
    LaunchedEffect(Unit) {
        onRefreshStorage()
        onRefreshCallInputs()
    }

    Column(modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        ScreenTitle(title = "Settings")

        // The panel's one-line answer to whatever the last action here reported — a saved log, a
        // cleared cache, a write the store refused. It stays until the next action replaces it,
        // because a success nobody notices is a button that looks like it did nothing.
        state.settings.notice?.let {
            Text(
                text = it,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
            )
        }

        // --- Chats & Log ---

        SectionLabel(text = "Chats & Log")
        ToggleRow(
            title = "Auto-save chat logs",
            sub = "Write a transcript of a conversation when its tab is closed",
            checked = preferences.autoSaveChatLogs,
            onChange = onAutoSaveChatLogs,
        )
        Text(
            text = "A saved log is plaintext: readable without the app and without the keys, " +
                "by anyone holding this phone. One newest log per conversation, at most twenty " +
                "conversations kept, and signing out deletes every saved log.",
            style = MaterialTheme.typography.labelSmall,
            color = LocalMigoExtra.current.faint,
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        TextButton(
            onClick = { exportLogs.launch("migo-chat-logs.txt") },
            modifier = Modifier.padding(horizontal = 8.dp),
        ) {
            Text(
                text = "Export semua chat",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
            )
        }
        Text(
            text = "Every conversation opened this session, as one text file through the " +
                "system's save sheet.",
            style = MaterialTheme.typography.labelSmall,
            color = LocalMigoExtra.current.faint,
            modifier = Modifier.padding(horizontal = 16.dp),
        )

        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

        // --- Privasi & Keamanan ---

        SectionLabel(text = "Privasi & Keamanan")
        ToggleRow(
            title = "Read receipts",
            sub = "Tell the sender when this device has read their message",
            checked = preferences.sendReadReceipts,
            onChange = onSendReadReceipts,
        )
        ToggleRow(
            title = "Typing indicators",
            sub = "Show the peer when somebody here is typing",
            checked = preferences.sendTypingIndicators,
            onChange = onSendTypingIndicators,
        )
        Text(
            text = "Both are sends this device makes about you, so both are yours to switch off; " +
                "messages arrive and display exactly the same either way.",
            style = MaterialTheme.typography.labelSmall,
            color = LocalMigoExtra.current.faint,
            modifier = Modifier.padding(horizontal = 16.dp),
        )

        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

        // --- Penyimpanan & Data ---

        SectionLabel(text = "Penyimpanan & Data")
        ChoiceRow(
            title = "Media auto-download",
            sub = "When an image, document or voice note is fetched without being tapped",
            selected = preferences.mediaAutoDownload,
            choices = listOf(
                MediaAutoDownload.Never to "Never",
                MediaAutoDownload.Unmetered to "Wi-Fi only",
                MediaAutoDownload.Always to "Always",
            ),
            onChoice = onMediaAutoDownload,
        )
        FactRow(
            label = "Cache",
            value = state.settings.cacheBytes?.let { formatBytes(it) } ?: "measuring…",
        )
        FactRow(
            label = "Chat logs",
            value = if (state.settings.logBytes == null) {
                "measuring…"
            } else {
                formatBytes(state.settings.logBytes) + " · " + state.settings.logCount + " chats"
            },
        )
        Row(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
            Button(onClick = onClearCaches, enabled = !state.settings.clearing) {
                Text(if (state.settings.clearing) "Clearing…" else "Hapus cache")
            }
        }
        Text(
            text = "Clears temporary media, voice-note recordings and playback scratch. Chat " +
                "logs and the account's own files are not touched.",
            style = MaterialTheme.typography.labelSmall,
            color = LocalMigoExtra.current.faint,
            modifier = Modifier.padding(horizontal = 16.dp),
        )

        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

        // --- Panggilan ---

        SectionLabel(text = "Panggilan")
        MicrophonePicker(
            inputs = callInputs,
            saved = preferences.callInputDevice,
            onChoose = onCallInputDevice,
        )
        Text(
            text = microphoneNote(callInputs, preferences.callInputDevice),
            style = MaterialTheme.typography.labelSmall,
            color = LocalMigoExtra.current.faint,
            modifier = Modifier.padding(horizontal = 16.dp),
        )

        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

        // --- Tampilan ---

        SectionLabel(text = "Tampilan")
        ChoiceRow(
            title = "Theme",
            sub = "Light, dark, or whatever the system is doing",
            selected = preferences.theme,
            choices = listOf(
                ThemeChoice.System to "System",
                ThemeChoice.Light to "Light",
                ThemeChoice.Dark to "Dark",
            ),
            onChoice = onTheme,
        )
        ChoiceRow(
            title = "Navigation Mode",
            sub = "How the app is navigated once signed in",
            selected = preferences.navigationMode,
            choices = listOf(
                NavigationMode.Tabbed to "Tabbed",
                NavigationMode.ChatList to "Chat List",
            ),
            onChoice = onNavigationMode,
        )
        Text(
            text = "Tabbed is the window strip along the top: the home tabs and one tab per open " +
                "conversation. Chat List is the bottom bar — Main, Friends, Rooms, Feed — where " +
                "Main is the conversation list and a tapped conversation opens as its own screen. " +
                "Both read the same session: the conversations, their unread, and the open chat " +
                "stay put across a switch.",
            style = MaterialTheme.typography.labelSmall,
            color = LocalMigoExtra.current.faint,
            modifier = Modifier.padding(horizontal = 16.dp),
        )

        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

        // --- Akun ---

        SectionLabel(text = "Akun")
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        ) {
            Column {
                Text(
                    text = state.username,
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                OneLine(text = state.accountId.value)
            }
        }
        TextButton(onClick = onSignOut, modifier = Modifier.padding(horizontal = 8.dp)) {
            Text("Sign out", color = MaterialTheme.colorScheme.error)
        }
        Spacer(modifier = Modifier.padding(8.dp))
    }
}

/**
 * One on/off choice: the label and its consequence on the left, the switch on the right. The
 * switch is the whole row's meaning, so the row is not clickable — a mis-tap next to a switch is
 * a setting changed by accident.
 */
@Composable
private fun ToggleRow(
    title: String,
    sub: String,
    checked: Boolean,
    onChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = title,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Text(
                text = sub,
                style = MaterialTheme.typography.labelSmall,
                color = LocalMigoExtra.current.faint,
            )
        }
        Spacer(modifier = Modifier.width(12.dp))
        Switch(checked = checked, onCheckedChange = onChange)
    }
}

/**
 * One few-way choice as a row of pills, the presence pills' own shape: the selected pill is the
 * container colour and carries its check by being the only filled one.
 */
@Composable
private fun <T> ChoiceRow(
    title: String,
    sub: String,
    selected: T,
    choices: List<Pair<T, String>>,
    onChoice: (T) -> Unit,
) {
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        Text(
            text = title,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Text(
            text = sub,
            style = MaterialTheme.typography.labelSmall,
            color = LocalMigoExtra.current.faint,
        )
        Spacer(modifier = Modifier.padding(4.dp))
        Row(modifier = Modifier.fillMaxWidth()) {
            choices.forEachIndexed { index, (value, label) ->
                if (index > 0) Spacer(modifier = Modifier.width(8.dp))
                ChoicePill(
                    label = label,
                    selected = value == selected,
                    onClick = { onChoice(value) },
                    modifier = Modifier.weight(1f),
                )
            }
        }
    }
}

/** One pill of a [ChoiceRow], the presence pill's flat shape with no dot to carry. */
@Composable
private fun ChoicePill(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scheme = MaterialTheme.colorScheme
    Box(
        modifier = modifier
            .heightIn(min = 40.dp)
            .background(
                if (selected) scheme.primaryContainer else scheme.surfaceVariant,
                RoundedCornerShape(MigoRadius.md),
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = label,
            fontSize = MigoType.body,
            fontWeight = FontWeight.SemiBold,
            color = if (selected) scheme.onPrimaryContainer else scheme.onSurfaceVariant,
            maxLines = 1,
        )
    }
}

/** One read-only fact of the storage group: the label, and the value or its "not yet". */
@Composable
private fun FactRow(label: String, value: String) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Spacer(modifier = Modifier.width(12.dp))
        OneLine(text = value, modifier = Modifier.weight(1f))
    }
}

/**
 * The microphone a call records from, as the list the phone is offering.
 *
 * Ticked rather than pill-shaped like the few-way rows above, because a phone's microphone list is
 * as long as its hardware makes it and equal-width pills would set every row's width to the longest
 * device name on the phone. This is the same shape the in-call route menu uses, for the same list
 * of the same platform's devices.
 *
 * The first row is the phone's own choice and not a device: a null in the store, a tick with no
 * device behind it, and the state every fresh install is in.
 */
@Composable
private fun MicrophonePicker(
    inputs: List<CallInput>,
    saved: CallInputDevice?,
    onChoose: (CallInputDevice?) -> Unit,
) {
    val resolution = resolveCallInput(saved, inputs.map { it.device })
    DeviceRow(
        label = "System default",
        sub = "Whichever microphone the phone picks for the call",
        chosen = resolution is CallInputResolution.SystemDefault,
        onClick = { onChoose(null) },
    )
    val devices = inputs.map { it.device }
    inputs.forEachIndexed { index, input ->
        // A device the phone neither names nor addresses cannot be found again on the next call, so
        // offering it would be offering a choice the app cannot keep. The row is drawn as a fact
        // rather than left out, because a person looking for their microphone in a list that omits
        // it would conclude the app does not see it at all.
        val findable = resolveCallInput(input.device, devices) is CallInputResolution.Resolved
        DeviceRow(
            label = input.label,
            sub = if (findable) null else "This phone names it too little to remember the choice",
            chosen = resolution is CallInputResolution.Resolved && resolution.index == index,
            enabled = findable,
            onClick = { onChoose(input.device) },
        )
    }
}

/**
 * What the microphone list cannot say by ticking a row.
 *
 * Three cases and never more than one sentence, in the order the person meets them: a phone that
 * lists nothing (an empty list is a legitimate answer, not a failure), a chosen microphone that is
 * not here (the choice is kept, so the sentence has to say that it is kept rather than that it was
 * lost), and the ordinary case, where the one thing worth saying is that Android treats the choice
 * as a preference. That last sentence is not a hedge: the platform hands the device to the recorder
 * as a preference and is free to ignore it, and a setting that appears to command a device the
 * phone may decline to use would be the interface overstating what it did.
 */
private fun microphoneNote(inputs: List<CallInput>, saved: CallInputDevice?): String = when {
    inputs.isEmpty() ->
        "This phone is not reporting any microphones right now, so calls record from whichever " +
            "one it picks."
    resolveCallInput(saved, inputs.map { it.device }) is CallInputResolution.Missing -> {
        val name = saved?.name?.takeIf { it.isNotEmpty() } ?: "The microphone you chose"
        "$name is not connected right now; calls record from the phone's own choice until it is " +
            "back, and the choice is kept."
    }
    else ->
        "Android treats this as a preference rather than a command: if the chosen microphone is " +
            "gone or another app is holding it, the call uses the phone's own choice instead of " +
            "failing."
}

/**
 * One selectable device: a tick on the one in use, then the name and what needs saying about it.
 *
 * The tick leads rather than trails so the names line up in one column whatever their length, and
 * the unticked rows keep the same leading space rather than shifting left. A row that is not
 * enabled is a device the app cannot remember the choice of (see [MicrophonePicker]), drawn in the
 * faint ink the panel uses for a fact rather than a control.
 */
@Composable
private fun DeviceRow(
    label: String,
    sub: String?,
    chosen: Boolean,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = if (chosen) "\u2713" else "",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.width(20.dp),
        )
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = label,
                style = MaterialTheme.typography.bodyMedium,
                color = if (enabled) {
                    MaterialTheme.colorScheme.onSurface
                } else {
                    LocalMigoExtra.current.faint
                },
            )
            if (sub != null) {
                Text(
                    text = sub,
                    style = MaterialTheme.typography.labelSmall,
                    color = LocalMigoExtra.current.faint,
                )
            }
        }
    }
}

/**
 * A byte count as a person reads it: the largest unit that keeps a whole number before the
 * decimal, one place after it. A cache size is an approximation the moment it is measured, so a
 * false precision of bytes would be the least honest way to state it.
 */
private fun formatBytes(bytes: Long): String = when {
    bytes < 1024L -> "$bytes B"
    bytes < 1024L * 1024 -> "%.1f KB".format(bytes / 1024.0)
    bytes < 1024L * 1024 * 1024 -> "%.1f MB".format(bytes / (1024.0 * 1024))
    else -> "%.1f GB".format(bytes / (1024.0 * 1024 * 1024))
}
