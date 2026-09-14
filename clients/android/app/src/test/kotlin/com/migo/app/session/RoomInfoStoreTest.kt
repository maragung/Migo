package com.migo.app.session

import com.migo.core.protocol.RoomKind
import com.migo.core.protocol.RoomRole
import com.migo.core.wire.Id
import com.migo.core.wire.parseId
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The followed-room record's round trip, its account scoping, and its failure floors — the same
 * properties the web client's room-info store holds, pinned in this app's phone-free idiom.
 *
 * What is tested is the store's own contract, not the shell that keeps it: the bridge and the
 * room's public facts survive a save/load pair exactly; a stored copy naming a different account
 * is the caller's to discard (the store hands back the account so it can); a corrupt entry or an
 * unreadable file reads as absent rather than throwing, because a session that started without
 * remembered rooms is a floor, not a failure.
 */
class RoomInfoStoreTest {

    private val account: Id = parseId("0123456789ABCDEFGHJKMNPQRS")
    private val other: Id = parseId("0123456789ABCDEFGHJKMNPQRT")

    private val room = RoomRecord(
        roomId = parseId("0123456789ABCDEFGHJKMNPQRV"),
        conversationId = parseId("0123456789ABCDEFGHJKMNPQRW"),
        name = "The Long Room",
        kind = RoomKind.Public,
        publicId = "long-room",
        topic = "the sea, mostly",
        memberCount = 33L,
        onlineCount = 2L,
        maxMembers = 64L,
        myRole = RoomRole.Moderator,
    )

    @Test
    fun aSavedRecordSurvivesTheRoundTrip() {
        val base = Files.createTempDirectory("room-info").toFile()
        RoomInfoStore.save(base, account, listOf(room))
        val stored = RoomInfoStore.load(base)
        assertEquals(account, stored?.accountId)
        // Every field the restart re-derives the shell from: the bridge both ways, the name the
        // row and the title draw, and the counts and role the header seeds from.
        assertEquals(listOf(room), stored?.rooms)
    }

    @Test
    fun aLoadWithNothingStoredReadsAsNull() {
        val base = Files.createTempDirectory("room-info").toFile()
        assertNull(RoomInfoStore.load(base))
    }

    @Test
    fun aClearRemovesTheRecord() {
        val base = Files.createTempDirectory("room-info").toFile()
        RoomInfoStore.save(base, account, listOf(room))
        RoomInfoStore.clear(base)
        assertNull(RoomInfoStore.load(base))
    }

    @Test
    fun aSaveReplacesWhateverWasHeld() {
        val base = Files.createTempDirectory("room-info").toFile()
        RoomInfoStore.save(base, account, listOf(room))
        val fresh = room.copy(conversationId = parseId("0123456789ABCDEFGHJKMNPQRX"))
        RoomInfoStore.save(base, other, listOf(fresh))
        // The store keeps one record, scoped to the account that last wrote it: a second account
        // writing does not merge with the first's rooms, and the account named is the reader's
        // test for whether the copy is theirs at all.
        val stored = RoomInfoStore.load(base)
        assertEquals(other, stored?.accountId)
        assertEquals(listOf(fresh), stored?.rooms)
    }

    @Test
    fun anUnreadableFileReadsAsNull() {
        val base = Files.createTempDirectory("room-info").toFile()
        RoomInfoStore.save(base, account, listOf(room))
        // A truncated or binary file is a first run as far as the session is concerned.
        val file = base.resolve("room-info").resolve("rooms.properties")
        Files.write(file.toPath(), ByteArray(16) { 0xFF.toByte() })
        assertNull(RoomInfoStore.load(base))
    }

    @Test
    fun aRecordProjectingFromASummaryKeepsTheBridge() {
        val summary = room.summary()
        val projected = RoomInfoStore.record(summary, room.conversationId)
        assertEquals(room, projected)
        // The summary the record rebuilds is the summary it came from, for the paths that read
        // the room cache's shape after a restart.
        assertEquals(summary, projected.summary())
        assertTrue("an unheld role reads back as absent, not as Unknown-as-a-fact", projected.myRole != RoomRole.Unknown)
    }
}
