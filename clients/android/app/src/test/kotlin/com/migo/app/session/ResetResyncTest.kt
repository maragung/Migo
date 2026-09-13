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
 *   2. **The held conversations sync only the gap, the open chat first.** The catch-up starts at
 *      the watermark the shell already holds, never from the top, because a full resync of a held
 *      conversation is the one thing section 158 forbids -- on the wire it is indistinguishable
 *      from a client that had nothing. The chat on screen goes before the background ones (the
 *      reader is watching it), and a background conversation with nothing held has no gap to
 *      sync -- its first open fetches fresh.
 *
 * The actions are injected doubles that record, so the test observes the hook's effect rather
 * than re-deriving it: this is the `onReset` wiring's own shape, with the network calls swapped
 * for a pen and paper.
 */
class ResetResyncTest {

    private val conversation: Id = parseId("0123456789ABCDEFGHJKMNPQRS")
    private val other: Id = parseId("0123456789ABCDEFGHJKMNPQRT")
    private val third: Id = parseId("0123456789ABCDEFGHJKMNPQRV")

    private class Recorder {
        var reloads = 0
        val catchUps = mutableListOf<Pair<Id, Long>>()
    }

    private fun resync(with: Recorder) = ResetResync(
        reloadConversations = { with.reloads += 1 },
        catchUpConversation = { conversationId, haveSeq -> with.catchUps += conversationId to haveSeq },
    )

    @Test
    fun aResetReloadsTheConversationList() {
        val seen = Recorder()
        resync(seen).run(openConversationId = null, heldSeq = { null }, conversations = { emptyList() })
        assertEquals(1, seen.reloads)
        assertTrue("no held conversation means no catch-up to do", seen.catchUps.isEmpty())
    }

    @Test
    fun theOpenChatCatchesUpFromItsWatermarkNeverFromTheTop() {
        val seen = Recorder()
        resync(seen).run(openConversationId = conversation, heldSeq = { 41L }, conversations = { emptyList() })
        assertEquals(1, seen.reloads)
        // The fetch asks for exactly the messages the outage cost: seq 41 and beyond, not the
        // conversation's whole history.
        assertEquals(listOf(conversation to 41L), seen.catchUps)
    }

    @Test
    fun aChatWithNothingHeldStartsFromTheTop() {
        val seen = Recorder()
        resync(seen).run(openConversationId = conversation, heldSeq = { null }, conversations = { emptyList() })
        assertEquals(listOf(conversation to ResetResync.FROM_THE_TOP), seen.catchUps)
    }

    @Test
    fun theWatermarkIsReadForTheOpenConversationOnly() {
        val seen = Recorder()
        resync(seen).run(
            openConversationId = conversation,
            heldSeq = { held -> if (held == other) 99L else null },
            conversations = { emptyList() },
        )
        // The other conversation's watermark is nobody's business here -- the open chat's own
        // held tail is the only floor the catch-up may start from.
        assertEquals(listOf(conversation to ResetResync.FROM_THE_TOP), seen.catchUps)
    }

    @Test
    fun backgroundConversationsCatchUpAfterTheOpenChat() {
        val seen = Recorder()
        resync(seen).run(
            openConversationId = conversation,
            // The open chat holds 41, `other` holds 12, and `third` holds nothing — the point
            // being that a listed conversation with no held cursor stays out of the run.
            heldSeq = { held -> if (held == other) 12L else if (held == conversation) 41L else null },
            conversations = { listOf(other, third, conversation) },
        )
        // Section 158's "sync visible conversations first": the chat on screen syncs before the
        // ones behind it, and the open conversation is not fetched twice for appearing in the
        // list as well.
        assertEquals(
            listOf(conversation to 41L, other to 12L),
            seen.catchUps,
        )
    }

    @Test
    fun aBackgroundConversationWithNothingHeldIsSkipped() {
        val seen = Recorder()
        resync(seen).run(
            openConversationId = null,
            heldSeq = { held -> if (held == other) null else 7L },
            conversations = { listOf(other, third) },
        )
        // Nothing was ever held for `other`, so it has no gap to sync -- its first open fetches
        // fresh. `third` holds a cursor, so it catches up even with no chat on screen.
        assertEquals(listOf(third to 7L), seen.catchUps)
    }
}
