/**
 * The "Otomatis" half of the server picker: probe the deployment's nodes, use the fastest.
 *
 * A multi-node deployment (§170's client-side routing) names its doors with
 * `NEXT_PUBLIC_MIGO_SERVERS`, and a user facing the list should not have to guess which node to
 * type. Auto mode is the reachability default, not a promise about where an account lives:
 *
 *   - Every server in the list is probed *in parallel* with an unauthenticated `GET {rest}/health`
 *     (the liveness probe migod always answers) under a short deadline, and the fastest responder
 *     wins. The probe and the ranking are the SDK's own `rankNodesByLatency` — the same recipe the
 *     transport uses for `/v1/config`'s node list — so the web picker and the SDK agree on what
 *     "fastest" means instead of carrying two implementations of it.
 *   - A latency pick may land on a node where the account does not live; that is acceptable and
 *     expected. Auto is a default for reachability, and the manual picker stays for an exact
 *     choice. (The nodes share the account store through the mesh; the client does not need to
 *     know how.)
 *   - When nothing answers, auto stays auto: the last resolution (or the first listed server)
 *     is what a sign-in attempt runs against, so the failure surfaces through the sign-in form's
 *     existing error path instead of a picker that quietly pinned a dead node.
 *
 * Resolution is re-taken on every load in auto mode — a saved endpoint is the *last* resolution,
 * shown as such, never a commitment.
 */

import { rankNodesByLatency } from '@migo/sdk';
import type { FetchLike, NodeCandidate, ServerEndpoint, Transport } from '@migo/sdk';

import { defaultServerEndpoint } from '@/lib/config.js';
import type { StoredServerChoice, ServerChoiceMode } from '@/lib/storage/server-endpoint-store.js';

/** How long one health probe may take before the node counts as unreachable. */
export const AUTO_PROBE_TIMEOUT_MS = 3_000;

/** Options the probe accepts, so a test can inject a fetch and shrink the deadline. */
export interface AutoProbeOptions {
  fetch?: FetchLike;
  timeoutMs?: number;
}

/** What a load resolves to: the mode in force, the endpoint to use, and the live auto pick. */
export interface ResolvedServer {
  mode: ServerChoiceMode;
  endpoint: ServerEndpoint;
  /**
   * The endpoint the auto probe just measured, or `null` when the mode is not auto or nothing
   * answered. The caller shows it as the current resolution, never as a permanent choice.
   */
  autoResolved: ServerEndpoint | null;
}

/**
 * Probes every server in parallel and returns the fastest responder, or `null` when none
 * answered within the deadline. Any 2xx on `/health` counts as up — it is the liveness
 * question, and anything a live node answers it with means the door opens.
 */
export async function pickFastestServer(
  servers: ServerEndpoint[],
  options: AutoProbeOptions = {},
): Promise<ServerEndpoint | null> {
  if (servers.length === 0) {
    return null;
  }
  // The node metadata is descriptive in the SDK's ranking; the picker has none to add, so the
  // endpoints carry themselves and the entries stay distinguishable by index.
  const candidates: NodeCandidate[] = servers.map((endpoint, index) => ({
    node: { id: String(index), region: '', country: '', publicUrl: '' },
    endpoint,
  }));
  const ranked = await rankNodesByLatency(candidates, {
    ...(options.fetch !== undefined ? { fetch: options.fetch } : {}),
    timeoutMs: options.timeoutMs ?? AUTO_PROBE_TIMEOUT_MS,
  });
  const fastest = ranked.find((entry) => entry.latencyMs !== null);
  return fastest?.endpoint ?? null;
}

/**
 * Turns a persisted choice into the endpoint this load should use.
 *
 * The rules, in order:
 *
 *   - A stored manual or server choice stands as typed — those modes are exact choices, and
 *     re-probing them would be second-guessing the user.
 *   - A stored auto choice (and a first visit on a build with a list, where auto is the
 *     default) is re-resolved: probe now, use the current fastest. The stored endpoint rides
 *     along only as the fallback for when nothing answers.
 *   - Auto mode with no list left (the build stopped naming servers) is downgraded to manual,
 *     because a one-door deployment has nothing to choose between.
 *
 * A stored transport preference survives auto re-resolution: the fastest node's endpoint arrives
 * as the WebSocket pair its URL implies, and a user who picked QUIC keeps QUIC on whichever node
 * won this time.
 */
export async function resolveServerChoice(
  stored: StoredServerChoice | undefined,
  servers: ServerEndpoint[],
  options: AutoProbeOptions = {},
): Promise<ResolvedServer> {
  let mode: ServerChoiceMode = stored?.mode ?? (servers.length > 1 ? 'auto' : 'manual');
  if (mode === 'auto' && servers.length < 2) {
    mode = 'manual';
  }
  if (mode !== 'auto') {
    return {
      mode,
      endpoint: stored?.endpoint ?? defaultServerEndpoint(),
      autoResolved: null,
    };
  }
  const fastest = await pickFastestServer(servers, options);
  if (fastest !== null) {
    const transport = stored?.endpoint.transport ?? fastest.transport;
    return {
      mode,
      endpoint: transport === fastest.transport ? fastest : withTransport(fastest, transport),
      autoResolved: fastest,
    };
  }
  return {
    mode,
    endpoint: stored?.endpoint ?? servers[0] ?? defaultServerEndpoint(),
    autoResolved: null,
  };
}

/**
 * Restamps an endpoint for a transport swap, preserving the TLS posture the endpoint already
 * has: the schemes follow the *endpoint*, not the host-name rule the manual form applies to a
 * freshly typed host, because the endpoints that reach this function come from real URLs — an
 * env-supplied `http://` origin on a non-loopback host is a plain deployment (this repository's
 * own VPS shape), and restamping it to TLS because of the host would point the client at a
 * certificate the server does not have. A transport choice is orthogonal to which node won the
 * probe, so it is carried across resolutions instead of being reset by them.
 */
export function withTransport(endpoint: ServerEndpoint, transport: Transport): ServerEndpoint {
  if (endpoint.transport === transport) {
    return endpoint;
  }
  const tls = endpoint.restScheme === 'Https';
  return {
    ...endpoint,
    transport,
    scheme: transport === 'WebSocket' ? (tls ? 'Wss' : 'Ws') : tls ? 'QuicTls' : 'Quic',
  };
}
