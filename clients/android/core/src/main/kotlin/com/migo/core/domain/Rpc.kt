package com.migo.core.domain

import com.migo.core.net.Gateway
import com.migo.core.net.GatewayError
import com.migo.core.net.RealtimeTransport
import com.migo.core.wire.Frame
import com.migo.core.wire.Reader
import com.migo.core.wire.Writer
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/**
 * The typed request/event bridge every domain is built on.
 *
 * A domain never touches [Gateway] directly. It calls through this thin layer, which pairs an opcode
 * with the generated `encode(Writer)` and `decode(Reader)` for that opcode's payload, so a domain
 * method reads as one line and a mismatched struct is a compile error rather than a wire fault.
 * Mirrors `packages/sdk/src/domains/rpc.ts`.
 *
 * # Why there is a pump here and not in the transport
 *
 * The web SDK's transport dispatches events to listeners from inside its own receive loop. This
 * client cannot: OkHttp delivers frames on its own thread, and [Gateway] does the sequence
 * bookkeeping synchronously with arrival because the ACK watermark has to be exact. Running
 * application handlers on that thread would put arbitrary UI code in the path of the frame counter.
 * So [Gateway] hands uncorrelated frames to a channel and [deliver] is the seam: one coroutine drains
 * that channel and fans each frame out to the domains. Nothing else may consume [Gateway.inbound] --
 * a channel with two readers would deliver each event to one of them at random.
 *
 * [deliver] must be running for any `on` subscription to fire, and it must keep running for the whole
 * connection: the channel is unbounded, so a client that stopped draining would hold every broadcast
 * it ever received in memory rather than dropping them.
 *
 * # Why a handler cannot break the connection
 *
 * [deliver] decodes and dispatches inside a `try`/`catch`. One malformed event, or one application
 * handler with a bug, is routed to [EventErrorHandler] and dropped; the pump keeps delivering. The
 * alternative is a single throw from a UI callback ending the receive loop and silently costing the
 * user every subsequent message.
 */

/** Notified when an inbound event fails to decode, or its handler throws. Never fatal. */
typealias EventErrorHandler = (opcode: Long, cause: Throwable) -> Unit

/** A registration that can be undone. Returned by every `on` method in every domain. */
fun interface Subscription {
    /** Removes the handler. Idempotent: cancelling twice is not an error. */
    fun cancel()
}

/**
 * The request/notify/subscribe surface shared by all domains.
 *
 * Holds no protocol knowledge of its own -- the opcode and the codecs are always passed in by the
 * domain -- so it stays a mechanical adapter over the gateway.
 */
class Rpc(
    private val gateway: RealtimeTransport,
    private val onEventError: EventErrorHandler? = null,
) {
    /**
     * Guards the subscriber table.
     *
     * A plain lock rather than a [kotlinx.coroutines.sync.Mutex]: registering a handler is a map
     * insert, callers do it from arbitrary threads while composing a screen, and making `on` a
     * suspending function would push that into every caller for no benefit. Handlers are dispatched
     * outside the lock -- see [dispatch].
     */
    private val lock = ReentrantLock()

    private val subscribers = HashMap<Long, MutableList<Entry>>()

    /**
     * Sends a request and decodes its reply.
     *
     * An ERROR-flagged reply has already become [GatewayError.Refused] inside [Gateway.request], so
     * this only runs on the success path and the decode is always against the opcode's declared
     * response type.
     *
     * A reply this build cannot parse is [GatewayError.Malformed] rather than the underlying
     * [com.migo.core.wire.WireError]: which field ran short is a fact about bytes from the network,
     * and a caller can do nothing with it that it would not also do with "the server's answer was
     * unreadable".
     */
    suspend fun <Res> call(opcode: Long, encode: (Writer) -> Unit, decode: (Reader) -> Res): Res {
        val frame = gateway.request(opcode, encode)
        return decodePayload(frame, decode)
    }

    /**
     * Sends a fire-and-forget frame the protocol gives no reply to (TYPING, MESSAGE_RECEIPT).
     *
     * Correlation zero, which is what "not a reply to anything, and no reply expected" is on the
     * wire. Using a fresh correlation id instead would leave the server free to answer, and this
     * client with no waiter to answer to.
     */
    suspend fun notify(opcode: Long, encode: (Writer) -> Unit) {
        gateway.send(opcode, NOT_A_REPLY, encode)
    }

    /**
     * Subscribes to a server event opcode, decoding each frame before the handler sees it.
     *
     * Several handlers may share an opcode and all of them fire. A decode failure or a throw from
     * [handler] is delivered to the error sink and swallowed, so it never reaches [deliver]'s loop.
     */
    fun <Event> on(
        opcode: Long,
        decode: (Reader) -> Event,
        handler: (Event, Frame) -> Unit,
    ): Subscription {
        val entry = Entry { frame ->
            val event = try {
                decodePayload(frame, decode)
            } catch (cause: Throwable) {
                onEventError?.invoke(opcode, cause)
                return@Entry
            }
            try {
                handler(event, frame)
            } catch (cause: Throwable) {
                onEventError?.invoke(opcode, cause)
            }
        }
        lock.withLock { subscribers.getOrPut(opcode) { ArrayList() }.add(entry) }
        return Subscription {
            lock.withLock {
                val list = subscribers[opcode]
                if (list != null) {
                    list.remove(entry)
                    if (list.isEmpty()) subscribers.remove(opcode)
                }
            }
        }
    }

    /**
     * Drains the gateway's inbound frames until the connection ends. Suspends for the whole session.
     *
     * Returns normally when the socket was closed cleanly and throws the [GatewayError] the
     * connection died of otherwise, because that is the signal a reconnect policy needs and the pump
     * is where it surfaces. The caller launches this once per connection and treats its completion as
     * "this connection is over".
     */
    suspend fun deliver() {
        for (message in gateway.inbound) {
            dispatch(message.frame)
        }
    }

    /**
     * Hands one frame to every handler registered for its opcode.
     *
     * The subscriber list is copied under the lock and the handlers run outside it. Holding the lock
     * across dispatch would mean an application handler that registered another handler deadlocked
     * the connection, and reentrancy is not something a UI callback should have to know about.
     *
     * An ERROR-flagged frame with no waiter is reported rather than decoded. It is the server
     * refusing something whose reply nobody is holding any more -- a request whose caller was
     * cancelled, typically -- and its payload is an `Error`, not the struct this opcode declares. A
     * client that fed it to the opcode's decoder would report a malformed event and lose the reason.
     */
    private fun dispatch(frame: Frame) {
        val opcode = frame.header.opcode
        if (Gateway.isError(frame)) {
            onEventError?.invoke(opcode, Gateway.refusal(frame))
            return
        }
        val handlers = lock.withLock { subscribers[opcode]?.toList() } ?: return
        for (entry in handlers) {
            entry.handle(frame)
        }
    }

    /**
     * Decodes a frame's payload with [decode].
     *
     * [Reader.finish] is deliberately not called. A newer server may append an optional field to a
     * struct this build knows, and the generated decoders skip unknown optional fields by length --
     * that is the whole forward-compatibility mechanism. Asserting the payload was fully consumed
     * would turn a compatible addition into a client that refuses every frame of that opcode. This
     * matches [Gateway]'s own WELCOME decode.
     */
    private fun <Res> decodePayload(frame: Frame, decode: (Reader) -> Res): Res = try {
        decode(Reader(frame.payload))
    } catch (_: Exception) {
        throw GatewayError.Malformed
    }

    private companion object {
        /** The correlation for a frame that is neither a request nor a reply. */
        const val NOT_A_REPLY = 0L
    }
}

/**
 * One registered handler, wrapped so removal is by identity.
 *
 * A bare `(Frame) -> Unit` in the list would be removed by equality, and two subscriptions that
 * happened to capture nothing could compare equal -- unsubscribing one would then remove the other.
 */
private class Entry(val handle: (Frame) -> Unit)
