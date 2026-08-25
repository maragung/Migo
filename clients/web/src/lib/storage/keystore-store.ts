/**
 * Persistence for this device's {@link KeyStore} snapshot.
 *
 * The snapshot is the device's cryptographic identity: identity and prekey seeds as raw byte arrays.
 * It is written to IndexedDB and nowhere else, so the private key material never reaches localStorage,
 * a cookie, or the network. Restoring it on the next visit is what lets the device keep its identity —
 * and therefore its ability to decrypt history — across reloads without re-registering.
 */

import type { KeyStoreSnapshot } from '@migo/sdk';

import { idbDelete, idbGet, idbSet } from './idb.js';

const KEY = 'keystore-snapshot';

/** Loads the persisted key-store snapshot, or `undefined` on a first visit. */
export function loadKeyStoreSnapshot(): Promise<KeyStoreSnapshot | undefined> {
  return idbGet<KeyStoreSnapshot>(KEY);
}

/** Persists the current key-store snapshot. */
export function saveKeyStoreSnapshot(snapshot: KeyStoreSnapshot): Promise<void> {
  return idbSet(KEY, snapshot);
}

/** Removes the persisted key-store snapshot (on sign-out). */
export function clearKeyStoreSnapshot(): Promise<void> {
  return idbDelete(KEY);
}
