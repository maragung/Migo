package com.migo.core.domain

import com.migo.core.protocol.MessageEvent
import com.migo.core.protocol.Op
import com.migo.core.wire.Id
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * The seam the watermark tracker calls when a live sequence lands above watermark + 1 — a hole, in
 * a space section 152 says is gapless. Implemented by the client, which is the one place that can
 * page the sync domain and replay the missing events through the messaging domain's live path.
 */
fun interface GapFiller {
    /**
     * Fetches the missing pages for a conversation, up to but not including `toSeq`'s own event at
     * the latest. Background repair: a failure is surfaced by the tracker, never thrown into the
     * live path that asked for the fill.
     */
    suspend fun fillGap(conversationId: Id, toSeq: Long)
}

/**
 * How many above-the-watermark deliveries are remembered per conversation, so the page that later
 * fills the gap beneath them does not deliver them twice.
 *
 * The bound exists because the memory is filled by the wire: a peer that floods above a hole could
 * otherwise grow one entry's list without limit. Past the bound the *oldest* is dropped — the
 * oldest above-gap delivery is the one a still-running fill is least likely to re-reach, because
 * the fill pages forward from the watermark.
 */
const val MAX_AHEAD_REMEMBERED = 4096

/**
 * The contiguous-sequence ledger: section 158's local `last_seq`, held per conversation.
 *
 * A port of the watermark machinery in `packages/sdk/src/domains/messaging.ts`, and it has to stay
 * a port: the watermark is the half of section 158's reconnect comparison the *client* owns, and a
 * client that reported a different contiguity than the web client would fetch a different gap and
 * deliver a different history. The one structural difference is ownership — the TypeScript domain
 * is built once for the client's life, while this client rebuilds its domains per connection, so
 * the ledger lives here as its own object and the client injects the same instance into every
 * [MessagingDomain] it builds. A reconnect must not reset it: the watermark is exactly the state
 * the resync after a reconnect reads to know where to resume.
 *
 * # The three states a sequence number can be in
 *
 * Every ingested event is one of:
 *
 *  - **`held + 1`** — the next brick on the prefix. The watermark advances.
 *  - **at or below `held`** — a redelivery. The watermark stands still and the dispatch is the
 *    caller's own dedup's business (the crypto layers refuse a second decrypt, which is replay
 *    protection working).
 *  - **above `held + 1`** — a hole in a space section 152 says is gapless. The watermark stands
 *    still, the event is dispatched anyway (the hole below is no reason to hold it), and a fill is
 *    scheduled for the missing pages.
 *
 * The first event a conversation ever delivers becomes the floor: history below it may be gone
 * (the server's `Truncated`) or simply unfetched, which is the caller's floor to choose, not this
 * ledger's.
 *
 * # Why the fills cannot become a loop
 *
 * The ask is guarded twice. While a fill is in flight the guard is the `filling` set — every later
 * above-gap event for that conversation is the running fill's business. After a fill that could
 * not move the watermark the guard is `stalledAt`: the server has been asked and has answered
 * without progress, and re-asking on every later event would be exactly the hot loop a persistent
 * hole (history gone server-side, or a truncation the caller must render) must never become. The
 * stall lifts the moment the watermark moves; a fill that made progress records no stall at all.
 *
 * # Concurrency
 *
 * The TypeScript original runs on one thread. Here the entry points are called from the messaging
 * domain's serialized event path, but the fill completion runs on whatever coroutine finishes it,
 * so every method is `@Synchronized` — cheap, non-suspending, and reentrant where they nest (the
 * fill's completion calls back into [scheduleGapFill] under the same lock). Nothing here may
 * suspend: the tracker sits inside the event path, and a watermark that could park would reorder
 * the very events it is counting.
 */
class WatermarkTracker(
    /**
     * Where a scheduled fill runs. Injected so the fills die with the client that owes them; the
     * fill itself is launched, never run inline, because it re-enters the messaging domain's event
     * lock and the event path that detected the gap holds that lock.
     */
    private val scope: CoroutineScope,
    /** Who pages the missing events in. Null disables gap filling (the tracker still counts). */
    private val gapFiller: GapFiller?,
    /** Where a failed fill is reported; a fill is background repair and never throws to the caller. */
    private val onEventError: EventErrorHandler? = null,
) {
    /** Conversation id to its highest contiguous sequence number — the top of what is *held*. */
    private val watermarks = HashMap<Id, Long>()

    /**
     * Conversation id to the highest sequence ever seen, gap or no gap — the top of what has
     * *arrived*.
     *
     * The difference between this and the watermark is the hole a fill must target: a fill asked
     * for less would leave the tail of the hole unfetched, and one asked for more would tail live
     * traffic instead of filling.
     */
    private val highestSeen = HashMap<Id, Long>()

    /** Conversations with a gap fill in flight, so one hole spawns one fill, not one per event. */
    private val filling = HashSet<Id>()

    /**
     * The watermark a fill that could not close the gap stopped at, per conversation. See the
     * class note on why this is the second guard.
     */
    private val stalledAt = HashMap<Id, Long>()

    /**
     * Sequence numbers already dispatched above the watermark, so the page that later fills the
     * gap beneath them does not deliver them twice.
     */
    private val ahead = HashMap<Id, ArrayList<Long>>()

    /**
     * The highest sequence number this client holds contiguously for a conversation, or null when
     * nothing has been ingested for it yet.
     *
     * This is the local half of section 158's reconnect comparison and the cursor an open-chat
     * catch-up starts from: one past this number is where to resume.
     */
    @Synchronized
    fun watermark(conversationId: Id): Long? = watermarks[conversationId]

    /**
     * Advances the conversation's contiguous watermark, or notes a gap by standing still.
     *
     * Tombstones count: a deletion occupies a sequence number like any message, so the prefix this
     * ledger describes is of *events*, not of content. Called before dispatch so listeners and the
     * crypto layers below see the same accounting a later [watermark] read reports.
     */
    @Synchronized
    fun track(event: MessageEvent) {
        val held = watermarks[event.conversationId]
        if (held == null || event.seq == held + 1L) {
            // The floor, or the next brick on top of it.
            watermarks[event.conversationId] = event.seq
        }
        val high = highestSeen[event.conversationId]
        if (high == null || event.seq > high) {
            highestSeen[event.conversationId] = event.seq
        }
        if (held != null && event.seq > held + 1L) {
            // Above held + 1: a hole. The watermark waits for the missing pages, and something
            // goes to fetch them — a resync from what is truly held is exactly what section 158
            // asks for.
            scheduleGapFill(event.conversationId)
        }
        // At or below: a redelivery the caller's own dedup handles. Below the floor: the caller's
        // floor to choose, not ours to fill.
    }

    /**
     * Whether this event was already dispatched as an above-the-watermark live delivery.
     *
     * Consuming the memory: the seq is forgotten here, because the dispatch it guarded against
     * has either happened (this page copy is that dispatch's duplicate) or the page it would have
     * ridden on never came and the live copy stands alone.
     */
    @Synchronized
    fun alreadyDispatched(event: MessageEvent): Boolean {
        val live = ahead[event.conversationId] ?: return false
        val index = live.indexOf(event.seq)
        if (index != -1) {
            live.removeAt(index)
            return true
        }
        // Entries at or below the watermark can never be fetched again; prune them so the list
        // holds only the live window above the hole.
        val watermark = watermarks[event.conversationId]
        if (watermark != null) {
            val current = live.filter { it > watermark }
            if (current.size != live.size) {
                if (current.isEmpty()) {
                    ahead.remove(event.conversationId)
                } else {
                    live.clear()
                    live.addAll(current)
                }
            }
        }
        return false
    }

    /**
     * Remembers an above-the-watermark delivery, so the gap fill's page cannot repeat it.
     *
     * Only a seq above the current watermark can be re-fetched later — everything at or below it
     * is behind the cursor every fetch starts from — so those are the only deliveries worth
     * guarding.
     */
    @Synchronized
    fun rememberDispatch(event: MessageEvent) {
        val watermark = watermarks[event.conversationId] ?: return
        if (event.seq <= watermark) return
        val live = ahead.getOrPut(event.conversationId) { ArrayList() }
        if (!live.contains(event.seq)) {
            live.add(event.seq)
            if (live.size > MAX_AHEAD_REMEMBERED) {
                live.removeAt(0)
            }
        }
    }

    /**
     * Asks the gap filler for the pages a detected hole is missing, once per hole.
     *
     * See the class note for the two guards. A fill that made progress records no stall, so a fill
     * cut short by its page budget continues on the next above-gap event; and when events arrived
     * *above* the target while a fill ran, the continuation is scheduled as the fill ends, without
     * waiting for another event to notice the new hole.
     */
    @Synchronized
    private fun scheduleGapFill(conversationId: Id) {
        val filler = gapFiller ?: return
        if (filling.contains(conversationId)) return
        val haveSeq = watermarks[conversationId] ?: return
        val toSeq = highestSeen[conversationId] ?: return
        if (toSeq <= haveSeq) return
        if (stalledAt[conversationId] == haveSeq) return
        filling.add(conversationId)
        scope.launch {
            try {
                filler.fillGap(conversationId, toSeq)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (cause: Throwable) {
                // The fill is background repair; its failure is surfaced, not thrown into the
                // live path that asked for it.
                onEventError?.invoke(Op.SYNC, cause)
            } finally {
                endFill(conversationId, haveSeq, toSeq)
            }
        }
    }

    /**
     * Records what one finished fill achieved, and re-asks when the hole grew while it ran.
     *
     * The stall recorded on no progress still applies to the re-ask, so this cannot chain on its
     * own — only genuinely new arrivals reopen the question.
     */
    @Synchronized
    private fun endFill(conversationId: Id, haveSeq: Long, toSeq: Long) {
        filling.remove(conversationId)
        val after = watermarks[conversationId] ?: haveSeq
        if (after > haveSeq) {
            stalledAt.remove(conversationId)
        } else {
            stalledAt[conversationId] = after
        }
        val ceiling = highestSeen[conversationId] ?: toSeq
        if (ceiling > toSeq && after < ceiling) {
            // Events arrived above the target while the fill ran; their hole is a new ask.
            scheduleGapFill(conversationId)
        }
    }
}
