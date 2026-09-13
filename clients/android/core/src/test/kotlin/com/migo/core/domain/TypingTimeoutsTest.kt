package com.migo.core.domain

import com.migo.core.wire.Id
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The receiver's local typing timeout, in numbers: the cases brief section 15 names and the
 * one it takes for granted.
 *
 * The clock is a parameter, not `System.currentTimeMillis()`, so a four-second timeout is
 * tested in microseconds of arithmetic rather than seconds of sleeping — the same
 * hand-written-time discipline the wire tests keep.
 */
class TypingTimeoutsTest {

    private val conversation = Id("2".repeat(26))
    private val typer = Id("3".repeat(26))
    private val other = Id("4".repeat(26))
    private val ttl = 4_000L

    @Test
    fun `a start with no stop expires on its own`() {
        val timeouts = TypingTimeouts(ttl)
        timeouts.start(conversation, typer, now = 10_000)
        // Inside the timeout nothing is owed and nothing expires; the next
        // deadline is the millisecond the pair runs out.
        assertTrue(timeouts.expire(now = 10_000 + ttl - 1).isEmpty())
        assertEquals(1L, timeouts.nextDelay(now = 10_000 + ttl - 1))
        // At the deadline, with no Stop ever arriving, the pair is claimed.
        val expired = timeouts.expire(now = 10_000 + ttl)
        assertEquals(listOf(conversation to typer), expired)
        // And the claim is a claim: a second expiry has nothing left.
        assertTrue(timeouts.expire(now = 10_000 + ttl + 1).isEmpty())
        assertTrue(timeouts.isEmpty())
    }

    @Test
    fun `a refresh moves the deadline instead of expiring under a continuous typer`() {
        val timeouts = TypingTimeouts(ttl)
        timeouts.start(conversation, typer, now = 10_000)
        // The refresh, inside the first mark's lifetime.
        timeouts.start(conversation, typer, now = 10_000 + 3_000)
        // Past the first deadline but inside the refreshed one: still typing.
        assertTrue(timeouts.expire(now = 10_000 + 3_500).isEmpty())
        val expired = timeouts.expire(now = 10_000 + 3_000 + ttl)
        assertEquals(listOf(conversation to typer), expired)
    }

    @Test
    fun `a stop clears the deadline at once`() {
        val timeouts = TypingTimeouts(ttl)
        timeouts.start(conversation, typer, now = 10_000)
        timeouts.stop(conversation, typer)
        assertTrue(timeouts.isEmpty())
        assertTrue(timeouts.expire(now = 10_000 + ttl).isEmpty())
    }

    @Test
    fun `two typers expire independently and nextDelay names the earliest`() {
        val timeouts = TypingTimeouts(ttl)
        timeouts.start(conversation, typer, now = 10_000)
        timeouts.start(conversation, other, now = 12_000)
        assertEquals(4_000L, timeouts.nextDelay(now = 10_000))
        // Only the first deadline has passed.
        val expired = timeouts.expire(now = 14_000)
        assertEquals(listOf(conversation to typer), expired)
        // The other typer's deadline is what the ticker waits for now.
        assertEquals(2_000L, timeouts.nextDelay(now = 14_000))
        val rest = timeouts.expire(now = 12_000 + ttl)
        assertEquals(listOf(conversation to other), rest)
        assertNull(timeouts.nextDelay(now = 20_000))
    }
}
