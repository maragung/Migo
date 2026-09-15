package com.migo.app.media

import com.migo.core.wire.Id
import com.migo.core.wire.parseId
import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The listened-mark store, pinned the same way the draft store is in [VoiceNoteTest]: the
 * persistence half of brief section 179's receiver-local rule, which a plain JVM can prove without
 * a phone. What cannot be proven here — and is deliberately not attempted — is anything about a
 * `Played` receipt; the store has no such concept to test, because the wire has no such variant and
 * this device never sends one.
 */
class VoiceNoteListenedTest {

    private val account: Id = parseId("0123456789ABCDEFGHJKMNPQRS")
    private val other: Id = parseId("3111111111ABCDEFGHJKMNPQRS")

    private fun tempDir(): File = Files.createTempDirectory("voice-note-listened").toFile()

    @Test
    fun aSavedSetLoadsBackIdentical() {
        val dir = tempDir()
        val heard = setOf(
            parseId("1000000000ABCDEFGHJKMNPQRS"),
            parseId("2000000000ABCDEFGHJKMNPQRS"),
        )
        VoiceNoteListenedStore.save(dir, account, heard)
        val loaded = VoiceNoteListenedStore.load(dir)!!
        assertEquals(account, loaded.accountId)
        assertEquals(heard, loaded.listened)
    }

    @Test
    fun anEmptySetPersistsAsAnEmptyLoad() {
        // The unmark-everything case: a save of nothing is a record worth keeping, not a delete —
        // the account's marks exist and are empty, which a later load must say rather than null.
        val dir = tempDir()
        VoiceNoteListenedStore.save(dir, account, emptySet())
        val loaded = VoiceNoteListenedStore.load(dir)!!
        assertEquals(account, loaded.accountId)
        assertTrue(loaded.listened.isEmpty())
    }

    @Test
    fun noFileIsNoRecord() {
        assertNull(VoiceNoteListenedStore.load(tempDir()))
    }

    @Test
    fun savingAgainOverwritesRatherThanAccumulating() {
        // Unmarking is a save of the smaller set, so the store must replace rather than merge —
        // a merge would resurrect every unmarked note on the next restart.
        val dir = tempDir()
        val first = setOf(
            parseId("1000000000ABCDEFGHJKMNPQRS"),
            parseId("2000000000ABCDEFGHJKMNPQRS"),
        )
        VoiceNoteListenedStore.save(dir, account, first)
        val second = setOf(parseId("2000000000ABCDEFGHJKMNPQRS"))
        VoiceNoteListenedStore.save(dir, account, second)
        val loaded = VoiceNoteListenedStore.load(dir)!!
        assertEquals(second, loaded.listened)
    }

    @Test
    fun theAccountTheMarksBelongToSurvivesTheRoundTrip() {
        // The record is account-scoped: a load must say whose marks these are, so the caller can
        // refuse a different account's copy rather than inherit it.
        val dir = tempDir()
        VoiceNoteListenedStore.save(dir, other, setOf(parseId("1000000000ABCDEFGHJKMNPQRS")))
        val loaded = VoiceNoteListenedStore.load(dir)!!
        assertEquals(other, loaded.accountId)
    }

    @Test
    fun anUnparsableEntryIsDroppedWhileTheRestSurvive() {
        val dir = tempDir()
        val heard = setOf(
            parseId("1000000000ABCDEFGHJKMNPQRS"),
            parseId("2000000000ABCDEFGHJKMNPQRS"),
        )
        VoiceNoteListenedStore.save(dir, account, heard)
        // Corrupt one line of the saved record by hand: the entry must read as absent, not as a
        // crash, and the marks around it must still come back.
        val file = File(File(dir, "voice-note-listened"), "listened.properties")
        val text = file.readText().replace("listened.0=", "listened.0=not-an-id-")
        file.writeText(text)
        val loaded = VoiceNoteListenedStore.load(dir)!!
        assertEquals(account, loaded.accountId)
        assertEquals(setOf(parseId("2000000000ABCDEFGHJKMNPQRS")), loaded.listened)
    }

    @Test
    fun clearRemovesTheRecordAndTheLoadSaysSo() {
        val dir = tempDir()
        VoiceNoteListenedStore.save(dir, account, setOf(parseId("1000000000ABCDEFGHJKMNPQRS")))
        VoiceNoteListenedStore.clear(dir)
        assertNull(VoiceNoteListenedStore.load(dir))
    }
}
