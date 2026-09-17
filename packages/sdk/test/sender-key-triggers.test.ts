/**
 * Section 163's first client-side trigger: the sender-key redistribution a member event owes.
 *
 * Every test runs two real devices' crypto — the distributing device's `SessionCrypto` initiating
 * against the receiving store's published prekeys, and the receiving `MessagingDomain` accepting
 * what arrives over the recorded `GROUP_KEY_DISTRIBUTE` frame — so the assertions span the whole
 * path the spec cares about: one frame per member device, each sealed under the pairwise session
 * with that device, arriving as a distribution the receiver adopts under the ratchet's own rules
 * (first distribution is the baseline, non-advancing epochs are refused, duplicate sends are
 * no-ops).
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  OP,
  encodeAcknowledged,
  encodeGroupKeyDistribution,
  decodeGroupKeyDistribution,
} from '@migo/protocol';
import type { GroupKeyDistribution } from '@migo/protocol';

import {
  ContentType,
  decodeBody,
  encodeBody,
  encodeContent,
  GroupCrypto,
  MessagingDomain,
  Rpc,
  SessionCrypto,
  SdkError,
} from '../src/index.js';
import type { DeviceAddress, DeviceDirectory, EventErrorHandler } from '../src/index.js';

import { RecordingTransport, StaticBundleSource, bundleFrom, idOf, newStore } from './harness.js';
import type { KeyStore } from '../src/index.js';

const CONVERSATION = idOf(1);
/** The distributing device, as the client stamps onto every frame it sends. */
const OWN_DEVICE = idOf(2);
/** Two member devices the redistribution must reach, one frame each. */
const PEER_B_USER = idOf(3);
const PEER_B_DEVICE = idOf(4);
const PEER_C_USER = idOf(5);
const PEER_C_DEVICE = idOf(6);
/** A late joiner: present in the directory, never yet sent a distribution. */
const PEER_D_USER = idOf(7);
const PEER_D_DEVICE = idOf(8);

/** The control-event name the messaging domain treats as key material; a client-to-client constant. */
const SENDER_KEY_EVENT = 'sender-key';

/** A directory over a fixed address list the test can swap out between calls. */
class ListDirectory implements DeviceDirectory {
  #devices: DeviceAddress[];

  constructor(devices: DeviceAddress[]) {
    this.#devices = devices;
  }

  set(devices: DeviceAddress[]): void {
    this.#devices = devices;
  }

  recipientDevices(): Promise<DeviceAddress[]> {
    return Promise.resolve(this.#devices);
  }
}

/**
 * One device's messaging rig: a real `SessionCrypto` over the given store, a real `GroupCrypto`,
 * and a `MessagingDomain` wired to a recording transport. `peers` are the stores whose published
 * bundles this device's session layer may initiate against.
 */
function rig(
  options: {
    store?: KeyStore;
    peers?: KeyStore[];
    deviceId?: number;
    devices?: DeviceAddress[];
    reply?: (opcode: number, body: Uint8Array) => Uint8Array;
    onEventError?: EventErrorHandler;
  } = {},
): {
  transport: RecordingTransport;
  messaging: MessagingDomain;
  sessionCrypto: SessionCrypto;
  groupCrypto: GroupCrypto;
  directory: ListDirectory;
} {
  const transport = new RecordingTransport();
  transport.reply = options.reply ?? (() => encodeBody(encodeAcknowledged, { ok: true }));
  const rpc = new Rpc(transport.asTransport());
  const store = options.store ?? newStore();
  const peer = options.peers?.[0];
  const ownDevice = idOf(options.deviceId ?? 2);
  const sessionCrypto = new SessionCrypto(store, new StaticBundleSource(bundleFrom(peer ?? store)));
  const groupCrypto = new GroupCrypto(store);
  const directory = new ListDirectory(options.devices ?? []);
  const messaging = new MessagingDomain(
    rpc,
    sessionCrypto,
    groupCrypto,
    directory,
    options.onEventError,
    undefined,
    options.deviceId === undefined ? undefined : ownDevice,
  );
  return { transport, messaging, sessionCrypto, groupCrypto, directory };
}

/** The `GROUP_KEY_DISTRIBUTE` frames recorded so far, decoded. */
function distributions(transport: RecordingTransport): GroupKeyDistribution[] {
  return transport.sent
    .filter((frame) => frame.opcode === OP.GROUP_KEY_DISTRIBUTE)
    .map((frame) => decodeBody(decodeGroupKeyDistribution, frame.body));
}

test('triggers: redistributeSenderKey refuses to run without the device id', async () => {
  const { messaging } = rig();
  await assert.rejects(
    () => messaging.redistributeSenderKey(CONVERSATION, 7),
    (error: unknown) => error instanceof SdkError,
    'the frame must honestly name its sender; a nil stamp would make every copy unopenable',
  );
});

test('triggers: a membership change rotates to the named epoch and reaches every member device', async () => {
  const receiverStore = newStore();
  const receiver = rig({ store: receiverStore });
  receiver.messaging.start();
  const sender = rig({
    deviceId: 2,
    peers: [receiverStore],
    devices: [
      { userId: PEER_B_USER, deviceId: PEER_B_DEVICE },
      { userId: PEER_C_USER, deviceId: PEER_C_DEVICE },
    ],
  });

  await sender.messaging.redistributeSenderKey(CONVERSATION, 7);
  assert.equal(sender.groupCrypto.currentEpoch(CONVERSATION), 7);

  // One frame per member device — not one broadcast, because each copy is sealed under the
  // pairwise session with exactly that device.
  const frames = distributions(sender.transport);
  assert.equal(frames.length, 2);
  for (const frame of frames) {
    assert.equal(frame.conversationId, CONVERSATION);
    assert.equal(frame.fromDevice, OWN_DEVICE);
  }
  const addressed = new Set(frames.map((frame) => `${frame.toAccount}|${frame.toDevice}`));
  assert.deepEqual(
    [...addressed].sort(),
    [`${PEER_B_USER}|${PEER_B_DEVICE}`, `${PEER_C_USER}|${PEER_C_DEVICE}`].sort(),
  );

  // The copy sealed for B opens on B's real crypto and lands as a distribution B can use: the
  // domain accepts it, reports it through onKeyExchange (the web client's persist hook), and B's
  // group layer can then open what A seals under the epoch-7 chain.
  const accepted: number[] = [];
  receiver.messaging.onKeyExchange(() => accepted.push(1));
  const forB = frames.find((frame) => frame.toDevice === PEER_B_DEVICE);
  assert.ok(forB !== undefined);
  receiver.transport.emit(OP.GROUP_KEY_DISTRIBUTE, encodeBody(encodeGroupKeyDistribution, forB));

  assert.equal(accepted.length, 1, 'an accepted distribution must fire the key-exchange listener');
  const plaintext = new TextEncoder().encode('after the change');
  const sealed = sender.groupCrypto.sealContent(CONVERSATION, plaintext);
  assert.deepEqual(receiver.groupCrypto.open(CONVERSATION, OWN_DEVICE, sealed.envelope), plaintext);
});

test('triggers: a stale or repeated epoch redistributes nothing new, but a late joiner still gets the chain', async () => {
  const sender = rig({
    deviceId: 2,
    devices: [
      { userId: PEER_B_USER, deviceId: PEER_B_DEVICE },
      { userId: PEER_C_USER, deviceId: PEER_C_DEVICE },
    ],
  });

  await sender.messaging.redistributeSenderKey(CONVERSATION, 7);
  assert.equal(distributions(sender.transport).length, 2);

  // The same epoch again (a duplicate member event): the rotation is a no-op and every device
  // already holds the chain, so nothing is re-sent.
  await sender.messaging.redistributeSenderKey(CONVERSATION, 7);
  assert.equal(distributions(sender.transport).length, 2);
  // A lower epoch (a stale event arriving late): never below what the chain holds.
  await sender.messaging.redistributeSenderKey(CONVERSATION, 3);
  assert.equal(distributions(sender.transport).length, 2);
  assert.equal(sender.groupCrypto.currentEpoch(CONVERSATION), 7);

  // A late joiner appears in the directory without a new membership change: the current chain —
  // not a re-rotation — is sent to exactly the device that still lacks it.
  sender.directory.set([
    { userId: PEER_B_USER, deviceId: PEER_B_DEVICE },
    { userId: PEER_C_USER, deviceId: PEER_C_DEVICE },
    { userId: PEER_D_USER, deviceId: PEER_D_DEVICE },
  ]);
  await sender.messaging.redistributeSenderKey(CONVERSATION, 7);
  const late = distributions(sender.transport).filter((frame) => frame.toDevice === PEER_D_DEVICE);
  assert.equal(late.length, 1, 'the late joiner gets the current chain exactly once');
  assert.equal(sender.groupCrypto.currentEpoch(CONVERSATION), 7, 'no re-rotation for a resend');
});

test('triggers: one refused device does not strand the rest of the membership', async () => {
  const errors: number[] = [];
  let refusals = 0;
  // The first GROUP_KEY_DISTRIBUTE is refused (the member was removed server-side between the
  // roster read and the send — PERMISSION_DENIED working as designed); the second must still go.
  const reply = (opcode: number): Uint8Array => {
    if (opcode === OP.GROUP_KEY_DISTRIBUTE && refusals === 0) {
      refusals += 1;
      throw new Error('refused');
    }
    return encodeBody(encodeAcknowledged, { ok: true });
  };
  const sender = rig({
    deviceId: 2,
    devices: [
      { userId: PEER_B_USER, deviceId: PEER_B_DEVICE },
      { userId: PEER_C_USER, deviceId: PEER_C_DEVICE },
    ],
    reply,
    onEventError: (opcode) => errors.push(opcode),
  });

  // A refusal is reported to the error sink, not thrown: the remaining member devices still need
  // the fresh chain.
  await sender.messaging.redistributeSenderKey(CONVERSATION, 7);

  assert.equal(distributions(sender.transport).length, 2, 'both devices were attempted');
  assert.equal(refusals, 1);
  assert.deepEqual(errors, [OP.GROUP_KEY_DISTRIBUTE], 'the refusal surfaced on the error sink');
});

test('triggers: fan-out noise — a copy sealed for another device is dropped silently', () => {
  const receiver = rig();
  receiver.messaging.start();
  const accepted: number[] = [];
  receiver.messaging.onKeyExchange(() => accepted.push(1));

  // A well-formed frame whose sealed body cannot open here (sealed for another device, or just
  // not a prekey envelope for this store): the pairwise open refuses it, exactly as the
  // KeyExchange fan-out path refuses foreign copies, and the refusal is not an error — every
  // device in the conversation sees every other device's copies.
  receiver.transport.emit(
    OP.GROUP_KEY_DISTRIBUTE,
    encodeBody(encodeGroupKeyDistribution, {
      conversationId: CONVERSATION,
      fromDevice: OWN_DEVICE,
      toAccount: PEER_B_USER,
      toDevice: PEER_B_DEVICE,
      sealedDistribution: new Uint8Array([1, 2, 3, 4]),
    }),
  );
  assert.equal(
    accepted.length,
    0,
    'a copy this device cannot open is fan-out noise, not key state',
  );
});

test('triggers: an accepted distribution is the baseline a later distribution must advance past', async () => {
  // The receiver accepts the sender's epoch-7 chain as its baseline; a distribution from an older
  // generation — the shape a stale event or a long-offline device produces — must be refused by
  // the receiver's own rules, and the refusal must not disturb the chain it holds.
  const receiverStore = newStore();
  const receiver = rig({ store: receiverStore });
  receiver.messaging.start();
  const accepted: number[] = [];
  receiver.messaging.onKeyExchange(() => accepted.push(1));

  const sender = rig({
    deviceId: 2,
    peers: [receiverStore],
    devices: [{ userId: PEER_B_USER, deviceId: PEER_B_DEVICE }],
  });
  await sender.messaging.redistributeSenderKey(CONVERSATION, 7);
  receiver.transport.emit(
    OP.GROUP_KEY_DISTRIBUTE,
    encodeBody(
      encodeGroupKeyDistribution,
      distributions(sender.transport)[0] as GroupKeyDistribution,
    ),
  );
  assert.equal(accepted.length, 1);

  // An epoch-5 chain from the same sender, sealed through the same session: older than the
  // baseline the receiver holds, so the crypto layer's adopt refuses it. The *session* still
  // advanced when the copy opened (that is why the listener fires again — a moved ratchet is
  // moved keystore state), but the held chain does not: the sender's epoch-7 messages keep
  // opening and the stale chain never seals anything the receiver reads.
  const staleCrypto = new GroupCrypto(newStore());
  staleCrypto.rotateTo(CONVERSATION, 5);
  const sealed = await sender.sessionCrypto.seal(
    CONVERSATION,
    OWN_DEVICE,
    PEER_B_USER,
    PEER_B_DEVICE,
    encodeContent({
      type: ContentType.ControlEvent,
      event: SENDER_KEY_EVENT,
      data: staleCrypto.distributionFor(CONVERSATION),
    }),
  );
  receiver.transport.emit(
    OP.GROUP_KEY_DISTRIBUTE,
    encodeBody(encodeGroupKeyDistribution, {
      conversationId: CONVERSATION,
      fromDevice: OWN_DEVICE,
      toAccount: PEER_B_USER,
      toDevice: PEER_B_DEVICE,
      sealedDistribution: sealed.envelope,
    }),
  );
  assert.equal(accepted.length, 2, 'the opened copy moved the session, so the listener fired');

  const plaintext = new TextEncoder().encode('still the epoch-7 chain');
  const still = sender.groupCrypto.sealContent(CONVERSATION, plaintext);
  assert.deepEqual(
    receiver.groupCrypto.open(CONVERSATION, OWN_DEVICE, still.envelope),
    plaintext,
    'the held chain keeps opening what the sender seals',
  );
  // And the stale chain seals nothing the receiver can read — the refused install left no trace.
  const staleMessage = staleCrypto.sealContent(CONVERSATION, new TextEncoder().encode('stale'));
  assert.throws(() => {
    receiver.groupCrypto.open(CONVERSATION, OWN_DEVICE, staleMessage.envelope);
  }, 'a refused distribution must not install its chain');
});
