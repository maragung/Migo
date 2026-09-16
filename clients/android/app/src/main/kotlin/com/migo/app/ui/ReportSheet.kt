package com.migo.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.migo.app.model.ReportSheetView
import com.migo.core.domain.REPORT_NOTE_MAX_LEN
import com.migo.core.domain.ReportReason

/**
 * One reason a person can point at, as the sheet offers it: the code that travels and the two
 * lines that do not.
 *
 * The labels and hints are the web client's own words, deliberately identical, because the reason
 * menu is one question asked in two places: a person who read it on the desk must recognise it on
 * the phone, and a reason that read differently in each client would make the two answer different
 * questions.
 */
data class ReportReasonOption(
    /** The code that goes on the wire. */
    val reason: ReportReason,
    /** The short name, the line a reader scans. */
    val label: String,
    /** The sentence under it, which is what "Spam" or "Violence" actually means here. */
    val hint: String,
)

/**
 * The reason menu, in the order it is read.
 *
 * Nine of the thirteen codes, and the four left out are left out on purpose. Flood is not a thing a
 * person reports — it is a rate the server already counts. Self-harm is not a queue entry a reporter
 * should have to pick out of a list while looking at somebody they are worried about. Child safety
 * and bot abuse are real codes with real handlers, but they are reached through the channels that
 * exist for them rather than through a generic report menu, and offering them here would promise a
 * response path this sheet does not own.
 *
 * [ReportReason.Other] is last and is the only catch-all: a menu that put it first would collect
 * every report that was not read to the bottom.
 */
val REPORT_REASONS: List<ReportReasonOption> = listOf(
    ReportReasonOption(ReportReason.Spam, "Spam", "Unwanted bulk messages or invites."),
    ReportReasonOption(
        ReportReason.Scam,
        "Scam or fraud",
        "Trying to get money or details by deception.",
    ),
    ReportReasonOption(
        ReportReason.MaliciousLink,
        "Malicious link",
        "A link to malware, phishing, or a page that steals sign-ins.",
    ),
    ReportReasonOption(ReportReason.Harassment, "Harassment", "Threats or targeted abuse of a person."),
    ReportReasonOption(ReportReason.HateSpeech, "Hate speech", "Hateful content aimed at a group."),
    ReportReasonOption(
        ReportReason.SexualContent,
        "Sexual content",
        "Sexual content where it does not belong.",
    ),
    ReportReasonOption(ReportReason.Violence, "Violence", "Graphic violence."),
    ReportReasonOption(ReportReason.Impersonation, "Impersonation", "Pretending to be somebody else."),
    ReportReasonOption(ReportReason.Other, "Something else", "None of the above."),
)

/**
 * The report sheet: one question, one optional note, one priced act.
 *
 * Opened from a message's menu, an account's card, and a room's menu — one sheet for all three,
 * because filing a report is one act with one shape on the wire. What the sheet says about its
 * subject is only the [ReportSheetView.label] the opening surface supplied: the conversations here
 * are end-to-end encrypted, so the sheet cannot quote the message it is about, and a preview would
 * either be unreadable or would mean shipping the node a plaintext it is not meant to hold.
 *
 * The note says plainly who reads it. That is not decoration: a reporter who assumes the note is
 * private will write something they would not want staff to see, and the one field in this whole
 * path a human typed is the one field where that assumption would hurt them.
 *
 * Send stays dark until a reason is picked, which is where this differs from the web dialog's
 * preselected first reason: on a phone the menu is read with a thumb already moving, and a live Send
 * over an unread menu is a report filed under a reason nobody chose.
 */
@Composable
fun ReportSheet(
    view: ReportSheetView,
    onPickReason: (ReportReason) -> Unit,
    onNote: (String) -> Unit,
    onSubmit: () -> Unit,
    onClose: () -> Unit,
) {
    MigoSheet(title = if (view.filed) "Report sent" else "Report ${view.label}", onDismiss = onClose) {
        if (view.filed) {
            // The outcome, stated in the node's own terms: the report is in the queue, the reporter
            // learns nothing further, and the person reported is not told who filed it. Every one
            // of those three is a fact about the design rather than a courtesy, so the sentence
            // says all three rather than leaving a person to assume the worst of the silence.
            Text(
                text = "Thanks — ${view.label} has been reported. Our moderators will review it. " +
                    "You will not be told the outcome, and the person is not told who reported them.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
            SheetPrimaryAction(label = "Close", onClick = onClose)
            Spacer(modifier = Modifier.height(8.dp))
            return@MigoSheet
        }

        Text(
            text = "What is wrong with ${view.label}? This helps us send it to the right person.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
        )

        // The menu scrolls inside a bounded height: nine rows with their hints are taller than the
        // sheet on a short screen, and the note and the Send under them must stay reachable without
        // the person having to dismiss and reopen.
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(max = 264.dp)
                .verticalScroll(rememberScrollState()),
        ) {
            for (option in REPORT_REASONS) {
                ReasonRow(
                    option = option,
                    picked = view.reason == option.reason,
                    enabled = !view.busy,
                    onClick = { onPickReason(option.reason) },
                )
            }
        }

        // What is left of the ceiling, counted here rather than written as a word inside the
        // sentence below: the sheet says how much room remains, so a person writing a long note
        // knows before the field quietly stops taking it. The count is honest because the model
        // truncates on the way in, which is the same ceiling the domain checks before it spends a
        // frame -- so what this says is what can still be sent.
        val left = REPORT_NOTE_MAX_LEN - view.note.length
        OutlinedTextField(
            value = view.note,
            onValueChange = onNote,
            enabled = !view.busy,
            placeholder = { Text("Anything else? (optional)") },
            supportingText = {
                Text(
                    text = "A moderator will read this, so do not include anything you would not " +
                        "want staff to see. $left characters left",
                    style = MaterialTheme.typography.labelSmall,
                )
            },
            minLines = 2,
            maxLines = 4,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 4.dp),
        )

        if (view.failure != null) {
            Text(
                text = view.failure,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }

        SheetPrimaryAction(
            label = if (view.busy) "Sending…" else "Send report",
            enabled = view.reason != null && !view.busy,
            onClick = onSubmit,
        )
        TextButton(
            onClick = onClose,
            enabled = !view.busy,
            modifier = Modifier.padding(horizontal = 10.dp),
        ) { Text("Cancel") }
        Spacer(modifier = Modifier.height(8.dp))
    }
}

/**
 * One reason in the menu: a radio mark, the short name, and the sentence that says what it means.
 *
 * The whole row is the target rather than the mark alone — the platform's own minimum touch target
 * is 48dp and a radio button is 20 — and the picked row is stated twice over, by the filled mark and
 * by the weight of its label, because colour alone is a fact a colour-blind reader cannot read.
 */
@Composable
private fun ReasonRow(
    option: ReportReasonOption,
    picked: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    val scheme = MaterialTheme.colorScheme
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 54.dp)
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            modifier = Modifier
                .size(22.dp)
                .background(
                    if (picked) scheme.tertiary else scheme.surfaceVariant,
                    CircleShape,
                ),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text = if (picked) "✓" else "",
                fontSize = MigoGlyph.small,
                color = if (picked) scheme.onTertiary else scheme.onSurfaceVariant,
            )
        }
        Spacer(modifier = Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = option.label,
                style = MaterialTheme.typography.bodyLarge,
                fontWeight = if (picked) FontWeight.Bold else FontWeight.SemiBold,
                color = if (enabled) scheme.onSurface else scheme.outline,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = option.hint,
                style = MaterialTheme.typography.bodySmall,
                color = if (enabled) scheme.onSurfaceVariant else scheme.outline,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}
