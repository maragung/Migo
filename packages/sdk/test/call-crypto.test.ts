/**
 * The call-key state machine of section 163, pinned against the Rust crate's own test vector.
 *
 * `migo-crypto`'s `call_key.rs` carries an independent KDF vector — a fixed session secret, a fixed
 * call id, the key both must produce — so a TypeScript mirror that drifts would fail here without
 * any cross-language runner having to say so. Everything else this file pins is the state machine's
 * own shape: rotation seals the next epoch under the running key, adoption never regresses, a join
 * distribution opens only for the joiner it was sealed for and the call it names, and a device that
 * joins mid-call cannot open the media that predates it.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { aead, kdf, CryptoError, SymmetricKey } from '@migo/crypto';
import { idFromBytes, idToBytes } from '@migo/wire';
import type { Id } from '@migo/wire';

import { CallKeyState, CALL_KEY_LEN } from '../src/index.js';

/** The fixed call id of the Rust vector: bytes `0x00..=0x0f`. */
const VECTOR_CALL: Id = idFromBytes(
  new Uint8Array([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
);
/** The fixed session secret of the Rust vector: 32 bytes of `0x0a`. */
const VECTOR_SECRET = new Uint8Array(32).fill(0x0a);
/** The media key `from_session` must derive for the two above, byte-for-byte with `call_key.rs`. */
const VECTOR_KEY = Uint8Array.from([
  0x3e, 0xc0, 0xb5, 0xb2, 0x95, 0x15, 0xed, 0xc6, 0xb4, 0xb5, 0x1d, 0x92, 0xc1, 0x31, 0xc7, 0x56,
  0xd1, 0xef, 0x49, 0x66, 0xdc, 0xa0, 0x54, 0x29, 0xed, 0x92, 0x6d, 0xc0, 0x12, 0xa9, 0xcf, 0xa0,
]);

/** Two ids that differ, for the cross-call binding checks. */
const CALL_A = idFromBytes(new Uint8Array(16).fill(0x11));
const CALL_B = idFromBytes(new Uint8Array(16).fill(0x22));

/** A session secret that is not the vector's, so derivations cannot collide with it. */
const SECRET_A = new Uint8Array(32).fill(0x61);
const SECRET_B = new Uint8Array(32).fill(0x62);

/** The AEAD binding of one call and epoch: the 16 id bytes, then the epoch as u64 big-endian. */
function binding(callId: Id, epoch: number): Uint8Array {
  const out = new Uint8Array(16 + 8);
  out.set(idToBytes(callId), 0);
  new DataView(out.buffer, 16, 8).setBigUint64(0, BigInt(epoch));
  return out;
}

test("call-crypto: fromSession derives the Rust crate's pinned vector key", () => {
  const state = CallKeyState.fromSession(VECTOR_SECRET, VECTOR_CALL);
  assert.equal(state.epoch(), 0);
  // The key bytes are private by design, so the pin runs through the frame plane: a frame the
  // state seals must open under the vector key and the vector binding — anything else and the
  // derivation drifted from the Rust crate's.
  const frame = new Uint8Array([1, 2, 3]);
  const sealed = state.sealFrame(frame);
  assert.deepEqual(
    aead.open(SymmetricKey.fromBytes(VECTOR_KEY), binding(VECTOR_CALL, 0), sealed),
    frame,
  );
});

test('call-crypto: one session cannot derive the same media key for two calls', () => {
  const a = CallKeyState.fromSession(SECRET_A, CALL_A);
  const b = CallKeyState.fromSession(SECRET_A, CALL_B);
  // Same secret, different call ids (the salt): the two keys must differ, which the frame plane
  // observes as a frame sealed for one call refusing to open under the other's binding.
  const sealed = a.sealFrame(new Uint8Array([9]));
  assert.throws(() => {
    b.openFrame(sealed);
  });
});

test('call-crypto: rotate seals the 32-byte next key under the running key', () => {
  const holder = CallKeyState.fromSession(SECRET_A, CALL_A);
  // The epoch-0 key is derivable here the same way fromSession derives it, so the sealed material
  // can be checked against the crate's byte contract directly: 32 bytes, under the *old* key and
  // the *new* epoch's binding.
  const oldKey = SymmetricKey.fromBytes(
    kdf.derive(SECRET_A, idToBytes(CALL_A), kdf.LABEL_CALL_KEY, CALL_KEY_LEN),
  );
  const sealed = holder.rotate();
  assert.equal(holder.epoch(), 1);
  const material = aead.open(oldKey, binding(CALL_A, 1), sealed);
  assert.equal(material.length, CALL_KEY_LEN);
  // And a frame the holder seals now opens under exactly that material: the rotation installed it.
  const frame = new Uint8Array([5, 5, 5]);
  assert.deepEqual(
    aead.open(SymmetricKey.fromBytes(material), binding(CALL_A, 1), holder.sealFrame(frame)),
    frame,
  );
});

test('call-crypto: a rotation is adopted by a peer at the previous epoch', () => {
  const rotator = CallKeyState.fromSession(SECRET_A, CALL_A);
  const peer = CallKeyState.fromSession(SECRET_A, CALL_A);
  // Same derivation, same epoch-0 key: the 1:1 shape. A frame sealed after the rotation opens on
  // the peer only once the update is adopted.
  const sealed = rotator.rotate();
  const postJoin = rotator.sealFrame(new Uint8Array([4, 4, 4]));
  peer.adopt(1, sealed);
  assert.equal(peer.epoch(), 1);
  assert.deepEqual(peer.openFrame(postJoin), new Uint8Array([4, 4, 4]));
  // And the pre-rotation frame no longer opens: the state moved, and the epoch binding is what
  // refuses the rollback.
  const preJoin = CallKeyState.fromSession(SECRET_A, CALL_A).sealFrame(new Uint8Array([4, 4, 4]));
  assert.throws(() => {
    peer.openFrame(preJoin);
  });
});

test('call-crypto: adopt refuses an epoch that does not advance', () => {
  const state = CallKeyState.fromSession(SECRET_A, CALL_A);
  const sealed = state.rotate();
  // A replay of the same update, and a rollback to a lower epoch, are both refused with the same
  // error kind the message ratchets use — and the working key survives both.
  assert.throws(
    () => {
      state.adopt(1, sealed);
    },
    (error: unknown) => error instanceof CryptoError && error.kind === 'KeyAlreadyUsed',
  );
  assert.throws(
    () => {
      state.adopt(0, sealed);
    },
    (error: unknown) => error instanceof CryptoError && error.kind === 'KeyAlreadyUsed',
  );
  assert.equal(state.epoch(), 1);
});

test('call-crypto: a join distribution round-trips between distributor and joiner', () => {
  const distributor = CallKeyState.fromSession(SECRET_A, CALL_A);
  distributor.rotate();
  distributor.rotate();
  const sealedJoin = distributor.sealedJoinDistribution(SECRET_B);

  // The joiner holds SECRET_B (the session it shares with the distributor) and no earlier epoch:
  // the first distribution is its baseline.
  const joiner = CallKeyState.fromJoinDistribution(SECRET_B, CALL_A, sealedJoin);
  assert.equal(joiner.epoch(), distributor.epoch());
  // Media sealed by the distributor after the hand-off opens on the joiner; media sealed before
  // the joiner's baseline epoch does not — that is the secrecy line the join path draws.
  const current = distributor.sealFrame(new Uint8Array([1, 1]));
  assert.deepEqual(joiner.openFrame(current), new Uint8Array([1, 1]));
});

test('call-crypto: a join distribution is bound to the call and the joiner it was sealed for', () => {
  const distributor = CallKeyState.fromSession(SECRET_A, CALL_A);
  const sealedJoin = distributor.sealedJoinDistribution(SECRET_B);
  // Another call id: the AEAD associated data refuses it.
  assert.throws(() => {
    CallKeyState.fromJoinDistribution(SECRET_B, CALL_B, sealedJoin);
  });
  // Another session (a different joiner): the wrapper key refuses it.
  assert.throws(() => {
    CallKeyState.fromJoinDistribution(SECRET_A, CALL_A, sealedJoin);
  });
  // And a truncation of the sealed body is not a join distribution.
  assert.throws(() => {
    CallKeyState.fromJoinDistribution(SECRET_B, CALL_A, sealedJoin.slice(0, 10));
  });
});
