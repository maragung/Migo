package com.migo.core.domain

import com.migo.core.protocol.Op
import com.migo.core.protocol.TypingEvent
import com.migo.core.protocol.TypingState
import com.migo.core.wire.Id

/**
 * Typing indicators: the "..." under a conversation.
 *
 * A port of `packages/sdk/src/domains/typing.ts`. The smallest domain in the SDK, and deliberately the
 * least reliable one.
 *
 * # Sent as a notification, never as a call
 *
 * [set] uses [Rpc.notify] rather than [Rpc.call], so nothing is awaited and no correlation id is
 * spent. A typing indicator is worthless a second after it was true; a client that awaited an
 * acknowledgement for one would be holding a round trip open to confirm the delivery of something
 * whose value had already expired. A dropped indicator costs a user nothing.
 *
 * # Not encrypted, and it does not need to be
 *
 * A [TypingEvent] carries a conversation id and a state, no content. There is nothing to seal: the
 * fact that somebody is typing is metadata the server routes, exactly like presence. This is worth
 * being explicit about because it is the one place a reader might expect the crypto layers and find
 * none.
 *
 * # Why the state is an enum and not a boolean
 *
 * [TypingState] distinguishes composing from recording a voice note, which a receiving client shows
 * differently ("typing..." versus "recording audio..."). A boolean would have forced a second opcode
 * later, and the protocol has one place for this.
 *
 * # The client owes the throttling
 *
 * Neither this domain nor the server debounces. A keystroke handler wired straight to [set] would send
 * one frame per character; the convention the other clients follow is to send on the first keystroke,
 * repeat no more than every few seconds while composing continues, and send
 * [TypingState.Stopped] when the field empties or the message goes out. The server also lets an
 * indicator lapse on its own, so a client that disconnects mid-sentence does not leave a permanent
 * "typing..." behind.
 */
class TypingDomain(
    private val rpc: Rpc,
    onEventError: EventErrorHandler? = null,
) {
    private val listeners = ListenerSet<TypingEvent>(Op.TYPING, onEventError)

    @Volatile
    private var subscription: Subscription? = null

    /** Begins delivering typing events to registered handlers. Idempotent. */
    fun start() {
        if (subscription != null) return
        subscription = rpc.on(Op.TYPING, { r -> TypingEvent.decode(r) }) { event, _ ->
            listeners.deliver(event)
        }
    }

    /** Stops delivery. Registered handlers are kept for a later [start]. */
    fun stop() {
        val live = subscription ?: return
        subscription = null
        live.cancel()
    }

    /**
     * Registers a handler for other participants' typing events.
     *
     * [TypingEvent.userId] is filled in by the server on the way out and is absent on the copy this
     * client sent, so a handler can rely on it being present here.
     */
    fun onTyping(listener: Listener<TypingEvent>): Subscription = listeners.add(listener)

    /**
     * Tells the conversation what this client is doing. Fire and forget.
     *
     * Returns as soon as the frame is queued. A failure to reach the server is not reported, because
     * there is no useful reaction to a lost typing indicator and a caller that had to handle one would
     * be writing error paths for a decoration.
     */
    suspend fun set(conversationId: Id, state: TypingState) {
        val event = TypingEvent(conversationId, state)
        rpc.notify(Op.TYPING) { w -> event.encode(w) }
    }
}
