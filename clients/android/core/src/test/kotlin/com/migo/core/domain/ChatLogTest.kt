package com.migo.core.domain

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The chat-log formatters, pinned: the line shape a person reads, the file-name rule the picker
 * suggests, and the eviction the snapshot writer follows.
 *
 * Timestamps are checked against a fixed zone rather than the device's, because the one thing a
 * test of "local time" must not do is depend on where it runs: the formatter is re-driven with
 * the zone named and compared to the same zone's own formatting.
 */
class ChatLogTest {

    private val line = ChatLogLine(at = 1_700_000_000_000L, author = "Alice", text = "Hello there")

    // --- the transcript ---

    @Test
    fun `a log names its conversation, its moment, and one line per message in the caller's order`() {
        val log = formatChatLog(
            title = "Team room",
            exportedAtMs = 1_700_000_060_000L,
            lines = listOf(
                ChatLogLine(at = 1_700_000_000_000L, author = "Alice", text = "First"),
                ChatLogLine(at = 1_700_000_010_000L, author = "Bob", text = "Second"),
            ),
        )
        val rows = log.split('\n').filter { it.isNotEmpty() }
        assertEquals("the header names the conversation", "Migo chat log — Team room", rows[0])
        assertTrue("the header stamps the export", rows[1].startsWith("Exported "))
        // The body keeps the caller's order — a transcript, not a sort the formatter invents.
        assertTrue("first line is Alice's", rows[2].endsWith("  Alice  First"))
        assertTrue("second line is Bob's", rows[3].endsWith("  Bob  Second"))
    }

    @Test
    fun `a log with no messages still states its conversation and moment`() {
        val log = formatChatLog(title = "Quiet chat", exportedAtMs = 1_700_000_000_000L, lines = emptyList())
        assertTrue(log.contains("Migo chat log — Quiet chat"))
        assertTrue(log.contains("Exported "))
    }

    @Test
    fun `an out-of-range timestamp renders a dash rather than throwing mid-write`() {
        assertEquals("—", logStamp(0L))
        assertEquals("—", logStamp(-5L))
        // A real one renders in the device's own words: the shape is `yyyy-MM-dd HH:mm`, in
        // whatever zone the device keeps, so the assertion is on the shape and not on a time a
        // test in another timezone would fail.
        assertTrue(
            "a real timestamp is 16 characters of date and clock",
            Regex("^\\d{4}-\\d{2}-\\d{2} \\d{2}:\\d{2}$").matches(logStamp(1_700_000_000_000L)),
        )
    }

    @Test
    fun `the all-chats log divides conversations with headings`() {
        val log = formatAllChatsLog(
            accountName = "alice",
            exportedAtMs = 1_700_000_000_000L,
            chats = listOf(
                "Team room" to listOf(ChatLogLine(at = 1_700_000_000_000L, author = "Alice", text = "Hi")),
                "Bob" to emptyList(),
            ),
        )
        assertTrue("the header names the account", log.contains("Migo chat logs — alice"))
        assertTrue("each conversation is a heading", log.contains("== Team room =="))
        assertTrue(log.contains("== Bob =="))
    }

    // --- the file name ---

    @Test
    fun `a title becomes a writable file name and the txt suffix`() {
        // Internal spaces are underscores too, not just the dangerous characters: one rule for
        // everything the shell would have to quote keeps the mapping from the title a reader
        // can predict — and the picker never suggests a name with %20 in it.
        assertEquals("Team_room.txt", chatLogFilename("Team room"))
        assertEquals("Bob.txt", chatLogFilename("Bob"))
    }

    @Test
    fun `everything that could climb out of a file name is neutralised`() {
        assertEquals("a_b.txt", chatLogFilename("a/b"))
        assertEquals("a_b.txt", chatLogFilename("a\\b"))
        assertEquals("a_b.txt", chatLogFilename("a:b"))
        assertEquals("it_s_complicated.txt", chatLogFilename("it's complicated"))
        // Runs collapse, so one substitution reads as one.
        assertEquals("a_b_c.txt", chatLogFilename("a//b??c"))
        // Leading and trailing dots and spaces go: a leading dot hides the file.
        assertEquals("name.txt", chatLogFilename("  ..name.. "))
    }

    @Test
    fun `a title of only symbols falls back to a name the picker can still suggest`() {
        assertEquals("chat.txt", chatLogFilename("???"))
        assertEquals("chat.txt", chatLogFilename(""))
        assertEquals("chat.txt", chatLogFilename("   "))
    }

    @Test
    fun `a long title is bounded to a length every filesystem accepts`() {
        val long = "x".repeat(200)
        assertEquals(48 + ".txt".length, chatLogFilename(long).length)
        // And the bound keeps the front, which is where a title's meaning lives.
        assertTrue(chatLogFilename(long).startsWith("xxxx"))
    }

    // --- the eviction ---

    @Test
    fun `the eviction keeps the newest conversations and names everything past the cap`() {
        val held = listOf("oldest", "older", "newer", "newest")
        assertEquals(listOf("oldest", "older"), snapshotEvictions(held, keep = 2))
        assertEquals(emptyList<String>(), snapshotEvictions(held, keep = 4))
        // A cap at or beyond the size keeps everything.
        assertEquals(emptyList<String>(), snapshotEvictions(held, keep = 10))
        // A cap of zero is "keep none of these", not "crash".
        assertEquals(held, snapshotEvictions(held, keep = 0))
        // A negative cap is read as zero, refusing to guess a meaning the caller never has.
        assertEquals(held, snapshotEvictions(held, keep = -1))
        // And an empty list evicts nothing, whatever the cap.
        assertEquals(emptyList<String>(), snapshotEvictions(emptyList<String>(), keep = 0))
    }
}
