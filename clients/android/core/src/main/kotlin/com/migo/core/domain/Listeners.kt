package com.migo.core.domain

import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/**
 * A small fan-out primitive shared by every event-driven domain.
 *
 * The subscribe-style domains -- typing, presence, rooms, notifications, games -- all do the same
 * thing with a server event: keep a set of application handlers, and when an event arrives, hand it
 * to each. Mirrors `packages/sdk/src/domains/listeners.ts`.
 *
 * The one subtlety is failure isolation. [Rpc.on] already routes a decode failure or a single handler
 * throw to the error sink, but it dispatches to the domain with *one* callback; if that callback
 * iterated the handlers itself and let one throw escape, the remaining handlers would be starved of
 * the event. So delivery is centralised here and each handler is invoked inside its own `try`/`catch`,
 * and a bug in one subscriber can never cost another subscriber its events.
 *
 * The messaging domain does not use this -- it juggles three distinct listener kinds and an
 * out-of-order buffer, so its dispatch is bespoke -- but every simpler domain does.
 */

/** An application handler. Unsubscribed through the [Subscription] that [ListenerSet.add] returns. */
typealias Listener<T> = (value: T) -> Unit

/**
 * A set of handlers for one event opcode, with per-handler failure isolation.
 *
 * A domain holds one of these per event it exposes, wires its [Rpc.on] subscription to [deliver], and
 * hands callers [add] to register interest.
 *
 * The set is guarded by a plain lock, and handlers are invoked outside it: an application handler
 * calling back into [add] or cancelling its own subscription is ordinary, and it must not be able to
 * deadlock the frame pump. [opcode] labels errors routed to [onError] so a report says which event a
 * failing handler was for.
 */
class ListenerSet<T>(
    private val opcode: Long,
    private val onError: EventErrorHandler? = null,
) {
    private val lock = ReentrantLock()
    private val listeners = ArrayList<Listener<T>>()

    /** Registers a handler and returns the [Subscription] that removes it. */
    fun add(listener: Listener<T>): Subscription {
        lock.withLock { listeners.add(listener) }
        return Subscription {
            // Reference equality, which is what `remove` uses for a function value. Two handlers that
            // happen to capture nothing are still distinct objects, so one cancellation cannot remove
            // another's registration.
            lock.withLock { listeners.remove(listener) }
        }
    }

    /** How many handlers are registered, so a domain can skip work when nobody is listening. */
    val size: Int get() = lock.withLock { listeners.size }

    /** Delivers a value to every handler, isolating a throw from one so the rest still receive it. */
    fun deliver(value: T) {
        val snapshot = lock.withLock { listeners.toList() }
        for (listener in snapshot) {
            try {
                listener(value)
            } catch (cause: Throwable) {
                onError?.invoke(opcode, cause)
            }
        }
    }
}
