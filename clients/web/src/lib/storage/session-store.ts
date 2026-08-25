/**
 * Persistence for the session {@link Grant}.
 *
 * The grant carries this session's access and refresh tokens. They are the user's own credentials —
 * not a server secret — and are stored in IndexedDB alongside the key-store snapshot so the session
 * can be resumed on the next visit. They must never be written to a log or sent anywhere but the Migo
 * API they came from; this module only reads and writes them locally.
 */

import type { Grant } from '@migo/sdk';

import { idbDelete, idbGet, idbSet } from './idb.js';

const KEY = 'session';

/** The persisted session: the grant is sufficient to {@link MigoClient.resume} on the next visit. */
export interface PersistedSession {
  grant: Grant;
}

/** Loads the persisted session, or `undefined` when signed out. */
export function loadSession(): Promise<PersistedSession | undefined> {
  return idbGet<PersistedSession>(KEY);
}

/** Persists the session grant. */
export function saveSession(session: PersistedSession): Promise<void> {
  return idbSet(KEY, session);
}

/** Removes the persisted session (on sign-out). */
export function clearSession(): Promise<void> {
  return idbDelete(KEY);
}
