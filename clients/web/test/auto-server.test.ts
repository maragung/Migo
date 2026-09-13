/**
 * The "Otomatis" probe: how the web client turns a list of servers into the one it uses.
 *
 * Auto mode is a reachability default for multi-node deployments (§170's client-side routing):
 * every server the build names is probed with `GET {rest}/health` in parallel, the fastest
 * responder wins, and the answer is shown to the user before anything commits. The tests pin
 * the behaviours the picker's honesty depends on, all against a `fetch` double — never a socket:
 *
 *   1. The fastest responder wins, every listed server is probed, any 2xx counts as up, and a
 *      node that answers with an error status or not at all is never chosen.
 *   2. A saved auto choice re-resolves on load: the current fastest node is the endpoint, the
 *      saved address only the fallback for when nothing answers — and in that failure the mode
 *      stays auto, because pinning a dead probe as a fixed server is the one outcome the
 *      picker must never produce.
 *   3. Exact choices (manual, a named server) are never re-probed — auto is the only mode that
 *      measures, and a first visit defaults to it exactly when the build names more than one
 *      server.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { serverEndpointFromUrl } from '@migo/sdk';
import type { FetchLike, ServerEndpoint } from '@migo/sdk';

import { pickFastestServer, resolveServerChoice, withTransport } from '../src/lib/auto-server.js';
import type { StoredServerChoice } from '../src/lib/storage/server-endpoint-store.js';

/** Two doors a deployment might name; non-loopback so both parse to the plain single-port pair. */
const NODE_A = serverEndpointFromUrl('http://10.0.0.1:8080');
const NODE_B = serverEndpointFromUrl('http://10.0.0.2:8080');
const LIST = [NODE_A, NODE_B];

interface Planned {
  url: string;
  status: number;
  delayMs: number;
}

/**
 * A `fetch` double that answers the servers it knows after the delay each was given; a server
 * with no plan answers 200 immediately, and a delay of `-1` means "never answer at all".
 */
function plannedFetch(plans: Record<string, Planned>): FetchLike {
  return (input) => {
    const url = input;
    const plan = plans[url];
    if (plan === undefined) {
      return Promise.resolve(new Response(null, { status: 200 }));
    }
    if (plan.delayMs === -1) {
      return new Promise<Response>(() => undefined);
    }
    return new Promise<Response>((resolve) => {
      setTimeout(() => {
        resolve(new Response(null, { status: plan.status }));
      }, plan.delayMs);
    });
  };
}

function healthUrl(endpoint: ServerEndpoint): string {
  return `http://${endpoint.host}:${endpoint.port}/health`;
}

test('the probe picks the fastest responder of the list', async () => {
  const fetchImpl = plannedFetch({
    [healthUrl(NODE_A)]: { url: healthUrl(NODE_A), status: 200, delayMs: 40 },
    [healthUrl(NODE_B)]: { url: healthUrl(NODE_B), status: 200, delayMs: 1 },
  });
  const picked = await pickFastestServer(LIST, { fetch: fetchImpl });
  assert.deepEqual(picked, NODE_B);
});

test('every server in the list is probed, in parallel', async () => {
  const seen: string[] = [];
  const fetchImpl: FetchLike = (input) => {
    seen.push(input);
    return Promise.resolve(new Response(null, { status: 200 }));
  };
  await pickFastestServer(LIST, { fetch: fetchImpl });
  assert.deepEqual(seen.sort(), [healthUrl(NODE_A), healthUrl(NODE_B)].sort());
});

test('a node answering a non-2xx health check is never chosen', async () => {
  const fetchImpl = plannedFetch({
    [healthUrl(NODE_A)]: { url: healthUrl(NODE_A), status: 503, delayMs: 1 },
    [healthUrl(NODE_B)]: { url: healthUrl(NODE_B), status: 200, delayMs: 40 },
  });
  const picked = await pickFastestServer(LIST, { fetch: fetchImpl });
  // The error status is not "up", so the slower healthy node wins.
  assert.deepEqual(picked, NODE_B);
});

test('a node that misses the deadline counts as unreachable', async () => {
  const fetchImpl = plannedFetch({
    [healthUrl(NODE_A)]: { url: healthUrl(NODE_A), status: 200, delayMs: -1 },
    [healthUrl(NODE_B)]: { url: healthUrl(NODE_B), status: 200, delayMs: 1 },
  });
  const picked = await pickFastestServer(LIST, { fetch: fetchImpl, timeoutMs: 60 });
  assert.deepEqual(picked, NODE_B);
});

test('when no server answers, the probe says so instead of guessing', async () => {
  const fetchImpl = plannedFetch({
    [healthUrl(NODE_A)]: { url: healthUrl(NODE_A), status: 503, delayMs: 1 },
    [healthUrl(NODE_B)]: { url: healthUrl(NODE_B), status: 500, delayMs: 1 },
  });
  assert.equal(await pickFastestServer(LIST, { fetch: fetchImpl }), null);
});

test('an empty list has nothing to pick', async () => {
  assert.equal(await pickFastestServer([]), null);
});

// --- how a persisted choice resolves on load ---

test('a stored auto choice re-probes and uses the current fastest node', async () => {
  const stored: StoredServerChoice = { mode: 'auto', endpoint: NODE_A };
  const fetchImpl = plannedFetch({
    [healthUrl(NODE_A)]: { url: healthUrl(NODE_A), status: 200, delayMs: 40 },
    [healthUrl(NODE_B)]: { url: healthUrl(NODE_B), status: 200, delayMs: 1 },
  });
  const resolved = await resolveServerChoice(stored, LIST, { fetch: fetchImpl });
  assert.equal(resolved.mode, 'auto');
  assert.deepEqual(resolved.endpoint, NODE_B);
  assert.deepEqual(resolved.autoResolved, NODE_B);
});

test('a stored transport preference rides along onto the freshly probed node', async () => {
  const stored: StoredServerChoice = {
    mode: 'auto',
    endpoint: { ...NODE_A, transport: 'Quic', scheme: 'Quic', restScheme: 'Http' },
  };
  const fetchImpl = plannedFetch({
    // Both nodes get explicit plans: a server with no plan answers immediately in this
    // double, so leaving NODE_A unplanned would make the *stored* node win the probe
    // and the test would no longer prove anything about the winner carrying the
    // transport over.
    [healthUrl(NODE_A)]: { url: healthUrl(NODE_A), status: 200, delayMs: 40 },
    [healthUrl(NODE_B)]: { url: healthUrl(NODE_B), status: 200, delayMs: 1 },
  });
  const resolved = await resolveServerChoice(stored, LIST, { fetch: fetchImpl });
  assert.equal(resolved.endpoint.transport, 'Quic');
  assert.equal(resolved.endpoint.host, NODE_B.host);
  // The QUIC pair preserves the plain posture the probed node's URL carries.
  assert.equal(resolved.endpoint.scheme, 'Quic');
  assert.equal(resolved.endpoint.restScheme, 'Http');
});

test('when nothing answers, auto stays auto and falls back to the last resolution', async () => {
  const stored: StoredServerChoice = { mode: 'auto', endpoint: NODE_A };
  const fetchImpl = plannedFetch({
    [healthUrl(NODE_A)]: { url: healthUrl(NODE_A), status: 503, delayMs: 1 },
    [healthUrl(NODE_B)]: { url: healthUrl(NODE_B), status: 500, delayMs: 1 },
  });
  const resolved = await resolveServerChoice(stored, LIST, { fetch: fetchImpl });
  assert.equal(resolved.mode, 'auto');
  assert.deepEqual(resolved.endpoint, NODE_A);
  assert.equal(resolved.autoResolved, null);
});

test('a first visit on a build with a list defaults to auto and resolves it', async () => {
  const fetchImpl = plannedFetch({
    [healthUrl(NODE_A)]: { url: healthUrl(NODE_A), status: 200, delayMs: 1 },
    [healthUrl(NODE_B)]: { url: healthUrl(NODE_B), status: 200, delayMs: 40 },
  });
  const resolved = await resolveServerChoice(undefined, LIST, { fetch: fetchImpl });
  assert.equal(resolved.mode, 'auto');
  assert.deepEqual(resolved.endpoint, NODE_A);
});

test('a first visit with nothing answering falls back to the first listed server', async () => {
  const fetchImpl = plannedFetch({
    [healthUrl(NODE_A)]: { url: healthUrl(NODE_A), status: 500, delayMs: 1 },
    [healthUrl(NODE_B)]: { url: healthUrl(NODE_B), status: 500, delayMs: 1 },
  });
  const resolved = await resolveServerChoice(undefined, LIST, { fetch: fetchImpl });
  assert.equal(resolved.mode, 'auto');
  assert.deepEqual(resolved.endpoint, NODE_A);
  assert.equal(resolved.autoResolved, null);
});

test('a manual choice is exact and is never re-probed', async () => {
  let probed = 0;
  const fetchImpl: FetchLike = () => {
    probed += 1;
    return Promise.resolve(new Response(null, { status: 200 }));
  };
  const stored: StoredServerChoice = { mode: 'manual', endpoint: NODE_A };
  const resolved = await resolveServerChoice(stored, LIST, { fetch: fetchImpl });
  assert.equal(resolved.mode, 'manual');
  assert.deepEqual(resolved.endpoint, NODE_A);
  assert.equal(resolved.autoResolved, null);
  assert.equal(probed, 0, 'an exact choice must not trigger the probe');
});

test('a named-server choice stands as typed, without a probe', async () => {
  let probed = 0;
  const fetchImpl: FetchLike = () => {
    probed += 1;
    return Promise.resolve(new Response(null, { status: 200 }));
  };
  const stored: StoredServerChoice = { mode: 'server', endpoint: NODE_B };
  const resolved = await resolveServerChoice(stored, LIST, { fetch: fetchImpl });
  assert.equal(resolved.mode, 'server');
  assert.deepEqual(resolved.endpoint, NODE_B);
  assert.equal(probed, 0);
});

test('a first visit with a single server is manual, on the default endpoint', async () => {
  let probed = 0;
  const fetchImpl: FetchLike = () => {
    probed += 1;
    return Promise.resolve(new Response(null, { status: 200 }));
  };
  const resolved = await resolveServerChoice(undefined, [NODE_A], { fetch: fetchImpl });
  assert.equal(resolved.mode, 'manual');
  assert.equal(resolved.endpoint.host, 'localhost');
  assert.equal(resolved.endpoint.port, 8080);
  assert.equal(probed, 0, 'a single-door build has nothing to choose between');
});

test('a stored auto choice downgrades to manual when the build no longer names a list', async () => {
  const stored: StoredServerChoice = { mode: 'auto', endpoint: NODE_A };
  const resolved = await resolveServerChoice(stored, [NODE_B]);
  assert.equal(resolved.mode, 'manual');
  assert.deepEqual(resolved.endpoint, NODE_A);
});

// --- the transport restamp ---

test('withTransport preserves the endpoint TLS posture across the scheme families', () => {
  // A plain non-loopback deployment (the env-supplied http:// shape) keeps its plain pair.
  const web = withTransport(NODE_A, 'WebSocket');
  assert.equal(web.transport, 'WebSocket');
  assert.equal(web.scheme, 'Ws');
  assert.equal(web.restScheme, 'Http');

  const quicPlain = withTransport(NODE_A, 'Quic');
  assert.equal(quicPlain.scheme, 'Quic');
  assert.equal(quicPlain.restScheme, 'Http');

  // A TLS endpoint keeps TLS in both families.
  const tls = serverEndpointFromUrl('https://migo.example.com');
  const quicTls = withTransport(tls, 'Quic');
  assert.equal(quicTls.scheme, 'QuicTls');
  assert.equal(quicTls.restScheme, 'Https');
  const backToWeb = withTransport(quicTls, 'WebSocket');
  assert.equal(backToWeb.scheme, 'Wss');
  assert.equal(backToWeb.restScheme, 'Https');

  // Restamping onto the transport the endpoint already has is a no-op, identity included.
  assert.equal(withTransport(NODE_A, 'WebSocket'), NODE_A);
});
