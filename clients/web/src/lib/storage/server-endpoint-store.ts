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
export async function loadServerEndpoint(): Promise<ServerEndpoint | undefined> {
  const stored = await idbGet<ServerEndpoint>(KEY);
  if (stored === undefined) {
    return undefined;
  }
  // A snapshot saved by an earlier build (or by the SDK's non-loopback default) may carry
  // Wss/Https against a server that serves plain HTTP. The page's own protocol is the
  // ground truth for whether this deployment has TLS: if the page came over http:// and
  // the stored endpoint says Wss, the stored scheme is a stale guess that would send the
  // WebSocket into a TLS handshake the server cannot answer. Correct it in memory — the
  // corrected endpoint is what the form shows and what the next save persists.
  if (typeof window !== 'undefined' && window.location?.protocol === 'http:') {
    if (stored.scheme === 'Wss' || stored.restScheme === 'Https') {
      return { ...stored, scheme: 'Ws', restScheme: 'Http' };
    }
  }
  return stored;
}

/** Persists the user's chosen endpoint so the next load picks it up. */
export function saveServerEndpoint(endpoint: ServerEndpoint): Promise<void> {
  return idbSet(KEY, endpoint);
}

/** Removes the persisted endpoint (e.g. on sign-out, when the user wants a clean slate). */
export function clearServerEndpoint(): Promise<void> {
  return idbDelete(KEY);
}
