package com.migo.app.session

import com.migo.core.wire.Id
import com.migo.core.wire.parseId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * What a fresh-session reset owes the shell, pinned the way the web suite pins its providers'
 * projections (section 158). The two debts are the ones the web client's `resetNonce` drives and
 * this build's `onReset` hook now drives too:
 *
 *   1. **The conversation list always re-reads.** A reset that left the list alone would show a
 *      stale inventory behind a live connection -- a row for a conversation that was left from
 *      another device, no row for one that was started.
 *   2. **The open chat syncs only the gap.** The catch-up starts at the watermark the shell
 *      already holds, never from the top, because a full resync of a held conversation is the
 *      one thing section 158 forbids -- on the wire it is indistinguishable from a client that
 *      had nothing. No chat open, or nothing held, and the reload is the whole debt.
 *
 * The actions are injected doubles that record, so the test observes the hook's effect rather
 * than re-deriving it: this is the `onReset` wiring's own shape, with the network calls swapped
 * for a pen and paper.
 */
class ResetResyncTest {

    private val conversation: Id = parseId("0123456789ABCDEFGHJKMNPQRS")
    private val other: Id = parseId("0123456789ABCDEFGHJKMNPQRT")

    private class Recorder {
        var reloads = 0
        val catchUps = mutableListOf<Pair<Id, Long>>()
    }

    private fun resync(with: Recorder) = ResetResync(
        reloadConversations = { with.reloads += 1 },
        catchUpOpenChat = { conversationId, haveSeq -> with.catchUps += conversationId to haveSeq },
    )

    @Test
    fun aResetReloadsTheConversationList() {
        val seen = Recorder()
        resync(seen).run(openConversationId = null, heldSeq = { null })
        assertEquals(1, seen.reloads)
        assertTrue("no chat open means no catch-up to do", seen.catchUps.isEmpty())
    }

    @Test
    fun theOpenChatCatchesUpFromItsWatermarkNeverFromTheTop() {
        val seen = Recorder()
        resync(seen).run(openConversationId = conversation, heldSeq = { 41L })
        assertEquals(1, seen.reloads)
        // The fetch asks for exactly the messages the outage cost: seq 41 and beyond, not the
        // conversation's whole history.
        assertEquals(listOf(conversation to 41L), seen.catchUps)
    }

    @Test
    fun aChatWithNothingHeldStartsFromTheTop() {
        val seen = Recorder()
        resync(seen).run(openConversationId = conversation, heldSeq = { null })
        assertEquals(listOf(conversation to ResetResync.FROM_THE_TOP), seen.catchUps)
    }

    @Test
    fun theWatermarkIsReadForTheOpenConversationOnly() {
        val seen = Recorder()
        resync(seen).run(openConversationId = conversation, heldSeq = { held -> if (held == other) 99L else null })
        // The other conversation's watermark is nobody's business here -- the open chat's own
        // held tail is the only floor the catch-up may start from.
        assertEquals(listOf(conversation to ResetResync.FROM_THE_TOP), seen.catchUps)
    }
}
