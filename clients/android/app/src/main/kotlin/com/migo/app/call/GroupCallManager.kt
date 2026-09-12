package com.migo.app.call

import com.migo.core.ConnectionState
import com.migo.core.MigoClient
import com.migo.core.domain.CallMediaKind
import com.migo.core.domain.GroupCallJoinedEvent
import com.migo.core.domain.GroupCallLeftEvent
import com.migo.core.domain.GroupCallNote
import com.migo.core.domain.GroupCallRoster
import com.migo.core.domain.GroupCallSeat
import com.migo.core.domain.Subscription
import com.migo.core.domain.generateCallKey
import com.migo.core.domain.isCallRetired
import com.migo.core.domain.namesOwnSeat
import com.migo.core.domain.placeholderSealedOffer
import com.migo.core.domain.seatArrived
import com.migo.core.domain.seatDeparted
import com.migo.core.domain.seatsFromSnapshot
import com.migo.core.wire.Id
import com.migo.core.wire.newId
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** What a failed join says, in the web overlay's own words. */
const val GROUP_CALL_JOIN_FAILED: String = "Could not join the group call."

/**
 * The group-call manager: one instance per live session, owning this device's single group-call
 * screen.
 *
 * A port of `clients/web/src/lib/migo/group-call-manager.tsx`. The core's group-calls domain is pure
 * signaling -- it joins, it leaves, it hands each frame to a typed listener. This class is the
 * piece above it the roster overlay actually needs: it projects the tracked [ActiveGroupCall] the
 * screen renders through a single [StateFlow], folding the roster snapshot and the join/leave
 * announcements into a seat list, and telling the four possible end stories apart ([GroupCallNote])
 * so the screen can say the true one. One instance is created by [com.migo.app.AppViewModel] where
 * the session is established and closed where it ends; the screens never see the domain, only the
 * state and the action methods.
 *
 * This build renders the roster only -- media for a group call arrives in a later build, and the
 * join's offer is the sealed placeholder the protocol defines for exactly that state: a genuine
 * sealed envelope (an empty-sdp offer, sealed under a per-call key with the call id as associated
 * data), so the join is end-to-end opaque to the server from the first frame. The key is minted
 * here and kept for the call's life; the frame-key agreement between participants -- how every
 * seat ends up holding the same call key -- is the piece that is still specification (migo.md
 * section 165), which is why the roster below has no media to attach to it.
 *
 * # The join's two halves
 *
 * A join is not one round trip. The `CALL_SFU_JOIN` reply carries the TURN relays; the roster
 * arrives *separately*, published to this account's own topic -- and either order is possible, so
 * both the reply and the snapshot are allowed to mark the seat accepted, whichever lands first
 * wins and the other only fills in what it still can. The screen has two phases:
 * [GroupCallPhase.Joining] from the tap until one of them lands, [GroupCallPhase.Seated] after.
 * The seat list itself only exists once the snapshot has (the reply carries none), and the
 * membership events fold into it from then on.
 *
 * # The four endings
 *
 * A group call can stop being live on this screen for four distinguishable reasons, and each one
 * owes the person holding the phone a different sentence: this device hung up ([GroupCallNote.Left]),
 * the last seat left and the call retired ([GroupCallNote.Ended]), the seat moved to another device
 * of this same account -- observed as a departure naming *this* account and *this* device
 * ([GroupCallNote.Moved]) -- or the session dropped and the facts simply stopped arriving
 * ([GroupCallNote.Connection]). A note, once set, is final for the screen: [leaveGroupCall] and
 * every event handler ignore a call that already has one, because a screen that first says
 * "continued on another device" and then "you left the call" is narrating two different calls.
 */
class GroupCallManager(
    private val client: MigoClient,
    private val accountId: Id,
    /** This session's own device, the second half of the moved-to-another-device test. */
    private val ownDeviceId: Id,
    /** The session's own scope: every ordinary launch dies with it. */
    private val scope: CoroutineScope,
    /**
     * A scope that outlives the session's own -- the application's. [close] fires its last
     * best-effort leave here, the web build's `beforeunload` analogue, because the view model is
     * cleared after the session scope is already cancelled.
     */
    private val closingScope: CoroutineScope,
) {
    // --- the state the screen reads ---

    private val _state = MutableStateFlow(GroupCallUiState())

    /** The one thing the group-call overlay is a function of. */
    val state: StateFlow<GroupCallUiState> = _state.asStateFlow()

    /** The tracked call, written only through [setActive] so the state and the ref never disagree. */
    @Volatile private var active: ActiveGroupCall? = null

    /**
     * Writes the tracked call and its state together. A local read of the old call first, the same
     * discipline [CallManager] keeps: `active` is a volatile another thread may retire between a
     * check and a use, and a handler that read it twice could fold an event into a call that is
     * no longer tracked.
     */
    private fun setActive(next: ActiveGroupCall?) {
        active = next
        _state.update { it.copy(call = next, error = if (next == null) null else it.error) }
    }

    // --- lifecycle ---

    /**
     * Bridges the manager onto the client's reconnect-surviving streams. The returned
     * subscriptions are the caller's to cancel; [close] does the rest (the best-effort leave).
     */
    fun attach(): List<Subscription> = listOf(
        client.onGroupCallRoster(::handleRoster),
        client.onGroupCallJoined(::handleParticipantJoined),
        client.onGroupCallLeft(::handleParticipantLeft),
    )

    /**
     * Ends everything this manager holds. A call still live is left on the wire first, best
     * effort on the outliving scope -- this runs from sign-out and from the view model's
     * clearing, and in the latter the session scope is already dead.
     */
    fun close() {
        // A local read, so the note check and the leave name the same call.
        val call = active ?: return
        if (call.note == null) {
            closingScope.launch {
                try {
                    client.groupCalls.leave(call.callId)
                } catch (_: Exception) {
                    // Whether the frame beats the socket's death is the same race the web build's
                    // beforeunload fires into; losing it costs only the peers' roster.
                }
            }
        }
        setActive(null)
    }

    /**
     * The session's connection state, relayed from the session hooks. A *Closed* session is the
     * web build's "client is gone": no roster events survive it, so a live call gets the
     * connection note rather than waiting for a snapshot that can never arrive. A *Reconnecting*
     * session does not -- the server still holds the seat, and the events queue behind the resume.
     */
    fun onConnectionState(next: ConnectionState) {
        if (next != ConnectionState.Closed) return
        val call = active ?: return
        if (call.note != null) return
        setActive(call.copy(note = GroupCallNote.Connection))
    }

    // --- the flows the UI calls ---

    /**
     * Joins the group call of a conversation, minting the call's key and publishing the sealed
     * placeholder offer.
     *
     * The key is minted before the join so the offer can be sealed under it in the same breath;
     * the call id is minted here so a retried join re-seats the same call (the id is the server's
     * dedupe key). A tap while a call is tracked does nothing -- one seat per account is the
     * server's rule, and this screen already holds this account's.
     */
    fun joinGroupCall(conversationId: Id) {
        if (active != null) return
        scope.launch {
            val callId = newId()
            val callKey = generateCallKey()
            setActive(
                ActiveGroupCall(
                    callId = callId,
                    conversationId = conversationId,
                    mediaKind = CallMediaKind.Audio,
                    phase = GroupCallPhase.Joining,
                    seats = emptyList(),
                    participantCount = 0L,
                ),
            )
            try {
                client.groupCalls.join(
                    conversationId,
                    CallMediaKind.Audio,
                    placeholderSealedOffer(callKey, callId),
                    callId,
                )
                // The snapshot may already have marked the seat accepted (it is published, not
                // replied); this only fills in the timestamp for a reply that won the race.
                val call = active
                if (call != null && call.callId == callId && call.phase == GroupCallPhase.Joining) {
                    setActive(call.copy(phase = GroupCallPhase.Seated, joinedAt = call.joinedAt ?: now()))
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                // The join never landed (membership refused, the socket gone). A snapshot that
                // arrived despite the failure means it did land -- only fail a call still waiting
                // for its seat.
                val call = active
                if (call != null && call.callId == callId && call.phase == GroupCallPhase.Joining) {
                    setActive(null)
                    _state.update { it.copy(error = GROUP_CALL_JOIN_FAILED) }
                }
            }
        }
    }

    /**
     * Leaves the group call. No-op once a note is set: an ended call was already left by whatever
     * set the note, and a second `CALL_END` for it would be a frame about a call nobody holds.
     */
    fun leaveGroupCall() {
        val call = active ?: return
        if (call.note != null) return
        setActive(call.copy(note = GroupCallNote.Left))
        scope.launch {
            try {
                client.groupCalls.leave(call.callId)
            } catch (_: Exception) {
                // Best effort: the server retires the seat on its own liveness rules, and the
                // screen has already said the true thing.
            }
        }
    }

    /**
     * Dismisses the ended screen or the failure card, leaving nothing tracked. Refused while a
     * call is live -- the web build's own rule, because a back press during a live call is a
     * navigation instinct, not a wish to hang up on the whole group.
     */
    fun dismissGroupCall() {
        val call = active
        if (call != null && call.note == null) {
            return
        }
        setActive(null)
    }

    // --- the streams' handlers ---

    /**
     * The roster snapshot: the authoritative seat list for a join this device made. It marks the
     * seat accepted (the reply carries relays, not a roster, and either can land first), keeps the
     * server's join order, and takes the server's own tally. A snapshot for any other call id is
     * this account's *other* device joining -- that device's screen, not this one's -- and is
     * ignored.
     */
    private fun handleRoster(roster: GroupCallRoster) {
        val call = active ?: return
        if (roster.callId != call.callId) {
            return
        }
        setActive(
            call.copy(
                phase = GroupCallPhase.Seated,
                seats = seatsFromSnapshot(roster.participants),
                participantCount = roster.participantCount,
                joinedAt = call.joinedAt ?: now(),
            ),
        )
    }

    /**
     * A participant joined, on the conversation's topic. Fold the seat and take the server's
     * tally; the fold itself decides whether this appends or replaces -- a seat replacement (this
     * account, new device) arrives here right after the departure that emptied the old line, and
     * ends with the account's seat at the end of the join order on its new device. An announcement
     * for any other call is another member's client hearing its own facts; the call-id match keeps
     * this manager to this device's call.
     */
    private fun handleParticipantJoined(event: GroupCallJoinedEvent) {
        val call = active ?: return
        if (event.callId != call.callId || call.note != null) {
            return
        }
        setActive(
            call.copy(
                seats = seatArrived(call.seats, event.userId, event.deviceId, now()),
                participantCount = event.participantCount,
            ),
        )
    }

    /**
     * A participant left. Which note the screen shows, if any, is decided here and only here:
     *
     *  - the departure names *this* account on *this* device -- this connection never hears its
     *    own leave (the server skips the origin session when publishing), so this exact pair is
     *    the seat being replaced from this account's other device; the call continues, just not
     *    here ([GroupCallNote.Moved]), and the seat list is left as it stood, frozen under the
     *    note;
     *  - otherwise, a remaining count of zero is the retirement ([GroupCallNote.Ended]);
     *  - otherwise the call simply got smaller, and the screen keeps showing it (no note).
     *
     * The own-seat test compares both halves exactly: a departure naming this account on a
     * *different* device is somebody else's seat replacement being observed, not ours.
     */
    private fun handleParticipantLeft(event: GroupCallLeftEvent) {
        val call = active ?: return
        if (event.callId != call.callId || call.note != null) {
            return
        }
        if (namesOwnSeat(event.userId, event.deviceId, accountId, ownDeviceId)) {
            setActive(call.copy(note = GroupCallNote.Moved))
            return
        }
        setActive(
            call.copy(
                note = if (isCallRetired(event.participantCount)) GroupCallNote.Ended else null,
                seats = seatDeparted(call.seats, event.userId),
                participantCount = event.participantCount,
            ),
        )
    }

    private fun now(): Long = System.currentTimeMillis()
}

/** The two halves of a group-call join: before and after the roster snapshot lands. */
enum class GroupCallPhase {
    /** The join is in flight; the screen shows it joining and nothing else. */
    Joining,

    /** The snapshot arrived; the screen folds membership events from here on. */
    Seated,
}

/** The tracked group call, as the overlay renders it. */
data class ActiveGroupCall(
    /** The id the join minted and the server dedupes on; the events that patch this call carry it. */
    val callId: Id,
    /** The conversation whose group call this is. */
    val conversationId: Id,
    /** What the join carried; the title's label and nothing else in this build -- no media rides it yet. */
    val mediaKind: CallMediaKind,
    /** Whether the join reply or the roster snapshot has landed (see [GroupCallManager]). */
    val phase: GroupCallPhase,
    /** The seat list, in join order, one line per account. */
    val seats: List<GroupCallSeat>,
    /** The server's own tally, taken from the latest frame that carried one. */
    val participantCount: Long,
    /** When this account's seat joined, for the running duration; the anchor arrives with the roster. */
    val joinedAt: Long? = null,
    /** Why the call stopped being live on this screen, once it did. Final once set. */
    val note: GroupCallNote? = null,
)

/** The slice of the group-call manager the screen reads. */
data class GroupCallUiState(
    /** The group call this device is in, including one that just ended (until dismissed). */
    val call: ActiveGroupCall? = null,
    /** Why a join could not even be placed, when nothing else is showing. */
    val error: String? = null,
)
