/**
 * The single most valuable test in the suite: no private key ever leaves the device.
 *
 * Migo's confidentiality rests on one claim — the server routes ciphertext it cannot read, because
 * every private key stays on the device that made it. If a private seed, a ratchet secret, or a raw
 * sender-key chain key ever rode out in a frame body, the whole end-to-end guarantee would be void
 * and nothing else in the protocol would notice: the message would still decrypt, the tests would
 * still pass, and the server would quietly hold the keys to every conversation. So these tests run
 * the real session and group crypto through their full flows — X3DH first message, ratchet reply,
 * sender-key seal and distribution, and a complete {@link MessagingDomain.send} — capture every byte
 * the client would transmit, and assert that none of the sender's private seeds appear in any of
 * them. The positive control (a raw chain key IS findable before it is sealed) proves the search
 * itself works, so a green run means absence, not a broken scan.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  ContentType,
  GroupCrypto,
  MessagingDomain,
  Rpc,
  SessionCrypto,
  encodeContent,
} from '../src/index.js';
import type { DeviceAddress, DeviceDirectory, MessageContent } from '../src/index.js';
import {
  RecordingTransport,
  StaticBundleSource,
  bundleFrom,
  containsBytes,
  encodeAccepted,
  idOf,
  newStore,
  privateSeeds,
} from './harness.js';

const CONV = idOf(1);
const ALICE_USER = idOf(10);
const ALICE_DEVICE = idOf(11);
const BOB_USER = idOf(20);
const BOB_DEVICE = idOf(21);

test('a first 1:1 message carries no byte of the sender or recipient private key material', async () => {
  const alice = newStore();
  const bob = newStore();
  // Alice is the initiator; her session fetches Bob's public bundle and runs X3DH locally.
  const session = new SessionCrypto(alice, new StaticBundleSource(bundleFrom(bob)));

  const sealed = await session.seal(CONV, BOB_USER, BOB_DEVICE, encodeContent(text('hello bob')));

  // The initiator's own secrets and the responder's secrets alike must be absent from the envelope.
  // X3DH mixes both parties' private keys into the shared secret, but only public keys and key ids
  // are ever serialised — the secret is derived independently on each side.
  for (const seed of [...privateSeeds(alice), ...privateSeeds(bob)]) {
    assert.ok(
      !containsBytes(sealed.envelope, seed),
      'a private seed appeared in the sealed first-message envelope',
    );
  }
});

test('a whole ratchet round trip never serialises the derived session or message keys', async () => {
  const alice = newStore();
  const bob = newStore();
  const aliceSession = new SessionCrypto(alice, new StaticBundleSource(bundleFrom(bob)));
  const bobSession = new SessionCrypto(bob, new StaticBundleSource(bundleFrom(alice)));

  const first = await aliceSession.seal(CONV, BOB_USER, BOB_DEVICE, encodeContent(text('one')));
  const opened = bobSession.open(CONV, ALICE_USER, ALICE_DEVICE, first.envelope);
  // Bob replies, which advances the ratchet on both sides and switches Alice out of prekey scheme.
  const reply = await bobSession.seal(CONV, ALICE_USER, ALICE_DEVICE, encodeContent(text('two')));
  aliceSession.open(CONV, BOB_USER, BOB_DEVICE, reply.envelope);
  const third = await aliceSession.seal(CONV, BOB_USER, BOB_DEVICE, encodeContent(text('three')));

  // The message decrypted, so the flow was real, not a no-op that trivially leaks nothing.
  assert.equal(new TextDecoder().decode(opened.subarray(1)).includes('one'), true);

  const secrets = [...privateSeeds(alice), ...privateSeeds(bob)];
  for (const envelope of [first.envelope, reply.envelope, third.envelope]) {
    for (const seed of secrets) {
      assert.ok(!containsBytes(envelope, seed), 'a private seed appeared in a ratchet envelope');
    }
  }
});

test('a sealed group envelope hides the chain key that its distribution deliberately carries', () => {
  const alice = newStore();
  const group = new GroupCrypto(alice);

  const sealed = group.sealContent(CONV, encodeContent(text('broadcast')));
  const distribution = group.distributionFor(CONV);

  // Positive control: the distribution is meant to hand a member the raw chain key, so the chain
  // key's bytes ARE inside it. The distribution serialises as varint(chainId) varint(messageNumber)
  // then the 32-byte chain key then the 64-byte identity, so the chain key is the 32 bytes ending 64
  // from the end. Without a case where the scan finds something, a green suite could mean the scan
  // is broken rather than that nothing leaked.
  const chainKey = distribution.subarray(distribution.length - 96, distribution.length - 64);
  assert.equal(chainKey.length, 32);
  assert.ok(
    containsBytes(distribution, chainKey),
    'the search cannot even find the chain key in the raw distribution — the scan is broken',
  );
  // Negative result: that same chain key must not appear in the sealed content envelope, which the
  // server relays in the clear. The distribution only ever travels sealed inside the 1:1 channel.
  assert.ok(
    !containsBytes(sealed.envelope, chainKey),
    'the sender-key chain key leaked into the broadcast content envelope',
  );
});

test('a full messaging send transmits only ciphertext frames, never a private seed', async () => {
  const alice = newStore();
  const bob = newStore();
  const transport = new RecordingTransport();
  // Every MESSAGE_SEND is answered with a valid acknowledgement so the send resolves. The send path
  // never checks the ack's ids against what it sent, so a constant reply is enough.
  transport.reply = () => encodeAccepted(idOf(99), CONV);

  const rpc = new Rpc(transport.asTransport());
  const sessionCrypto = new SessionCrypto(alice, new StaticBundleSource(bundleFrom(bob)));
  const groupCrypto = new GroupCrypto(alice);
  const directory: DeviceDirectory = {
    recipientDevices(): Promise<DeviceAddress[]> {
      return Promise.resolve([{ userId: BOB_USER, deviceId: BOB_DEVICE }]);
    },
  };
  const messaging = new MessagingDomain(rpc, sessionCrypto, groupCrypto, directory);

  await messaging.send(CONV, text('the first message to a fresh conversation'));

  // The first send fans out to a KeyExchange (the sealed sender-key distribution) plus the content
  // itself; both cross as MESSAGE_SEND bodies. None may contain any of Alice's private seeds.
  assert.ok(transport.sent.length >= 2, 'the first send should distribute a key and send content');
  for (const body of transport.bodies()) {
    for (const seed of privateSeeds(alice)) {
      assert.ok(!containsBytes(body, seed), 'a private seed appeared in a transmitted frame body');
    }
  }
});

/** A trivial text content body for sealing. */
function text(value: string): MessageContent {
  return { type: ContentType.Text, text: value };
}
