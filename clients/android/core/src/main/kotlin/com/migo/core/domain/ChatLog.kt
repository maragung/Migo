package com.migo.core.domain

import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/**
 * The chat-log half of the client, as pure functions.
 *
 * A chat log is a plaintext-by-design artifact: the transcript this device already decrypted,
 * written down in a form a person can read without the app. That is the whole feature, and it is
 * also the whole risk, so the formatting lives here — in the pure half of the SDK, testable
 * without a device — and every caller that writes a log says in its own interface that the file
 * contains decrypted conversation and lives only on this device.
 *
 * The line shape is deliberately plain text rather than JSON: a log's reader is a person in a
 * text editor or a share sheet, not a program, and a format only a program could read would be a
 * backup, which is a different feature with different obligations (the `.migo` container is the
 * backup, and it is sealed).
 */

/** One transcript line, as a log renders it — the app-side message row reduced to its words. */
data class ChatLogLine(
    /** Unix milliseconds, the message's own time as the server stamped it. */
    val at: Long,
    /** The sender's display name; the caller decides what "You" means. */
    val author: String,
    /** The line's text — a chat row's label, so a media message logs its caption or its kind. */
    val text: String,
)

/**
 * Formats one conversation's transcript.
 *
 * The header names the conversation and the moment of the export, because a log that does not say
 * when it was taken silently answers "is this current?" with a guess. Each line is
 * `date  time  author  text` — two spaces between fields, so a fixed-width glance parses it and a
 * paste into anything keeps the columns.
 */
fun formatChatLog(title: String, exportedAtMs: Long, lines: List<ChatLogLine>): String {
    val builder = StringBuilder()
    builder.append("Migo chat log — ").append(title).append('\n')
    builder.append("Exported ").append(logStamp(exportedAtMs)).append('\n')
    builder.append('\n')
    for (line in lines) {
        builder.append(logStamp(line.at))
            .append("  ")
            .append(line.author)
            .append("  ")
            .append(line.text)
            .append('\n')
    }
    return builder.toString()
}

/**
 * Formats every held conversation into one log, in the order the caller lists them.
 *
 * The same header rule as [formatChatLog], plus a per-conversation heading, because a file of
 * transcripts with no dividers is a file one reads by accident into the wrong conversation.
 */
fun formatAllChatsLog(accountName: String, exportedAtMs: Long, chats: List<Pair<String, List<ChatLogLine>>>): String {
    val builder = StringBuilder()
    builder.append("Migo chat logs — ").append(accountName).append('\n')
    builder.append("Exported ").append(logStamp(exportedAtMs)).append('\n')
    for ((title, lines) in chats) {
        builder.append('\n')
        builder.append("== ").append(title).append(" ==\n")
        for (line in lines) {
            builder.append(logStamp(line.at))
                .append("  ")
                .append(line.author)
                .append("  ")
                .append(line.text)
                .append('\n')
        }
    }
    return builder.toString()
}

/**
 * A timestamp as `2026-09-10 19:03`, in the device's zone.
 *
 * Local rather than UTC because the log's reader is the person who read the chat, in the place
 * they read it — the app's own clock labels make the same choice. A stamp that cannot be built
 * (a negative or otherwise out-of-range instant) renders as `—` rather than throwing in the
 * middle of writing a file the person asked for.
 */
fun logStamp(epochMs: Long): String {
    if (epochMs <= 0L) return "—"
    return try {
        Instant.ofEpochMilli(epochMs)
            .atZone(ZoneId.systemDefault())
            .format(DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm"))
    } catch (_: RuntimeException) {
        "—"
    }
}

/**
 * A conversation title as a file name this device will accept.
 *
 * Everything that could climb out of a file name — separators, colons, controls, anything not
 * plainly writable — becomes `_`; runs collapse so the result reads as one substitution; leading
 * and trailing dots and spaces go, because a leading dot hides the file and a trailing one
 * confuses extensions; and the whole is bounded to 48 characters, which every filesystem this app
 * runs on accepts once suffixed. Empty after all that (a title of only emoji, or only slashes)
 * falls back to `chat`, because the alternative is a nameless file the picker cannot even
 * suggest.
 */
fun sanitizeFilename(raw: String): String {
    // A space is an underscore here, not a kept character: a filename with spaces is quoted by
    // every shell that touches it, and the picker suggests it with %20 ugliness — while the
    // underscore reads the same and never needs quoting. The same argument retires leading and
    // trailing underscores along with the dots and spaces in the trim.
    val cleaned = raw
        .map { character -> if (character.isLetterOrDigit() || character == '-' || character == '_' || character == '.') character else '_' }
        .joinToString("")
        .replace(Regex("_+"), "_")
        .replace(Regex("\\.{2,}"), ".")
        .trim('.', '_', ' ')
    val bounded = if (cleaned.length > 48) cleaned.take(48) else cleaned
    return bounded.ifEmpty { "chat" }
}

/**
 * A conversation title as a log's suggested file name: the sanitized title, the `.txt` the log
 * already is, and nothing else — no timestamps in the name, because each conversation keeps
 * exactly one newest snapshot and a per-save timestamp would accumulate files the eviction would
 * then have to hunt.
 */
fun chatLogFilename(title: String): String = sanitizeFilename(title) + ".txt"

/**
 * Which held snapshots to delete once a new one is written, given the existing files
 * oldest-first.
 *
 * The rule: one file per conversation (the writer always overwrites its own conversation's file,
 * so the caller never passes two of the same conversation), and at most [keep] conversations in
 * all. Everything past the cap is a deletion, oldest first by the caller's ordering — the file
 * system's own `lastModified`, in the caller's hands, because this function must stay pure and
 * the file system is anything but.
 */
fun <T> snapshotEvictions(oldestFirst: List<T>, keep: Int): List<T> {
    // A negative cap is read as zero — refusing to guess a meaning the caller never has — so
    // both it and a zero say "keep none of these". The deletions are the FRONT of the list:
    // the caller sorts oldest-first, so what survives a cap is the newest `keep`, and
    // everything before them is exactly what the cap retired.
    val cap = if (keep < 0) 0 else keep
    return if (oldestFirst.size > cap) oldestFirst.subList(0, oldestFirst.size - cap).toList() else emptyList()
}
