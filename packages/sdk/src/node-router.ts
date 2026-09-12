/**
 * Client node routing (§170): turning the config document's node list into a connection plan.
 *
 * A deployment may run several nodes side by side, and the doc assigns the routing decision to
 * the client: the server only names the doors (`/v1/config`'s `nodes` list, this node first),
 * and the client decides which one to walk through. The recipe is three steps:
 *
 *   1. Fetch the config document from any node the user pointed the app at
 *      ({@link BootstrapClient.config}).
 *   2. Convert every entry to the {@link ServerEndpoint} the transport speaks
 *      ({@link candidatesFromConfig}) and measure them in parallel ({@link rankNodesByLatency}).
 *   3. Hand the ranking to the transport: the fastest node as `server`, the rest as
 *      `failoverServers` ({@link routingPlanFromConfig} does both steps at once).
 *
 * The measurement is an unauthenticated `GET /health` — the liveness probe, the cheapest
 * question a node answers — timed wall-clock per candidate. It is a probe, not a promise: nodes
 * answer in whatever order their sockets and the network produce, so the ranking is taken as
 * advice and re-taken next time the app builds a connection. A node the probe cannot reach
 * sorts last rather than being dropped: unreachable-for-one-probe is not down, and a failover
 * list that silently lost a node is worse than one that tries a slow candidate.
 *
 * What routing does *not* carry over is the session. Session state lives on the node that minted
 * it (§150), so a client that lands on a different node — because the ranking picked it, or
 * because a failover got there — starts a fresh session and resyncs through the transport's
 * reset path, exactly as a resume that found nothing would. The nodes share the account store
 * through the mesh; the client does not need to know how.
 */

import type { FetchLike, NodeConfig, ServerConfig } from './rest.js';
import { restBaseUrl, serverEndpointFromUrl } from './server-endpoint.js';
import type { ServerEndpoint } from './server-endpoint.js';
import { TransportError } from './errors.js';

/** One candidate node, with the endpoint its public URL resolves to. */
export interface NodeCandidate {
  /** The node as the config document described it. */
  node: NodeConfig;
  /** The endpoint the transport would connect to for this node. */
  endpoint: ServerEndpoint;
}

/** A candidate plus what the probe measured, or `null` when the probe could not reach it. */
export interface RankedNode extends NodeCandidate {
  /** Round-trip milliseconds for the `GET /health` probe, or `null` when the node did not answer. */
  latencyMs: number | null;
}

/** Options for the ranking probe. */
export interface RankOptions {
  /** The `fetch` to probe with; defaults to the global one. */
  fetch?: FetchLike;
  /** How long to wait for one probe before calling the node unreachable; default 5000. */
  timeoutMs?: number;
}

/** The transport's connection inputs, derived from a measured ranking. */
export interface RoutingPlan {
  /** The node to connect to first: the fastest the probe could reach. */
  server: ServerEndpoint;
  /** The remaining nodes, fastest first, for the transport's failover (§170). */
  failoverServers: ServerEndpoint[];
}

/** How long a probe waits before the node counts as unreachable. */
const DEFAULT_PROBE_TIMEOUT_MS = 5_000;

/**
 * Converts the config document's node list into transport endpoints, in document order.
 *
 * The list always carries this node first, so the first candidate is the node the app just
 * fetched the document from — the ranking may still reorder it, but the order here is the
 * operator's declared preference and the tie-breaker the stable sort keeps.
 */
export function candidatesFromConfig(config: ServerConfig): NodeCandidate[] {
  return config.nodes.map((node) => ({ node, endpoint: serverEndpointFromUrl(node.publicUrl) }));
}

/**
 * Measures every candidate in parallel with an unauthenticated `GET /health`, and returns them
 * sorted fastest first. Unreachable nodes sort last, keeping their input order; they are not
 * dropped (see the module doc).
 */
export async function rankNodesByLatency(
  candidates: NodeCandidate[],
  options: RankOptions = {},
): Promise<RankedNode[]> {
  const fetchImpl = options.fetch ?? globalThis.fetch;
  if (fetchImpl === undefined) {
    throw new TypeError(
      'rankNodesByLatency needs a fetch implementation: none was found on globalThis',
    );
  }
  const timeoutMs = options.timeoutMs ?? DEFAULT_PROBE_TIMEOUT_MS;
  const ranked = await Promise.all(
    candidates.map(async (candidate): Promise<RankedNode> => {
      const startedAt = Date.now();
      try {
        const reply = await withTimeout(
          fetchImpl(`${restBaseUrl(candidate.endpoint)}/health`, { method: 'GET' }),
          timeoutMs,
        );
        if (!reply.ok) {
          // A node that answers a liveness probe with an error is not a candidate the
          // client should prefer; treat it like an unreachable one.
          return { ...candidate, latencyMs: null };
        }
        return { ...candidate, latencyMs: Date.now() - startedAt };
      } catch {
        return { ...candidate, latencyMs: null };
      }
    }),
  );
  // Stable sort: reachable nodes ascending by latency, unreachable last in input order, and
  // equal latencies keep the document order (this node first).
  return ranked.sort((a, b) => {
    if (a.latencyMs === null) return b.latencyMs === null ? 0 : 1;
    if (b.latencyMs === null) return -1;
    return a.latencyMs - b.latencyMs;
  });
}

/**
 * The whole recipe in one call: rank the config document's nodes and hand the ranking to the
 * transport's two inputs.
 *
 * When no node answers the probe at all, the document order stands — the transport will try
 * them itself, report the failure honestly, and a flapping measurement never strands the client
 * with an empty plan.
 */
export async function routingPlanFromConfig(
  config: ServerConfig,
  options: RankOptions = {},
): Promise<RoutingPlan> {
  const ranked = await rankNodesByLatency(candidatesFromConfig(config), options);
  if (ranked.length === 0) {
    throw new TransportError('the config document named no nodes to route between');
  }
  const reachable = ranked.filter((entry) => entry.latencyMs !== null);
  const order = reachable.length > 0 ? reachable : ranked;
  return {
    server: order[0]?.endpoint as ServerEndpoint,
    failoverServers: order.slice(1).map((entry) => entry.endpoint),
  };
}

/**
 * Races a promise against a deadline, so a probe cannot hang a ranking forever.
 *
 * The timer is cleared on every settlement path; the racing promise's own result (or rejection)
 * wins whenever it lands first. Written against the bare promise rather than `AbortSignal` so an
 * injected `fetch` that ignores signals is still bounded.
 */
function withTimeout<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new TransportError(`probe timed out after ${timeoutMs}ms`)),
      timeoutMs,
    );
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (cause: unknown) => {
        clearTimeout(timer);
        reject(cause instanceof Error ? cause : new TransportError(String(cause)));
      },
    );
  });
}
