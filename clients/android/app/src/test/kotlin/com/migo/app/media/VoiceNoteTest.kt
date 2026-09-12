package com.migo.app.media

import com.migo.core.wire.Id
import com.migo.core.wire.parseId
import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The pure halves of a voice note, pinned the way the web suite pins its own recorder
 * (`clients/web/src/lib/migo/voice.ts`): the amplitude-to-bar scale, the fold that turns a
 * recording's samples into the fifty bars a bubble renders, and the draft store that keeps a
 * cancelled or interrupted note recoverable. These are the pieces a phone is not needed to prove —
 * the microphone half is the platform's own word.
 */
class VoiceNoteTest {

    private val conversation: Id = parseId("0123456789ABCDEFGHJKMNPQRS")

    // amplitudeToBar: the platform's 0–32767 peak becomes the 0–255 byte the message carries.

    @Test
    fun silenceIsZeroAndFullScaleIsMaxBar() {
        assertEquals(0, amplitudeToBar(0))
        assertEquals(255, amplitudeToBar(32767))
    }

    @Test
    fun amplitudeScalesLinearly() {
        assertEquals(127, amplitudeToBar(16383))
        assertEquals(1, amplitudeToBar(129))
    }

    @Test
    fun hostileAmplitudesReadAsSilence() {
        assertEquals(0, amplitudeToBar(-1))
        assertEquals(0, amplitudeToBar(32768))
        assertEquals(0, amplitudeToBar(Int.MIN_VALUE))
        assertEquals(0, amplitudeToBar(Int.MAX_VALUE))
    }

    // downsampleWaveform: the fold the web build performs on its own samples — max per bucket,
    // never the average, because a loud syllable between two quiet ones must survive.

    @Test
    fun noSamplesFoldToSilence() {
        val bars = downsampleWaveform(IntArray(0))
        assertEquals(WAVEFORM_BARS, bars.size)
        assertTrue(bars.all { it == 0.toByte() })
    }

    @Test
    fun fewerSamplesThanBarsPadWithSilence() {
        val bars = downsampleWaveform(intArrayOf(10, 200, 30))
        assertEquals(WAVEFORM_BARS, bars.size)
        assertEquals(10.toByte(), bars[0])
        assertEquals(200.toByte(), bars[1])
        assertEquals(30.toByte(), bars[2])
        assertTrue(bars.drop(3).all { it == 0.toByte() })
    }

    @Test
    fun eachBucketKeepsItsLoudestSample() {
        val samples = IntArray(200) { if (it % 4 == 0) 250 else 5 }
        val bars = downsampleWaveform(samples, barCount = 10)
        assertEquals(10, bars.size)
        // Every bucket of 20 samples contains a 250 at its first position, and 5s around it; the
        // peak is the bucket's word, so every bar is 250 and none is the 66 an average would give.
        assertTrue(bars.all { it == 250.toByte() })
    }

    @Test
    fun outOfRangeSamplesReadAsSilenceNotGarbage() {
        val bars = downsampleWaveform(intArrayOf(-5, 300, 40))
        assertEquals(0.toByte(), bars[0])
        assertEquals(0.toByte(), bars[1])
        assertEquals(40.toByte(), bars[2])
    }

    @Test
    fun aFullRecordingFoldsIntoItsBarCount() {
        // One loud moment in a thousand samples of quiet: the fold must land it in the bucket it
        // was spoken in — bar 5 of 10 — and leave the rest of the minute as the silence it was.
        val samples = IntArray(1_000) { if (it == 550) 200 else 0 }
        val bars = downsampleWaveform(samples, barCount = 10)
        assertEquals(10, bars.size)
        assertEquals(200.toByte(), bars[5])
        assertTrue(bars.withIndex().all { (i, bar) -> i == 5 || bar == 0.toByte() })
    }

    // VoiceNoteDrafts: the store's round trip, and the two ways a draft stops existing.

    private fun tempDir(): File = Files.createTempDirectory("voice-note-drafts").toFile()

    private fun draftIn(baseDir: File, amplitudes: IntArray = intArrayOf(3, 200, 77)): VoiceNoteDraft {
        val file = VoiceNoteDrafts.fileFor(baseDir, conversation, "audio/ogg")
        file.parentFile!!.mkdirs()
        file.writeBytes(byteArrayOf(1, 2, 3))
        return VoiceNoteDraft(conversation, file, "audio/ogg", 4_321L, amplitudes)
    }

    @Test
    fun aSavedDraftLoadsBackIdentical() {
        val dir = tempDir()
        val draft = draftIn(dir)
        VoiceNoteDrafts.save(dir, draft)
        val loaded = VoiceNoteDrafts.load(dir, conversation)!!
        assertEquals(draft.conversationId, loaded.conversationId)
        assertEquals(draft.file, loaded.file)
        assertEquals(draft.mimeType, loaded.mimeType)
        assertEquals(draft.durationMs, loaded.durationMs)
        assertArrayEquals(draft.amplitudes, loaded.amplitudes)
        // The folded waveform survives the round trip byte for byte, because the fold is
        // deterministic and the store's amplitudes are exactly what was saved.
        assertArrayEquals(draft.waveform, loaded.waveform)
    }

    @Test
    fun aDraftWithoutAmplitudesLoadsWithNone() {
        val dir = tempDir()
        val draft = draftIn(dir, amplitudes = IntArray(0))
        VoiceNoteDrafts.save(dir, draft)
        val loaded = VoiceNoteDrafts.load(dir, conversation)!!
        assertEquals(0, loaded.amplitudes.size)
        assertNull(loaded.waveform)
    }

    @Test
    fun aDescriptorWithoutItsFileIsNoDraft() {
        val dir = tempDir()
        val draft = draftIn(dir)
        VoiceNoteDrafts.save(dir, draft)
        draft.file.delete()
        assertNull(VoiceNoteDrafts.load(dir, conversation))
    }

    @Test
    fun noDescriptorIsNoDraft() {
        val dir = tempDir()
        assertNull(VoiceNoteDrafts.load(dir, conversation))
    }

    @Test
    fun clearRemovesBothHalvesAndTheLoadSaysSo() {
        val dir = tempDir()
        val draft = draftIn(dir)
        VoiceNoteDrafts.save(dir, draft)
        VoiceNoteDrafts.clear(dir, conversation, draft.mimeType)
        assertNull(VoiceNoteDrafts.load(dir, conversation))
        assertFalse(draft.file.isFile)
        assertFalse(File(VoiceNoteDrafts.dir(dir), "${conversation.value}.properties").isFile)
    }

    @Test
    fun savingAgainOverwritesRatherThanAccumulating() {
        val dir = tempDir()
        VoiceNoteDrafts.save(dir, draftIn(dir, amplitudes = intArrayOf(1, 2, 3)))
        val second = draftIn(dir, amplitudes = intArrayOf(9))
        VoiceNoteDrafts.save(dir, second)
        val loaded = VoiceNoteDrafts.load(dir, conversation)!!
        assertArrayEquals(intArrayOf(9), loaded.amplitudes)
    }
}
