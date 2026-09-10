package com.migo.app.media

import android.content.Context
import android.media.MediaRecorder
import android.os.Build
import android.os.SystemClock
import java.io.File

/**
 * One voice-note recording, as the recorder hands it to the uploader.
 *
 * The container is what the device actually produced — AAC in an MPEG-4 file, the `audio/mp4` the
 * server's sniffer accepts for a VoiceNote kind — never a type the recorder was merely asked for,
 * the same rule the web client's recorder keeps. [durationMs] is measured off the wall clock
 * between start and stop, because the finished file's own duration header is not readable until
 * after the recorder finalises it.
 */
class FinishedNote(
    val bytes: ByteArray,
    val mimeType: String,
    val durationMs: Long,
)

/**
 * The microphone half of a voice note: a thin, single-use wrapper around [MediaRecorder].
 *
 * Single-use on purpose — a `MediaRecorder` cannot be restarted after [stop], so a second
 * recording is a second instance, and the wrapper making that explicit is cheaper than the bug
 * where a reused one throws at the next [start]. The file is this app's own cache, deleted with
 * the read: the note's plaintext exists in memory for as long as the upload needs it and nowhere
 * else, the same no-store discipline the message plaintext keeps.
 */
class VoiceNoteRecorder(context: Context) {

    /** The claimed container type, for the uploader and the message. */
    val mimeType: String = "audio/mp4"

    private val recorder: MediaRecorder = if (Build.VERSION.SDK_INT >= 31) {
        MediaRecorder(context)
    } else {
        @Suppress("DEPRECATION")
        MediaRecorder()
    }
    private val file = File(context.cacheDir, "voice-note-${System.currentTimeMillis()}.m4a")
    private var startedAt: Long = 0

    init {
        // Voice, not music: a mono AAC stream at a speech-friendly rate. The encoder is the
        // platform's own — the same hardware path the call uses — and the bit rate is what keeps
        // a five-minute note a small upload rather than a small movie.
        recorder.setAudioSource(MediaRecorder.AudioSource.MIC)
        recorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
        recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
        recorder.setAudioEncodingBitRate(32_000)
        recorder.setAudioSamplingRate(24_000)
        recorder.setAudioChannels(1)
        recorder.setOutputFile(file.absolutePath)
        recorder.prepare()
        recorder.start()
        startedAt = SystemClock.elapsedRealtime()
    }

    /** How long the recording has run so far, for the composer's timer and the cap's auto-stop. */
    fun elapsedMs(): Long = SystemClock.elapsedRealtime() - startedAt

    /** Finishes the recording and hands back the note; the file is gone once the bytes are read. */
    fun stop(): FinishedNote {
        recorder.stop()
        recorder.release()
        val bytes = file.readBytes()
        file.delete()
        return FinishedNote(bytes, mimeType, elapsedMs())
    }

    /** Throws the recording away entirely; nothing was said if nobody will hear it. */
    fun cancel() {
        try {
            recorder.stop()
        } catch (_: RuntimeException) {
            // A stop before any audio was captured is the one state stop() refuses; the file is
            // being deleted either way, so the refusal has nothing to protect.
        }
        recorder.release()
        file.delete()
    }
}
