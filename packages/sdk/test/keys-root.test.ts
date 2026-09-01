/**
 * The account root's path through the key store (§182).
 *
 * A registration is the founding device of a brand-new account: it mints the root, derives the E2EE
 * identity from the root's E2EE domain, and carries both in the snapshot the caller persists. An
 * additional device creates a store with no root at all, and a snapshot from one must never grow
 * one on the way back. These tests pin that split: the same root founds the same identity every
 * time, the root and the tracked transactions survive a snapshot round-trip, and a root-less
 * snapshot stays root-less.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { account, KeyStore } from '../src/index.js';
import type { TrackedTx } from '../src/index.js';

/** A pending record with every optional field absent — the shape a broadcast writes. */
function pendingRecord(): TrackedTx {
  return {
    txHash: new Uint8Array(32).fill(0x5a),
    chainId: 43_114,
    to: new Uint8Array(20).fill(0x11),
    valueWei: 1_500_000_000_000_000_000n,
    feeWei: 2_625_000_000_000_000n,
    gasLimit: 21_000,
    atUnix: 42,
    outcome: 'PENDING',
  };
}

test('the same root founds the same identity, a fresh root a different one', () => {
  const root = account.MigoRoot.generate();
  const again = KeyStore.founding(root);
  const repeated = KeyStore.founding(root);
  const other = KeyStore.founding(account.MigoRoot.generate());

  assert.deepEqual(
    again.publicIdentity().toBytes(),
    repeated.publicIdentity().toBytes(),
    'the same root derived two identities',
  );
  assert.notDeepEqual(
    again.publicIdentity().toBytes(),
    other.publicIdentity().toBytes(),
    'two roots derived one identity',
  );
  // The E2EE identity and the ML-DSA identity key are two domains of the same root, not one key in
  // two forms: domain separation is the whole point, so they must disagree.
  assert.notDeepEqual(
    again.publicIdentity().toBytes(),
    account.IdentityKey.fromRoot(root).publicKey(),
    'the E2EE domain and the identity domain derived the same key',
  );
});

test('a founding snapshot round-trips the root and the tracked transactions', () => {
  const root = account.MigoRoot.generate();
  const store = KeyStore.founding(root, 4);
  store.trackedTxs().unshift(pendingRecord());

  const restored = KeyStore.restore(store.snapshot());

  assert.deepEqual(
    restored.root()?.asBytes(),
    root.asBytes(),
    'the root did not survive the snapshot',
  );
  assert.equal(
    restored.trackedTxs().length,
    1,
    'the tracked transaction did not survive the snapshot',
  );
  const record = restored.trackedTxs()[0];
  assert.ok(record !== undefined, 'the tracked transaction was absent');
  assert.equal(record.outcome, 'PENDING');
  assert.equal(record.valueWei, 1_500_000_000_000_000_000n);
  assert.equal(record.chainId, 43_114);
  // The restored store is the same device: its published bundle is byte-identical.
  assert.deepEqual(restored.publish(), store.publish());
});

test('a snapshot with no root restores to a store with no root', () => {
  const store = KeyStore.create(4);
  const snapshot = store.snapshot();
  assert.equal(snapshot.root, undefined, 'a fresh device snapshot carried a root');
  assert.equal(snapshot.trackedTxs, undefined, 'an empty tx list was written anyway');

  const restored = KeyStore.restore(snapshot);
  assert.equal(restored.root(), null, 'a root-less snapshot grew a root on restore');
  assert.deepEqual(restored.trackedTxs(), [], 'a root-less snapshot grew transactions on restore');
});

test('an ended record keeps its ending through a round-trip', () => {
  const store = KeyStore.founding(account.MigoRoot.generate(), 4);
  const record = pendingRecord();
  store.trackedTxs().unshift(record);
  // The settle step: the same record, its ending written.
  store.trackedTxs()[0] = {
    ...record,
    outcome: 'CONFIRMED',
    block: 620_000_000,
    gasUsed: 21_000n,
  };

  const restored = KeyStore.restore(store.snapshot());
  const ended = restored.trackedTxs()[0];
  assert.ok(ended !== undefined, 'the settled record did not survive the snapshot');
  assert.equal(ended.outcome, 'CONFIRMED');
  assert.equal(ended.block, 620_000_000);
  assert.equal(ended.gasUsed, 21_000n);
});
