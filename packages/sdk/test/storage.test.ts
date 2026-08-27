/**
 * No key material is ever written to web storage.
 *
 * On the web, `localStorage`, `sessionStorage`, and `document.cookie` are readable by any script that
 * runs in the origin — an XSS foothold, a rogue extension, a mis-scoped third-party include. A
 * private key placed in any of them is a private key one bug away from exfiltration, and section 145
 * forbids putting secrets there at all. The SDK's contract is that its crypto layers keep key
 * material in memory only and hand persistence to the caller (who is expected to use IndexedDB or a
 * platform keystore), so this file installs recording stubs for all three web stores, drives the
 * operations that hold secrets — minting a device, running a session, handling a group key, minting
 * ids — and asserts the stubs saw no write at all. A regression that reached for `localStorage` to
 * "conveniently" cache a key would trip this immediately.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  ContentType,
  GroupCrypto,
  KeyStore,
  SessionCrypto,
  encodeContent,
  newId,
} from '../src/index.js';
import type { MessageContent } from '../src/index.js';
import { StaticBundleSource, bundleFrom, idOf, newStore } from './harness.js';

/** One touch of a web store, recorded so the test can prove none happened. */
interface StoreAccess {
  store: string;
  op: string;
}

const accesses: StoreAccess[] = [];

/** A `Storage` whose every operation is recorded; reads return empty, writes are captured. */
function recordingStorage(name: string): Storage {
  const backing = new Map<string, string>();
  return {
    get length(): number {
      return backing.size;
    },
    clear(): void {
      accesses.push({ store: name, op: 'clear' });
      backing.clear();
    },
    getItem(key: string): string | null {
      accesses.push({ store: name, op: 'getItem' });
      return backing.get(key) ?? null;
    },
    key(index: number): string | null {
      return [...backing.keys()][index] ?? null;
    },
    removeItem(key: string): void {
      accesses.push({ store: name, op: 'removeItem' });
      backing.delete(key);
    },
    setItem(key: string, value: string): void {
      accesses.push({ store: name, op: 'setItem' });
      backing.set(key, value);
    },
  };
}

// Install the stubs on the global object. The DOM lib types these as read-only accessors on the
// window, so reach them through a widened view rather than a plain assignment. Each test file runs
// in its own process, so these live only for this file's lifetime.
const globals = globalThis as {
  localStorage?: Storage;
  sessionStorage?: Storage;
  document?: unknown;
};
globals.localStorage = recordingStorage('localStorage');
globals.sessionStorage = recordingStorage('sessionStorage');
globals.document = {
  get cookie(): string {
    accesses.push({ store: 'document.cookie', op: 'get' });
    return '';
  },
  set cookie(_value: string) {
    accesses.push({ store: 'document.cookie', op: 'set' });
  },
};

/** Writes to any of the three stores, the only accesses that would leak a secret. */
function writes(): StoreAccess[] {
  return accesses.filter(
    (access) => access.op === 'setItem' || access.op === 'document.cookie set',
  );
}

function text(value: string): MessageContent {
  return { type: ContentType.Text, text: value };
}

test('minting a device key store writes nothing to any web store', () => {
  accesses.length = 0;
  const store = KeyStore.create(4);
  // Touch the material a real caller would, including the secret snapshot, which is the one thing
  // most tempting to auto-persist.
  store.snapshot();
  store.publish();
  assert.deepEqual(writes(), [], 'creating or snapshotting a key store touched web storage');
});

test('a private key is never written to localStorage', () => {
  accesses.length = 0;
  const store = KeyStore.create(4);
  store.rotateSignedPrekey();
  store.replenishOneTimePrekeys(2);
  const localStorageWrites = accesses.filter(
    (access) => access.store === 'localStorage' && access.op === 'setItem',
  );
  assert.deepEqual(localStorageWrites, [], 'a key operation wrote to localStorage');
});

test('running a full 1:1 session touches no web store', async () => {
  accesses.length = 0;
  const alice = newStore();
  const bob = newStore();
  const aliceSession = new SessionCrypto(alice, new StaticBundleSource(bundleFrom(bob)));
  const bobSession = new SessionCrypto(bob, new StaticBundleSource(bundleFrom(alice)));

  const first = await aliceSession.seal(idOf(1), idOf(20), idOf(21), encodeContent(text('hi')));
  bobSession.open(idOf(1), idOf(10), idOf(11), first.envelope);

  assert.deepEqual(accesses, [], 'the session layer read from or wrote to a web store');
});

test('handling a sender key — seal, distribute, accept, open — touches no web store', () => {
  accesses.length = 0;
  const alice = newStore();
  const bob = newStore();
  const aliceGroup = new GroupCrypto(alice);
  const bobGroup = new GroupCrypto(bob);
  const conv = idOf(1);

  // Distribute before sealing, the order the messaging domain uses: the distribution captures the
  // chain at message 0, so the receiver opens the message sealed at that same position.
  const distribution = aliceGroup.distributionFor(conv);
  const sealed = aliceGroup.sealContent(conv, encodeContent(text('broadcast')));
  bobGroup.acceptDistribution(conv, idOf(11), distribution);
  bobGroup.open(conv, idOf(11), sealed.envelope);

  assert.deepEqual(accesses, [], 'the group layer read from or wrote to a web store');
});

test('minting ids uses the CSPRNG, never web storage', () => {
  accesses.length = 0;
  for (let i = 0; i < 8; i += 1) {
    newId();
  }
  assert.deepEqual(accesses, [], 'id minting touched a web store');
});
