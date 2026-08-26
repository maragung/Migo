package com.migo.core.domain

import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.Op
import com.migo.core.protocol.PresenceEvent
import com.migo.core.protocol.PresenceState
import com.migo.core.protocol.PresenceUpdate

/**
 * Presence: whether contacts are online, and telling them about this account.
 *
 * A port of `packages/sdk/src/domains/presence.ts`. Two halves that mirror each other: [set] publishes
 * this account's state, and [onPresence] receives everybody else's.
 *
 * # Presence is per account, not per device
 *
 * A [PresenceUpdate] names no device. Somebody with a phone and a desktop open is *online*, not online
 * twice, and the server folds their devices into one state -- which is also why [set] from one device
 * changes what every contact sees. A client that treated presence as per-device would show a contact
 * as away because one of their machines went to sleep.
 *
 * # Why this one is acknowledged when typing is not
 *
 * [set] is an [Rpc.call] and waits for an [Acknowledged], because presence is *state* rather than a
 * momentary hint: a lost "Away" leaves an account showing as online until something else corrects it,
 * possibly for hours. The acknowledgement is what lets a client retry the one that mattered.
 *
 * # The custom status is metadata, and the server can read it
 *
 * [PresenceUpdate.customStatus] is a short line a person sets for themselves, and it is not sealed:
 * the server has to store it and hand it to every contact who looks. It is deliberately not part of
 * the encrypted world, and worth surfacing in a UI as clearly public -- somebody who put something
 * private in it would be putting it somewhere quite different from a message.
 *
 * # No manual heartbeat here
 *
 * Being connected is what makes an account online; the transport's MWP PING already proves the
 * connection is alive, so nothing in this domain needs a timer. [set] is for a *deliberate* change --
 * the person chose Away, or Do Not Disturb -- and the server derives Offline from the connection
 * ending. A client that pinged presence on a timer would be sending a second heartbeat to say what the
 * first one already said.
 */
class PresenceDomain(
    private val rpc: Rpc,
    onEventError: EventErrorHandler? = null,
) {
    private val listeners = ListenerSet<PresenceEvent>(Op.PRESENCE_EVENT, onEventError)

    @Volatile
    private var subscription: Subscription? = null

    /** Begins delivering presence events to registered handlers. Idempotent. */
    fun start() {
        if (subscription != null) return
        subscription = rpc.on(Op.PRESENCE_EVENT, { r -> PresenceEvent.decode(r) }) { event, _ ->
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
     * Registers a handler for contacts' presence changes.
     *
     * [PresenceEvent.lastSeen] is present for an account that is not currently online and is what a UI
     * renders as "last seen ..."; for an online account there is nothing to show and the field is
     * absent.
     */
    fun onPresence(listener: Listener<PresenceEvent>): Subscription = listeners.add(listener)

    /**
     * Publishes this account's presence state.
     *
     * Passing [customStatus] as null *clears* any status line currently set, which is what a UI's
     * "clear status" does; there is no separate clear operation. Retaining the previous line would
     * make an update that omitted it a silent no-op, and a person who cleared their status would find
     * it still showing.
     */
    suspend fun set(state: PresenceState, customStatus: String? = null): Acknowledged {
        val update = PresenceUpdate(state, customStatus)
        return rpc.call(
            Op.PRESENCE_SET,
            { w -> update.encode(w) },
            { r -> Acknowledged.decode(r) },
        )
    }
}
