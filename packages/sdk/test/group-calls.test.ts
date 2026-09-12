/**
 * The group-call domain: what a join sends, and how the three roster events are classified.
 *
 * Like {@link calls.test.ts}, every test drives a real {@link Rpc} over the {@link
 * RecordingTransport} double, so both halves of each method are exercised against the generated
 * codecs: what the domain *sent* is decoded back out of the recorded frame body, and what its
 * listeners deliver is decoded from frames the test encodes exactly as the server publishes them.
 *
 * Three assertions carry protocol weight beyond shape:
 *
 *   1. **The join's `callId` is minted, not echoed.** It is the join's idempotency key, and the
 *      snapshot and announcements all name the call under that id — the application must track
 *      the call under the id the join resolved with.
 *   2. **The join is an invite frame with group semantics.** The server reads a `CallInvite` with
 *      `calleeId` ignored and `callerDevice` taken from the connection — but the frame is the
 *      1:1 struct, so the honest values ride the slots: nil callee, this session's device, zero
 *      capabilities.
 *   3. **Classification is by shape, not topic.** The wire gives all three group-call facts one
 *      opcode; the snapshot is the frame carrying a participant list, a join is `Connected`, a
 *      departure is `Ended`. A roster UI must never have to re-derive which kind it just got —
 *      and a departure with `participantCount` 0 (the retirement) must still arrive, because
 *      that is the frame that tells a screen to clear the call.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  decodeBody,
  encodeBody,
  GroupCallDomain,
  Rpc,
  CallEndReason,
  CallMediaKind,
  CallState,
} from '../src/index.js';
import { OP } from '@migo/protocol';
import { NIL_ID } from '@migo/wire';
import {
  decodeCallEnd,
  decodeCallInvite,
  encodeAcknowledged,
  encodeCallStateEvent,
  encodeCallTurnResponse,
} from '@migo/protocol';
import type { CallSfuParticipant, TurnServer } from '@migo/protocol';

import { RecordingTransport, idOf } from './harness.js';

/** This device's id, as the client stamps onto the join. */
const DEVICE = idOf(1);
const CONVERSATION = idOf(2);
const CALL = idOf(4);
const PEER = idOf(5);
const PEER_DEVICE = idOf(6);

/** A Unix-ms instant after the Migo epoch (2024-01-01), so timestamps round-trip through the codec. */
const AT = 1_767_225_600_000;
const SEALED = new Uint8Array([1, 2, 3, 4, 5]);

/** Builds a domain over one recording transport, with per-opcode canned replies. */
function rig(replies: Map<number, (body: Uint8Array) => Uint8Array>): {
  transport: RecordingTransport;
  calls: GroupCallDomain;
} {
  const transport = new RecordingTransport();
  transport.reply = (opcode, body) => (replies.get(opcode) ?? (() => new Uint8Array()))(body);
  const rpc = new Rpc(transport.asTransport());
  return { transport, calls: new GroupCallDomain(rpc, DEVICE) };
}

/** A `CallTurnResponse` reply with one relay, as the join's answer carries. */
function turnReply(): Uint8Array {
  const servers: TurnServer[] = [
    {
      url: 'turn:sg.example:3478',
      username: 'u1',
      credential: 'c1',
      ttlSeconds: 300,
      region: 'sg',
    },
  ];
  return encodeBody(encodeCallTurnResponse, { servers });
}

/** One roster line, as the server projects a seated participant. */
function seat(userId: ReturnType<typeof idOf>, sealedOffer = SEALED): CallSfuParticipant {
  return { userId, deviceId: userId, joinedAt: AT, sealedOffer };
}

/** The frame recorded at `index`, narrowed to present (see domains.test.ts for the rationale). */
function sentAt(
  transport: RecordingTransport,
  index: number,
): { opcode: number; body: Uint8Array } {
  const frame = transport.sent[index];
  assert.ok(frame !== undefined, `expected a recorded frame at index ${index}`);
  return frame;
}

test('group calls: join mints the call id, stamps the group invite shape, and returns the relays', async () => {
  const { transport, calls } = rig(new Map([[OP.CALL_SFU_JOIN, turnReply]]));
  const result = await calls.join(CONVERSATION, CallMediaKind.Video, SEALED);

  assert.equal(transport.sent.length, 1);
  assert.equal(sentAt(transport, 0).opcode, OP.CALL_SFU_JOIN);
  const invite = decodeBody(decodeCallInvite, sentAt(transport, 0).body);
  assert.equal(invite.conversationId, CONVERSATION);
  assert.equal(invite.mediaKind, CallMediaKind.Video);
  assert.equal(invite.callerDevice, DEVICE, 'the join must name this session’s device');
  assert.equal(
    invite.calleeId,
    NIL_ID,
    'a group call has no single callee; the slot rides as the nil id',
  );
  assert.equal(invite.capabilities, 0n, 'no capability bits are negotiated in this version');
  assert.deepEqual(invite.sealedOffer, SEALED, 'the sealed offer must pass through verbatim');

  // The joiner tracks the call under the id the server dedupes on — the one the domain minted.
  assert.deepEqual(result.callId, invite.callId);
  assert.equal(result.servers.length, 1);
  assert.equal(result.servers[0]?.url, 'turn:sg.example:3478');
});

test('group calls: two joins never share a call id, and an app-minted id rides verbatim', async () => {
  const { transport, calls } = rig(new Map([[OP.CALL_SFU_JOIN, turnReply]]));
  await calls.join(CONVERSATION, CallMediaKind.Audio, SEALED);
  await calls.join(CONVERSATION, CallMediaKind.Audio, SEALED);
  const first = decodeBody(decodeCallInvite, sentAt(transport, 0).body);
  const second = decodeBody(decodeCallInvite, sentAt(transport, 1).body);
  assert.notEqual(
    first.callId,
    second.callId,
    'a retried join re-seats the same call, but two distinct joins must not collide',
  );

  // A joiner that fetches TURN relays before joining must address the fetch with the call's own
  // id, so the app mints first and hands the join the id: the idempotency key the server sees is
  // the one the credentials were minted under.
  const mine = idOf(42);
  const result = await calls.join(CONVERSATION, CallMediaKind.Audio, SEALED, mine);
  const third = decodeBody(decodeCallInvite, sentAt(transport, 2).body);
  assert.equal(third.callId, mine, 'the app-minted id is the id the server must dedupe on');
  assert.equal(result.callId, mine, 'the reply resolves with the id the caller already tracks');
});

test('group calls: leave is the 1:1 end frame with the withdrawal reason', async () => {
  const { transport, calls } = rig(
    new Map([[OP.CALL_END, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await calls.leave(CALL);
  assert.equal(sentAt(transport, 0).opcode, OP.CALL_END);
  assert.deepEqual(decodeBody(decodeCallEnd, sentAt(transport, 0).body), {
    callId: CALL,
    // The same reason the departure announcement carries, because that is the fact: the
    // participant withdrew themselves.
    reason: CallEndReason.ByCaller,
  });
});

test('group calls: a snapshot on the user topic delivers the roster, verbatim and in join order', () => {
  const { transport, calls } = rig(new Map());
  const rosters: unknown[] = [];
  calls.onRoster((event) => rosters.push(event));
  calls.start();

  const roster = [seat(PEER), seat(DEVICE)];
  transport.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Connected,
      conversationId: CONVERSATION,
      userId: DEVICE,
      deviceId: DEVICE,
      participantCount: roster.length,
      participants: roster,
    }),
  );

  assert.equal(rosters.length, 1, 'the snapshot was not delivered');
  const event = rosters[0] as {
    callId: unknown;
    participants: CallSfuParticipant[];
    participantCount: number;
  };
  assert.deepEqual(event.participants, roster, 'the roster must arrive in join order, verbatim');
  assert.equal(event.participantCount, roster.length);

  calls.stop();
  transport.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Connected,
      conversationId: CONVERSATION,
      userId: DEVICE,
      deviceId: DEVICE,
      participantCount: 2,
      participants: roster,
    }),
  );
  assert.equal(rosters.length, 1, 'an event after stop() must not be delivered');
});

test('group calls: a join announcement delivers the joiner with their sealed offer', () => {
  const { transport, calls } = rig(new Map());
  const joins: { userId: unknown; participantCount: number; sealedOffer?: Uint8Array }[] = [];
  calls.onParticipantJoined((event) => joins.push(event));
  calls.start();

  transport.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Connected,
      conversationId: CONVERSATION,
      userId: PEER,
      deviceId: PEER_DEVICE,
      participantCount: 2,
      sealedOffer: SEALED,
    }),
  );

  assert.equal(joins.length, 1);
  assert.deepEqual(joins[0]?.userId, PEER);
  assert.equal(joins[0]?.participantCount, 2);
  assert.deepEqual(
    joins[0]?.sealedOffer,
    SEALED,
    'the sealed offer is the E2E hand-off and must arrive verbatim',
  );
});

test('group calls: a departure delivers the leaver, and the retirement still arrives', () => {
  const { transport, calls } = rig(new Map());
  const lefts: { userId: unknown; participantCount: number }[] = [];
  calls.onParticipantLeft((event) => lefts.push(event));
  calls.start();

  transport.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Ended,
      reason: CallEndReason.ByCaller,
      conversationId: CONVERSATION,
      userId: PEER,
      deviceId: PEER_DEVICE,
      participantCount: 1,
    }),
  );
  assert.equal(lefts.length, 1);
  assert.deepEqual(lefts[0]?.userId, PEER);
  assert.equal(lefts[0]?.participantCount, 1);

  // The retirement: the last seat leaves, the count hits zero, and the frame is the one that
  // tells a screen to clear the call — it must arrive like any other departure.
  transport.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Ended,
      reason: CallEndReason.ByCaller,
      conversationId: CONVERSATION,
      userId: DEVICE,
      deviceId: DEVICE,
      participantCount: 0,
    }),
  );
  assert.equal(lefts.length, 2);
  assert.equal(lefts[1]?.participantCount, 0, 'the retirement is a departure like any other');
});

test('group calls: classification is by shape — a Connected frame with a roster is a snapshot, not a join', () => {
  const { transport, calls } = rig(new Map());
  const rosters: unknown[] = [];
  const joins: unknown[] = [];
  const lefts: unknown[] = [];
  calls.onRoster((event) => rosters.push(event));
  calls.onParticipantJoined((event) => joins.push(event));
  calls.onParticipantLeft((event) => lefts.push(event));
  calls.start();

  // The snapshot rides state Connected — the same state a join announcement carries — and is
  // told apart only by the participant list. A classifier keyed on state alone would hand a
  // joiner their own roster as a stranger's arrival.
  transport.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Connected,
      conversationId: CONVERSATION,
      userId: DEVICE,
      deviceId: DEVICE,
      participantCount: 1,
      participants: [seat(DEVICE)],
    }),
  );
  assert.equal(rosters.length, 1);
  assert.equal(joins.length, 0, 'a snapshot must not also deliver as a join');
  assert.equal(lefts.length, 0);

  // Neither the 1:1 state stream's frames nor any other state arrives on this opcode, but a
  // malformed group frame — a snapshot missing its conversation — goes to the error sink, not a
  // handler.
  const errors: number[] = [];
  const transport2 = new RecordingTransport();
  const calls2 = new GroupCallDomain(new Rpc(transport2.asTransport()), DEVICE, (opcode) =>
    errors.push(opcode),
  );
  calls2.onRoster((event) => rosters.push(event));
  calls2.start();
  transport2.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Connected,
      userId: DEVICE,
      deviceId: DEVICE,
      participantCount: 1,
      participants: [seat(DEVICE)],
    }),
  );
  assert.equal(rosters.length, 1, 'only the well-formed snapshot was delivered');
  assert.deepEqual(errors, [OP.CALL_SFU_EVENT], 'the malformed snapshot went to the error sink');
});

test('group calls: a throwing handler does not starve the other listeners', () => {
  const { transport, calls } = rig(new Map());
  const seen: unknown[] = [];
  calls.onParticipantJoined(() => {
    throw new Error('handler bug');
  });
  calls.onParticipantJoined((event) => seen.push(event));
  calls.start();
  transport.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Connected,
      conversationId: CONVERSATION,
      userId: PEER,
      deviceId: PEER_DEVICE,
      participantCount: 2,
    }),
  );
  assert.equal(seen.length, 1);
});
