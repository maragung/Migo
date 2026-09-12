package com.migo.app.media

import android.content.Context
import android.media.MediaRecorder
import android.os.Build
import android.os.SystemClock
import java.io.File

/**
 * The container this device records a voice note into — the same choice [VoiceNoteRecorder] makes
 * internally, stated for the caller that must name the draft file *before* the recorder exists.
 */
fun voiceNoteContainerMime(): String = if (Build.VERSION.SDK_INT >= 29) "audio/ogg" else "audio/mp4"

/**
 * One voice-note recording, as the recorder hands it to the uploader.
 *
 * The container is what the device actually produced — Ogg Opus on a device that can record it,
 * AAC in an MPEG-4 file below that — never a type the recorder was merely asked for, the same
 * rule the web client's recorder keeps. [durationMs] is measured off the wall clock between start
 * and stop with every paused span excluded, because the finished file's own duration header is not
 * readable until after the recorder finalises it.
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
 * where a reused one throws at the next [start]. The recording is written *incrementally* into
 * [outFile] — a path the caller owns, in the app's private storage — so brief 179's rule that a
 * recording is never held whole in memory is the recorder's own behaviour, and so an app death
 * mid-recording leaves the bytes on disk for the draft store to find rather than nowhere at all.
 *
 * # The container, per API level
 *
 * On API 29 and up the recording is Ogg Opus: Opus is the speech codec the web client's recorder
 * already produces (in a WebM), an Ogg page stream survives an ungraceful stop — a player reads
 * every page that was written — and the server's sniffer accepts `audio/ogg`. Below 29 there is
 * no Opus encoder, so the recording is AAC in an MPEG-4 file as before; that container finalises
 * only at [stop], so a draft recovered after an ungraceful death on those devices is judged by
 * the platform's own parser and discarded when it cannot be played.
 */
class VoiceNoteRecorder(context: Context, outFile: File) {

    /** The claimed container type, for the uploader and the message. */
    val mimeType: String

    private val recorder: MediaRecorder = if (Build.VERSION.SDK_INT >= 31) {
        MediaRecorder(context)
    } else {
        @Suppress("DEPRECATION")
        MediaRecorder()
    }
    private val file = outFile

    private var startedAt: Long = 0
    private var pausedTotalMs: Long = 0
    private var pauseStartedAt: Long = 0

    /** Whether the recording is between [pause] and [resume]: the clock and the cap both stand still. */
    var isPaused: Boolean = false
        private set

    init {
        if (Build.VERSION.SDK_INT >= 29) {
            mimeType = "audio/ogg"
            recorder.setAudioSource(MediaRecorder.AudioSource.MIC)
            recorder.setOutputFormat(MediaRecorder.OutputFormat.OGG)
            recorder.setAudioEncoder(MediaRecorder.AudioEncoder.OPUS)
            // Voice, not music: mono Opus at a speech-friendly rate and bitrate, the same posture
            // the web recorder's encoder settings keep, so a five-minute note is a small upload.
            recorder.setAudioEncodingBitRate(24_000)
            recorder.setAudioSamplingRate(24_000)
            recorder.setAudioChannels(1)
        } else {
            mimeType = "audio/mp4"
            recorder.setAudioSource(MediaRecorder.AudioSource.MIC)
            recorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            recorder.setAudioEncodingBitRate(32_000)
            recorder.setAudioSamplingRate(24_000)
            recorder.setAudioChannels(1)
        }
        recorder.setOutputFile(file.absolutePath)
        recorder.prepare()
        recorder.start()
        startedAt = SystemClock.elapsedRealtime()
    }

    /**
     * How long the recording has run, excluding paused spans — for the composer's timer and the
     * cap's auto-stop, both of which must not count a pause against the speaker.
     */
    fun elapsedMs(): Long {
        val paused = if (isPaused) {
            pausedTotalMs + (SystemClock.elapsedRealtime() - pauseStartedAt)
        } else {
            pausedTotalMs
        }
        return SystemClock.elapsedRealtime() - startedAt - paused
    }

    /**
     * The maximum amplitude sampled since the last call, as the platform reports it (0–32767).
     *
     * Called once per tick by the owner, only while recording is live: the platform resets its
     * own peak on every read, so this is the sampling — there is no analyser graph to build.
     */
    fun maxAmplitude(): Int = try {
        recorder.maxAmplitude
    } catch (_: RuntimeException) {
        0
    }

    /** Pauses the capture; the file stays open and the paused span is kept out of [elapsedMs]. */
    fun pause() {
        if (isPaused) return
        recorder.pause()
        pauseStartedAt = SystemClock.elapsedRealtime()
        isPaused = true
    }

    /** Resumes a paused capture. */
    fun resume() {
        if (!isPaused) return
        pausedTotalMs += SystemClock.elapsedRealtime() - pauseStartedAt
        recorder.resume()
        isPaused = false
    }

    /**
     * Finalises the recording and hands back the note. The file is *not* deleted: the caller owns
     * the draft's fate now — a cancelled note stays recoverable for the undo window, and only the
     * draft store's clear actually removes the bytes.
     */
    fun stop(): FinishedNote {
        recorder.stop()
        recorder.release()
        return FinishedNote(file.readBytes(), mimeType, elapsedMs())
    }

    /**
     * Throws the recording away *as a recording* — the capture is stopped and released, the file
     * is finalised but left in place, because brief 179's cancel rule keeps a cancelled note as a
     * draft the undo window can still send. Deleting the bytes is the draft store's decision, on
     * the undo window's timeout, not the recorder's.
     */
    fun cancel() {
        try {
            recorder.stop()
        } catch (_: RuntimeException) {
            // A stop before any audio was captured is the one state stop() refuses; the caller
            // discards the file either way, so the refusal has nothing to protect.
        }
        recorder.release()
    }
}
