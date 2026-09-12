'use client';

/**
 * The pure halves of the group-call roster: the projection every roster event updates, the
 * placeholder this build seals as its offer, and the words the roster screen shows.
 *
 * A group call is a *roster* (see the SDK's group-call domain): the server stores one seat per
 * account and re-serves the sealed blobs between them, and what a screen renders is the seat list
 * plus three events — the snapshot a joiner builds from, and the join/leave announcements everyone
 * else keeps the list true with. Everything here is a pure function over that state so a test can
 * pin it without a socket, a React tree, or a media plane.
 *
 * # One seat per account, observed rather than enforced
 *
 * One seat per account is a *server* rule: a second join from the same account replaces the seat.
 * The client observes it as a departure announcement followed by an arrival naming the same
 * account on a new device. The projection below folds an arrival for a known account by replacing
 * that seat — the honest reading of the announcement stream, never a second seat for one account —
 * and puts the replacement at the end of the list, because a replaced seat is a fresh join and the
 * list's order is join order.
 *
 * # The placeholder offer
 *
 * This build carries the roster, not yet the media plane: no WebRTC, no microphone. But the join
 * frame's `sealedOffer` slot is required on the wire, and section 165's rule is unconditional —
 * media descriptions are sealed before they reach the server, because an SDP body says what the
 * end-to-end promise exists to protect. So the placeholder is a *real* sealed envelope: an empty
 * offer description under a freshly minted per-call key, indistinguishable from a media-bearing
 * offer from the server's side and from every other participant's. The day the media plane lands,
 * the same seal carries a real offer and nothing about the wire changes.
 */

import type { GroupCallJoinedEvent, GroupCallLeftEvent, GroupCallRoster, Id } from '@migo/sdk';

import { encodeSdpDescription, sealCallSignal } from './call-signal.js';

/** One seat in a group call: the account holding it, the device they joined with, and when. */
export interface GroupCallSeat {
  userId: Id;
  deviceId: Id;
  joinedAt: number;
}

/**
 * Why a roster screen stopped showing a live call. Kept as a closed set — not a free string — so
 * the screen states stay pinnable and the note labels stay distinct, the same discipline the 1:1
 * screen keeps for its ended reasons (section 180: a screen that names every state is a screen a
 * user can trust).
 */
export type GroupCallNote = 'left' | 'ended' | 'moved' | 'connection';

/** What each note says on the screen. All four are distinct facts; none is a bare "call ended". */
export const GROUP_CALL_NOTES: Readonly<Record<GroupCallNote, string>> = {
  left: 'You left the call',
  ended: 'The call ended',
  moved: 'Continued on another device',
  connection: 'Connection lost',
};

/** The label for a note; a note this build does not know cannot happen by construction. */
export function groupCallNoteLabel(note: GroupCallNote): string {
  return GROUP_CALL_NOTES[note];
}

/**
 * Builds the seat list from a roster snapshot, keeping the server's join order.
 *
 * The snapshot is the one authoritative frame a joining screen builds from — the announcements
 * that follow it are deltas — so its order is kept verbatim rather than re-derived.
 */
export function seatsFromSnapshot(roster: GroupCallRoster): GroupCallSeat[] {
  return roster.participants.map((participant) => ({
    userId: participant.userId,
    deviceId: participant.deviceId,
    joinedAt: participant.joinedAt,
  }));
}

/**
 * Folds a join announcement into the seat list.
 *
 * An account already seated is replaced, not appended (one seat per account — the arrival is a
 * seat replacement or a re-join, and the new seat takes the end of the join order); an account
 * new to the list is appended. The arrival announcement carries no join timestamp, so the fold
 * takes the caller's `now` — honest to the moment this device learned of the seat, which is the
 * only moment it has.
 */
export function seatArrived(
  seats: readonly GroupCallSeat[],
  event: GroupCallJoinedEvent,
  now: number,
): GroupCallSeat[] {
  return [
    ...seats.filter((seat) => seat.userId !== event.userId),
    { userId: event.userId, deviceId: event.deviceId, joinedAt: now },
  ];
}

/**
 * Folds a departure announcement into the seat list: the named account's seat is gone.
 *
 * Which account left is the announcement's to say; the *device* it names is what distinguishes a
 * peer's replacement (their seat continues under a new device) from this session's own seat being
 * withdrawn — see {@link namesOwnSeat}.
 */
export function seatDeparted(
  seats: readonly GroupCallSeat[],
  event: GroupCallLeftEvent,
): GroupCallSeat[] {
  return seats.filter((seat) => seat.userId !== event.userId);
}

/**
 * Whether a departure announcement retires the call outright: the last seat has left and the call
 * no longer exists server-side. A UI that keeps showing a retired call is showing a seat list
 * nothing can ever change again.
 */
export function isCallRetired(event: GroupCallLeftEvent): boolean {
  return event.participantCount === 0;
}

/**
 * Whether an event names this session's own seat: the account this session runs as *and* the
 * device it runs on. One seat per account is what makes the device half matter — an account can
 * hold a seat from another device while this one shows the roster, and that seat's movement is
 * roster news, not this screen's end.
 *
 * A departure naming this exact pair while this device is seated can only mean the seat was
 * withdrawn from under the session — the account joined from another device and replaced it
 * (this connection never hears its *own* leave: the server skips the origin session when
 * publishing announcements). The screen owes that fact its own words, not "the call ended".
 */
export function namesOwnSeat(
  who: { userId: Id; deviceId: Id },
  me: { accountId: Id; deviceId: Id },
): boolean {
  return who.userId === me.accountId && who.deviceId === me.deviceId;
}

/**
 * Seals this build's placeholder offer for a join: an empty offer description under the call's
 * key, with the call id as associated data exactly as a media-bearing offer would be.
 *
 * Pure over the key so a test can open the envelope again; the manager mints the key.
 */
export function placeholderSealedOffer(callKey: Uint8Array, callId: Id): Uint8Array {
  return sealCallSignal(encodeSdpDescription({ type: 'offer', sdp: '' }), callKey, callId);
}
