package com.migo.app.call

import android.content.Context
import com.migo.core.ConnectionState
import com.migo.core.MigoClient
import com.migo.core.domain.CallMediaKind
import com.migo.core.domain.GroupCallInProgress
import com.migo.core.domain.GroupCallJoinedEvent
import com.migo.core.domain.GroupCallLeftEvent
import com.migo.core.domain.GroupCallNote
import com.migo.core.domain.GroupCallProgressTracker
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
import com.migo.core.protocol.TurnServer
import com.migo.core.wire.Id
import com.migo.core.wire.newId
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import org.webrtc.EglBase
import org.webrtc.VideoTrack

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
 * The media itself is [GroupMediaPlane]'s: a full mesh, one WebRTC connection per other seat,
 * dialed and sealed under the call's frame key exactly as section 165 requires and section 163's key
 * agreement makes possible. This class owns the roster and the four endings; the plane owns the
 * connections, and the two meet in three places -- the seat list the roster folds is the seat list
 * the plane builds links from, the relays the join reply carries are the relays its links are built
 * over, and the frame-key rotations the roster's movement triggers are what make a link that has not
 * finished negotiating rebuild itself under the key that actually stands.
 *
 * The join's offer is still the sealed placeholder the protocol defines for a roster-only join: a
 * genuine sealed envelope (an empty-sdp offer, sealed under a per-call key with the call id as
 * associated data), so the join stays end-to-end opaque to the server from the first frame, and the
 * media descriptions that matter are relayed seat to seat afterwards. The key is minted here and
 * kept for the call's life -- a call this device *starts*; a call it *joins* keeps no minted key,
 * because its running key arrives by the ask-and-answer below. The frame-key agreement between
 * participants is the core's own domain (section 163): a started call's minted key becomes its
 * epoch-0 frame key, a joined call's first key is the running one a seated participant hands over,
 * and the manager below fires the domain's triggers on roster movement -- every join, departure and
 * mid-call ask re-keys the call exactly as the section requires.
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
    /** The application context, which is what the media plane's engine is built against. */
    private val context: Context,
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
    /**
     * The mesh: one connection per other seat, and this device's own tracks on all of them.
     *
     * Declared before the state it is published through, because the screen reads the plane's flows
     * through this class rather than reaching past it -- a screen that held the plane itself could
     * outlive the call it belongs to.
     */
    private val media = GroupMediaPlane(context, client, accountId, ownDeviceId, scope)

    /** One entry per other seat, in the order the plane built the links. */
    val mediaLinks: StateFlow<List<GroupLinkState>> = media.links

    /** This device's own camera track, for the call screen's self-view. */
    val localVideo: StateFlow<VideoTrack?> = media.localVideo

    /** The GL context the call screen's renderers must be initialized against. */
    val eglContext: EglBase.Context?
        get() = media.eglContext

    /**
     * Where this call is played, as the phone's own routing layer.
     *
     * The one-to-one call holds one of these too and neither reaches into the other's: a route is a
     * property of *the phone carrying a call*, so what the two calls share is the behaviour rather
     * than the object, and the behaviour lives in [CallAudioRoute]. This class's part in it is the
     * *when* -- the watch starts where the seat goes live and stops beside `media.stop()`, in
     * [setActive], which is the one place every ending passes through.
     */
    private val audioRoute = CallAudioRoute(context)

    /**
     * The routes this phone offers this group call right now, in the order the platform lists them.
     *
     * Empty on a phone older than Android 12, and empty is the honest answer there rather than a
     * short list -- the layer's own doc has the whole argument. The screen draws the control only
     * when there is more than one entry, exactly as the one-to-one call screen does.
     */
    val outputs: StateFlow<List<AudioOutput>> = audioRoute.outputs

    /**
     * The route this call is playing through, or null while the phone is choosing for itself.
     *
     * Null is a real answer and not a missing one: a call the app has never routed anywhere is a
     * call the platform is routing, and the menu shows no tick rather than ticking a device it
     * merely guessed at.
     */
    val chosenOutputId: StateFlow<Int?> = audioRoute.chosenOutputId

    /**
     * Plays this call through one of the routes the phone listed; false means the phone refused.
     *
     * A refusal is not swallowed: it is what a Bluetooth route dropping between the menu being
     * drawn and the tap landing looks like, and the caller re-reads the list rather than leaving
     * the menu showing a choice the call is not on.
     */
    fun chooseOutput(deviceId: Int): Boolean = audioRoute.choose(deviceId)

    /** The TURN relays the join reply carried, kept for every link built afterwards. */
    @Volatile private var joinRelays: List<TurnServer> = emptyList()

    // --- the state the screen reads ---

    private val _state = MutableStateFlow(GroupCallUiState())

    /** The one thing the group-call overlay is a function of. */
    val state: StateFlow<GroupCallUiState> = _state.asStateFlow()

    /**
     * The join affordance's fold of the announcements this device is not seated in: which
     * conversations have a call running, keyed by conversation, published through the same state
     * the overlay reads. The manager routes each announcement to exactly one of the two folds --
     * the roster's, for the call this screen holds, or this one, for every other -- so the two
     * never disagree about the same call.
     */
    private val progress = GroupCallProgressTracker()

    /** The tracked call, written only through [setActive] so the state and the ref never disagree. */
    @Volatile private var active: ActiveGroupCall? = null

    /** Publishes the tracker's snapshot beside the tracked call, the header affordance's own state. */
    private fun publishProgress() {
        _state.update { it.copy(inProgress = progress.snapshot()) }
    }

    /**
     * Writes the tracked call and its state together. A local read of the old call first, the same
     * discipline [CallManager] keeps: `active` is a volatile another thread may retire between a
     * check and a use, and a handler that read it twice could fold an event into a call that is
     * no longer tracked. A call that stops being live here -- cleared, or given its ending note --
     * also drops its frame key: a key for a seat this screen no longer holds is key material with
     * nothing left to seal.
     */
    private fun setActive(next: ActiveGroupCall?) {
        val previous = active
        when {
            next == null -> if (previous != null) forgetFrameKey(previous.callId)
            next.note != null -> forgetFrameKey(next.callId)
        }
        // A call that stops being live here takes its media with it: every reason a screen stops
        // showing a call -- a hang-up, a retirement, the seat moving to another device, a dropped
        // session -- is a reason this device must stop sending a microphone and a camera into it.
        if (next == null || next.note != null) {
            if (previous != null) {
                media.stop()
                // The route goes with the media, at the same moment and for the same reason: a
                // phone left pinned to a speaker by a call that ended is loud in a room where
                // nobody asked for it, and a menu still listing that call's routes would be
                // offering a tap that moves a call nobody is on. The one-to-one call clears this
                // in `teardownMedia`; this is that place for a group call.
                audioRoute.stop()
            }
            _state.update { it.copy(muted = false, cameraOn = false, cameraAvailable = false) }
        } else {
            // The seat is live, so the phone is carrying a call and the routes it can carry it
            // over exist to be chosen. Guarded inside the route rather than here, because this
            // runs on every roster update and a group call's roster moves constantly: a
            // registration per update would be a report per update.
            audioRoute.start()
        }
        active = next
        _state.update { it.copy(call = next, error = if (next == null) null else it.error) }
    }

    /** Drops the frame key of a call this screen is done with, whatever the session's own state. */
    private fun forgetFrameKey(callId: Id) {
        try {
            client.groupCallKeys.forget(callId)
        } catch (_: Exception) {
            // The session may already be closing; the store is cleared at disconnect.
        }
    }

    /**
     * Runs one frame-key trigger off the session's scope, best effort: the trigger's own failure
     * (a rotation frame the socket could not carry, an ask that could not leave) never takes the
     * roster screen with it, and each trigger is idempotent enough to be re-fired by the next
     * event of its kind.
     */
    private fun launchFrameKeyTrigger(trigger: suspend () -> Unit) {
        scope.launch {
            try {
                trigger()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                // See the doc: the roster is the screen's truth, the key channel is the core's.
            }
        }
    }

    // --- lifecycle ---

    /**
     * Bridges the manager onto the client's reconnect-surviving streams. The returned
     * subscriptions are the caller's to cancel; [close] does the rest (the best-effort leave).
     *
     * The listing ask goes out here, once per session: the announcements below are the tracker's
     * source while a session is up, and what they cannot cover is the session that was not there --
     * a member offline through a whole group call hears no join, and no departure is coming, so
     * without the ask the header would offer no way into a call that is still running.
     */
    fun attach(): List<Subscription> {
        refreshInProgress()
        // The camera control's availability follows the links: a call past the product's stream
        // limit negotiates no video line at all, so the button is absent there rather than present
        // and unable to send anything.
        scope.launch {
            media.links.collect { links ->
                val available = links.any { it.videoCapable }
                if (available != _state.value.cameraAvailable) {
                    _state.update { it.copy(cameraAvailable = available) }
                }
            }
        }
        return listOf(
            client.onGroupCallRoster(::handleRoster),
            client.onGroupCallJoined(::handleParticipantJoined),
            client.onGroupCallLeft(::handleParticipantLeft),
            // The plane's own: it claims every relay that opens under the call's frame key, which
            // is the media describing itself, and quietly passes on the ones that do not -- the key
            // exchange, which the core's key domain is already reading (see [GroupMediaPlane]).
            *media.attach().toTypedArray(),
        )
    }

    /**
     * Asks the server which calls this account can see and folds the answer into the tracker.
     *
     * The announcements are the tracker's newer source -- they are the conversation's own news, so
     * the fold adds and never replaces (see [GroupCallProgressTracker.onListing]) -- which is why
     * this is safe to run beside a live session, and why it is worth re-running it whenever a fresh
     * session replaces one the server could not resume. Best effort throughout: an ask that fails,
     * or a server that does not know the opcode yet, leaves the tracker exactly as the
     * announcements keep it.
     */
    fun refreshInProgress() {
        scope.launch {
            try {
                progress.onListing(client.calls.listCalls())
                publishProgress()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                // See the doc: the announcements remain the tracker's source.
            }
        }
    }

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
     * The key is minted before the join so the offer can be sealed under it in the same breath; the
     * call id is minted here *unless the join names one* -- a call the tracker has watched running
     * is joined under its own id, because the id is the server's dedupe key and a fresh one would
     * seat a second call beside the running one rather than in it -- and a minted id is what makes
     * a retried join re-seat the same call. Either way, the conversation's affordance entry is
     * dropped here: a device holding a seat is shown the call, not an invitation to it. A tap
     * while a call is tracked does nothing -- one seat per account is the server's rule, and this
     * screen already holds this account's.
     */
    fun joinGroupCall(conversationId: Id, callId: Id? = null) {
        if (active != null) return
        scope.launch {
            val joinedCallId = callId ?: newId()
            progress.forget(conversationId)
            publishProgress()
            val callKey = generateCallKey()
            setActive(
                ActiveGroupCall(
                    callId = joinedCallId,
                    conversationId = conversationId,
                    mediaKind = CallMediaKind.Audio,
                    phase = GroupCallPhase.Joining,
                    seats = emptyList(),
                    participantCount = 0L,
                ),
            )
            try {
                // A fresh call's key is seated as its epoch-0 frame key before the join frame can
                // leave: the roster it answers with may already carry other seats, and the triggers
                // below need the state standing by the time it lands. A join of a call already
                // running seats nothing -- the running key is not this device's to mint, and a
                // state standing here would both silence the ask the roster trigger owes (the
                // store's "am I a potential holder" test would pass) and refuse the answer's
                // install, which only lands where no state stands. The offer is a genuine sealed
                // envelope either way; the mid-call joiner's key seals it and is discarded, the
                // web build's own discipline for every join.
                if (callId == null) {
                    client.groupCallKeys.seated(joinedCallId, conversationId, callKey)
                }
                val accepted = client.groupCalls.join(
                    conversationId,
                    CallMediaKind.Audio,
                    placeholderSealedOffer(callKey, joinedCallId),
                    joinedCallId,
                )
                // The plane opens on the accept rather than on the join frame: the relays are half
                // of what a link is built over, and a link built before they arrived would be one
                // the reconcile below rebuilds a moment later.
                joinRelays = accepted.servers
                media.begin(joinedCallId, active?.seats.orEmpty(), joinRelays)
                // The snapshot may already have marked the seat accepted (it is published, not
                // replied); this only fills in the timestamp for a reply that won the race.
                val call = active
                if (call != null && call.callId == joinedCallId && call.phase == GroupCallPhase.Joining) {
                    setActive(call.copy(phase = GroupCallPhase.Seated, joinedAt = call.joinedAt ?: now()))
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                // The join never landed (membership refused, the socket gone). A snapshot that
                // arrived despite the failure means it did land -- only fail a call still waiting
                // for its seat.
                val call = active
                if (call != null && call.callId == joinedCallId && call.phase == GroupCallPhase.Joining) {
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
     * Mutes or unmutes this device on the whole call.
     *
     * One microphone feeds every link, so the mute is one write rather than one per peer. Refused
     * once the call has a note, which is the same rule every other control keeps: there is no call
     * left to mute.
     */
    fun toggleGroupMute() {
        val call = active ?: return
        if (call.note != null) return
        val next = !media.isMuted()
        media.setMuted(next)
        _state.update { it.copy(muted = next) }
    }

    /**
     * Turns this device's camera on or off on the whole call.
     *
     * The state is read back from the plane rather than assumed, because turning a camera on can
     * fail -- another application may hold it -- and a button that keeps claiming a picture the
     * call is not sending is the one answer worse than no picture. A no-op where no link carries a
     * video line, which is a call past the product's stream limit: the control is absent there, and
     * an absent control that still answered would be a lie about what the call can do.
     */
    fun toggleGroupCamera() {
        val call = active ?: return
        if (call.note != null) return
        if (!media.cameraAvailable()) return
        media.setCameraOn(!media.isCameraOn())
        _state.update { it.copy(cameraOn = media.isCameraOn()) }
    }

    /** Flips to the other camera, on every link at once. */
    fun switchGroupCamera() {
        media.switchCamera()
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
        val seats = seatsFromSnapshot(roster.participants)
        setActive(
            call.copy(
                phase = GroupCallPhase.Seated,
                seats = seats,
                participantCount = roster.participantCount,
                joinedAt = call.joinedAt ?: now(),
            ),
        )
        // The mesh opens or grows on the authoritative seat list. Both halves of the join pass
        // through here -- the reply's is the relays and no seats, this one is the seats -- so this
        // is the one place the plane can be told the truth about who is in the call.
        media.begin(roster.callId, seats, joinRelays)
        // The frame-key trigger (section 163): a snapshot that shows other seats means this device
        // joined a call people are already in. A device holding the call's key is seated among
        // them and rotates -- its own arrival changed the membership; a device holding none is the
        // mid-call joiner, and asks a seated participant for the running key instead.
        if (roster.participants.any { it.userId != accountId }) {
            if (client.groupCallKeys.holdsKey(call.callId)) {
                launchFrameKeyTrigger { client.groupCallKeys.rotateForMembership(call.callId) }
            } else {
                launchFrameKeyTrigger { client.groupCallKeys.requestJoinKey(roster) }
            }
        }
    }

    /**
     * A participant joined, on the conversation's topic. Fold the seat and take the server's
     * tally; the fold itself decides whether this appends or replaces -- a seat replacement (this
     * account, new device) arrives here right after the departure that emptied the old line, and
     * ends with the account's seat at the end of the join order on its new device. An announcement
     * for any other call is another member's client hearing its own facts, and the call-id match
     * keeps this manager's *roster* to this device's call -- but a call this screen holds no seat
     * in is still a fact worth keeping: it goes to the tracker instead, and becomes the header's
     * "join the running call" affordance. That includes the seated call once a note has ended its
     * screen -- this device hung up or the seat moved, and the call that continues without it is
     * exactly the call the header may offer to rejoin.
     */
    private fun handleParticipantJoined(event: GroupCallJoinedEvent) {
        val call = active
        if (call == null || event.callId != call.callId || call.note != null) {
            progress.onJoined(event)
            publishProgress()
            return
        }
        val seats = seatArrived(call.seats, event.userId, event.deviceId, now())
        setActive(
            call.copy(
                seats = seats,
                participantCount = event.participantCount,
            ),
        )
        media.syncSeats(seats)
        // The join changed the call's membership, so the frame key rotates (section 163). This
        // connection never hears its own join announced -- the server publishes it excluding the
        // origin session -- so a rotation here is always for somebody else's arrival.
        launchFrameKeyTrigger { client.groupCallKeys.rotateForMembership(call.callId) }
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
     *
     * A departure for a call this screen holds no seat in -- none at all, another conversation's,
     * or the seated call once its note has ended the screen -- goes to the tracker rather than the
     * roster: the moved seat's own departure is fed there too, because the call it names continues
     * without this device, and a retirement (count zero) retires the tracker's entry exactly as it
     * ends the screen's call.
     */
    private fun handleParticipantLeft(event: GroupCallLeftEvent) {
        val call = active
        if (call == null || event.callId != call.callId || call.note != null) {
            progress.onLeft(event)
            publishProgress()
            return
        }
        if (namesOwnSeat(event.userId, event.deviceId, accountId, ownDeviceId)) {
            setActive(call.copy(note = GroupCallNote.Moved))
            // The call continues on the account's other device; the rejoin is the header's to
            // offer once this screen is dismissed.
            progress.onLeft(event)
            publishProgress()
            return
        }
        val seats = seatDeparted(call.seats, event.userId)
        setActive(
            call.copy(
                note = if (isCallRetired(event.participantCount)) GroupCallNote.Ended else null,
                seats = seats,
                participantCount = event.participantCount,
            ),
        )
        // A retired call's note has already stopped the plane in [setActive]; a call that simply
        // got smaller drops the one seat and keeps the rest of the mesh standing.
        media.syncSeats(seats)
        // A departure changed the call's membership too, so the frame key rotates here as well
        // (section 163) -- for the retirement the rotation is moot (nobody is left to adopt it,
        // and the store is dropped with the note), but for a call that simply got smaller it is
        // the whole point: what follows the departure must not open for the one who left.
        launchFrameKeyTrigger { client.groupCallKeys.rotateForMembership(call.callId) }
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
    /** What the join carried. The mesh negotiates its own media; this stays the join's own label. */
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
    /**
     * The group calls running that this device holds no seat in, keyed by conversation -- the
     * header's join affordance, one entry per conversation with a live call to join.
     */
    val inProgress: Map<Id, GroupCallInProgress> = emptyMap(),
    /** Whether this device's microphone is muted on the call. */
    val muted: Boolean = false,
    /** Whether this device's camera is on. Read back from the plane, so a refusal shows as off. */
    val cameraOn: Boolean = false,
    /** Whether any link carries a video line, which is what makes the camera control meaningful. */
    val cameraAvailable: Boolean = false,
)
