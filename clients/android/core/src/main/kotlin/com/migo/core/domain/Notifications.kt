package com.migo.core.domain

import com.migo.core.protocol.NotificationEvent
import com.migo.core.protocol.Op

/**
 * Receiving server-pushed notification events.
 *
 * A port of `packages/sdk/src/domains/notifications.ts`. These are the lightweight nudges a client
 * turns into a badge, a banner, or a system notification: a mention, a friend request, activity in a
 * watched conversation. Receive-only -- the server decides what to push, and there is nothing to send
 * here.
 *
 * # A notification never carries private plaintext
 *
 * A [NotificationEvent] names a [com.migo.core.protocol.NotificationKind] and points at a
 * conversation, room, or actor by id. Any title or body on it is metadata the server can compose --
 * a room name, a sender's display name -- and never the text of a private message, because the
 * server has no plaintext to put there (section 174).
 *
 * The consequence for an Android client is concrete: a system notification for a new message shows
 * the sender and "New message" until the app has opened the conversation and decrypted the body
 * locally. A client that wanted the message text in the notification shade would have to decrypt in
 * the push handler, which is the design this protocol makes possible and this event deliberately does
 * not do for it.
 *
 * # The stream is droppable
 *
 * Under load the server sheds these before anything carrying state, so a missed notification is a cue
 * to reconcile -- sync the conversation, recount unreads -- and not a guaranteed one-to-one signal. A
 * client that derived its unread count purely from notifications received would drift.
 */
class NotificationsDomain(
    private val rpc: Rpc,
    onEventError: EventErrorHandler? = null,
) {
    private val listeners = ListenerSet<NotificationEvent>(Op.NOTIFICATION_EVENT, onEventError)

    @Volatile
    private var subscription: Subscription? = null

    /**
     * Begins delivering notifications to registered handlers. Idempotent.
     *
     * Register handlers with [onNotification] first: nothing is delivered before this is called, so
     * the ordering is what stops a client from missing the first push after connecting.
     */
    fun start() {
        if (subscription != null) return
        subscription = rpc.on(Op.NOTIFICATION_EVENT, { r -> NotificationEvent.decode(r) }) { e, _ ->
            listeners.deliver(e)
        }
    }

    /** Stops delivery. Registered handlers are kept for a later [start]. */
    fun stop() {
        val live = subscription ?: return
        subscription = null
        live.cancel()
    }

    /** Registers a handler for inbound notifications. */
    fun onNotification(listener: Listener<NotificationEvent>): Subscription =
        listeners.add(listener)
}
