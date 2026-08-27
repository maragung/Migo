/**
 * Client-minted ids keep the exact 128-bit layout the protocol sorts and dedupes on.
 *
 * A `message_id` and a game `action_id` are chosen by the client so a retry carries the same id and
 * the server dedupes instead of double-sending. That only works if the id is what {@link Id} promises
 * everywhere: six bytes of big-endian Unix milliseconds then ten random bytes, so ids sort by
 * creation time as both bytes and text and collide only on a 2^80 accident within one millisecond.
 * Two failure modes here are silent and expensive — a predictable id (a `Math.random` fallback) lets
 * one client's retry collide with another's, and a mis-encoded time prefix breaks the time ordering
 * the server and every log reader assume — so this file pins the layout, the ordering, the
 * uniqueness, and the deliberate refusal to run without a CSPRNG.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { newId } from '../src/index.js';
import { idFromBytes, idToBytes, isId, parseId } from '@migo/wire';

/** Runs `body` with `Date.now` pinned to `ms`, then restores the real clock. */
function atTime<T>(ms: number, body: () => T): T {
  const realNow = Date.now;
  Date.now = () => ms;
  try {
    return body();
  } finally {
    Date.now = realNow;
  }
}

test('a minted id is a valid 16-byte, 26-character identifier', () => {
  const id = newId();
  assert.ok(isId(id), 'newId produced something the id guard rejects');
  assert.equal(id.length, 26, 'the text form is not 26 Crockford characters');
  assert.equal(idToBytes(id).length, 16, 'the id is not 16 bytes');
});

test('an id round-trips through bytes and text without change', () => {
  const id = newId();
  assert.equal(idFromBytes(idToBytes(id)), id, 'bytes -> id -> bytes changed the value');
  assert.equal(parseId(id), id, 'parsing the text form did not yield the same id');
});

test('the first six bytes are the big-endian millisecond the id was minted at', () => {
  // 0x0192... is a plausible 2024-era millisecond; spell it out so the expectation is independent of
  // the encoder under test.
  const ms = 0x0192abcd_ef01;
  const bytes = idToBytes(atTime(ms, newId));
  let decoded = 0;
  for (let i = 0; i < 6; i += 1) {
    decoded = decoded * 256 + (bytes[i] ?? 0);
  }
  assert.equal(decoded, ms, 'the time prefix does not decode to the mint time');
});

test('ids minted in different milliseconds order by time as both bytes and text', () => {
  const earlier = atTime(1_000, newId);
  const later = atTime(2_000, newId);
  assert.ok(earlier < later, 'the earlier id does not sort before the later one as text');
  assert.ok(
    Buffer.compare(Buffer.from(idToBytes(earlier)), Buffer.from(idToBytes(later))) < 0,
    'the earlier id does not sort before the later one as bytes',
  );
});

test('ids minted in the same millisecond still differ in their random tail', () => {
  const seen = new Set<string>();
  atTime(1_234_567, () => {
    for (let i = 0; i < 64; i += 1) {
      seen.add(newId());
    }
  });
  assert.equal(seen.size, 64, 'two ids minted in the same millisecond collided');
});

test('minting refuses to run without a CSPRNG rather than fall back to a predictable source', () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
  assert.ok(original !== undefined, 'expected a crypto global to remove');
  Object.defineProperty(globalThis, 'crypto', { value: undefined, configurable: true });
  try {
    // A guessable message_id is a correctness bug, so the absence of Web Crypto must throw, never
    // silently degrade to Math.random.
    assert.throws(() => newId(), TypeError);
  } finally {
    Object.defineProperty(globalThis, 'crypto', original);
  }
});

test('the id byte codec the minter builds on rejects a wrong-length buffer', () => {
  // newId always hands idFromBytes exactly 16 bytes; a codec that accepted 15 would let a truncated
  // id through and defeat the fixed-width sort.
  assert.throws(() => idFromBytes(new Uint8Array(15)));
});
