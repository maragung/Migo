/**
 * Where this device's secrets are allowed to live.
 *
 * The single most expensive thing this client could do is leak private key material or a session
 * token into a store the audit forbids. Project brief section 178 is unambiguous: a private key must
 * never reach `localStorage`, `sessionStorage`, or a cookie; IndexedDB (or memory) is the only
 * sanctioned home. That rule fails silently — a snapshot written to `localStorage` works perfectly in
 * every functional test and simply hands the keys to any script on the origin — so it needs a test
 * that watches the forbidden surfaces directly.
 *
 * These tests install a recording IndexedDB and recording, write-refusing `localStorage` /
 * `sessionStorage` / `document.cookie`, then drive the three persistence entry points the client uses
 * (key setup, sign-in, and the on-receive re-persist) and assert two things: the secret bytes did land
 * in IndexedDB, and not one byte was written to — or even read from — any forbidden surface. They also
 * pin the smaller contract of `idb.ts`: a missing key reads back as `undefined`, a delete is a no-op on
 * an absent key, and every helper rejects cleanly rather than hanging when IndexedDB is unavailable.
 */

import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';

import type { Grant, Id, KeyStoreSnapshot } from '@migo/sdk';

import { idbDelete, idbGet, idbSet } from '../src/lib/storage/idb.js';
import {
  clearKeyStoreSnapshot,
  loadKeyStoreSnapshot,
  saveKeyStoreSnapshot,
} from '../src/lib/storage/keystore-store.js';
import { clearSession, loadSession, saveSession } from '../src/lib/storage/session-store.js';
import type { PersistedSession } from '../src/lib/storage/session-store.js';
import { installFakeIndexedDb, installRecordingWebStorage } from './support/dom-stubs.js';

const SIGNING_SEED = new Uint8Array(32).fill(0xa1);
const EXCHANGE_SEED = new Uint8Array(32).fill(0xb2);
const ONE_TIME_SEED = new Uint8Array(32).fill(0xd4);
const ACCESS_TOKEN = 'ACCESS-TOKEN-must-not-leak';
const REFRESH_TOKEN = 'REFRESH-TOKEN-must-not-leak';

function sampleSnapshot(): KeyStoreSnapshot {
  return {
    identitySigningSeed: SIGNING_SEED,
    identityExchangeSeed: EXCHANGE_SEED,
    signedPrekeyId: 1,
    signedPrekeySeed: new Uint8Array(32).fill(0xc3),
    oneTimePrekeys: [{ keyId: 7, seed: ONE_TIME_SEED }],
    nextSignedPrekeyId: 2,
    nextOneTimePrekeyId: 8,
  };
}

function sampleSession(): PersistedSession {
  const grant: Grant = {
    accountId: 'acct_0001' as Id,
    deviceId: 'dev_0001' as Id,
    sessionId: 'sess_0001' as Id,
    accessToken: ACCESS_TOKEN,
    refreshToken: REFRESH_TOKEN,
    accessExpiresAtMs: 1_000,
    refreshExpiresAtMs: 2_000,
    // A bigint bitset: the reason IndexedDB is chosen over a JSON store, which cannot represent it.
    capabilities: 0b1011n,
    isNewAccount: true,
  };
  return { grant };
}

let idb: ReturnType<typeof installFakeIndexedDb>;

beforeEach(() => {
  idb = installFakeIndexedDb();
});

afterEach(() => {
  idb.restore();
});

test('a value set under a key reads back equal, as a fresh clone rather than the stored reference', async () => {
  const value = { seed: new Uint8Array([1, 2, 3]), count: 42n };
  await idbSet('probe', value);
  const read = await idbGet<typeof value>('probe');
  assert.deepEqual(read, value);
  // A structured-clone store must not hand back the caller's own object.
  assert.notEqual(read, value);
});

test('reading an absent key yields undefined rather than throwing', async () => {
  assert.equal(await idbGet('never-written'), undefined);
});

test('deleting an absent key is a silent no-op', async () => {
  await idbDelete('never-written');
  assert.equal(await idbGet('never-written'), undefined);
});

test('a deleted key no longer reads back', async () => {
  await idbSet('temp', 'value');
  await idbDelete('temp');
  assert.equal(await idbGet('temp'), undefined);
});

test('every idb helper rejects cleanly when IndexedDB is unavailable', async () => {
  idb.restore(); // no fake, and Node has no native IndexedDB
  await assert.rejects(idbGet('k'), /indexedDB is unavailable/);
  await assert.rejects(idbSet('k', 1), /indexedDB is unavailable/);
  await assert.rejects(idbDelete('k'), /indexedDB is unavailable/);
  idb = installFakeIndexedDb(); // re-install so afterEach's restore is balanced
});

test('a key-store snapshot round-trips through IndexedDB with its private seeds intact', async () => {
  const snapshot = sampleSnapshot();
  await saveKeyStoreSnapshot(snapshot);
  const restored = await loadKeyStoreSnapshot();
  assert.deepEqual(restored, snapshot);
  // The private seeds specifically must survive byte-for-byte, or history becomes unreadable.
  assert.deepEqual(restored?.identitySigningSeed, SIGNING_SEED);
  assert.deepEqual(restored?.oneTimePrekeys[0]?.seed, ONE_TIME_SEED);
});

test('a first visit has no persisted snapshot', async () => {
  assert.equal(await loadKeyStoreSnapshot(), undefined);
});

test('clearing the snapshot on sign-out removes it', async () => {
  await saveKeyStoreSnapshot(sampleSnapshot());
  await clearKeyStoreSnapshot();
  assert.equal(await loadKeyStoreSnapshot(), undefined);
});

test('a session grant round-trips through IndexedDB, bigint capabilities and all', async () => {
  const session = sampleSession();
  await saveSession(session);
  const restored = await loadSession();
  assert.deepEqual(restored, session);
  assert.equal(restored?.grant.capabilities, 0b1011n);
});

test('clearing the session on sign-out removes it', async () => {
  await saveSession(sampleSession());
  await clearSession();
  assert.equal(await loadSession(), undefined);
});

test('no private key or token is ever written to localStorage, sessionStorage, or a cookie', async () => {
  const web = installRecordingWebStorage();
  try {
    // The three moments the client persists: key setup, sign-in, and the on-receive re-persist.
    await saveKeyStoreSnapshot(sampleSnapshot());
    await saveSession(sampleSession());
    await saveKeyStoreSnapshot(sampleSnapshot());

    // The secrets did land in the sanctioned store...
    const persistedSnapshot = await loadKeyStoreSnapshot();
    assert.deepEqual(persistedSnapshot?.identitySigningSeed, SIGNING_SEED);
    assert.equal((await loadSession())?.grant.refreshToken, REFRESH_TOKEN);

    // ...and the forbidden surfaces were not written to — nor even read from.
    assert.deepEqual(web.writes(), []);
    assert.deepEqual(web.accesses, []);

    // A defensive check independent of the recorder's own bookkeeping: the raw secret material
    // never appears in anything the web-storage doubles observed.
    const observed = JSON.stringify(web.accesses);
    assert.ok(!observed.includes(REFRESH_TOKEN));
    assert.ok(!observed.includes(ACCESS_TOKEN));
  } finally {
    web.restore();
  }
});

test('the persisted secrets live under the documented IndexedDB keys and nowhere else', async () => {
  await saveKeyStoreSnapshot(sampleSnapshot());
  await saveSession(sampleSession());
  // Exactly the two documented keys, so a stray write to a third key would be caught.
  assert.deepEqual([...idb.store.keys()].sort(), ['keystore-snapshot', 'session']);
});
