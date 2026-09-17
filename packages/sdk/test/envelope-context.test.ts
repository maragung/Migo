/**
 * What envelope version 2 binds, at the layer that has to get it right rather than at the ratchet.
 *
 * `@migo/crypto`'s own tests pin that the ratchet feeds its context to the AEAD, and the conformance
 * vectors pin the context's bytes. Neither pins the thing that actually protects a conversation:
 * that *this* layer builds the context out of the frame's claimed metadata and hands it to the
 * ratchet on both sides. A policy layer that accepted a context parameter and passed an empty one
 * would leave every test above green.
 *
 * Section 11 states the stake exactly. Content never travels through the pairwise layer — every
 * message is sealed once under a sender key and this layer carries the *distribution* of that key to
 * one device. Relocating a distribution is therefore worse than a denial of service: it installs a
 * chain in a conversation the sender never authorised, and the recipient then reads whatever the
 * relocator sends under it. The first test below is that attack, and it is the only test here that
 * would have passed on the old code.
 *
 * The rest pin the mechanics the first one rests on: that the version byte really selects the
 * associated data at this layer, that a version no build writes is refused rather than guessed at,
 * and that the sender-key envelope did not move with the pairwise one — the two schemes reach their
 * binding differently, so one version byte shared between them would have been a claim about group
 * messages that was not true.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  ContentType,
  ENVELOPE_VERSION,
  GroupCrypto,
  SessionCrypto,
  encodeContent,
} from '../src/index.js';
import type { MessageContent } from '../src/index.js';
import { StaticBundleSource, bundleFrom, idOf, newStore } from './harness.js';

const CONVERSATION = idOf(1);
const ELSEWHERE = idOf(2);
const ALICE_USER = idOf(10);
const ALICE_DEVICE = idOf(11);
const BOB_USER = idOf(20);
const BOB_DEVICE = idOf(21);

function text(body: string): MessageContent {
  return { type: ContentType.Text, text: body };
}

/** One pairing: Alice initiator, Bob responder, each with its own store and bundle source. */
function pair(): {
  alice: SessionCrypto;
  bob: SessionCrypto;
  bobStore: ReturnType<typeof newStore>;
} {
  const aliceStore = newStore();
  const bobStore = newStore();
  return {
    alice: new SessionCrypto(aliceStore, new StaticBundleSource(bundleFrom(bobStore))),
    bob: new SessionCrypto(bobStore, new StaticBundleSource(bundleFrom(aliceStore))),
    bobStore,
  };
}

test('a distribution relocated to another conversation does not open', async () => {
  const { alice, bob, bobStore } = pair();
  const oneTimePrekey = bobStore.publish().oneTimePrekeys[0];
  assert.ok(oneTimePrekey !== undefined, 'the fixture publishes a one-time prekey');

  // A first message, which is the shape that makes the attack work: it carries X3DH material, so a
  // receiver with no session for the sender derives one rather than refusing outright. Nothing in
  // that derivation mentions the conversation — the identities and the prekeys are the same in every
  // conversation the two devices share — so before version 2 the ciphertext opened anywhere the
  // server chose to put it.
  const sealed = await alice.seal(
    CONVERSATION,
    ALICE_DEVICE,
    BOB_USER,
    BOB_DEVICE,
    encodeContent(text('chain')),
  );

  // The attack first, while the prekey is still unspent, so the assertions below are about a prekey
  // that must survive rather than one the genuine open has already taken.
  assert.throws(
    () => bob.open(ELSEWHERE, ALICE_USER, ALICE_DEVICE, sealed.envelope),
    'a distribution for one conversation must not open in another',
  );
  assert.notEqual(
    bobStore.oneTimePrekeyPair(oneTimePrekey.keyId),
    null,
    'a message that failed to authenticate must not spend the prekey it named',
  );

  // And the control: in the conversation it was sealed for, the same bytes open.
  const opened = bob.open(CONVERSATION, ALICE_USER, ALICE_DEVICE, sealed.envelope);
  assert.equal(new TextDecoder().decode(opened.subarray(1)), 'chain');
  assert.equal(
    bobStore.oneTimePrekeyPair(oneTimePrekey.keyId),
    null,
    'and once it has genuinely opened, the prekey is spent',
  );
});

test('the version byte selects the associated data this layer builds', async () => {
  const { alice, bob } = pair();
  const sealed = await alice.seal(
    CONVERSATION,
    ALICE_DEVICE,
    BOB_USER,
    BOB_DEVICE,
    encodeContent(text('satu')),
  );

  // The envelope says what it is.
  assert.equal(sealed.envelope[0], ENVELOPE_VERSION);
  assert.equal(ENVELOPE_VERSION, 2, 'the pairwise layer writes version 2');

  // Relabelling it version 1 must break it. The ratchet would then seam an empty context where the
  // sender sealed a real one, so a tag that verifies can only mean the context never reached the
  // tag — which is precisely the failure this whole step exists to rule out.
  const relabelled = sealed.envelope.slice();
  relabelled[0] = 1;
  assert.throws(
    () => bob.open(CONVERSATION, ALICE_USER, ALICE_DEVICE, relabelled),
    'a message relabelled to version 1 must not open under the version-2 associated data',
  );

  // The genuine envelope still opens, so the failure above is the relabelling and not a session the
  // failed attempt damaged — the ratchet commits nothing on a message that does not authenticate.
  const opened = bob.open(CONVERSATION, ALICE_USER, ALICE_DEVICE, sealed.envelope);
  assert.equal(new TextDecoder().decode(opened.subarray(1)), 'satu');
});

test('a version no build writes is refused rather than guessed at', async () => {
  const { alice, bob } = pair();
  const sealed = await alice.seal(
    CONVERSATION,
    ALICE_DEVICE,
    BOB_USER,
    BOB_DEVICE,
    encodeContent(text('dua')),
  );
  const future = sealed.envelope.slice();
  future[0] = 3;
  assert.throws(
    () => bob.open(CONVERSATION, ALICE_USER, ALICE_DEVICE, future),
    /unsupported envelope version 3/,
  );
});

test('the sender-key envelope keeps its own version', () => {
  // Content travels under scheme 3, which reaches its binding a different way: the conversation id is
  // the AEAD's associated data outright rather than a context appended to it. Sharing one version
  // constant across the two schemes would have made the pairwise flip relabel every group message as
  // carrying something it does not carry.
  const group = new GroupCrypto(newStore());
  const sealed = group.sealContent(CONVERSATION, encodeContent(text('broadcast')));
  assert.equal(sealed.scheme, 3);
  assert.equal(sealed.envelope[0], 1, 'the sender-key envelope is still version 1');
});
