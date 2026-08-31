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

import { config, defaultServerEndpoint } from '@/lib/config.js';

import type { ServerEndpoint } from '@migo/sdk';

import { idbDelete, idbGet, idbSet } from './idb.js';

const KEY = 'migo:server-endpoint:v1';

/** Loads the persisted endpoint, or `undefined` on a first visit. */
export async function loadServerEndpoint(): Promise<ServerEndpoint | undefined> {
  const stored = await idbGet<ServerEndpoint>(KEY);
  if (stored === undefined) {
    return undefined;
  }
  return healStaleEndpoint(stored);
}

/**
 * Reconciles a snapshot saved by an earlier build with the deployment this page belongs to.
 *
 * A stale snapshot is not a hypothetical: the deployment moved to its current single-port layout
 * after early builds had already persisted an endpoint, and a snapshot carrying the old ports or
 * the TLS guesses of the SDK's non-loopback default sends the REST call at a socket nothing
 * answers — a raw fetch failure the sign-in form can only report as "something went wrong".
 *
 * Two rules, both narrow on purpose so a self-hoster's record is never rewritten:
 *
 *   1. The page's own protocol is the ground truth for TLS. A page served over `http:` came from
 *      a server with no certificate, so `Wss`/`Https` in the snapshot is a stale guess, and the
 *      gateway belongs on the REST port because `migod` serves `/ws` on its HTTP listener.
 *   2. When the build was made with a baked deployment address and the snapshot names the *same
 *      host* with different ports, the ports are from the deployment's older layout — adopt the
 *      baked ones. A snapshot naming any other host is somebody else's server and stays as typed.
 *
 * The correction happens in memory; the corrected endpoint is what the form shows and what the
 * next save persists. The `deployment` parameter overrides the baked default so a test can pin
 * the deployment address without re-evaluating the build-time environment.
 */
export function healStaleEndpoint(
  stored: ServerEndpoint,
  deployment?: ServerEndpoint,
): ServerEndpoint {
  const baked =
    deployment ?? (config.defaultApiUrl !== undefined ? defaultServerEndpoint() : undefined);
  let healed = stored;
  if (typeof window !== 'undefined' && window.location?.protocol === 'http:') {
    if (
      healed.scheme === 'Wss' ||
      healed.restScheme === 'Https' ||
      healed.gatewayPort !== healed.port
    ) {
      healed = { ...healed, scheme: 'Ws', restScheme: 'Http', gatewayPort: healed.port };
    }
  }
  if (baked !== undefined) {
    if (
      healed.host === baked.host &&
      (healed.port !== baked.port ||
        healed.gatewayPort !== baked.gatewayPort ||
        healed.scheme !== baked.scheme ||
        healed.restScheme !== baked.restScheme)
    ) {
      healed = {
        ...healed,
        port: baked.port,
        gatewayPort: baked.gatewayPort,
        scheme: baked.scheme,
        restScheme: baked.restScheme,
      };
    }
  }
  return healed;
}

/** Persists the user's chosen endpoint so the next load picks it up. */
export function saveServerEndpoint(endpoint: ServerEndpoint): Promise<void> {
  return idbSet(KEY, endpoint);
}

/** Removes the persisted endpoint (e.g. on sign-out, when the user wants a clean slate). */
export function clearServerEndpoint(): Promise<void> {
  return idbDelete(KEY);
}
