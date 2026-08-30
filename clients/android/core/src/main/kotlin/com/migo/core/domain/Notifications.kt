package com.migo.core.domain

import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.InboxItem
import com.migo.core.protocol.InboxReq
import com.migo.core.protocol.InboxResponse
import com.migo.core.protocol.NotificationAck
import com.migo.core.protocol.NotificationEvent
import com.migo.core.protocol.Op
import com.migo.core.wire.Id
import com.migo.core.wire.idFromBytes

/**
 * Notifications: the pushed event stream and the durable inbox behind it.
 *
 * A port of `packages/sdk/src/domains/notifications.ts`. These are the lightweight nudges a client
 * turns into a badge, a banner, or a system notification: a mention, a friend request, activity in a
 * watched conversation. The push stream ([onNotification]) is droppable by design; the inbox
 * ([listNotifications]) is the durable record that survives the recipient being offline, and the two
 * are deliberately one domain: a pushed event is a cue to re-read the inbox, never a substitute for
 * it.
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

    /**
     * Reads one page of the caller's inbox, newest first.
     *
     * The server keeps no pagination cursor for the inbox: a client pages by re-asking with a higher
     * limit. Rows arrive as [InboxItem]s -- kind, timestamp, and the ids the kind points at --
     * carrying no message content, per the no-plaintext rule above.
     */
    suspend fun listNotifications(limit: Long = DEFAULT_INBOX_LIMIT): List<InboxItem> {
        val request = InboxReq(limit)
        val response = rpc.call(
            Op.NOTIFICATION_LIST,
            { w -> request.encode(w) },
            { r -> InboxResponse.decode(r) },
        )
        return response.items
    }

    /**
     * Marks every notification at or before one instant as read.
     *
     * `through` is a Unix-millisecond watermark, normally the `at` of the newest item the caller has
     * rendered (from [listNotifications]) -- the "I have opened the bell" gesture. The wire carries a
     * single notification id rather than a timestamp, and the server reads the id's embedded time
     * prefix as the watermark, so this synthesises an id whose prefix *is* `through`; one call then
     * clears the named instant and everything older, and a notification landing mid-flight is simply
     * left for the next ack rather than raced.
     */
    suspend fun acknowledgeNotifications(through: Long) {
        val request = NotificationAck(watermarkId(through))
        rpc.call(Op.NOTIFICATION_ACK, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    private companion object {
        /** The inbox page the caller asks for by default. */
        const val DEFAULT_INBOX_LIMIT: Long = 50L
    }
}

/**
 * The id whose time prefix is `unixMs`: six big-endian bytes of it, then zeros.
 *
 * Only the prefix is ever read server-side ([NotificationsDomain.acknowledgeNotifications]), so the
 * random tail a real id carries is replaced with zeros -- this value names an instant, not an entity,
 * and must not be mistaken for a persisted notification's id.
 */
private fun watermarkId(unixMs: Long): Id {
    val bytes = ByteArray(16)
    var ms = unixMs
    for (i in 5 downTo 0) {
        bytes[i] = (ms and 0xff).toByte()
        ms = ms shr 8
    }
    return idFromBytes(bytes)
}
