/**
 * Runtime configuration, resolved from the browser's own location first and build-time
 * environment variables second.
 *
 * Everything here is public by construction: the values are inlined into the browser bundle. The API
 * origin and gateway URL are endpoints, not secrets. The client never embeds a server secret of any
 * kind; see `.env.example`.
 *
 * The user's actual choice of host, port and scheme is the {@link ServerEndpoint} persisted in
 * IndexedDB. The resolution order on a fresh visit (no snapshot):
 *
 *   1. `NEXT_PUBLIC_MIGO_API_URL`, when the build was made with one — the deployer's explicit
 *      instruction, and the strongest signal there is.
 *   2. The browser's own origin: the server that served this page, on port 8080. When the web
 *      client and the Migo server are deployed together (the docker-compose and single-VPS
 *      posture), this is the correct guess on the very first load, and it saves the user from
 *      staring at a "could not reach the server" that was only ever a wrong default.
 *   3. `http://localhost:8080`, the development fallback.
 *
 * A multi-node deployment can also name its nodes with `NEXT_PUBLIC_MIGO_SERVERS`, a
 * comma-separated list of server URLs; the sign-in screens then offer an "Otomatis" mode that
 * probes every node and uses the fastest (lib/auto-server.js). Without it there is exactly one
 * server to talk to, and the picker stays the manual form it always was.
 */

import { serverEndpointFromUrl } from '@migo/sdk';
import type { ServerEndpoint } from '@migo/sdk';

export interface WebConfig {
  /** The REST origin the bundled env supplies, when the build was made with one. */
  defaultApiUrl: string | undefined;
  /**
   * The comma-separated server URLs the bundled env supplies, when the build was made with them
   * (`NEXT_PUBLIC_MIGO_SERVERS`). Raw here; parsed into endpoints by {@link serverListFromEnv}.
   */
  servers: string | undefined;
  /** Reported to the server in the handshake; informational only. */
  appVersion: string;
}

const DEFAULT_APP_VERSION = '0.1.0';

export const config: WebConfig = {
  defaultApiUrl: process.env.NEXT_PUBLIC_MIGO_API_URL,
  servers: process.env.NEXT_PUBLIC_MIGO_SERVERS,
  appVersion: process.env.NEXT_PUBLIC_MIGO_APP_VERSION ?? DEFAULT_APP_VERSION,
};

/**
 * The endpoint a fresh visit (no persisted snapshot) uses.
 *
 * Same-origin detection rather than a burned-in default: the page that served this
 * JavaScript is the page whose server the user is most likely trying to reach, and
 * port 8080 is where that server listens in every deployment this repository ships.
 */
export function defaultServerEndpoint(): ServerEndpoint {
  if (config.defaultApiUrl !== undefined) {
    // The URL's own scheme is the ground truth now (the SDK helper honours it the same way
    // Android and desktop do): an env-supplied `http://152.53.102.150:8080` resolves to the
    // plain single-port pair — gateway on the same port — with no re-derivation here.
    return serverEndpointFromUrl(config.defaultApiUrl);
  }
  if (typeof window !== 'undefined' && window.location) {
    const { protocol, hostname } = window.location;
    const scheme = protocol === 'https:' ? 'https' : 'http';
    // The gateway rides the same port as REST (migod serves /ws on its HTTP listener), and the
    // schemes follow the page's own protocol: a server that served this page over plain HTTP
    // has no TLS certificate, so the endpoint stays plain.
    return serverEndpointFromUrl(`${scheme}://${hostname}:8080`);
  }
  // The last-resort fallback is this repository's own dev shape: `make dev` runs migod on
  // 127.0.0.1:8080 with /ws on that same HTTP listener. The SDK helper's loopback rule keeps
  // the split-port dev policy (gateway on the next port), so the fallback pins the single-port
  // shape explicitly rather than inheriting it.
  return {
    ...serverEndpointFromUrl('http://localhost:8080'),
    gatewayPort: 8080,
  };
}

/**
 * Parses the comma-separated server list `NEXT_PUBLIC_MIGO_SERVERS` carries.
 *
 * One typo in a deployer's list must not take the picker down, so entries that do not resolve
 * through `serverEndpointFromUrl` are skipped rather than fatal, and duplicates collapse (a
 * node named twice is one node). An unset or all-invalid list is the empty array — the caller
 * decides what that means, which for this client is "there is no list, behave as before".
 */
export function serverListFromEnv(raw: string | undefined): ServerEndpoint[] {
  if (raw === undefined) {
    return [];
  }
  const seen = new Set<string>();
  const servers: ServerEndpoint[] = [];
  for (const entry of raw.split(',')) {
    if (entry.trim() === '') {
      continue;
    }
    let endpoint: ServerEndpoint;
    try {
      endpoint = serverEndpointFromUrl(entry);
    } catch {
      continue;
    }
    // A schemeless `host:port` typo can parse as a bogus protocol with an empty hostname; an
    // endpoint that names no host is not a door, so it is skipped with the invalid entries.
    if (endpoint.host === '') {
      continue;
    }
    const identity = `${endpoint.restScheme}://${endpoint.host}:${endpoint.port}`;
    if (!seen.has(identity)) {
      seen.add(identity);
      servers.push(endpoint);
    }
  }
  return servers;
}

/**
 * The servers this deployment names, for the picker and the "Otomatis" probe.
 *
 * With no list the answer is the single endpoint a fresh visit would use — the deployment URL
 * or the same-origin default — so every caller can treat the result as "the doors this build
 * knows about" without a second fallback of its own.
 */
export function knownServers(): ServerEndpoint[] {
  const listed = serverListFromEnv(config.servers);
  if (listed.length === 0) {
    return [defaultServerEndpoint()];
  }
  return listed;
}
