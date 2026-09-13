package com.migo.core.domain

import com.migo.core.protocol.MessageEvent
import com.migo.core.protocol.MessageKind
import com.migo.core.wire.Id
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The contiguous-sequence ledger in numbers: the cases section 158 names and the ones it takes
 * for granted, pinned the way the web suite pins the same machinery in the TypeScript SDK.
 *
 * The gap filler is an injected double that records, and the fills run on an
 * [Dispatchers.Unconfined] scope so a fill that does not suspend completes inline — the
 * hand-written-time discipline the wire tests keep, applied to scheduling instead: no test
 * sleeps to watch a fill fire. A fill that must be observed *in flight* suspends on a gate the
 * test opens, and the double can play a fetched page back through the tracker, which is exactly
 * what a real fill does when its page replays through the live path.
 */
class WatermarkTrackerTest {

    private val conversation: Id = Id("2".repeat(26))

    /**
     * One tracker under test plus the pen and paper: every fill ask is recorded, a gate holds a
     * fill in flight when the test wants one, and the player is what the fill "fetches".
     */
    private class Harness {
        val asks = mutableListOf<Pair<Id, Long>>()
        var gate: CompletableDeferred<Unit>? = null
        var player: (WatermarkTracker, Long) -> Unit = { _, _ -> }
        lateinit var tracker: WatermarkTracker

        private val filler = GapFiller { conversationId, toSeq ->
            asks.add(conversationId to toSeq)
            gate?.await()
            player(tracker, toSeq)
        }

        fun build(): WatermarkTracker {
            tracker = WatermarkTracker(CoroutineScope(Dispatchers.Unconfined), filler)
            return tracker
        }
    }

    private fun event(seq: Long): MessageEvent = MessageEvent(
        messageId = Id("5".repeat(26)),
        conversationId = conversation,
        seq = seq,
        senderId = Id("6".repeat(26)),
        senderDevice = Id("7".repeat(26)),
        kind = MessageKind.Text,
        envelope = ByteArray(0),
        createdAt = 0L,
    )

    @Test
    fun theFirstEventIsTheFloorNotAGap() {
        val harness = Harness()
        val tracker = harness.build()
        tracker.track(event(5L))
        // History below the first delivery is the caller's floor to choose, not the ledger's to
        // fill: seq 5 out of nowhere sets the watermark and asks nobody for 1 to 4.
        assertEquals(5L, tracker.watermark(conversation))
        assertTrue(harness.asks.isEmpty())
    }

    @Test
    fun theNextSequenceAdvancesAndARedeliveryStandsStill() {
        val harness = Harness()
        val tracker = harness.build()
        tracker.track(event(5L))
        tracker.track(event(6L))
        assertEquals(6L, tracker.watermark(conversation))
        // A redelivery is the caller's own dedup's business; the ledger only refuses to go
        // backwards.
        tracker.track(event(6L))
        assertEquals(6L, tracker.watermark(conversation))
        assertTrue(harness.asks.isEmpty())
    }

    @Test
    fun aHoleHoldsTheWatermarkAndAsksForTheMissingPages() {
        val harness = Harness()
        val tracker = harness.build()
        tracker.track(event(6L))
        tracker.track(event(9L))
        // The watermark names the top of what is held; the ask names the top of what arrived —
        // a fill bounded by less would leave the hole's tail unfetched.
        assertEquals(6L, tracker.watermark(conversation))
        assertEquals(listOf(conversation to 9L), harness.asks)
    }

    @Test
    fun aHoleThatGrewWhileAFillRanIsReAskedWhenTheFillEnds() {
        val harness = Harness()
        val tracker = harness.build()
        val gate = CompletableDeferred<Unit>()
        harness.gate = gate
        // A real fill replays its page through the live path, which is the tracker itself.
        harness.player = { ledger, toSeq -> for (seq in 7L..toSeq) ledger.track(event(seq)) }
        tracker.track(event(6L))
        tracker.track(event(9L))
        tracker.track(event(11L))
        // One hole, one fill: the 11 that arrived while the fill ran is the running fill's
        // business, not a second fill's.
        assertEquals(listOf(conversation to 9L), harness.asks)
        gate.complete(Unit)
        // The fill made progress, so no stall was recorded, and its end notices the hole grew
        // past the new watermark and re-asks without waiting for another event to notice.
        assertEquals(listOf(conversation to 9L, conversation to 11L), harness.asks)
        assertEquals(11L, tracker.watermark(conversation))
    }

    @Test
    fun aFillThatCannotMoveTheWatermarkStallsAndNoLaterEventReAsks() {
        val harness = Harness()
        val tracker = harness.build()
        tracker.track(event(6L))
        tracker.track(event(9L))
        // The server answered without progress: the hole is the server's to answer, and the
        // stall is what keeps a persistent hole from becoming a hot loop.
        assertEquals(listOf(conversation to 9L), harness.asks)
        tracker.track(event(10L))
        tracker.track(event(11L))
        assertEquals("a stalled hole is not re-asked per event", 1, harness.asks.size)
    }

    @Test
    fun theStallLiftsWhenTheWatermarkMoves() {
        val harness = Harness()
        val tracker = harness.build()
        tracker.track(event(6L))
        tracker.track(event(9L))
        assertEquals(1, harness.asks.size)
        // Live delivery filled part of the hole: the watermark moved, and a stall names the
        // watermark it stopped at, so the moved watermark is a question worth asking again.
        tracker.track(event(7L))
        tracker.track(event(11L))
        assertEquals(listOf(conversation to 9L, conversation to 11L), harness.asks)
    }

    @Test
    fun anAboveTheHoleDeliveryIsNotRepeatedWhenThePageReachesIt() {
        val harness = Harness()
        val tracker = harness.build()
        tracker.track(event(6L))
        tracker.track(event(9L))
        // The 9 was dispatched live, above the hole; the page that later fills 7 to 9 must not
        // deliver it twice.
        assertTrue(tracker.alreadyDispatched(event(9L)))
        // The memory is consumed: the same seq a second time is the page's own duplicate of a
        // duplicate, and the caller's dedup owns it from here.
        assertFalse(tracker.alreadyDispatched(event(9L)))
    }

    @Test
    fun entriesTheWatermarkPassedArePrunedFromTheAheadMemory() {
        val harness = Harness()
        val tracker = harness.build()
        tracker.track(event(6L))
        tracker.track(event(9L))
        // The watermark catches up to and past the remembered 9 — a later lookup prunes it,
        // because an entry at or below the watermark can never be fetched again.
        for (seq in 7L..10L) {
            tracker.track(event(seq))
        }
        assertFalse(tracker.alreadyDispatched(event(12L)))
        for (seq in 7L..10L) {
            assertFalse("the passed entries are gone", tracker.alreadyDispatched(event(seq)))
        }
    }

    @Test
    fun anUnknownConversationHasNoWatermark() {
        val tracker = WatermarkTracker(CoroutineScope(Dispatchers.Unconfined), null)
        assertNull(tracker.watermark(Id("4".repeat(26))))
        // The null filler disables gap filling; counting still works.
        tracker.track(event(3L))
        assertEquals(3L, tracker.watermark(conversation))
    }
}
