/**
 * The identity-key rotation's ordering contract, as the web client runs it (§2402).
 *
 * The successor is minted from fresh randomness — deliberately not derived from the root — so from
 * the moment the server accepts the rotation, its seed exists in exactly two places: the server's
 * record of the public key, and this device. These tests drive `rotateAccountIdentity` against a
 * real {@link KeyStore} (so the store's own rotation semantics are exercised, not doubled) with only
 * the network ceremony faked, and pin the order that leaves no window where an accepted successor
 * exists nowhere:
 *
 *   1. **Success** pre-commits before the call: the successor is the active key in the store, sealed
 *      into the snapshot, and mirrored into the device record — and the ceremony's signature was
 *      made with the root's derivation, the key the server knew.
 *   2. **A lost answer is not a refusal.** The pre-commit survives (`unfinished`), because the call
 *      may have landed with only its answer lost, and rolling back then would strand the account.
 *   3. **A refusal without a prior seed rolls everything back**, store and sealed copy alike.
 *   4. **The crash-window heal.** A store already carrying a successor the server never accepted
 *      signs with that successor and is refused (`INVALID_CREDENTIALS`); the retry signs with the
 *      root's derivation — the key the server still knows — and the same successor is accepted.
 *   5. **A heal that is refused too** rolls back to the seed the store held before the attempt.
 *   6. **A successor that cannot be persisted aborts before anything is sent** — the one ordering
 *      rule that makes the whole ceremony crash-safe.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { KeyStore, RemoteError, TransportError, account } from '@migo/sdk';
import type { Id, MigoClient } from '@migo/sdk';

import {
  ROTATION_PERSIST_FAILED,
  ROTATION_UNFINISHED,
  rotateAccountIdentity,
} from '../src/lib/migo/identity-rotation.js';
import { loadDeviceRecord, saveDeviceRecord } from '../src/lib/storage/device-record-store.js';
import { loadKeyStoreSnapshot } from '../src/lib/storage/keystore-store.js';
import { installFakeIndexedDb } from './support/dom-stubs.js';

const ACCOUNT_ID = 'acct_rotate' as Id;
const DEVICE_ID = 'dev_rotate' as Id;

/** One ceremony as the rotation ran it, for asserting which key signed and which succeeded. */
interface CeremonyCall {
  current: account.IdentityKey;
  successor: account.IdentityKey;
}

/**
 * A client double: a real key store, and a `rotateIdentity` the test scripts. Everything else the
 * rotation touches — the snapshot store, the device record — runs for real against the fake
 * IndexedDB, because the ordering contract is about those writes, and a double would test the
 * double.
 */
function rotationClient(
  keyStore: KeyStore,
  rotate: (call: CeremonyCall, index: number) => void | Promise<void>,
): { client: MigoClient; calls: CeremonyCall[] } {
  const calls: CeremonyCall[] = [];
  const client = {
    keyStore,
    rotateIdentity: async (
      current: account.IdentityKey,
      successor: account.IdentityKey,
    ): Promise<account.IdentityKey> => {
      const call = { current, successor };
      calls.push(call);
      await rotate(call, calls.length);
      return successor;
    },
  };
  return { client: client as unknown as MigoClient, calls };
}

/** The store with a device record on disk, the state a browser that signed in from a file is in. */
async function seededStore(): Promise<KeyStore> {
  const keyStore = KeyStore.founding(account.MigoRoot.generate());
  await saveDeviceRecord({
    accountId: ACCOUNT_ID,
    deviceId: DEVICE_ID,
    username: 'ada',
    credentialSeed: new Uint8Array(32).fill(1),
    savedAt: 1_000,
  });
  return keyStore;
}

/**
 * A key store whose snapshot cannot be taken, standing in for a persist step that fails. Only the
 * methods the rotation reads are delegated, which is the whole surface the contract touches.
 */
function snapshotFails(store: KeyStore): KeyStore {
  return {
    root: store.root.bind(store),
    accountIdentityKey: store.accountIdentityKey.bind(store),
    rotatedIdentitySeed: store.rotatedIdentitySeed.bind(store),
    installRotatedIdentity: store.installRotatedIdentity.bind(store),
    clearRotatedIdentity: store.clearRotatedIdentity.bind(store),
    snapshot: (): never => {
      throw new Error('the snapshot could not be sealed');
    },
  } as unknown as KeyStore;
}

test('a completed rotation pre-commits, signs with the root key, and seals the successor twice over', async () => {
  const fake = installFakeIndexedDb();
  try {
    const keyStore = await seededStore();
    const rootKey = account.IdentityKey.fromRoot(keyStore.root()!);
    const { client, calls } = rotationClient(keyStore, () => {});

    const outcome = await rotateAccountIdentity(client, ACCOUNT_ID);

    assert.equal(outcome.state, 'done');
    assert.equal(calls.length, 1, 'one ceremony, no heal needed');
    assert.deepEqual(
      calls[0]!.current.publicKey(),
      rootKey.publicKey(),
      'the signature came from the root derivation, the key the server knew',
    );
    assert.notDeepEqual(
      calls[0]!.successor.publicKey(),
      rootKey.publicKey(),
      'the successor is fresh randomness, not another derivation of the root',
    );
    assert.deepEqual(
      keyStore.accountIdentityKey()?.publicKey(),
      calls[0]!.successor.publicKey(),
      'the store now answers account ceremonies with the successor',
    );

    // The seed is sealed into both of its homes: the snapshot (which a reload restores)...
    const snapshot = await loadKeyStoreSnapshot();
    assert.ok(snapshot !== undefined, 'the sealed snapshot reads back');
    assert.ok(
      snapshot.rotatedIdentitySeed !== undefined,
      'the snapshot carries the successor seed',
    );
    assert.deepEqual(
      new Uint8Array(snapshot.rotatedIdentitySeed),
      keyStore.rotatedIdentitySeed(),
      'the sealed seed is the store seed, byte for byte',
    );
    // ...and the device record (which a sign-out spares).
    const record = await loadDeviceRecord(ACCOUNT_ID);
    assert.ok(record !== undefined);
    assert.deepEqual(
      new Uint8Array(record.rotatedIdentitySeed ?? new Uint8Array(0)),
      keyStore.rotatedIdentitySeed(),
      'the device record mirrors the successor, so a sign-out does not strand the file login',
    );
  } finally {
    fake.restore();
  }
});

test("a lost answer keeps the pre-commit: 'unfinished', the successor sealed, nothing rolled back", async () => {
  const fake = installFakeIndexedDb();
  try {
    const keyStore = await seededStore();
    const { client, calls } = rotationClient(keyStore, () => {
      throw new TransportError('the socket closed before the answer');
    });

    const outcome = await rotateAccountIdentity(client, ACCOUNT_ID);

    assert.equal(outcome.state, 'unfinished');
    assert.equal(outcome.message, ROTATION_UNFINISHED);
    assert.equal(calls.length, 1);
    // The call may have landed with only its answer lost: rolling back now could strand an account
    // whose active key exists nowhere, so the successor stays the store's active key — and sealed.
    assert.deepEqual(
      keyStore.accountIdentityKey()?.publicKey(),
      calls[0]!.successor.publicKey(),
      'the sealed successor stays active',
    );
    const snapshot = await loadKeyStoreSnapshot();
    assert.ok(snapshot?.rotatedIdentitySeed !== undefined, 'the sealed copy keeps it too');
  } finally {
    fake.restore();
  }
});

test('a refusal with no prior seed rolls the store and the sealed copy back', async () => {
  const fake = installFakeIndexedDb();
  try {
    const keyStore = await seededStore();
    const rootKey = account.IdentityKey.fromRoot(keyStore.root()!);
    const { client } = rotationClient(keyStore, () => {
      throw new RemoteError(1101, 'INVALID_CREDENTIALS', '');
    });

    await assert.rejects(rotateAccountIdentity(client, ACCOUNT_ID), /not rotated/);

    assert.equal(keyStore.rotatedIdentitySeed(), null, 'the store holds no successor');
    assert.deepEqual(
      keyStore.accountIdentityKey()?.publicKey(),
      rootKey.publicKey(),
      'the root derivation is the active key again',
    );
    const snapshot = await loadKeyStoreSnapshot();
    assert.equal(
      snapshot?.rotatedIdentitySeed,
      undefined,
      'the sealed copy holds no successor either — a refused rotation costs nothing',
    );
  } finally {
    fake.restore();
  }
});

test('the crash-window heal: a refused successor signature is re-answered with the root key', async () => {
  const fake = installFakeIndexedDb();
  try {
    const keyStore = await seededStore();
    const rootKey = account.IdentityKey.fromRoot(keyStore.root()!);
    // The state a previous unfinished rotation left: a successor this server never accepted.
    const prior = account.IdentityKey.generate();
    keyStore.installRotatedIdentity(prior);

    const { client, calls } = rotationClient(keyStore, (_call, index) => {
      if (index === 1) {
        // The signature made with the never-accepted successor is refused; the challenge was not
        // consumed, so answering again with the root's key is exactly the heal.
        throw new RemoteError(1101, 'INVALID_CREDENTIALS', '');
      }
    });

    const outcome = await rotateAccountIdentity(client, ACCOUNT_ID);

    assert.equal(outcome.state, 'done');
    assert.equal(calls.length, 2, 'the heal is one retry, no more');
    assert.deepEqual(
      calls[0]!.current.publicKey(),
      prior.publicKey(),
      'the first attempt signs with the sealed successor',
    );
    assert.deepEqual(
      calls[1]!.current.publicKey(),
      rootKey.publicKey(),
      'the retry signs with the root derivation, the key the server still knows',
    );
    assert.deepEqual(
      calls[0]!.successor.publicKey(),
      calls[1]!.successor.publicKey(),
      'both attempts introduce the same successor',
    );
    assert.deepEqual(
      keyStore.accountIdentityKey()?.publicKey(),
      calls[0]!.successor.publicKey(),
      'the accepted successor is the active key, not the never-accepted prior',
    );
  } finally {
    fake.restore();
  }
});

test('a heal that is refused too rolls back to the seed the store held before the attempt', async () => {
  const fake = installFakeIndexedDb();
  try {
    const keyStore = await seededStore();
    const prior = account.IdentityKey.generate();
    keyStore.installRotatedIdentity(prior);

    const { client, calls } = rotationClient(keyStore, () => {
      throw new RemoteError(1101, 'INVALID_CREDENTIALS', '');
    });

    await assert.rejects(rotateAccountIdentity(client, ACCOUNT_ID), /not rotated/);

    assert.equal(calls.length, 2, 'the heal was tried before giving up');
    assert.deepEqual(
      keyStore.accountIdentityKey()?.publicKey(),
      prior.publicKey(),
      'the store goes back to the seed it held before the attempt',
    );
  } finally {
    fake.restore();
  }
});

test('a successor that cannot be persisted aborts before the server is asked anything', async () => {
  const fake = installFakeIndexedDb();
  try {
    const keyStore = await seededStore();
    const rootKey = account.IdentityKey.fromRoot(keyStore.root()!);
    const flaky = snapshotFails(keyStore);
    const { client, calls } = rotationClient(flaky, () => {});

    await assert.rejects(
      rotateAccountIdentity(client, ACCOUNT_ID),
      new RegExp(ROTATION_PERSIST_FAILED),
    );

    assert.equal(
      calls.length,
      0,
      'nothing is sent when the successor cannot be sealed — the order that makes the ceremony crash-safe',
    );
    assert.deepEqual(
      keyStore.accountIdentityKey()?.publicKey(),
      rootKey.publicKey(),
      'the in-memory store is rolled back to the key it held',
    );
  } finally {
    fake.restore();
  }
});
