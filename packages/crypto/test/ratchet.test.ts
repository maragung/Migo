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
 * # Why the context tests exist
 *
 * The context a version-2 envelope binds is what stops a server relocating a ciphertext into another
 * conversation: the receiver rebuilds the context from the metadata the frame claims, and a tag
 * computed over different bytes does not verify. `aad` and its conformance vectors pin the *bytes*;
 * what nothing pinned until these tests is that the ratchet actually feeds them to the AEAD on both
 * sides rather than accepting the parameter and ignoring it. An ignored parameter is silent: every
 * vector still passes, every round trip still works, and the protection section 11 asks for is
 * absent. The four tests below are ports of the four in the Rust reference — the round trip, a
 * context one bit apart, the empty context's compatibility with version 1, and the binding surviving
 * a turn of the DH ratchet, which is the case a per-chain bug would leave passing above and failing
 * in a real conversation's second reply.
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
  aad,
  x3dh,
} from '../src/index.js';

/** A version-1 envelope binds no context, which is what every call here passes. */
const NO_CONTEXT = new Uint8Array(0);

/**
 * A well-formed version-2 context, the same shape the Rust reference builds.
 *
 * Not a random byte string: a context that failed to build at all would make the negative tests
 * below pass for the wrong reason, so this is produced by `aad.context` — the same function a client
 * will call — over three fixed ids.
 */
function aContext(): Uint8Array {
  return aad.context(
    aad.EnvelopeVersion.V2,
    aad.SCHEME_DOUBLE_RATCHET,
    new Uint8Array(16).fill(0xa0),
    new Uint8Array(16).fill(0xc0),
    new Uint8Array(16).fill(0xe0),
  );
}

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

test('a context is bound to the message', () => {
  const { alice, bob } = pair();
  const context = aContext();
  const sealed = alice.encrypt(new TextEncoder().encode('halo'), context);
  assert.deepEqual(
    bob.decrypt(sealed.header, sealed.ciphertext, context),
    new TextEncoder().encode('halo'),
    'a context the receiver rebuilds identically opens the message',
  );
});

test('a message does not open under a different context', () => {
  // One byte apart, because a context that only matched on whole fields would pass a test that
  // swapped a field and fail in the field on a version byte. A fresh pair each time: a failed open is
  // not something this test then retries, since the message key it derived is gone.
  const context = aContext();
  const other = context.slice();
  const last = other.length - 1;
  // `noUncheckedIndexedAccess` is on, so the read is spelled out rather than hidden in a `^=`.
  other[last] = (other[last] ?? 0) ^ 0x01;

  const first = pair();
  const sealed = first.alice.encrypt(new TextEncoder().encode('halo'), context);
  assert.throws(
    () => first.bob.decrypt(sealed.header, sealed.ciphertext, other),
    'a context changed by one bit must not authenticate',
  );

  // And the converse: an empty context is not a wildcard that opens a message sealed under a real
  // one.
  const second = pair();
  const sealedToo = second.alice.encrypt(new TextEncoder().encode('halo'), context);
  assert.throws(
    () => second.bob.decrypt(sealedToo.header, sealedToo.ciphertext, NO_CONTEXT),
    'an empty context must not open a message sealed under a real one',
  );
});

test('an empty context is the version one associated data', () => {
  // The compatibility claim, at the layer that would break if it were false: two sessions that both
  // pass nothing agree, and an empty context adds no bytes to what a version-1 envelope
  // authenticated.
  const { alice, bob } = pair();
  const encoder = new TextEncoder();
  const sealed = alice.encrypt(encoder.encode('halo'), NO_CONTEXT);
  assert.deepEqual(
    bob.decrypt(sealed.header, sealed.ciphertext, NO_CONTEXT),
    encoder.encode('halo'),
    'two sessions that both pass no context still agree',
  );

  const prefix = new Uint8Array(64).map((_, i) => i);
  const header = sealed.header.toBytes();
  const plain = new Uint8Array(prefix.length + header.length);
  plain.set(prefix, 0);
  plain.set(header, prefix.length);
  assert.equal(
    aad.assemble(prefix, header, NO_CONTEXT).length,
    prefix.length + RatchetHeader.ENCODED_LEN,
    'an empty context must add no bytes to the associated data',
  );
  assert.deepEqual(
    aad.assemble(prefix, header, NO_CONTEXT),
    plain,
    'and the assembled bytes are the version-1 prefix and header, nothing else',
  );
});

test('a context survives the dh ratchet', () => {
  // The context is per message and the ratchet turns between messages, so a binding that only worked
  // within one chain would look correct in the test above and fail on the second reply of every real
  // conversation.
  const { alice, bob } = pair();
  const encoder = new TextEncoder();
  const first = aContext();
  const one = alice.encrypt(encoder.encode('one'), first);
  assert.deepEqual(
    bob.decrypt(one.header, one.ciphertext, first),
    encoder.encode('one'),
    'the first chain carries its context',
  );

  const second = first.slice();
  const last = second.length - 1;
  second[last] = 0xe1;

  const two = bob.encryptNext(encoder.encode('two'), second);
  assert.deepEqual(
    alice.decrypt(two.header, two.ciphertext, second),
    encoder.encode('two'),
    'the next chain carries a different one',
  );

  // And the binding is per message rather than per session: the context the first chain used must
  // not open a message the next chain sealed.
  const other = pair();
  const sealed = other.bob.encryptNext(encoder.encode('empat'), second);
  assert.throws(
    () => other.alice.decrypt(sealed.header, sealed.ciphertext, first),
    "the previous chain's context must not open a message from the next chain",
  );
  assert.deepEqual(
    other.alice.decrypt(sealed.header, sealed.ciphertext, second),
    encoder.encode('empat'),
    'and the context it was sealed under still does',
  );
});
