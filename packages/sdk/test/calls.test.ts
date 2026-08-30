/**
 * The calls domain: what each signal carries, and what its listeners deliver.
 *
 * Like {@link domains.test.ts}, every test drives a real {@link Rpc} over the {@link
 * RecordingTransport} double, so both halves of each method are exercised against the generated
 * codecs: what the domain *sent* is decoded back out of the recorded frame body (a mismatched
 * struct would fail to decode or decode wrong), and what the domain *returned* is decoded from a
 * reply the test encoded. The four listeners are exercised through the double's event injection.
 *
 * Three assertions carry protocol weight beyond shape:
 *
 *   1. **The invite's `callId` is minted, not echoed.** It is the protocol's idempotency key, so
 *      two invites must never share one — and the caller learns the id it must track the call
 *      under from the reply, because the method takes none.
 *   2. **`reportStats` sends only the fields it was given.** The optional quality slots encode by
 *      presence, so a partial report must leave the absent fields off the wire entirely, not send
 *      them as zeros — a zero `rttMs` is a measurement this call never made.
 *   3. **A relay sealed for another device is not delivered.** The server fans an invite out to
 *      every device of the callee account, and a `CALL_SDP`/`CALL_ICE` addressed to a sibling
 *      device is expected fan-out noise a handler must never see — it cannot open the blob.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  decodeBody,
  encodeBody,
  CallsDomain,
  Rpc,
  CallDeclineReason,
  CallEndReason,
  CallMediaKind,
  CallState,
} from '../src/index.js';
import { OP } from '@migo/protocol';
import {
  decodeCallAnswer,
  decodeCallDecline,
  decodeCallEnd,
  decodeCallIce,
  decodeCallId,
  decodeCallInvite,
  decodeCallSdp,
  decodeCallStats,
  decodeCallTurnFetch,
  encodeAcknowledged,
  encodeCallIce,
  encodeCallInviteEvent,
  encodeCallInviteResult,
  encodeCallSdp,
  encodeCallStateEvent,
  encodeCallTurnResponse,
} from '@migo/protocol';
import type { CallInviteResult, TurnServer } from '@migo/protocol';

import { RecordingTransport, idOf } from './harness.js';

/** This device's id, as the client stamps onto invites and relays. */
const DEVICE = idOf(1);
const CONVERSATION = idOf(2);
const CALLEE = idOf(3);
const CALL = idOf(4);
const PEER_DEVICE = idOf(5);

/** Builds a domain over one recording transport, with per-opcode canned replies. */
function rig(replies: Map<number, (body: Uint8Array) => Uint8Array>): {
  transport: RecordingTransport;
  calls: CallsDomain;
} {
  const transport = new RecordingTransport();
  transport.reply = (opcode, body) => (replies.get(opcode) ?? (() => new Uint8Array()))(body);
  const rpc = new Rpc(transport.asTransport());
  return { transport, calls: new CallsDomain(rpc, DEVICE) };
}

/** A `CallInviteResult` reply that echoes the invite's own call id, as the server does. */
function inviteEchoReply(body: Uint8Array): Uint8Array {
  const invite = decodeBody(decodeCallInvite, body);
  const result: CallInviteResult = {
    callId: invite.callId,
    status: 0,
    expiresAt: AT,
  };
  return encodeBody(encodeCallInviteResult, result);
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

const SEALED = new Uint8Array([1, 2, 3, 4, 5]);
/** A Unix-ms instant after the Migo epoch (2024-01-01), so timestamps round-trip through the codec. */
const AT = 1_767_225_600_000;

test('calls: invite mints the call id, stamps this device, and returns the server verdict', async () => {
  const { transport, calls } = rig(new Map([[OP.CALL_INVITE, inviteEchoReply]]));
  const result = await calls.invite(CONVERSATION, CALLEE, CallMediaKind.Video, SEALED);

  assert.equal(transport.sent.length, 1);
  assert.equal(sentAt(transport, 0).opcode, OP.CALL_INVITE);
  const invite = decodeBody(decodeCallInvite, sentAt(transport, 0).body);
  assert.equal(invite.conversationId, CONVERSATION);
  assert.equal(invite.calleeId, CALLEE);
  assert.equal(invite.mediaKind, CallMediaKind.Video);
  assert.equal(invite.callerDevice, DEVICE, 'the invite must name this session\u2019s device');
  assert.equal(invite.capabilities, 0n, 'no capability bits are negotiated in this version');
  assert.deepEqual(invite.sealedOffer, SEALED, 'the sealed offer must pass through verbatim');
  assert.notEqual(invite.callId, CALL, 'the call id must be minted, not borrowed from the reply');

  // The caller tracks the call under the id the reply echoes — the one the domain minted.
  assert.equal(result.callId, invite.callId);
  assert.equal(result.status, 0);
  assert.equal(result.expiresAt, AT);
});

test('calls: two invites never share a call id', async () => {
  const { transport, calls } = rig(new Map([[OP.CALL_INVITE, inviteEchoReply]]));
  await calls.invite(CONVERSATION, CALLEE, CallMediaKind.Audio, SEALED);
  await calls.invite(CONVERSATION, CALLEE, CallMediaKind.Audio, SEALED);
  const first = decodeBody(decodeCallInvite, sentAt(transport, 0).body);
  const second = decodeBody(decodeCallInvite, sentAt(transport, 1).body);
  assert.notEqual(
    first.callId,
    second.callId,
    'a retried invite must re-ring the same call, but two distinct calls must not collide',
  );
});

test('calls: invite carries an app-supplied call id verbatim when given one', async () => {
  // A caller that fetches TURN relays before placing the call must address the fetch with the
  // call's own id, so the app mints the id first and hands it to the invite: the idempotency key
  // the server sees is the one the TURN credentials were minted under.
  const mine = idOf(42);
  const { transport, calls } = rig(new Map([[OP.CALL_INVITE, inviteEchoReply]]));
  const result = await calls.invite(CONVERSATION, CALLEE, CallMediaKind.Audio, SEALED, mine);
  const invite = decodeBody(decodeCallInvite, sentAt(transport, 0).body);
  assert.equal(invite.callId, mine, 'the app-minted id is the id the server must dedupe on');
  assert.equal(result.callId, mine, 'the reply echoes the id the caller already tracks');
});

test('calls: answer stamps the callee device and carries the sealed answer verbatim', async () => {
  const { transport, calls } = rig(
    new Map([[OP.CALL_ANSWER, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await calls.answer(CALL, SEALED);
  assert.equal(sentAt(transport, 0).opcode, OP.CALL_ANSWER);
  assert.deepEqual(decodeBody(decodeCallAnswer, sentAt(transport, 0).body), {
    callId: CALL,
    calleeDevice: DEVICE,
    sealedAnswer: SEALED,
  });
});

test('calls: decline defaults to a human Declined, and Busy is explicit', async () => {
  const { transport, calls } = rig(
    new Map([[OP.CALL_DECLINE, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await calls.decline(CALL);
  await calls.decline(CALL, CallDeclineReason.Busy);
  assert.deepEqual(decodeBody(decodeCallDecline, sentAt(transport, 0).body), {
    callId: CALL,
    reason: CallDeclineReason.Declined,
  });
  assert.deepEqual(decodeBody(decodeCallDecline, sentAt(transport, 1).body), {
    callId: CALL,
    reason: CallDeclineReason.Busy,
  });
});

test('calls: cancel names only the call', async () => {
  const { transport, calls } = rig(
    new Map([[OP.CALL_CANCEL, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await calls.cancel(CALL);
  assert.equal(sentAt(transport, 0).opcode, OP.CALL_CANCEL);
  assert.deepEqual(decodeBody(decodeCallId, sentAt(transport, 0).body), { callId: CALL });
});

test('calls: end always carries a reason', async () => {
  const { transport, calls } = rig(
    new Map([[OP.CALL_END, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await calls.end(CALL, CallEndReason.Network);
  assert.equal(sentAt(transport, 0).opcode, OP.CALL_END);
  assert.deepEqual(decodeBody(decodeCallEnd, sentAt(transport, 0).body), {
    callId: CALL,
    reason: CallEndReason.Network,
  });
});

test('calls: sendSdp and sendIce relay from this device to the named one', async () => {
  const { transport, calls } = rig(
    new Map([
      [OP.CALL_SDP, () => encodeBody(encodeAcknowledged, { ok: true })],
      [OP.CALL_ICE, () => encodeBody(encodeAcknowledged, { ok: true })],
    ]),
  );
  await calls.sendSdp(CALL, PEER_DEVICE, SEALED);
  await calls.sendIce(CALL, PEER_DEVICE, SEALED);

  const sdp = decodeBody(decodeCallSdp, sentAt(transport, 0).body);
  assert.equal(sdp.callId, CALL);
  assert.equal(sdp.fromDevice, DEVICE, 'a relay must name the device it came from');
  assert.equal(sdp.toDevice, PEER_DEVICE);
  assert.deepEqual(sdp.sealedSdp, SEALED);

  const ice = decodeBody(decodeCallIce, sentAt(transport, 1).body);
  assert.equal(ice.fromDevice, DEVICE);
  assert.equal(ice.toDevice, PEER_DEVICE);
  assert.deepEqual(ice.sealedCandidates, SEALED, 'the whole batch rides as one blob');
});

test('calls: getTurnServers asks for the call and hands back the relay list', async () => {
  const servers: TurnServer[] = [
    {
      url: 'turn:sg.example:3478',
      username: 'u1',
      credential: 'c1',
      ttlSeconds: 300,
      region: 'sg',
    },
    {
      url: 'turn:jp.example:3478',
      username: 'u2',
      credential: 'c2',
      ttlSeconds: 300,
      region: 'jp',
    },
  ];
  const { transport, calls } = rig(
    new Map([[OP.CALL_TURN_FETCH, () => encodeBody(encodeCallTurnResponse, { servers })]]),
  );
  const fetched = await calls.getTurnServers(CALL);
  assert.equal(sentAt(transport, 0).opcode, OP.CALL_TURN_FETCH);
  assert.deepEqual(decodeBody(decodeCallTurnFetch, sentAt(transport, 0).body), { callId: CALL });
  assert.deepEqual(fetched, servers);
});

test('calls: reportStats sends the call id plus only the fields it measured', async () => {
  const { transport, calls } = rig(
    new Map([[OP.CALL_STATS, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await calls.reportStats(CALL, { rttMs: 42, usedTurn: false });
  const stats = decodeBody(decodeCallStats, sentAt(transport, 0).body);
  assert.equal(stats.callId, CALL);
  assert.equal(stats.rttMs, 42);
  assert.equal(stats.usedTurn, false);
  // Absent fields must be absent, not zero: a zero setupMs would be a measurement never made.
  assert.equal(stats.setupMs, undefined);
  assert.equal(stats.packetLoss, undefined);
  assert.equal(stats.jitterMs, undefined);

  await calls.reportStats(CALL, {});
  const bare = decodeBody(decodeCallStats, sentAt(transport, 1).body);
  assert.deepEqual(
    bare,
    { callId: CALL },
    'a report with nothing measured carries only the call id',
  );
});

test('calls: the four listeners deliver decoded events once started, and stop cleanly', () => {
  const { transport, calls } = rig(new Map());
  const invites: string[] = [];
  const states: number[] = [];
  const sdps: string[] = [];
  const ices: string[] = [];
  calls.onIncomingCall((event) => invites.push(event.callId));
  calls.onCallState((event) => states.push(event.state));
  calls.onSdp((event) => sdps.push(event.callId));
  calls.onIce((event) => ices.push(event.callId));

  calls.start();

  transport.emit(
    OP.CALL_INVITE_EVENT,
    encodeBody(encodeCallInviteEvent, {
      callId: CALL,
      conversationId: CONVERSATION,
      callerId: CALLEE,
      callerDevice: PEER_DEVICE,
      mediaKind: CallMediaKind.Audio,
      expiresAt: AT,
      sealedOffer: SEALED,
    }),
  );
  transport.emit(
    OP.CALL_STATE_EVENT,
    encodeBody(encodeCallStateEvent, { callId: CALL, state: CallState.Connected }),
  );
  transport.emit(
    OP.CALL_STATE_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Ended,
      reason: CallEndReason.ByCallee,
    }),
  );
  transport.emit(
    OP.CALL_SDP,
    encodeBody(encodeCallSdp, {
      callId: CALL,
      fromDevice: PEER_DEVICE,
      toDevice: DEVICE,
      sealedSdp: SEALED,
    }),
  );
  transport.emit(
    OP.CALL_ICE,
    encodeBody(encodeCallIce, {
      callId: CALL,
      fromDevice: PEER_DEVICE,
      toDevice: DEVICE,
      sealedCandidates: SEALED,
    }),
  );

  assert.equal(invites.length, 1, 'the invite event was not delivered');
  assert.deepEqual(states, [CallState.Connected, CallState.Ended]);
  assert.equal(sdps.length, 1);
  assert.equal(ices.length, 1);

  calls.stop();
  transport.emit(
    OP.CALL_STATE_EVENT,
    encodeBody(encodeCallStateEvent, { callId: CALL, state: CallState.Ringing }),
  );
  assert.equal(states.length, 2, 'an event after stop() must not be delivered');
});

test('calls: a relay sealed for another device is fan-out noise and is never delivered', () => {
  const { transport, calls } = rig(new Map());
  const sdps: string[] = [];
  const ices: string[] = [];
  calls.onSdp((event) => sdps.push(event.callId));
  calls.onIce((event) => ices.push(event.callId));
  calls.start();

  const sibling = idOf(9);
  transport.emit(
    OP.CALL_SDP,
    encodeBody(encodeCallSdp, {
      callId: CALL,
      fromDevice: PEER_DEVICE,
      toDevice: sibling,
      sealedSdp: SEALED,
    }),
  );
  transport.emit(
    OP.CALL_ICE,
    encodeBody(encodeCallIce, {
      callId: CALL,
      fromDevice: PEER_DEVICE,
      toDevice: sibling,
      sealedCandidates: SEALED,
    }),
  );
  assert.equal(
    sdps.length,
    0,
    'a relay for a sibling device reached a handler that cannot open it',
  );
  assert.equal(ices.length, 0);

  transport.emit(
    OP.CALL_SDP,
    encodeBody(encodeCallSdp, {
      callId: CALL,
      fromDevice: PEER_DEVICE,
      toDevice: DEVICE,
      sealedSdp: SEALED,
    }),
  );
  assert.equal(sdps.length, 1, 'our own relay must still arrive');
});

test('calls: a throwing handler does not starve the other listeners', () => {
  const { transport, calls } = rig(new Map());
  const seen: number[] = [];
  calls.onCallState(() => {
    throw new Error('handler bug');
  });
  calls.onCallState((event) => seen.push(event.state));
  calls.start();
  transport.emit(
    OP.CALL_STATE_EVENT,
    encodeBody(encodeCallStateEvent, { callId: CALL, state: CallState.Connecting }),
  );
  assert.deepEqual(seen, [CallState.Connecting]);
});
