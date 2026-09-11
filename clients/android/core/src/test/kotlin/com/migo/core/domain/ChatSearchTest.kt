package com.migo.core.domain

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The in-thread search, pinned to the web client's contract: blank query is no filter, matching is
 * a case-insensitive substring, only text bodies ever match, and results keep the thread's order.
 */
class ChatSearchTest {

    // --- the matcher ---

    @Test
    fun `a case-insensitive substring matches`() {
        assertTrue(chatSearchMatches("hello", "Hello there"))
        assertTrue(chatSearchMatches("HELLO", "well, hello there"))
        assertTrue(chatSearchMatches("  hello  ", "says hello mid-sentence"))
        assertFalse(chatSearchMatches("hello!", "hello"))
        assertFalse(chatSearchMatches("hello", "hell"))
    }

    @Test
    fun `a blank or whitespace-only query matches nothing`() {
        assertFalse(chatSearchMatches("", "anything"))
        assertFalse(chatSearchMatches("   ", "anything"))
        assertFalse(chatSearchMatches("\t\n", "anything"))
    }

    // --- the filter ---

    /** One held message, reduced to the two facts the filter asks for: an id and its text body. */
    private data class Held(val id: Int, val text: String?)

    private val thread = listOf(
        Held(1, "The meeting moved to three"),
        Held(2, null), // an attachment — a caption, never a text body
        Held(3, "see the attached notes"),
        Held(4, null), // an unsupported body from a newer peer
        Held(5, "Three it is"),
    )

    @Test
    fun `a blank query returns the original list untouched`() {
        // Same reference, not a copy: the caller's remember keys stay stable while the field
        // sits empty, so the thread draws exactly as it did before search was opened.
        assertSame(thread, filterChatSearch(thread, "") { it.text })
        assertSame(thread, filterChatSearch(thread, "   ") { it.text })
    }

    @Test
    fun `a query keeps the thread order and drops what does not match`() {
        assertEquals(
            listOf(Held(1, "The meeting moved to three"), Held(5, "Three it is")),
            filterChatSearch(thread, "three") { it.text },
        )
    }

    @Test
    fun `matching is case-insensitive through the filter too`() {
        assertEquals(listOf(Held(3, "see the attached notes")), filterChatSearch(thread, "ATTACHED") { it.text })
    }

    @Test
    fun `messages without a text body never match`() {
        // A null textOf is an attachment or an unsupported body — the web client's ContentType.Text
        // check. Even a query that echoes a caption's own words cannot find that message, because
        // the caller hands the filter null for a caption, never the caption itself.
        assertTrue(filterChatSearch(thread, "three") { it.text }.none { it.id == 2 || it.id == 4 })
        assertTrue(filterChatSearch(listOf(Held(2, null)), "photo") { it.text }.isEmpty())
    }

    @Test
    fun `a query that matches nothing yields an empty list, distinct from the blank-query pass-through`() {
        val none = filterChatSearch(thread, "zebra") { it.text }
        assertTrue(none.isEmpty())
        assertNotSame(thread, none)
    }
}
