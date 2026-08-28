/**
 * Persistence for the user's chosen {@link ServerEndpoint}.
 *
 * The endpoint is the single piece of configuration that has to outlive a reload: a user who typed
 * a self-hosted address and rebooted would be otherwise be back on the build default. It is stored
 * in IndexedDB and nowhere else -- the same audit rule that bans localStorage for the key-store
 * snapshot applies here too, because a leak of the server address is the leak of where the user's
 * account lives. IndexedDB is the one store the bundle is allowed to write.
 *
 * The key is suffixed with `:v1` so a future shape change can introduce `:v2` and migrate, rather
 * than the silent corruption a bare `endpoint` would suffer when the field set shifts.
 */

import type { ServerEndpoint } from '@migo/sdk';

import { idbDelete, idbGet, idbSet } from './idb.js';

const KEY = 'migo:server-endpoint:v1';

/** Loads the persisted endpoint, or `undefined` on a first visit. */
export function loadServerEndpoint(): Promise<ServerEndpoint | undefined> {
  return idbGet<ServerEndpoint>(KEY);
}

/** Persists the user's chosen endpoint so the next load picks it up. */
export function saveServerEndpoint(endpoint: ServerEndpoint): Promise<void> {
  return idbSet(KEY, endpoint);
}

/** Removes the persisted endpoint (e.g. on sign-out, when the user wants a clean slate). */
export function clearServerEndpoint(): Promise<void> {
  return idbDelete(KEY);
}
