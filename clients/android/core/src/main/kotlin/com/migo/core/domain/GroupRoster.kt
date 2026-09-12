package com.migo.core.domain

import com.migo.core.protocol.CallSfuParticipant
import com.migo.core.wire.Id

/**
 * One line of a group-call roster: an account's seat in a call.
 *
 * A seat belongs to a *user* (the account), not a device -- the server allows one seat per account,
 * so a roster never holds two lines for the same [userId] -- but the line remembers which [deviceId]
 * holds it, because "the call moved to my other phone" and "someone else joined" are different
 * facts, and [namesOwnSeat] tells them apart only by comparing both.
 */
data class GroupCallSeat(
    val userId: Id,
    val deviceId: Id,
    /** When this seat joined, in milliseconds since the epoch; the roster fold keeps it stable. */
    val joinedAt: Long,
)

/**
 * Why a group call is no longer live on this screen.
 *
 * A port of the web client's four end notes, kept distinct because they say different things to a
 * person holding the phone:
 *
 *  - [Left] -- this device hung up, or never finished joining.
 *  - [Ended] -- the call retired: the last seat left, and there is nothing to dial back into.
 *  - [Moved] -- the seat still exists, but on another device of *this same account*.
 *  - [Connection] -- the session dropped, so this screen's facts stopped arriving, not the call.
 */
enum class GroupCallNote {
    Left,
    Ended,
    Moved,
    Connection,
}

/** The sentence each note renders as, in the same order the web overlay shows them. */
fun groupCallNoteLabel(note: GroupCallNote): String =
    when (note) {
        GroupCallNote.Left -> "You left the call"
        GroupCallNote.Ended -> "The call ended"
        GroupCallNote.Moved -> "Continued on another device"
        GroupCallNote.Connection -> "Connection lost"
    }

/** True when a departure's remaining count means the call itself is gone: nobody is seated. */
fun isCallRetired(participantCount: Long): Boolean = participantCount == 0L

/**
 * True when an event names this session's own seat: same account *and* same device.
 *
 * This is the moved-to-another-device test. A departure naming this account but a different device
 * means the account's seat moved here from somewhere else (or a second join replaced this one); a
 * departure naming this account on *this* device means the seat that left is the one this screen is
 * sitting in. Only the second is "continued on another device" -- and note it is reported as an
 * arrival's [GroupCallJoinedEvent] on the *new* device, whose `deviceId` is not this one, so the
 * exact-match rule reads the same in both directions: it fires only when both halves match this
 * session.
 */
fun namesOwnSeat(userId: Id, deviceId: Id, accountId: Id, ownDeviceId: Id): Boolean =
    userId == accountId && deviceId == ownDeviceId

/**
 * Folds a roster snapshot into seats, keeping the server's order.
 *
 * The snapshot is already the whole truth at join time -- server-ordered, one seat per account --
 * so the fold is just a mapping; the projection takes over only for the announcements that follow.
 */
fun seatsFromSnapshot(participants: List<CallSfuParticipant>): List<GroupCallSeat> =
    participants.map { GroupCallSeat(userId = it.userId, deviceId = it.deviceId, joinedAt = it.joinedAt) }

/**
 * Folds a join announcement into a seat list.
 *
 * An arrival either appends a new line or replaces the line the same account already held -- a seat
 * replacement, heard as a departure-then-arrival pair, ends with the account's seat moved to the
 * end of the join order on its new device. The `joinedAt` stamp is the caller's "now" so a screen's
 * timer stays honest even when the announcement is processed late.
 */
fun seatArrived(seats: List<GroupCallSeat>, userId: Id, deviceId: Id, now: Long): List<GroupCallSeat> =
    seats.filter { it.userId != userId } + GroupCallSeat(userId = userId, deviceId = deviceId, joinedAt = now)

/**
 * Folds a departure announcement into a seat list: the account's line is gone, whatever device held
 * it. The server's one-seat-per-account rule means a user id names exactly one line to drop.
 */
fun seatDeparted(seats: List<GroupCallSeat>, userId: Id): List<GroupCallSeat> =
    seats.filter { it.userId != userId }

/**
 * The sealed blob this client publishes as its join offer, before any media exists to describe.
 *
 * It is a real sealed envelope, not a placeholder *packet*: the offer is a genuine
 * [SdpDescription] -- `type: "offer"` with an empty sdp, exactly the frame a media stack will fill
 * in later -- sealed under the call's key with the call id as associated data, so the join is
 * end-to-end opaque to the server from the very first frame. The key never leaves this device
 * except through the call's key channel; the server stores and re-serves the blob unopened.
 *
 * The caller mints the key ([generateCallKey]) so this stays one expression at the join site, and
 * the empty-sdp offer is why a group call can join before its media plane has anything to say --
 * the roster arrives, the media description catches up.
 */
fun placeholderSealedOffer(callKey: ByteArray, callId: Id): ByteArray =
    sealCallSignal(encodeSdpDescription(SdpDescription(type = "offer", sdp = "")), callKey, callId)

/** The label a group-call screen titles itself with; group calls in this build are voice-only. */
fun groupCallKindLabel(): String = mediaKindLabel(CallMediaKind.Audio)
