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
import androidx.compose.ui.unit.sp
import com.migo.app.model.AppState
import com.migo.core.store.AppSettings
import com.migo.core.store.MediaAutoDownload
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
 */
@Composable
fun SettingsScreen(
    state: AppState.SignedIn,
    preferences: AppSettings,
    onTheme: (ThemeChoice) -> Unit,
    onSendReadReceipts: (Boolean) -> Unit,
    onSendTypingIndicators: (Boolean) -> Unit,
    onMediaAutoDownload: (MediaAutoDownload) -> Unit,
    onAutoSaveChatLogs: (Boolean) -> Unit,
    onSaveAllChats: (Uri) -> Unit,
    onRefreshStorage: () -> Unit,
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
    LaunchedEffect(Unit) { onRefreshStorage() }

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
                RoundedCornerShape(10.dp),
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = label,
            fontSize = 12.sp,
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
