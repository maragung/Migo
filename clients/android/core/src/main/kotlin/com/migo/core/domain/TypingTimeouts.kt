package com.migo.core.domain

import com.migo.core.wire.Id

/**
 * The receiver's local typing timeout, as pure bookkeeping: which `(conversation, typer)` pairs
 * are showing, and when each one's last `Start` runs out.
 *
 * Brief section 15's rule — "penerima menerapkan timeout lokal, sehingga Start yang tidak
 * pernah diikuti Stop tetap hilang sendiri" — is a receiver's duty, not a sender's: the typer
 * whose app died mid-word cannot send the `Stop`, and the server's sweep is the backstop
 * rather than the floor, so the receiver ends the indicator on its own clock. The web client
 * arms the same four seconds this tracker keeps.
 *
 * Pure on purpose: the clock arrives as a `now` parameter, so the arithmetic — start, stop,
 * refresh, expire — is testable without a coroutine dispatcher, and the view model only has to
 * keep a ticker running while the map is non-empty.
 */
class TypingTimeouts(private val ttlMs: Long) {

    /** One deadline per `(conversation, typer)`: the last Start's own expiry. */
    private val deadlines = HashMap<Pair<Id, Id>, Long>()

    /**
     * A `Start`: arms (or re-arms) the pair's deadline. Re-arming rather than refusing is the
     * whole point of a repeated `Start` — the protocol's refresh — because a continuous typer
     * sends one every few seconds and an indicator that expired under them would blink.
     */
    fun start(conversation: Id, typer: Id, now: Long) {
        deadlines[conversation to typer] = now + ttlMs
    }

    /** A `Stop`: the pair goes at once, deadline and all, because the typer said so themselves. */
    fun stop(conversation: Id, typer: Id) {
        deadlines.remove(conversation to typer)
    }

    /**
     * Claims the pairs whose deadline has passed, removing each one it returns. A claim, not a
     * read, so a ticker that runs the expiry twice — or a refresh racing the sweep — cannot
     * clear the same entry twice.
     */
    fun expire(now: Long): List<Pair<Id, Id>> {
        val expired = deadlines.filterValues { it <= now }.keys.toList()
        for (key in expired) deadlines.remove(key)
        return expired
    }

    /** Whether anything is pending, so the view model's ticker can stop when there is nothing to do. */
    fun isEmpty(): Boolean = deadlines.isEmpty()

    /**
     * How long until the next deadline, or `null` when nothing is pending — the delay a ticker
     * should sleep, so the expiry lands on time instead of on the next tick.
     */
    fun nextDelay(now: Long): Long? {
        val earliest = deadlines.values.minOrNull() ?: return null
        return (earliest - now).coerceAtLeast(0)
    }
}
