/**
 * Client node routing (§170): the client picks the node it talks to, and survives one dying.
 *
 * The doc's division of labour: the server *names* the doors (`/v1/config`'s `nodes` list, this
 * node first) and the client *chooses* — by measuring the list per node and connecting to the
 * fastest, keeping the rest as failover candidates. Three behaviours are pinned here, because
 * each would otherwise be easy to quietly lose:
 *
 *   1. The ranking is measured, not assumed: a node that answers its health probe slower sorts
 *      lower, and a node that does not answer sorts last instead of vanishing from the list.
 *   2. Failover advances only on a *dead link*. A node that answered with a refusal spoke and
 *      meant it; a node whose socket never opened is exactly the case the next candidate exists
 *      for. Conflating the two would turn every rejected token into a fan-out to every node.
 *   3. Failover is honest about the session. Session state lives on the node that minted it
 *      (§150), so landing on a neighbour is a fresh session and the reset path
 *      (re-subscribe + resync), never a silent pretence that nothing happened.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  BootstrapClient,
  GatewayTransport,
  RemoteError,
  TransportError,
  candidatesFromConfig,
  decodeBody,
  encodeBody,
  rankNodesByLatency,
  routingPlanFromConfig,
} from '../src/index.js';
import type { FetchLike, ServerConfig, ServerEndpoint } from '../src/index.js';
import { decodeFrame, encodeFrame, frameHeader } from '@migo/wire';
import {
  BandwidthMode,
  CODE,
  OP,
  Platform,
  decodeHello,
  encodeError,
  encodeWelcome,
  FLAG,
} from '@migo/protocol';
import type { Welcome } from '@migo/protocol';
import { idOf } from './harness.js';

/** Lets all pending microtasks and the transport's async frame build settle. */
function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** One gateway address per fake node, distinct hosts so every socket's URL names its node. */
function nodeEndpoint(host: string): ServerEndpoint {
  return {
    host,
    port: 443,
    gatewayPort: 443,
    transport: 'WebSocket',
    scheme: 'Wss',
    restScheme: 'Https',
  };
}

/**
 * A stand-in WebSocket that records what the transport sends and lets the test play the server.
 *
 * The same shape `transport.test.ts` uses; duplicated here because a test double is cheaper to
 * keep honest than an export the production module only needs for its own tests.
 */
class FakeSocket {
  static readonly OPEN = 1;
  static readonly CLOSED = 3;

  binaryType = 'blob';
  readyState = 0;
  readonly sent: unknown[] = [];
  readonly url: string;
  #closed = false;

  constructor(url: string) {
    this.url = url;
  }

  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;

  send(data: unknown): void {
    this.sent.push(data);
  }

  close(code = 1000, reason = ''): void {
    if (this.#closed) {
      return;
    }
    this.#closed = true;
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.({ code, reason });
  }

  /** Simulates the socket opening once the transport has attached its handlers. */
  fireOpen(): void {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }

  /** Delivers a binary server frame the way `binaryType = 'arraybuffer'` would. */
  deliver(bytes: Uint8Array): void {
    this.onmessage?.({ data: bytes });
  }
}

/** A WELCOME that authenticates inline, so the handshake reaches Ready without an AUTHENTICATE. */
function welcomeFrame(): Uint8Array {
  const welcome: Welcome = {
    sessionId: idOf(1),
    node: { nodeId: 'node-1', region: 'eu', country: 'DE' },
    // No feature bits, so nothing the transport sends is compressed and each frame decodes plainly.
    features: 0n,
    serverTime: 1_700_000_000_000,
    limits: {
      maxFrameBytes: 1 << 20,
      maxBatchItems: 64,
      maxSubscriptions: 256,
      heartbeatMs: 30_000,
    },
    authenticatedUser: idOf(10),
  };
  return encodeFrame({
    header: frameHeader(OP.HELLO, 1),
    payload: encodeBody(encodeWelcome, welcome),
  });
}

/** A handshake refusal the server delivers as a WELCOME-shaped error frame. */
function errorFrame(code: number, symbol: string, message: string): Uint8Array {
  return encodeFrame({
    header: { ...frameHeader(OP.HELLO, 1), flags: FLAG.ERROR },
    payload: encodeBody(encodeError, { code, symbol, message }),
  });
}

/** A config document as `/v1/config` serves it: this node first, then its peers. */
function configDocument(nodeIds: string[]): ServerConfig {
  return {
    node: {
      id: nodeIds[0] ?? '',
      region: 'eu',
      country: 'DE',
      publicUrl: `https://${nodeIds[0]}.example`,
    },
    nodes: nodeIds.map((id) => ({
      id,
      region: 'eu',
      country: 'DE',
      publicUrl: `https://${id}.example`,
    })),
    features: 0n,
    limits: {
      allowRegistration: true,
      passphraseMinLength: 10,
      maxDevicesPerUser: 5,
      maxBodyBytes: 1 << 20,
      maxPageSize: 200,
    },
  };
}

/** Builds a transport over two fake nodes and records every socket the factory builds. */
function twoNodeTransport(): {
  transport: GatewayTransport;
  sockets: FakeSocket[];
} {
  const sockets: FakeSocket[] = [];
  const transport = new GatewayTransport({
    server: nodeEndpoint('node-a.example'),
    failoverServers: [nodeEndpoint('node-b.example')],
    hello: {
      platform: Platform.Web,
      appVersion: '1.0.0',
      locale: 'en',
      bandwidthMode: BandwidthMode.Normal,
      accessToken: 'test-token',
      deviceId: idOf(11),
      features: 0n,
    },
    // Far enough out that the heartbeat never fires during a test.
    heartbeatMs: 600_000,
    webSocketFactory: (url: string) => {
      const socket = new FakeSocket(url);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    },
  });
  return { transport, sockets };
}

// --- transport failover -------------------------------------------------------------------------

test('a first connect whose node is unreachable fails over to the next candidate', async () => {
  const { transport, sockets } = twoNodeTransport();
  const ready = transport.connect();

  // The first node never opens its socket: the link is refused before a HELLO can exist.
  const first = sockets[0];
  assert.ok(first !== undefined, 'the transport did not build a socket synchronously');
  assert.ok(first.url.includes('node-a.example'), 'the first socket went to node-a');
  first.close(1006, 'connection refused');
  await tick();

  // The candidate list is consulted, not retried in place: the second socket names node-b,
  // under the *same* connect() promise the caller is still holding.
  const second = sockets[1];
  assert.ok(second !== undefined, 'a dead first node did not produce a second socket');
  assert.ok(second.url.includes('node-b.example'), 'the failover socket did not go to node-b');
  assert.equal(transport.currentServer.host, 'node-b.example');

  second.fireOpen();
  await tick(); // the HELLO is built and sent
  second.deliver(welcomeFrame());
  await ready;
  assert.equal(transport.state, 'ready');
  assert.equal(transport.session?.node.nodeId, 'node-1');

  transport.close();
});

test('a mid-session drop that cannot reach its node fails over and lands as a fresh session', async () => {
  const resets: number[] = [];
  const sockets: FakeSocket[] = [];
  const transport = new GatewayTransport({
    server: nodeEndpoint('node-a.example'),
    failoverServers: [nodeEndpoint('node-b.example')],
    hello: {
      platform: Platform.Web,
      appVersion: '1.0.0',
      locale: 'en',
      bandwidthMode: BandwidthMode.Normal,
      accessToken: 'test-token',
      deviceId: idOf(11),
      features: 0n,
    },
    heartbeatMs: 600_000,
    onReset: () => resets.push(Date.now()),
    webSocketFactory: (url: string) => {
      const socket = new FakeSocket(url);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    },
  });

  // The first session stands up normally on node-a.
  const firstReady = transport.connect();
  const first = sockets[0];
  assert.ok(first !== undefined, 'the transport did not build a socket synchronously');
  first.fireOpen();
  await tick();
  first.deliver(welcomeFrame());
  await firstReady;
  assert.equal(transport.state, 'ready');

  // The network drops the socket; the backoff would wait it out, so pull it forward.
  first.close(1006, 'network drop');
  transport.reconnectNow();
  await tick();

  // The reconnect tries node-a again first — a blip deserves a same-node retry — and its HELLO
  // carries a resume request for the session it just lost.
  const retry = sockets[1];
  assert.ok(retry !== undefined, 'the reconnect did not open a socket');
  assert.ok(retry.url.includes('node-a.example'), 'the reconnect did not try the same node first');
  retry.fireOpen();
  await tick();
  const retryHello = decodeBody(decodeHello, decodeFrame(retry.sent[0] as Uint8Array).payload);
  assert.ok(retryHello.resume !== undefined, 'the same-node retry did not ask to resume');

  // node-a is down for good: the socket is refused without an answer. The transport advances to
  // node-b under the reconnect's own promise.
  retry.close(1006, 'connection refused');
  await tick();
  const failover = sockets[2];
  assert.ok(failover !== undefined, 'a dead node did not produce a failover socket');
  assert.ok(failover.url.includes('node-b.example'), 'the failover socket did not go to node-b');
  assert.equal(transport.currentServer.host, 'node-b.example');

  // node-b answers with a fresh session. The old session's state lives on node-a, so this is a
  // reset: the app hears onReset and re-subscribes and resyncs — never a silent pretence.
  failover.fireOpen();
  await tick();
  const failoverHello = decodeBody(
    decodeHello,
    decodeFrame(failover.sent[0] as Uint8Array).payload,
  );
  assert.ok(failoverHello.resume !== undefined, 'the failover HELLO should still offer the resume');
  failover.deliver(welcomeFrame());
  await tick();
  assert.equal(transport.state, 'ready', 'the failover session did not reach Ready');
  assert.equal(resets.length, 1, 'the failover session did not fire onReset exactly once');

  transport.close();
});

test('a failover node that does not know the session answers RESUME_REQUIRED and gets a fresh HELLO', async () => {
  const resets: number[] = [];
  const sockets: FakeSocket[] = [];
  const transport = new GatewayTransport({
    server: nodeEndpoint('node-a.example'),
    failoverServers: [nodeEndpoint('node-b.example')],
    hello: {
      platform: Platform.Web,
      appVersion: '1.0.0',
      locale: 'en',
      bandwidthMode: BandwidthMode.Normal,
      accessToken: 'test-token',
      deviceId: idOf(11),
      features: 0n,
    },
    heartbeatMs: 600_000,
    onReset: () => resets.push(Date.now()),
    webSocketFactory: (url: string) => {
      const socket = new FakeSocket(url);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    },
  });

  // Session on node-a, then the drop.
  const firstReady = transport.connect();
  const first = sockets[0];
  assert.ok(first !== undefined, 'the transport did not build a socket synchronously');
  first.fireOpen();
  await tick();
  first.deliver(welcomeFrame());
  await firstReady;
  first.close(1006, 'network drop');
  transport.reconnectNow();
  await tick();

  // node-a is unreachable, so the reconnect fails over to node-b — whose HELLO still carries the
  // resume request, because the transport cannot know node-b is a different node until it says so.
  const retry = sockets[1];
  assert.ok(retry !== undefined, 'the reconnect did not open a socket');
  retry.close(1006, 'connection refused');
  await tick();
  const failover = sockets[2];
  assert.ok(failover !== undefined, 'a dead node did not produce a failover socket');
  assert.ok(failover.url.includes('node-b.example'), 'the failover socket did not go to node-b');
  failover.fireOpen();
  await tick();

  // node-b is alive but has never heard of the session: it answers RESUME_REQUIRED, exactly as
  // §150 prescribes for a session the answering node does not hold.
  failover.deliver(
    errorFrame(CODE.RESUME_REQUIRED, 'RESUME_REQUIRED', 'no resumable session for that id'),
  );
  await tick();

  // The contract: the transport stays on node-b (it is alive; the session was the problem),
  // opens another socket there, and that HELLO carries no resume request. The session that
  // results is fresh and fires onReset once.
  const fresh = sockets[3];
  assert.ok(fresh !== undefined, 'RESUME_REQUIRED did not lead to a fresh connection attempt');
  assert.ok(fresh.url.includes('node-b.example'), 'the fresh attempt left the failover node');
  assert.notEqual(transport.state, 'closed', 'the transport treated RESUME_REQUIRED as terminal');
  fresh.fireOpen();
  await tick();
  const freshHello = decodeBody(decodeHello, decodeFrame(fresh.sent[0] as Uint8Array).payload);
  assert.ok(freshHello.resume === undefined, 'the fresh HELLO still asked to resume');
  fresh.deliver(welcomeFrame());
  await tick();
  assert.equal(transport.state, 'ready', 'the fresh session did not reach Ready');
  assert.equal(resets.length, 1, 'the fresh session did not fire onReset exactly once');

  transport.close();
});

test('connect() rejects once every candidate has failed, naming the last failure', async () => {
  const { transport, sockets } = twoNodeTransport();
  const ready = transport.connect();

  sockets[0]?.close(1006, 'connection refused');
  await tick();
  sockets[1]?.close(1006, 'connection refused');
  await assert.rejects(ready, (error: unknown) => {
    assert.ok(error instanceof TransportError, `expected TransportError, got ${String(error)}`);
    assert.match(error.message, /handshake/);
    return true;
  });
  assert.equal(sockets.length, 2, 'the transport tried a candidate more than once');
  assert.equal(transport.state, 'closed');

  transport.close();
});

test('a server that answers with a refusal is not failed over', async () => {
  const { transport, sockets } = twoNodeTransport();
  const ready = transport.connect();

  // The node is alive and answers — with a refusal. A rejected token is rejected by every node,
  // so trying node-b would only collect the same refusal from a second server.
  const first = sockets[0];
  assert.ok(first !== undefined, 'the transport did not build a socket synchronously');
  first.fireOpen();
  await tick(); // the HELLO is built and sent
  first.deliver(errorFrame(CODE.TOKEN_INVALID, 'TOKEN_INVALID', 'the token is not valid'));
  await assert.rejects(ready, (error: unknown) => {
    assert.ok(error instanceof RemoteError, `expected RemoteError, got ${String(error)}`);
    assert.equal(error.code, CODE.TOKEN_INVALID);
    return true;
  });
  assert.equal(sockets.length, 1, 'a server refusal must not open a failover socket');
  assert.equal(transport.state, 'closed');

  transport.close();
});

test('without a failover list, a dead node is a terminal failure exactly as before', async () => {
  const sockets: FakeSocket[] = [];
  const transport = new GatewayTransport({
    server: nodeEndpoint('node-a.example'),
    hello: {
      platform: Platform.Web,
      appVersion: '1.0.0',
      locale: 'en',
      bandwidthMode: BandwidthMode.Normal,
      accessToken: 'test-token',
      deviceId: idOf(11),
      features: 0n,
    },
    heartbeatMs: 600_000,
    webSocketFactory: (url: string) => {
      const socket = new FakeSocket(url);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    },
  });
  const ready = transport.connect();
  sockets[0]?.close(1006, 'connection refused');
  await assert.rejects(ready, (error: unknown) => {
    assert.ok(error instanceof TransportError, `expected TransportError, got ${String(error)}`);
    return true;
  });
  assert.equal(sockets.length, 1, 'the single-node transport retried in place');

  transport.close();
});

// --- the latency ranking ------------------------------------------------------------------------

/**
 * A fetch double keyed by URL substring: each node's probe behaves as its handler says, and an
 * unknown URL rejects the way a browser's fetch does.
 */
function scriptedFetch(
  handlers: Array<{ match: string; respond: () => Promise<Response> }>,
): FetchLike {
  return (input: string) => {
    const handler = handlers.find((entry) => input.includes(entry.match));
    if (handler === undefined) {
      return Promise.reject(new TypeError('Failed to fetch'));
    }
    return handler.respond();
  };
}

/** A probe answer: the liveness probe's `{"status":"ok"}`. */
function healthResponse(): Promise<Response> {
  return Promise.resolve(new Response('{"status":"ok"}', { status: 200 }));
}

/** Resolves after `delayMs`, so one probe is measurably slower than another. */
function delayed(ms: number): () => Promise<Response> {
  return () =>
    new Promise<Response>((resolve) => {
      setTimeout(() => resolve(new Response('{"status":"ok"}', { status: 200 })), ms);
    });
}

test('rankNodesByLatency orders by measured latency and keeps unreachable nodes last', async () => {
  const candidates = candidatesFromConfig(configDocument(['node-a', 'node-b', 'node-c']));
  const ranked = await rankNodesByLatency(candidates, {
    // node-a answers at once, node-b is slower, node-c does not answer at all.
    fetch: scriptedFetch([
      { match: 'node-a.example', respond: healthResponse },
      { match: 'node-b.example', respond: delayed(40) },
      { match: 'node-c.example', respond: () => Promise.reject(new TypeError('Failed to fetch')) },
    ]),
  });

  assert.equal(ranked.length, 3, 'an unreachable node must not drop out of the ranking');
  assert.equal(ranked[0]?.node.id, 'node-a', 'the fastest node did not sort first');
  assert.equal(ranked[1]?.node.id, 'node-b', 'the slower node did not sort second');
  assert.ok((ranked[0]?.latencyMs ?? -1) < (ranked[1]?.latencyMs ?? -1), 'latencies did not order');
  assert.equal(ranked[2]?.node.id, 'node-c');
  assert.equal(ranked[2]?.latencyMs, null, 'the unreachable node must read as null, not a number');
});

test('rankNodesByLatency bounds a hanging probe by its timeout', async () => {
  const candidates = candidatesFromConfig(configDocument(['node-a', 'node-h']));
  const ranked = await rankNodesByLatency(candidates, {
    timeoutMs: 25,
    // node-h's probe never completes — a black-holed route, not a refusal.
    fetch: scriptedFetch([
      { match: 'node-a.example', respond: healthResponse },
      { match: 'node-h.example', respond: () => new Promise<Response>(() => {}) },
    ]),
  });

  assert.equal(ranked[0]?.node.id, 'node-a');
  assert.equal(ranked[1]?.node.id, 'node-h');
  assert.equal(ranked[1]?.latencyMs, null, 'a probe past its timeout must read as unreachable');
});

test('routingPlanFromConfig puts the fastest node first and the rest in the failover list', async () => {
  const plan = await routingPlanFromConfig(configDocument(['node-a', 'node-b', 'node-c']), {
    // node-b is the fastest this time: the plan follows the measurement, not the document order.
    fetch: scriptedFetch([
      { match: 'node-a.example', respond: delayed(40) },
      { match: 'node-b.example', respond: healthResponse },
      { match: 'node-c.example', respond: delayed(80) },
    ]),
  });

  assert.equal(plan.server.host, 'node-b.example');
  assert.deepEqual(
    plan.failoverServers.map((endpoint) => endpoint.host),
    ['node-a.example', 'node-c.example'],
  );
});

test('routingPlanFromConfig keeps the document order when no node answers the probe', async () => {
  // A flapping measurement must not strand the client with an empty plan: the document order —
  // this node first, then the operator's peers — stands, and the transport's own attempts will
  // report the outage honestly.
  const plan = await routingPlanFromConfig(configDocument(['node-a', 'node-b']), {
    fetch: () => Promise.reject(new TypeError('Failed to fetch')),
  });

  assert.equal(plan.server.host, 'node-a.example');
  assert.deepEqual(
    plan.failoverServers.map((endpoint) => endpoint.host),
    ['node-b.example'],
  );
});

test('candidatesFromConfig converts the node list in document order, this node first', () => {
  const candidates = candidatesFromConfig(configDocument(['node-a', 'node-b']));
  assert.equal(candidates.length, 2);
  assert.equal(candidates[0]?.node.id, 'node-a');
  assert.equal(candidates[0]?.endpoint.host, 'node-a.example');
  // An https public URL is the production posture: TLS on both sides, gateway on the same port.
  assert.equal(candidates[0]?.endpoint.port, 443);
  assert.equal(candidates[0]?.endpoint.gatewayPort, 443);
  assert.equal(candidates[0]?.endpoint.scheme, 'Wss');
  assert.equal(candidates[0]?.endpoint.restScheme, 'Https');
  assert.equal(candidates[1]?.node.id, 'node-b');
});

// --- the config document's node list ------------------------------------------------------------

/** A fetch answering one canned JSON document, as `/v1/config` would. */
function configFetch(document: Record<string, unknown>): FetchLike {
  return () => Promise.resolve(new Response(JSON.stringify(document), { status: 200 }));
}

test('the config document carries the node list, this node first then its peers', async () => {
  const client = new BootstrapClient(nodeEndpoint('node-a.example'), {
    fetch: configFetch({
      node: { id: 'node-a', region: 'eu', country: 'DE', public_url: 'https://node-a.example' },
      nodes: [
        { id: 'node-a', region: 'eu', country: 'DE', public_url: 'https://node-a.example' },
        { id: 'node-b', region: 'ap', country: 'SG', public_url: 'https://node-b.example' },
      ],
      features: 0,
      limits: {
        allow_registration: true,
        passphrase_min_length: 10,
        max_devices_per_user: 5,
        max_body_bytes: 1 << 20,
        max_page_size: 200,
      },
    }),
  });

  const config = await client.config();
  assert.equal(config.nodes.length, 2);
  assert.equal(config.nodes[0]?.id, 'node-a');
  assert.equal(config.nodes[1]?.id, 'node-b');
  assert.equal(config.nodes[1]?.publicUrl, 'https://node-b.example');
  // The single `node` field still describes this node alone.
  assert.equal(config.node.id, 'node-a');
});

test('an older server without a nodes field still yields this node as the whole list', async () => {
  const client = new BootstrapClient(nodeEndpoint('node-a.example'), {
    fetch: configFetch({
      node: { id: 'node-a', region: 'eu', country: 'DE', public_url: 'https://node-a.example' },
      features: 0,
      limits: {
        allow_registration: true,
        passphrase_min_length: 10,
        max_devices_per_user: 5,
        max_body_bytes: 1 << 20,
        max_page_size: 200,
      },
    }),
  });

  const config = await client.config();
  assert.deepEqual(
    config.nodes.map((node) => node.id),
    ['node-a'],
  );
});
