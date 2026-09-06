/**
 * Persistence for the login ceremony's device half: the credential this browser signed in with,
 * so the next sign-in reuses the same device instead of minting a new one.
 *
 * A `.migo` file login walks the ML-DSA identity ceremony (§182). The login purpose answers with
 * *two* signatures — the account identity key (derived from the root, so the file reproduces it)
 * and a per-device credential that is deliberately *not* derived from the root. The device
 * credential only exists where it was generated, which is why the add-device fallback mints a
 * fresh one and this store is its vault: without the record, every sign-in from the file would
 * have to introduce itself as a brand-new device, and the account can only hold eight.
 *
 * The record is private to this browser (IndexedDB, the sanctioned store for key material) and is
 * keyed by account id, because one browser may hold files for several accounts. It is removed when
 * the account is forgotten; signing out does not remove it — sign-out ends a session, not the
 * device's registration.
 */

import type { Id } from '@migo/sdk';

import { idbDelete, idbGet, idbSet } from './idb.js';

const keyFor = (accountId: Id): string => `device-record:${accountId}`;

/** What this browser needs to run the login ceremony again as the same device. */
export interface DeviceRecord {
  /** The account the credential belongs to (from the opened container). */
  accountId: Id;
  /** The server-side device the credential answers to; the challenge is bound to it. */
  deviceId: Id;
  /** The username, because the login challenge names the account by identifier, and the grant does not carry one. */
  username: string;
  /** The device credential's 32-byte seed. Private key material; lives only in IndexedDB. */
  credentialSeed: Uint8Array;
  /**
   * The rotated account identity key's 32-byte seed, present only after this browser rotated the
   * account's identity key (§2402) here.
   *
   * The rotation successor is a *fresh random* seed — deliberately not derived from the root — so a
   * `.migo` file cannot reproduce it: this browser becomes one of only two places it exists, the
   * other being the server's record of the public key. The key-store snapshot is the other home,
   * but sign-out destroys the snapshot while the device record deliberately survives it, so the seed
   * rides here too — without it, a sign-out would strand this browser's tier-1 file login signing
   * with the retired key, which the server answers by refusing. The rotation path keeps the two in
   * lockstep; `undefined` on a record this browser wrote before its first rotation, exactly like a
   * snapshot written before it.
   */
  rotatedIdentitySeed?: Uint8Array;
  /** When the record was written, Unix milliseconds. Display material, not security material. */
  savedAt: number;
}

/** Loads the device record for an account, or `undefined` when this browser has none for it. */
export function loadDeviceRecord(accountId: Id): Promise<DeviceRecord | undefined> {
  return idbGet<DeviceRecord>(keyFor(accountId));
}

/** Persists the device record for an account. */
export function saveDeviceRecord(record: DeviceRecord): Promise<void> {
  return idbSet(keyFor(record.accountId), record);
}

/** Removes the device record (forget this account on this browser). */
export function clearDeviceRecord(accountId: Id): Promise<void> {
  return idbDelete(keyFor(accountId));
}
