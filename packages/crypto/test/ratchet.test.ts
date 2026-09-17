/**
 * The ratchet's state discipline: what a frame that fails to authenticate is allowed to change.
 *
 * # Why this file exists
 *
 * Every other property of this ratchet is pinned somewhere — the associated data by the conformance
 * vectors, the round trip by the SDK's end-to-end suites. What nothing pinned is the order of two
 * operations inside the receive path: deriving along a chain, and committing the result. That order
 * is invisible on the happy path and is the whole security property on the unhappy one, because the
 * receive path is reached by anyone who can send a frame to the device.
 *
 * The ratchet public key travels in the header unencrypted, so a forged frame needs **nothing
 * secret at all** — a replay of the key the receiver already tracks, a message number at or past
 * what it expects, and arbitrary bytes for the ciphertext. If the chain is advanced before the tag
 * is checked, that frame leaves the chain moved while `receivedCount()` still points at the old
 * step: the genuine message at that number then derives its key from the wrong position in the
 * chain and never opens again. One frame, and that conversation is broken from then on — not
 * delayed, not garbled, permanently undecryptable, and reported by a person as "it says delivered
 * but nothing arrives".
 *
 * Both receive paths had it: the message in the chain being tracked, and the finishing of the
 * previous chain on a DH step. The two tests below are the two shapes, and each fails on the old
 * order for its own reason rather than as a side effect of the other.
 *
 * # What these tests deliberately do not check
 *
 * That the forged frame is *rejected* is asserted only so the rest of the test means something; a
 * rejection is the easy half and was never in doubt. The assertion that matters is the one after
 * it: the genuine message still opens.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  IdentitySecret,
  KeyPair,
  PrekeyBundle,
  RatchetHeader,
  RatchetSession,
  SignedPrekey,
  x3dh,
} from '../src/index.js';

/** A version-1 envelope binds no context, which is what every call here passes. */
const NO_CONTEXT = new Uint8Array(0);

/** Two sessions that have completed X3DH, ready to exchange messages. */
interface Pair {
  readonly alice: RatchetSession;
  readonly bob: RatchetSession;
}

function pair(): Pair {
  const aliceIdentity = IdentitySecret.generate();
  const bobIdentity = IdentitySecret.generate();
  const bobSignedPrekey = KeyPair.generate();
  const bobOneTime = KeyPair.generate();

  const bundle = new PrekeyBundle(
    bobIdentity.public(),
    SignedPrekey.create(bobIdentity, 1, bobSignedPrekey),
    { keyId: 2, publicKey: bobOneTime.public() },
  );

  const initiation = x3dh.initiate(aliceIdentity, bundle);
  const bobSeed = x3dh.respond(bobIdentity, bobSignedPrekey, bobOneTime, initiation.message);

  return {
    alice: RatchetSession.initiator(initiation.seed, bundle.signedPrekey.publicKey),
    bob: RatchetSession.responder(bobSeed, bobSignedPrekey),
  };
}

test('a forged frame does not advance the chain being tracked', () => {
  // The ratchet key is public, so this frame needs no secret: the key the attacker read off the
  // wire, a message number at or past what Bob expects, and any ciphertext. Two shapes, because the
  // two advance paths fail differently — no gap at all, and a gap wide enough to populate the
  // skipped-key list.
  const { alice, bob } = pair();
  const first = alice.encrypt(new TextEncoder().encode('satu'), NO_CONTEXT);
  assert.deepEqual(
    bob.decrypt(first.header, first.ciphertext, NO_CONTEXT),
    new TextEncoder().encode('satu'),
  );

  for (const gap of [0, 3]) {
    const forged = new RatchetHeader(
      first.header.ratchetKey,
      0,
      first.header.messageNumber + 1 + gap,
    );
    assert.throws(
      () => bob.decrypt(forged, first.ciphertext, NO_CONTEXT),
      `a replay of the current key with gap ${gap} must not open`,
    );
  }

  const second = alice.encrypt(new TextEncoder().encode('dua'), NO_CONTEXT);
  assert.deepEqual(
    bob.decrypt(second.header, second.ciphertext, NO_CONTEXT),
    new TextEncoder().encode('dua'),
    'the genuine message still opens after two forged frames',
  );
});

test('a forged frame does not advance the chain being left behind', () => {
  // The same corruption one chain over. A frame claiming a ratchet key Bob is not tracking sends
  // him down the DH-ratchet path, where he finishes the chain he is leaving so messages still in
  // flight from it stay readable — and that finishing is what must not happen until the frame is
  // proven. The attacker supplies a well-formed key of their own, so the rejection comes from the
  // tag rather than from a malformed point.
  const { alice, bob } = pair();
  const encoder = new TextEncoder();

  const a0 = alice.encrypt(encoder.encode('a0'), NO_CONTEXT);
  assert.deepEqual(bob.decrypt(a0.header, a0.ciphertext, NO_CONTEXT), encoder.encode('a0'));

  // Two more Alice sends that Bob never receives: in flight on the chain he is about to leave.
  const held0 = alice.encrypt(encoder.encode('a1'), NO_CONTEXT);
  const held1 = alice.encrypt(encoder.encode('a2'), NO_CONTEXT);

  // Bob replies, so Alice turns the DH ratchet and her next send opens a new chain whose
  // `previousChainLength` names all three she has sent.
  const b0 = bob.encryptNext(encoder.encode('b0'), NO_CONTEXT);
  assert.deepEqual(alice.decrypt(b0.header, b0.ciphertext, NO_CONTEXT), encoder.encode('b0'));
  const a3 = alice.encryptNext(encoder.encode('a3'), NO_CONTEXT);
  assert.equal(a3.header.previousChainLength, 3, 'three sent on the old chain');

  const forged = new RatchetHeader(KeyPair.generate().public(), 3, 0);
  assert.throws(
    () => bob.decrypt(forged, a3.ciphertext, NO_CONTEXT),
    'a frame on an unknown chain must not open',
  );

  assert.deepEqual(
    bob.decrypt(held0.header, held0.ciphertext, NO_CONTEXT),
    encoder.encode('a1'),
    'the in-flight message still opens after a forged frame',
  );
  assert.deepEqual(
    bob.decrypt(held1.header, held1.ciphertext, NO_CONTEXT),
    encoder.encode('a2'),
    'and the one after it',
  );

  // The genuine new-chain message still opens too, so the forged frame cost the session nothing in
  // either direction.
  assert.deepEqual(
    bob.decrypt(a3.header, a3.ciphertext, NO_CONTEXT),
    encoder.encode('a3'),
    'the new chain still opens',
  );
});
