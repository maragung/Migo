package com.migo.app.media

import com.migo.core.wire.Id
import com.migo.core.wire.IdParseResult
import com.migo.core.wire.tryParseId
import java.io.File
import java.util.Properties

/**
 * The receiver-local listened marks for voice notes: which voice-note messages this account has
 * heard, kept on this device and nowhere else.
 *
 * Brief section 179's rule is that "listened" is the receiver's own state. The wire has no
 * `Played` receipt — `ReceiptKind` carries no such variant, and this store is deliberately built so
 * it never needs one: nothing here is ever sent, and marking a note unlistened changes only what
 * this device draws, never a receipt that already left. The sender's copy of the conversation is
 * untouched by every mark this store holds.
 *
 * The persistence is the app's own properties-file idiom — the same shape
 * [com.migo.app.session.RoomInfoStore] keeps followed rooms in and [VoiceNoteDrafts] keeps
 * recordings in: a small `Properties` file in the app's private storage, rewritten whole on every
 * change, because a few hundred message ids are cheaper to rewrite than any incremental scheme and
 * cannot drift from what the session holds.
 *
 * The record is scoped to the account, for the same reason the room record is: which voice notes a
 * person has heard is a map of who talks to them and what they made time for, and the next account
 * on the same device inherits nothing of it. A stored copy naming a different account is the
 * caller's to discard, not this store's to merge.
 */
object VoiceNoteListenedStore {

    private const val DIR_NAME = "voice-note-listened"
    private const val FILE_NAME = "listened.properties"

    private const val ACCOUNT = "account"
    private const val COUNT = "count"
    private const val LISTENED = "listened"

    /** The persisted record: which account's marks these are, and the message ids themselves. */
    data class Stored(val accountId: Id, val listened: Set<Id>)

    /** Persists the listened set for `accountId`, replacing whatever was held. */
    fun save(baseDir: File, accountId: Id, listened: Set<Id>) {
        val properties = Properties()
        properties.setProperty(ACCOUNT, accountId.value)
        properties.setProperty(COUNT, listened.size.toString())
        for ((index, messageId) in listened.withIndex()) {
            properties.setProperty("$LISTENED.$index", messageId.value)
        }
        File(dir(baseDir), FILE_NAME).writer().use { properties.store(it, null) }
    }

    /**
     * Reads the persisted record, or null on a first run. An unreadable file reads as null rather
     * than throwing: the session simply starts with every note unmarked, which is the safe floor —
     * a note wrongly drawn unheard is a note played again, while a note wrongly drawn heard is a
     * note never played. An entry that does not parse as an id is dropped while the rest survive —
     * one corrupt line must not cost every mark.
     */
    fun load(baseDir: File): Stored? {
        val file = File(dir(baseDir), FILE_NAME)
        if (!file.isFile) return null
        val properties = Properties()
        return try {
            file.reader().use { properties.load(it) }
            val account = idOf(properties.getProperty(ACCOUNT)) ?: return null
            val count = properties.getProperty(COUNT)?.toIntOrNull() ?: return null
            val listened = LinkedHashSet<Id>(count)
            for (index in 0 until count) {
                idOf(properties.getProperty("$LISTENED.$index"))?.let { listened.add(it) }
            }
            Stored(account, listened)
        } catch (_: Exception) {
            null
        }
    }

    /** Removes the persisted record (a different account's copy must not survive a sign-out). */
    fun clear(baseDir: File) {
        File(dir(baseDir), FILE_NAME).delete()
    }

    /** The store's directory under [baseDir]; created on first use. */
    private fun dir(baseDir: File): File = File(baseDir, DIR_NAME).apply { mkdirs() }

    /** Parses a persisted id leniently, or null when the text is not one. */
    private fun idOf(text: String?): Id? = when (text) {
        null -> null
        else -> when (val result = tryParseId(text)) {
            is IdParseResult.Ok -> result.id
            is IdParseResult.Fail -> null
        }
    }
}
