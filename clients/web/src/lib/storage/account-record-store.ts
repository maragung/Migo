/**
 * Persistence for the signed-in account's public record: who this browser knows an account by.
 *
 * The key-store snapshot is the device's private identity and lives in its own store; this record
 * is the display side of the same fact — the username the account answers to, the server-side
 * account id, whether this device holds the root, and when the record was written. The login page
 * reads it to offer "Continue as {username}" with only a passphrase to type, and to know whether a
 * `.migo` account file has never been restored here.
 *
 * The record outlives sign-out on purpose: signing out ends a session, not the browser's
 * relationship with the account. It is removed only when the user asks to forget the account.
 */

import type { Id } from '@migo/sdk';

import { idbDelete, idbGet, idbSet } from './idb.js';

const KEY = 'account-record';

/** What the app remembers about the account this browser has signed in as. */
export interface AccountRecord {
  /** The username the account was registered under, as the sign-in form prefills it. */
  username: string;
  /** The server-side account id, so future surfaces can address the account without a session. */
  accountId: Id;
  /**
   * True when this device holds the account root (it founded the account, or restored a `.migo`
   * container). False on additional devices, which never hold the root.
   */
  hasRoot: boolean;
  /** When this record was written, Unix milliseconds. Display material, not security material. */
  savedAt: number;
}

/** Loads the remembered account record, or `undefined` when this browser knows none. */
export function loadAccountRecord(): Promise<AccountRecord | undefined> {
  return idbGet<AccountRecord>(KEY);
}

/** Persists the account record. */
export function saveAccountRecord(record: AccountRecord): Promise<void> {
  return idbSet(KEY, record);
}

/** Removes the account record (forget this account). */
export function clearAccountRecord(): Promise<void> {
  return idbDelete(KEY);
}
