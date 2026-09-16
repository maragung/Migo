package com.migo.core.domain

import com.migo.core.protocol.CallListEntry
import com.migo.core.wire.Id

/**
 * A group call this device is *not* seated in, as a join affordance renders it: the id a join must
 * carry to land inside the running call (the server dedupes on it -- a fresh id would seat a second
 * call beside the running one, not in it), and the size the affordance names.
 */
data class GroupCallInProgress(
    val callId: Id,
    /** The server's own tally, taken from the latest announcement that carried one. */
    val participantCount: Long,
)

/**
 * The join affordance's fold of the conversation topic's announcements: which group calls are
 * running that this device holds no seat in, one entry per conversation.
 *
 * The roster fold ([seatArrived]/[seatDeparted]) answers the seated screen's question -- *who is in
 * my call* -- and deliberately discards every announcement for any other call, because a roster is
 * one call's truth. This class is the other half of the same announcements, the question the chat
 * header asks before anyone taps: *is there a call to join here*. Every member receives the join
 * and departure announcements on the conversation's topic, seated or not, so a member who never
 * joined still watches the whole life of the call pass by -- and the count a departure carries is
 * the retirement signal, exactly as it is for the seated roster ([isCallRetired]).
 *
 * The tracker does not know which call this device is seated in; the manager routes around that
 * (the seated call's announcements fold into the roster, and the entry for a conversation this
 * device joins is dropped with [forget]), which keeps this fold pure announcement arithmetic --
 * the same discipline [WatermarkTracker] keeps.
 */
class GroupCallProgressTracker {
    /** conversationId -> the running call, keyed by conversation because one affordance is offered per conversation. */
    private val calls = LinkedHashMap<Id, GroupCallInProgress>()

    /**
     * Folds a join announcement: a call is running in that conversation, at the announced size. A
     * join always wins the entry, even over one held for an older call id -- two live calls in one
     * conversation cannot stand, so the arrival names the live one.
     */
    fun onJoined(event: GroupCallJoinedEvent) {
        calls[event.conversationId] = GroupCallInProgress(
            callId = event.callId,
            participantCount = event.participantCount,
        )
    }

    /**
     * Folds a departure announcement. A remaining count of zero retires the entry -- the call is
     * gone server-side and there is nothing to join. Any other count *creates* the entry as readily
     * as it moves one: a session that signs in mid-call hears no join announcement for it, and the
     * first departure it does hear still carries the call's id, its conversation and its size --
     * everything the affordance needs, so the mid-call arrival is discovered rather than missed.
     *
     * A departure naming a *different* call than the one held is a stale fact about a call that has
     * already been replaced, and is ignored -- it must not retire or resize the live entry, not
     * even when its count is the retirement signal, because that count is about the other call.
     */
    fun onLeft(event: GroupCallLeftEvent) {
        val held = calls[event.conversationId]
        if (held != null && held.callId != event.callId) {
            return
        }
        if (isCallRetired(event.participantCount)) {
            calls.remove(event.conversationId)
        } else {
            calls[event.conversationId] = GroupCallInProgress(
                callId = event.callId,
                participantCount = event.participantCount,
            )
        }
    }

    /**
     * Drops a conversation's entry: what a join of that conversation's call owes the tracker. The
     * device seated in a call is shown the call, not an invitation to it, and the announcements
     * that fold the seated roster never come here -- so this is the one correction the join itself
     * must make.
     */
    fun forget(conversationId: Id) {
        calls.remove(conversationId)
    }

    /**
     * Folds a *listing* -- the server's answer to `CALL_LIST` -- into the map: the calls this
     * session never heard announced because it was not connected to hear them.
     *
     * The announcements above are the map's source while a session is up, and they are the newer
     * one: they are the conversation's own news. What they cannot do is survive a session that was
     * absent -- a member offline through a whole call hears no join, and no departure is coming,
     * so the entry would stay missing until the *next* movement, which for a call already running
     * never comes. So the fold adds and never replaces: a conversation already held is left alone,
     * whatever the listing says. Only calls this device is offered but is not in (`joined` 0) are
     * taken -- a seat this device holds is the roster's, not the affordance's -- and only group
     * calls (`kind` 1), because a direct call's screen reads the invite stream for itself.
     */
    fun onListing(entries: List<CallListEntry>) {
        for (entry in entries) {
            if (entry.kind != CALL_LIST_GROUP || entry.joined != 0L) continue
            if (calls.containsKey(entry.conversationId)) continue
            calls[entry.conversationId] =
                GroupCallInProgress(
                    callId = entry.callId,
                    participantCount = entry.participantCount,
                )
        }
    }

    /** The running calls the affordance can offer, keyed by conversation. */
    fun snapshot(): Map<Id, GroupCallInProgress> = LinkedHashMap(calls)

    private companion object {
        /** `CallListEntry.kind`: 1 is a group call's roster, 0 a direct call. */
        const val CALL_LIST_GROUP = 1L
    }
}
