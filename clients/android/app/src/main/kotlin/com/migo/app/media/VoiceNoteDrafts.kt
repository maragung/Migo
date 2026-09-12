package com.migo.app.media

import com.migo.core.wire.Id
import java.io.File
import java.util.Base64
import java.util.Properties

/**
 * A recording that exists outside the recorder: a finalised (or streamable) file plus everything
 * the composer needs to present it — the two halves of brief 179's draft rule. A draft appears
 * when a recording is cancelled (kept for the undo window), stopped to preview, or left behind by
 * an app death mid-recording.
 */
data class VoiceNoteDraft(
    val conversationId: Id,
    val file: File,
    val mimeType: String,
    val durationMs: Long,
    /** The sampled amplitudes as 0–255 bars, or empty when none were sampled. */
    val amplitudes: IntArray,
) {
    /** The fixed-width waveform the message carries, folded from the sampled amplitudes. */
    val waveform: ByteArray?
        get() = if (amplitudes.isEmpty()) null else downsampleWaveform(amplitudes)
}

/**
 * The draft store: one voice-note draft per conversation, in the app's private files.
 *
 * The recording itself is written incrementally by [VoiceNoteRecorder] straight into this store's
 * file, so a draft exists the moment recording does. The descriptor beside it — a small
 * properties file, rewritten on the recording's own tick — carries the facts the composer needs
 * before anyone can read the audio back: which conversation, what container, how long it had run,
 * and the amplitudes sampled so far. An app death mid-recording therefore leaves both halves on
 * disk, and the next open of the conversation finds them.
 *
 * The store is deliberately dumb: it holds and describes bytes, and never judges them. Whether a
 * recovered file is playable is the platform's own question (an Ogg stream usually is; an MPEG-4
 * file without its finalising stop usually is not), asked by the caller before a draft is
 * offered as sendable.
 */
object VoiceNoteDrafts {

    private const val DIR_NAME = "voice-note-drafts"
    private const val CONVERSATION = "conversation"
    private const val MIME = "mime"
    private const val DURATION = "duration"
    private const val AMPLITUDES = "amplitudes"

    /** The store's directory under [baseDir]; created on first use. */
    fun dir(baseDir: File): File = File(baseDir, DIR_NAME).apply { mkdirs() }

    /** Where a conversation's recording is written — one draft per conversation, by design. */
    fun fileFor(baseDir: File, conversationId: Id, mimeType: String): File {
        val extension = if (mimeType == "audio/ogg") "ogg" else "m4a"
        return File(dir(baseDir), "${conversationId.value}.$extension")
    }

    /** Persists the descriptor beside the recording's own file. */
    fun save(baseDir: File, draft: VoiceNoteDraft) {
        val properties = Properties()
        properties.setProperty(CONVERSATION, draft.conversationId.value)
        properties.setProperty(MIME, draft.mimeType)
        properties.setProperty(DURATION, draft.durationMs.toString())
        if (draft.amplitudes.isNotEmpty()) {
            val bytes = ByteArray(draft.amplitudes.size)
            for (i in draft.amplitudes.indices) {
                bytes[i] = draft.amplitudes[i].coerceIn(0, 255).toByte()
            }
            properties.setProperty(AMPLITUDES, Base64.getEncoder().encodeToString(bytes))
        }
        File(dir(baseDir), "${draft.conversationId.value}.properties").apply {
            writer().use { properties.store(it, null) }
        }
    }

    /**
     * Reads the conversation's draft, or null when none is recorded. A descriptor whose recording
     * file has vanished is a draft that no longer exists, and reads back as null rather than as a
     * reference to nothing.
     */
    fun load(baseDir: File, conversationId: Id): VoiceNoteDraft? {
        val descriptor = File(dir(baseDir), "${conversationId.value}.properties")
        if (!descriptor.isFile) return null
        val properties = Properties()
        descriptor.reader().use { properties.load(it) }
        val mime = properties.getProperty(MIME) ?: return null
        val file = fileFor(baseDir, conversationId, mime)
        if (!file.isFile) return null
        val duration = properties.getProperty(DURATION)?.toLongOrNull() ?: 0L
        val amplitudes = properties.getProperty(AMPLITUDES)
            ?.let { encoded ->
                runCatching { Base64.getDecoder().decode(encoded) }.getOrNull()
            }
            ?.map { it.toInt() and 0xFF }
            ?.toIntArray()
            ?: IntArray(0)
        return VoiceNoteDraft(conversationId, file, mime, duration, amplitudes)
    }

    /** Removes the conversation's draft — both the bytes and the descriptor that describes them. */
    fun clear(baseDir: File, conversationId: Id, mimeType: String) {
        File(dir(baseDir), "${conversationId.value}.properties").delete()
        fileFor(baseDir, conversationId, mimeType).delete()
    }
}
