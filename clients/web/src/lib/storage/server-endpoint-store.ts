/**
 * Persistence for the user's chosen {@link ServerEndpoint}, and the mode the choice was made in.
 *
 * The endpoint is the single piece of configuration that has to outlive a reload: a user who typed
 * a self-hosted address and rebooted would be otherwise be back on the build default. It is stored
 * in IndexedDB and nowhere else -- the same audit rule that bans localStorage for the key-store
 * snapshot applies here too, because a leak of the server address is the leak of where the user's
 * account lives. IndexedDB is the one store the bundle is allowed to write.
 *
 * The key is suffixed with `:v2` because the record grew a `mode` field beside the endpoint, and a
 * future shape change introduces `:v3` and migrates the same way. `:v1` held a bare
 * {@link ServerEndpoint}; it is still read (as a manual choice -- the mode a v1 user was in all
 * along, so nobody loses their saved server) and deleted the next time a v2 record is written.
 *
 * The mode decides what the endpoint field means:
 *
 *   - `auto`: the endpoint is the *last resolution* of the "Otomatis" probe, not a commitment --
 *     every load re-probes the deployment's server list (lib/auto-server.js) and uses the current
 *     fastest, so a saved address here is display and fallback, never a pin. When nothing answers,
 *     the mode stays `auto` and the sign-in attempt surfaces the connection error through the
 *     normal path; the store never quietly converts a dead probe into a fixed address.
 *   - `server`: one of the known nodes from the build's list, picked explicitly.
 *   - `manual`: a hand-typed host/port/scheme.
 */

import { config, defaultServerEndpoint } from '@/lib/config.js';

import type { ServerEndpoint } from '@migo/sdk';

import { idbDelete, idbGet, idbSet } from './idb.js';

const KEY_V1 = 'migo:server-endpoint:v1';
const KEY_V2 = 'migo:server-endpoint:v2';

/** How the persisted endpoint was chosen. */
export type ServerChoiceMode = 'auto' | 'server' | 'manual';

/** The persisted record: an endpoint plus the mode that gives the endpoint its meaning. */
export interface StoredServerChoice {
  mode: ServerChoiceMode;
  endpoint: ServerEndpoint;
}

/** Narrows an untrusted stored mode to the three this build knows; anything else is manual. */
function asMode(value: unknown): ServerChoiceMode {
  return value === 'auto' || value === 'server' || value === 'manual' ? value : 'manual';
}

/** Whether a stored v2 record has the shape this build wrote. */
function isStoredChoice(value: unknown): value is StoredServerChoice {
  return (
    typeof value === 'object' &&
    value !== null &&
    'mode' in value &&
    'endpoint' in value &&
    typeof (value as { endpoint: unknown }).endpoint === 'object'
  );
}

/**
 * Loads the persisted choice, or `undefined` on a first visit.
 *
 * A v1 record (a bare endpoint from a build before modes existed) reads back as a manual choice
 * with the same healing it always got, so an upgrade costs a user nothing. The migration is
 * read-only on purpose: the v2 record is written by the next save, not by a load that might be
 * a visitor who never signs in.
 */
export async function loadServerChoice(): Promise<StoredServerChoice | undefined> {
  const v2 = await idbGet<unknown>(KEY_V2);
  if (v2 !== undefined && isStoredChoice(v2)) {
    return { mode: asMode(v2.mode), endpoint: healStaleEndpoint(v2.endpoint) };
  }
  const v1 = await idbGet<ServerEndpoint>(KEY_V1);
  if (v1 === undefined) {
    return undefined;
  }
  return { mode: 'manual', endpoint: healStaleEndpoint(v1) };
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

/**
 * Persists the user's choice so the next load picks it up.
 *
 * The v1 key is deleted alongside the write: the v1 record is a second copy of where the user's
 * account lives, and keeping it alive after the migration only preserves a stale address for a
 * build that will never read it again.
 */
export async function saveServerChoice(choice: StoredServerChoice): Promise<void> {
  await idbSet(KEY_V2, choice);
  await idbDelete(KEY_V1);
}

/** Removes the persisted choice (e.g. on sign-out, when the user wants a clean slate). */
export async function clearServerChoice(): Promise<void> {
  await idbDelete(KEY_V2);
  await idbDelete(KEY_V1);
}
