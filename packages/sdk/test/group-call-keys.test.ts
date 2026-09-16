/**
 * Section 163's group-call frame-key triggers: rotation on roster movement, and the sealed ask a
 * mid-call joiner sends for the running key.
 *
 * The suite runs two or three *devices* — each a real `GroupCallDomain` plus `GroupCallKeysDomain`
 * over its own recording transport and key store — and a minimal in-process stand-in for the
 * server's relay, which is the only third party these frames ever cross:
 *
 *   - a `CALL_KEY_UPDATE` one device sends is delivered to every other device, byte-for-byte (the
 *     server fans it out to the whole roster);
 *   - a `CALL_RENEGOTIATE` is delivered to its target as a `CALL_SDP` — the projection the group
 *     relay performs, which is what lets a key request ride the renegotiation frame;
 *   - a `CALL_SDP` is delivered to the device it addresses.
 *
 * Roster frames (snapshots and announcements) are emitted by the test exactly as the server
 * publishes them. Between steps, `settle` drains the fire-and-forget distribution promises and
 * relays whatever they recorded until nothing new appears, so each assertion reads a call whose
 * keys have actually converged — or demonstrably failed to.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  OP,
  encodeAcknowledged,
  encodeCallKeyUpdate,
  encodeCallSdp,
  encodeCallStateEvent,
  decodeCallKeyUpdate,
  decodeCallRenegotiate,
  decodeCallSdp,
} from '@migo/protocol';
import type { CallKeyUpdate, CallRenegotiate, CallSdp, CallSfuParticipant } from '@migo/protocol';
import { idFromBytes, idToBytes } from '@migo/wire';
import type { Id } from '@migo/wire';

import {
  CALL_KEY_ASK_EVENT,
  CallState,
  ContentType,
  decodeBody,
  decodeContent,
  encodeBody,
  GroupCallDomain,
  GroupCallKeysDomain,
  Rpc,
  SessionCrypto,
} from '../src/index.js';
import type { EventErrorHandler } from '../src/index.js';

import { RecordingTransport, StaticBundleSource, bundleFrom, idOf, newStore } from './harness.js';
import type { KeyStore } from '../src/index.js';

/** One device's place in the test: its ids, its transport, its two domains, and its key store. */
interface Node {
  user: Id;
  device: Id;
  store: KeyStore;
  transport: RecordingTransport;
  sessionCrypto: SessionCrypto;
  groupCalls: GroupCallDomain;
  callKeys: GroupCallKeysDomain;
  /** How many recorded frames the relay has already delivered. */
  cursor: number;
  /** Errors surfaced through the domain's error sink, opcode first (payloads stay out of logs). */
  errors: Array<{ opcode: number; cause: unknown }>;
}

const CONVERSATION = idOf(20);
const CALL = idOf(21);
/** A Unix-ms instant after the Migo epoch (2024-01-01), so timestamps round-trip through the codec. */
const AT = 1_767_225_600_000;
const SEALED_OFFER = new Uint8Array([1, 2, 3, 4, 5]);

/**
 * Builds one started device. `peers` are the stores whose published bundles this device's session
 * layer may initiate against — a mid-call joiner initiates to the device it asks, so a joiner's
 * rig is built with the distributor's store.
 */
function node(user: Id, device: Id, peers: KeyStore[] = []): Node {
  const transport = new RecordingTransport();
  transport.reply = () => encodeBody(encodeAcknowledged, { ok: true });
  const rpc = new Rpc(transport.asTransport());
  const store = newStore();
  const peer = peers[0];
  const sessionCrypto = new SessionCrypto(store, new StaticBundleSource(bundleFrom(peer ?? store)));
  const groupCalls = new GroupCallDomain(rpc, device);
  const errors: Array<{ opcode: number; cause: unknown }> = [];
  const onEventError: EventErrorHandler = (opcode, cause) => errors.push({ opcode, cause });
  const callKeys = new GroupCallKeysDomain(
    rpc,
    groupCalls,
    sessionCrypto,
    user,
    device,
    onEventError,
  );
  groupCalls.start();
  callKeys.start();
  return { user, device, store, transport, sessionCrypto, groupCalls, callKeys, cursor: 0, errors };
}

/** One roster line, as the server projects a seated participant. */
function seat(user: Id, device: Id): CallSfuParticipant {
  return { userId: user, deviceId: device, joinedAt: AT, sealedOffer: SEALED_OFFER };
}

/** The roster snapshot the server publishes to a joiner's own topic. */
function snapshot(to: Node, seats: Node[], size: number): void {
  to.transport.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Connected,
      conversationId: CONVERSATION,
      userId: to.user,
      deviceId: to.device,
      participantCount: size,
      participants: seats.map((seated) => seat(seated.user, seated.device)),
    }),
  );
}

/** The join announcement the conversation's topic carries to everyone seated but the joiner. */
function announceJoin(to: Node, joiner: Node, size: number): void {
  to.transport.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Connected,
      conversationId: CONVERSATION,
      userId: joiner.user,
      deviceId: joiner.device,
      participantCount: size,
      sealedOffer: SEALED_OFFER,
    }),
  );
}

/** The departure announcement the conversation's topic carries. */
function announceLeft(to: Node, leaver: Node, size: number): void {
  to.transport.emit(
    OP.CALL_SFU_EVENT,
    encodeBody(encodeCallStateEvent, {
      callId: CALL,
      state: CallState.Ended,
      conversationId: CONVERSATION,
      userId: leaver.user,
      deviceId: leaver.device,
      participantCount: size,
    }),
  );
}

/** Lets the fire-and-forget distribution promises run a few microtasks. */
async function flush(times = 12): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await Promise.resolve();
  }
}

/**
 * The in-process relay: delivers every frame a device recorded since its last relay exactly as
 * the server would — updates to everyone, renegotiations projected to `CALL_SDP` for their
 * target, `CALL_SDP`s to the device they address.
 */
function relay(nodes: Node[]): void {
  for (const from of nodes) {
    const fresh = from.transport.sent.slice(from.cursor);
    from.cursor = from.transport.sent.length;
    for (const frame of fresh) {
      if (frame.opcode === OP.CALL_KEY_UPDATE) {
        for (const to of nodes) {
          if (to !== from) {
            to.transport.emit(OP.CALL_KEY_UPDATE, frame.body);
          }
        }
      } else if (frame.opcode === OP.CALL_RENEGOTIATE) {
        // The group relay projects a renegotiation to CALL_SDP for its target; that projection is
        // what the ask rides, so the relay must perform it here too.
        const ask = decodeBody(decodeCallRenegotiate, frame.body);
        for (const to of nodes) {
          if (to.device === ask.toDevice) {
            const projected: CallSdp = {
              callId: ask.callId,
              fromDevice: ask.fromDevice,
              toDevice: ask.toDevice,
              sealedSdp: ask.sealedSdp,
            };
            to.transport.emit(OP.CALL_SDP, encodeBody(encodeCallSdp, projected));
          }
        }
      } else if (frame.opcode === OP.CALL_SDP) {
        const sdp = decodeBody(decodeCallSdp, frame.body);
        for (const to of nodes) {
          if (to !== from && to.device === sdp.toDevice) {
            to.transport.emit(OP.CALL_SDP, frame.body);
          }
        }
      }
    }
  }
}

/** Runs promises and relays frames until the call's key traffic stops moving, then one last drain. */
async function settle(nodes: Node[]): Promise<void> {
  for (let round = 0; round < 8; round += 1) {
    await flush();
    const before = nodes.reduce((total, each) => total + each.transport.sent.length, 0);
    relay(nodes);
    const after = nodes.reduce((total, each) => total + each.transport.sent.length, 0);
    if (before === after) {
      break;
    }
  }
  await flush();
}

/** The CALL_KEY_UPDATE frames a device has sent, decoded. */
function updatesOf(node: Node): CallKeyUpdate[] {
  return node.transport.sent
    .filter((frame) => frame.opcode === OP.CALL_KEY_UPDATE)
    .map((frame) => decodeBody(decodeCallKeyUpdate, frame.body));
}

/** The CALL_RENEGOTIATE frames a device has sent, decoded. */
function asksOf(node: Node): CallRenegotiate[] {
  return node.transport.sent
    .filter((frame) => frame.opcode === OP.CALL_RENEGOTIATE)
    .map((frame) => decodeBody(decodeCallRenegotiate, frame.body));
}

test('call keys: the first seat mints its key and sends nothing', async () => {
  const a = node(idOf(1), idOf(2));
  snapshot(a, [a], 1);
  await settle([a]);

  assert.equal(a.callKeys.hasKey(CALL), true);
  assert.equal(a.callKeys.keyEpoch(CALL), 0);
  // Alone in the call there is no membership movement to rotate for and nobody to ask or answer:
  // the first frame this device sends is none at all.
  assert.equal(a.transport.sent.length, 0);
});

test('call keys: a mid-call join asks the first seated participant and installs the running key', async () => {
  const a = node(idOf(1), idOf(2));
  snapshot(a, [a], 1);
  // A frame sealed under the epoch-0 key before anyone else joins: the joiner must never open it.
  const preJoin = a.callKeys.sealFrame(CALL, new TextEncoder().encode('before b joined'));

  // B joins a call in progress; its session layer must be able to initiate to A, so its rig is
  // built against A's store — the mirror of the server handing B A's prekey bundle.
  const b = node(idOf(3), idOf(4), [a.store]);
  snapshot(b, [a, b], 2);
  await settle([a, b]);

  // The ask rode the renegotiation frame, addressed to the first seat in the snapshot's join
  // order — the deterministic distributor choice every joiner computes the same way.
  const asks = asksOf(b);
  assert.equal(asks.length, 1);
  assert.equal(asks[0]?.callId, CALL);
  assert.equal(asks[0]?.fromDevice, b.device);
  assert.equal(asks[0]?.toDevice, a.device, 'the ask must go to the first seated participant');

  // A rotated on the join (the ask was the first news of it) and answered; B holds the running
  // key, and the two devices are on the same epoch without A depending on its own update coming
  // back — the server does not relay a CALL_KEY_UPDATE to its sender.
  assert.equal(b.callKeys.hasKey(CALL), true);
  assert.equal(a.callKeys.keyEpoch(CALL), 1);
  assert.equal(b.callKeys.keyEpoch(CALL), 1);
  assert.equal(updatesOf(a).length, 1, 'exactly one rotation for the join');

  // Media sealed by A after the hand-off opens on B; the pre-join frame does not — that is the
  // line the join path draws around the joiner.
  const post = a.callKeys.sealFrame(CALL, new TextEncoder().encode('after b joined'));
  assert.deepEqual(b.callKeys.openFrame(CALL, post), new TextEncoder().encode('after b joined'));
  assert.throws(() => {
    b.callKeys.openFrame(CALL, preJoin);
  }, 'a mid-call joiner must not open the media that predates it');
});

test('call keys: the ask carries the joiner account id as the control event data', async () => {
  const a = node(idOf(1), idOf(2));
  snapshot(a, [a], 1);
  const b = node(idOf(3), idOf(4), [a.store]);
  snapshot(b, [a, b], 2);
  // Flush only — no relay — so the distributor's session layer has not opened the ask yet and the
  // open below is the first, exactly the one a real holder performs.
  for (let round = 0; round < 8 && asksOf(b).length === 0; round += 1) {
    await flush();
  }

  const asks = asksOf(b);
  assert.equal(asks.length, 1, 'the joiner asked exactly once');
  const ask = asks[0];
  assert.ok(ask !== undefined);

  // Open the ask the way its holder does: the pairwise envelope, then the inner control event. The
  // data must be the joiner's *account* id — the one fact the frame's fromDevice cannot say, and
  // the one the desktop holder's designated-rotator rule requires (an ask without it is ignored
  // there, so this pins the cross-client contract byte-for-byte).
  const plaintext = a.sessionCrypto.open(CONVERSATION, b.user, b.device, ask.sealedSdp);
  const content = decodeContent(plaintext);
  assert.equal(content.type, ContentType.ControlEvent);
  assert.equal(content.event, CALL_KEY_ASK_EVENT);
  assert.ok(content.data !== undefined, 'the ask must carry data');
  assert.equal(content.data.length, 16, 'the data is one id, 16 bytes');
  assert.deepEqual(content.data, idToBytes(b.user), 'the data is the joiner account id');
});

test('call keys: only the first seat rotates on roster movement, and everyone else adopts', async () => {
  const a = node(idOf(1), idOf(2));
  snapshot(a, [a], 1);
  const b = node(idOf(3), idOf(4), [a.store]);
  snapshot(b, [a, b], 2);
  await settle([a, b]);
  const epochBeforeJoin = a.callKeys.keyEpoch(CALL);
  assert.ok(epochBeforeJoin !== null);
  const preJoin = a.callKeys.sealFrame(CALL, new TextEncoder().encode('epoch before c'));

  // C joins a call already two seats deep. The distributor choice is still the first seat (A),
  // even though B is seated too — join order decides, not proximity.
  const c = node(idOf(5), idOf(6), [a.store]);
  snapshot(c, [a, b, c], 3);
  announceJoin(a, c, 3);
  announceJoin(b, c, 3);
  await settle([a, b, c]);

  const rotatedTo = epochBeforeJoin + 1;
  // A — the first seat — rotated exactly once more; B — also seated, not the rotator — sent nothing.
  assert.equal(a.callKeys.keyEpoch(CALL), rotatedTo);
  assert.equal(updatesOf(a).length, 2, 'one rotation for B, one for C');
  assert.equal(updatesOf(b).length, 0, 'a device that is not the rotator never mints a key');
  // B adopted the announcement-driven update; C installed by the ask path. All three agree.
  assert.equal(b.callKeys.keyEpoch(CALL), rotatedTo);
  assert.equal(c.callKeys.keyEpoch(CALL), rotatedTo);

  const post = a.callKeys.sealFrame(CALL, new TextEncoder().encode('epoch after c'));
  for (const seated of [b, c]) {
    assert.deepEqual(
      seated.callKeys.openFrame(CALL, post),
      new TextEncoder().encode('epoch after c'),
    );
  }
  // And the pre-rotation media stays sealed to everyone who joined or adopted after it.
  assert.throws(() => {
    b.callKeys.openFrame(CALL, preJoin);
  });

  // A departure rotates again: the leaver must not decrypt what follows.
  const afterDeparture = rotatedTo + 1;
  announceLeft(a, c, 2);
  announceLeft(b, c, 2);
  await settle([a, b]);

  assert.equal(a.callKeys.keyEpoch(CALL), afterDeparture);
  assert.equal(b.callKeys.keyEpoch(CALL), afterDeparture, 'B adopted the departure rotation');
  const after = a.callKeys.sealFrame(CALL, new TextEncoder().encode('after c left'));
  assert.deepEqual(b.callKeys.openFrame(CALL, after), new TextEncoder().encode('after c left'));
});

test('call keys: a replayed update changes nothing, and an unopenable one surfaces and survives', async () => {
  const a = node(idOf(1), idOf(2));
  snapshot(a, [a], 1);
  const b = node(idOf(3), idOf(4), [a.store]);
  snapshot(b, [a, b], 2);
  await settle([a, b]);
  const epoch = a.callKeys.keyEpoch(CALL);
  assert.ok(epoch !== null);

  // A replay of the update B already adopted: the epoch does not advance, so it is dropped — the
  // call-side face of the rule the message ratchets enforce.
  const update = updatesOf(a)[0];
  assert.ok(update !== undefined);
  b.transport.emit(OP.CALL_KEY_UPDATE, encodeBody(encodeCallKeyUpdate, update));
  assert.equal(b.callKeys.keyEpoch(CALL), epoch, 'a replayed update must not move the epoch');

  // An update B cannot open (it missed the earlier rotation the material was sealed under): the
  // held key survives, and the fact is surfaced to the error sink — a stranded seat is a fact a
  // caller may want to act on.
  const before = b.errors.length;
  b.transport.emit(
    OP.CALL_KEY_UPDATE,
    encodeBody(encodeCallKeyUpdate, {
      callId: CALL,
      epoch: epoch + 5,
      sealedKeyMaterial: new Uint8Array([9, 9, 9]),
    }),
  );
  assert.equal(b.callKeys.keyEpoch(CALL), epoch, 'the held key survives an unopenable update');
  assert.equal(b.errors.length, before + 1, 'the failed adoption surfaced on the error sink');
  const still = a.callKeys.sealFrame(CALL, new TextEncoder().encode('still the same epoch'));
  assert.deepEqual(
    b.callKeys.openFrame(CALL, still),
    new TextEncoder().encode('still the same epoch'),
  );
});

test("call keys: this device's own departure drops its tracked state", async () => {
  const a = node(idOf(1), idOf(2));
  snapshot(a, [a], 1);
  const b = node(idOf(3), idOf(4), [a.store]);
  snapshot(b, [a, b], 2);
  await settle([a, b]);
  assert.equal(b.callKeys.hasKey(CALL), true);

  // A seat replacement: this account's departure naming this device is the seat being withdrawn
  // from under the session — this device is no longer in the call and holds no further duty in it.
  announceLeft(b, b, 1);
  assert.equal(b.callKeys.hasKey(CALL), false);
});

test('call keys: a frame for another call or device is left for the signaling layer', async () => {
  const a = node(idOf(1), idOf(2));
  snapshot(a, [a], 1);
  const other = idFromBytes(new Uint8Array(16).fill(0x33));

  // An update for a call this device is not seated in, and an SDP addressed to another device:
  // neither is key state, and neither is an error — the signaling domain owns those frames.
  const errorsBefore = a.errors.length;
  a.transport.emit(
    OP.CALL_KEY_UPDATE,
    encodeBody(encodeCallKeyUpdate, {
      callId: other,
      epoch: 3,
      sealedKeyMaterial: new Uint8Array([1]),
    }),
  );
  a.transport.emit(
    OP.CALL_SDP,
    encodeBody(encodeCallSdp, {
      callId: CALL,
      fromDevice: idOf(9),
      toDevice: idOf(10),
      sealedSdp: new Uint8Array([2]),
    }),
  );
  await settle([a]);
  assert.equal(a.errors.length, errorsBefore);
  assert.equal(a.callKeys.keyEpoch(CALL), 0, 'an untracked call changed nothing');
});

// --- onKeyChanged: the media plane's two moments ---

test('call keys: onKeyChanged fires when the first key is held and on every advance', async () => {
  const a = node(idOf(1), idOf(2));
  const events: Id[] = [];
  a.callKeys.onKeyChanged((callId) => events.push(callId));

  // The first seat's mint: the first moment the media plane can seal anything.
  snapshot(a, [a], 1);
  assert.deepEqual(events, [CALL], 'the mint announces the key');

  // A joins-in-progress peer: its key arrives as the distributor's answer, which is its first
  // held key — the listener's other first-moment shape.
  const b = node(idOf(3), idOf(4), [a.store]);
  const bEvents: Id[] = [];
  b.callKeys.onKeyChanged((callId) => bEvents.push(callId));
  snapshot(b, [a, b], 2);
  announceJoin(a, b, 2);
  await settle([a, b]);

  assert.equal(b.callKeys.hasKey(CALL), true);
  assert.equal(bEvents.length >= 1, true, 'the joiner heard its installed key');
  assert.deepEqual(bEvents.filter((id) => id === CALL).length, bEvents.length);

  // Every epoch advance after that — the rotator's own and the adopters' — announces too, because
  // a rotation invalidates the sealing of anything a media plane still had in flight.
  const rotatorEpochs: Array<number | null> = [];
  const adopterEpochs: Array<number | null> = [];
  a.callKeys.onKeyChanged(() => rotatorEpochs.push(a.callKeys.keyEpoch(CALL)));
  b.callKeys.onKeyChanged(() => adopterEpochs.push(b.callKeys.keyEpoch(CALL)));
  const c = node(idOf(5), idOf(6), [a.store]);
  snapshot(c, [a, b, c], 3);
  announceJoin(a, c, 3);
  announceJoin(b, c, 3);
  await settle([a, b, c]);

  assert.equal(a.callKeys.keyEpoch(CALL), b.callKeys.keyEpoch(CALL));
  assert.ok(rotatorEpochs.includes(a.callKeys.keyEpoch(CALL)), 'the rotator heard its own advance');
  assert.ok(adopterEpochs.includes(b.callKeys.keyEpoch(CALL)), 'the adopter heard the update');
});

test('call keys: onKeyChanged stays silent for frames that change no key state', async () => {
  const a = node(idOf(1), idOf(2));
  snapshot(a, [a], 1);
  await settle([a]);
  const events: Id[] = [];
  a.callKeys.onKeyChanged((callId) => events.push(callId));

  // A replayed epoch-0 update (the epoch does not advance) and an update for an untracked call:
  // neither is a key-state change, so neither may wake a media plane.
  a.transport.emit(
    OP.CALL_KEY_UPDATE,
    encodeBody(encodeCallKeyUpdate, {
      callId: CALL,
      epoch: 0,
      sealedKeyMaterial: new Uint8Array([1, 2, 3]),
    }),
  );
  a.transport.emit(
    OP.CALL_KEY_UPDATE,
    encodeBody(encodeCallKeyUpdate, {
      callId: idFromBytes(new Uint8Array(16).fill(0x44)),
      epoch: 5,
      sealedKeyMaterial: new Uint8Array([4]),
    }),
  );
  await settle([a]);
  assert.deepEqual(events, [], 'no key-state change, no event');
  assert.equal(a.callKeys.keyEpoch(CALL), 0);
});
