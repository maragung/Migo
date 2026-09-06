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
 *
 * The key-store snapshot's own tests grew with the sealing change (§9/§108/§164): the seeds must not
 * sit in the store as plaintext bytes but sealed under a master the store holds as a `CryptoKey`, a
 * legacy plaintext record must migrate on first load without ever becoming unreadable, and a record
 * that cannot open must be reported as absent rather than wipe the session.
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
  // Exactly the four documented keys — the sealed snapshot, its master key, the session, and nothing
  // else — so a stray write to a fifth key (a plaintext fallback left behind, say) would be caught.
  assert.deepEqual([...idb.store.keys()].sort(), [
    'keystore-master',
    'keystore-snapshot:v1',
    'session',
  ]);
});

test('the snapshot is sealed at rest: a CryptoKey master, ciphertext, and no plaintext seeds', async () => {
  await saveKeyStoreSnapshot(sampleSnapshot());

  // The master is a CryptoKey object in the store — §164's letter — and it is non-extractable by
  // construction: WebCrypto was handed `extractable: false`, and exporting it must be refused.
  const master = idb.store.get('keystore-master') as CryptoKey;
  assert.equal(
    Object.prototype.toString.call(master),
    '[object CryptoKey]',
    'the master must be stored as a CryptoKey object',
  );
  assert.equal(master.extractable, false);
  await assert.rejects(crypto.subtle.exportKey('raw', master));

  // The record is a sealed envelope, not the snapshot: a version, a nonce, and ciphertext.
  const sealed = idb.store.get('keystore-snapshot:v1') as {
    version: number;
    iv: Uint8Array;
    ciphertext: Uint8Array;
  };
  assert.equal(sealed.version, 1);
  assert.equal(sealed.iv.length, 12);
  assert.ok(sealed.ciphertext.length > 0);
  assert.equal(idb.store.has('keystore-snapshot'), false, 'no plaintext record may remain');

  // The constant-filled seeds would survive any partial plaintext as a run of identical bytes; AES-GCM
  // under a random key leaves no such run. A four-byte window is enough to see one if it existed.
  for (const fill of [0xa1, 0xb2, 0xc3, 0xd4]) {
    let run = 0;
    for (const byte of sealed.ciphertext) {
      run = byte === fill ? run + 1 : 0;
      assert.ok(run < 4, `a run of seed bytes (0x${fill.toString(16)}) survives in the ciphertext`);
    }
  }
});

test('a legacy plaintext record migrates to sealed on first load, and only once', async () => {
  // What an earlier build left behind: the snapshot in the clear under the legacy key.
  await idbSet('keystore-snapshot', sampleSnapshot());

  const restored = await loadKeyStoreSnapshot();
  assert.deepEqual(restored, sampleSnapshot());
  assert.equal(idb.store.has('keystore-snapshot'), false, 'the plaintext record must be deleted');
  assert.ok(idb.store.has('keystore-snapshot:v1'), 'the sealed record must replace it');
  assert.ok(idb.store.has('keystore-master'), 'the master key must exist to seal it under');

  // And the sealed record opens again on the next load, without the legacy one to lean on.
  assert.deepEqual(await loadKeyStoreSnapshot(), sampleSnapshot());
});

test('a legacy record stays readable when sealing is impossible', async () => {
  await idbSet('keystore-snapshot', sampleSnapshot());

  // An embedder with no WebCrypto: migration cannot seal, so the plaintext record must survive.
  const realCrypto = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
  Object.defineProperty(globalThis, 'crypto', { configurable: true, value: undefined });
  try {
    assert.deepEqual(await loadKeyStoreSnapshot(), sampleSnapshot());
    assert.equal(
      idb.store.has('keystore-snapshot'),
      true,
      'a failed migration must not delete the readable record',
    );

    // The same constraint on save: the legacy shape is the fallback, so the identity survives.
    await saveKeyStoreSnapshot(sampleSnapshot());
    assert.equal(idb.store.has('keystore-snapshot'), true);
  } finally {
    if (realCrypto) {
      Object.defineProperty(globalThis, 'crypto', realCrypto);
    }
  }
});

test('a sealed record that cannot open is absent, not a thrown error that wipes the session', async () => {
  await saveKeyStoreSnapshot(sampleSnapshot());

  // Tamper with the ciphertext: AES-GCM's tag must refuse it.
  const sealed = idb.store.get('keystore-snapshot:v1') as {
    iv: Uint8Array;
    ciphertext: Uint8Array;
  };
  sealed.ciphertext[0] = (sealed.ciphertext[0] ?? 0) ^ 0xff;

  assert.equal(await loadKeyStoreSnapshot(), undefined);
  // The damaged record stays in the store: load reports absence, it does not destroy evidence or
  // state on the way past.
  assert.ok(idb.store.has('keystore-snapshot:v1'));
});

test('sign-out clears the sealed record and the master key with it', async () => {
  await saveKeyStoreSnapshot(sampleSnapshot());
  await clearKeyStoreSnapshot();
  assert.equal(await loadKeyStoreSnapshot(), undefined);
  assert.equal(idb.store.has('keystore-snapshot:v1'), false);
  assert.equal(idb.store.has('keystore-master'), false);
  assert.equal(idb.store.has('keystore-snapshot'), false);
});
