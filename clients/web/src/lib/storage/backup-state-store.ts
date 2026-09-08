/**
 * Persistence for this browser's own account-backup record: when a `.migo` container last left
 * this device as a download, and whether a later identity rotation retired the key that file
 * carries (§50's "Backup" row).
 *
 * A checkup that says "Backed up" must be able to say *when*, and no server knows that — a
 * download is a strictly local event, so the record is local (IndexedDB, keyed by account id,
 * the same keying the device record keeps for the same reason: one browser may hold files for
 * several accounts).
 *
 * The rotation rule is the one honest wrinkle. A `.migo` container seals the account root, and
 * the identity key a rotation installs is fresh randomness the root cannot reproduce (§2402) —
 * so a backup sealed *before* a rotation carries the retired key, and a restore from it will be
 * refused. `markBackupOutdated` is what the rotation's success path calls: it keeps the
 * last-export time (the fact) and stamps *when* the fact stopped being good news, rather than
 * wiping it, because "you exported a file on Tuesday and it is already dead" is a different and
 * truer sentence than "you never exported one".
 *
 * Nothing here is private key material — timestamps only — so the store needs no sealing.
 */

import type { Id } from '@migo/sdk';

import { idbGet, idbSet } from './idb.js';

const keyFor = (accountId: Id): string => `backup-state:${accountId}`;

/** What the checkup's Backup row reads, for one account on this browser. */
export interface BackupState {
  /** The account the record belongs to. */
  accountId: Id;
  /** When a `.migo` container last left this device as a download, Unix milliseconds. */
  lastExportAtMs: number;
  /**
   * When an identity rotation retired the key the last export carries, Unix milliseconds.
   * Present only after a rotation; a fresh export clears it, because the fresh file vouches for
   * the new key.
   */
  invalidatedAtMs?: number;
  /** When the record was written. Display material, not security material. */
  savedAt: number;
}

/** Loads the backup record for an account, or `undefined` when this browser has never exported one. */
export function loadBackupState(accountId: Id): Promise<BackupState | undefined> {
  return idbGet<BackupState>(keyFor(accountId));
}

/**
 * Records a completed `.migo` export: the last-export time becomes now, and any rotation
 * invalidation is cleared — the file that just left the device carries the *current* identity
 * key, so it is the backup that counts.
 */
export async function recordBackupExport(accountId: Id): Promise<void> {
  await idbSet(keyFor(accountId), {
    accountId,
    lastExportAtMs: Date.now(),
    // Any prior invalidation is deliberately dropped rather than carried: the file that just
    // left this device carries the *current* identity key, so it is the backup that counts.
    savedAt: Date.now(),
  });
}

/**
 * Marks the last export as outdated after a completed identity rotation.
 *
 * A browser that has never exported a file is left alone: "Never backed up on this device" is
 * already the truer warning there, and a record that says so with a timestamp would invent an
 * export that never happened.
 */
export async function markBackupOutdated(accountId: Id): Promise<void> {
  const prior = await loadBackupState(accountId);
  if (prior === undefined || prior.invalidatedAtMs !== undefined) {
    return;
  }
  await idbSet(keyFor(accountId), {
    ...prior,
    invalidatedAtMs: Date.now(),
    savedAt: Date.now(),
  });
}
