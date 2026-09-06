/**
 * The safety-number suite: the grouping, the pair derivation, and the vectors that hold every
 * client to the same string.
 *
 * # Where the vectors come from — said plainly
 *
 * The Android client *defines* the pair form (`SafetyNumber.kt`), but it shipped no unit test and
 * no shared vector file, so the expected values here were derived once with an independent HKDF
 * call (`@noble/hashes` directly, not through this module) and pinned below. They are the vectors
 * to share back to Android and desktop so all three clients can assert the same bytes: until a
 * `shared/protocol/vectors` file carries them, this file is the only cross-client statement of the
 * pair form, and a client that disagrees with it shows a number nobody else can confirm.
 *
 * # What the shape pins beyond the bytes
 *
 * - **Eight blocks of five digits, joined by single spaces.** The desktop client's doc comment
 *   claims twelve blocks and sixty digits; its code produces eight and forty, and this port matches
 *   the code, because the code is what a person may already have read aloud.
 * - **Symmetry.** `pairSafetyNumber(a, b)` must equal `pairSafetyNumber(b, a)`: the two people
 *   comparing derive from their own side, and an order-sensitive number is a number only one of
 *   them can compute. The vectors include a pair whose first differing byte is above `0x7f`, which
 *   is the case a signed byte comparison would sort differently on the two sides.
 * - **The salt and label.** `migo-fingerprint` / `migo-safety-number-v1`, the same constants the
 *   single-fingerprint derivation uses as its salt family. The expected bytes below are the proof
 *   the label is exactly what left this file, character for character.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  FINGERPRINT_LEN,
  CryptoError,
  pairFingerprint,
  pairSafetyNumber,
  safetyNumber,
} from '../src/index.js';

/** Hex, for writing bytes down. */
function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (value) => value.toString(16).padStart(2, '0')).join('');
}

/** The 32 bytes a hex string names. */
function unhex(text: string): Uint8Array {
  return new Uint8Array((text.match(/../g) ?? []).map((pair) => parseInt(pair, 16)));
}

// --- the pinned vectors -----------------------------------------------------
//
// Derived independently (a direct `hkdf(sha256, input, salt, info, 32)` call) and copied here
// verbatim. Every field is load-bearing: changing the salt, the label, the sort, or the grouping
// changes at least one of them.

interface PairVector {
  readonly name: string;
  readonly own: string;
  readonly peer: string;
  readonly pairFingerprint: string;
  readonly number: string;
}

const PAIR_VECTORS: readonly PairVector[] = [
  {
    name: 'sequential bytes, own below peer',
    own: '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f',
    peer: '202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f',
    pairFingerprint: '46ffa918b84d2c8eee12d4f6ec9e3ab906735d0237885fc90d5efb55804227e2',
    number: '60088 65422 11574 92697 23746 84297 28533 19234',
  },
  {
    // The unsigned-sort case: own begins 0xff, so a *signed* byte comparison would call peer the
    // smaller and concatenate the halves the other way around — deriving, on the two sides, two
    // different "shared" numbers. The vector's expected bytes are only reproducible when the sort
    // is unsigned.
    name: 'high byte first, the unsigned-sort case',
    own: 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
    peer: '0001029999999999999999999999999999999999999999999999999999999999',
    pairFingerprint: '2d6a4316595c39da52d02219eca811014202ba96bb4ec4b4817a5d99b7556ecf',
    number: '38710 16346 71929 37377 75094 01556 80217 29455',
  },
  {
    // The degenerate pair: both sides the same fingerprint (a conversation with one's own other
    // device, in tests). Equal bytes resolve the sort to "peer does not precede own", which is the
    // order the input takes; a `sort` that swapped equal elements would still produce the same
    // concatenation, so this case pins the output rather than the order.
    name: 'both fingerprints equal',
    own: '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f',
    peer: '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f',
    pairFingerprint: 'd1573f29103d1a3557e939b4adc1a3af2fde658cdd7bf4d7e3850913a1252664',
    number: '55945 39861 02452 48719 04140 88343 46643 66436',
  },
];

// --- the grouping -------------------------------------------------------------

test('a fingerprint renders as eight five-digit blocks joined by single spaces', () => {
  const fingerprint = unhex(PAIR_VECTORS[0]!.pairFingerprint);
  const rendered = safetyNumber(fingerprint);
  assert.equal(rendered, PAIR_VECTORS[0]!.number);
  const blocks = rendered.split(' ');
  assert.equal(blocks.length, 8, 'thirty-two bytes are eight blocks, not twelve');
  for (const block of blocks) {
    assert.match(block, /^\d{5}$/, 'each block is exactly five decimal digits');
  }
});

test('a block is padded, so small words still render five digits', () => {
  // A zero word renders "00000" rather than "0": the grouping's whole point is that two people
  // reading aloud stay aligned digit for digit.
  assert.equal(safetyNumber(new Uint8Array(32)), '00000 00000 00000 00000 00000 00000 00000 00000');
});

test('a fingerprint of any other length is refused, not trimmed', () => {
  for (const length of [0, 31, 33, 64]) {
    assert.throws(
      () => safetyNumber(new Uint8Array(length)),
      CryptoError,
      `a ${length}-byte fingerprint is not a fingerprint`,
    );
  }
});

// --- the pair derivation --------------------------------------------------------

for (const vector of PAIR_VECTORS) {
  test(`pair vector: ${vector.name}`, () => {
    const own = unhex(vector.own);
    const peer = unhex(vector.peer);
    const derived = pairFingerprint(own, peer);
    assert.equal(hex(derived), vector.pairFingerprint);
    assert.equal(pairSafetyNumber(own, peer), vector.number);
  });

  test(`pair vector: ${vector.name} — the number is symmetric`, () => {
    // The person on the other side derives from their own "own", which is this side's peer. The
    // two calls must agree byte for byte or an aloud comparison can never succeed.
    const forward = pairFingerprint(unhex(vector.own), unhex(vector.peer));
    const backward = pairFingerprint(unhex(vector.peer), unhex(vector.own));
    assert.deepEqual(forward, backward);
    assert.equal(
      pairSafetyNumber(unhex(vector.own), unhex(vector.peer)),
      pairSafetyNumber(unhex(vector.peer), unhex(vector.own)),
    );
  });
}

test('a pair fingerprint is 32 bytes of fresh output, not a mix of its inputs', () => {
  const own = unhex(PAIR_VECTORS[0]!.own);
  const peer = unhex(PAIR_VECTORS[0]!.peer);
  const derived = pairFingerprint(own, peer);
  assert.equal(derived.length, FINGERPRINT_LEN);
  // Neither half of the sorted input may appear in the output: HKDF's expand must mix, and a
  // regression to a concatenating "derivation" is exactly what this assertion catches.
  assert.notEqual(hex(derived.slice(0, 16)), hex(own.slice(0, 16)));
  assert.notEqual(hex(derived.slice(16)), hex(peer.slice(16)));
});

test('a pair with a wrong-length side is refused before any derivation', () => {
  const own = unhex(PAIR_VECTORS[0]!.own);
  const peer = unhex(PAIR_VECTORS[0]!.peer);
  assert.throws(() => pairFingerprint(new Uint8Array(31), peer), CryptoError);
  assert.throws(() => pairFingerprint(own, new Uint8Array(33)), CryptoError);
  assert.throws(() => pairSafetyNumber(new Uint8Array(16), peer), CryptoError);
});
