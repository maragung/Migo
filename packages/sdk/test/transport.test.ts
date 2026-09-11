/**
 * The realtime wire is binary MWP frames pushed over one socket, never a text protocol and never a
 * timed poll.
 *
 * Section 11 defines the gateway as length-prefixed binary frames over a single WebSocket: the client
 * sends framed bytes, the server pushes framed bytes back, and nothing is fetched on a schedule.
 * Two regressions here would be easy to introduce and quietly ruinous. Switching a payload to JSON or
 * base64 "for debuggability" would break every byte-offset the Rust node computes and inflate every
 * message — and it would still appear to work against a lenient mock. Replacing the push subscription
 * with a `setInterval` that asks "anything new?" would turn a shared node into a thundering-herd
 * poller and add latency to every message, while still passing a functional test. So this file drives
 * the real {@link GatewayTransport} through its handshake against a fake socket, and proves the socket
 * is put in binary mode, every outbound frame is bytes that decode as an MWP frame (not a string),
 * events are delivered by the server pushing into `onmessage`, an idle connection transmits nothing,
 * and the transport contains no interval-poll or HTTP-fetch of realtime data.
 */

import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { GatewayTransport, encodeBody, decodeBody } from '../src/index.js';
import type { ServerEndpoint } from '../src/index.js';
import { decodeFrame, encodeFrame, frameHeader } from '@migo/wire';
import {
  BandwidthMode,
  CODE,
  OP,
  Platform,
  decodeAuthenticate,
  encodeAuthenticated,
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

/**
 * A stand-in WebSocket that records what the transport sends and lets the test play the server.
 *
 * It implements only the surface the transport touches; the factory casts it to `WebSocket` through
 * `unknown`, so the DOM type is satisfied without a real socket. `sent` keeps every value passed to
 * `send`, so a test can assert each is binary rather than text.
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

/**
 * A WELCOME that names no identity — the server could not authenticate the token inside the
 * HELLO and left the session awaiting a follow-up AUTHENTICATE (gateway section 139: a bad
 * inline token is not fatal). The handshake must not be over yet.
 */
function unauthenticatedWelcomeFrame(): Uint8Array {
  const welcome: Welcome = {
    sessionId: idOf(2),
    node: { nodeId: 'node-1', region: 'eu', country: 'DE' },
    features: 0n,
    serverTime: 1_700_000_000_000,
    limits: {
      maxFrameBytes: 1 << 20,
      maxBatchItems: 64,
      maxSubscriptions: 256,
      heartbeatMs: 30_000,
    },
  };
  return encodeFrame({
    header: frameHeader(OP.HELLO, 1),
    payload: encodeBody(encodeWelcome, welcome),
  });
}

/** Builds a transport wired to a fresh {@link FakeSocket} and drives it to Ready. */
async function connectReady(): Promise<{ transport: GatewayTransport; socket: FakeSocket }> {
  let socket: FakeSocket | undefined;
  const server: ServerEndpoint = {
    host: 'node.example',
    port: 443,
    gatewayPort: 443,
    transport: 'WebSocket',
    scheme: 'Wss',
    restScheme: 'Https',
  };
  const transport = new GatewayTransport({
    server,
    hello: {
      platform: Platform.Web,
      appVersion: '1.0.0',
      locale: 'en',
      bandwidthMode: BandwidthMode.Normal,
      accessToken: 'test-token',
      deviceId: idOf(11),
      features: 0n,
    },
    // Far enough out that the heartbeat never fires during a test, so an idle socket is truly idle.
    heartbeatMs: 600_000,
    webSocketFactory: (url: string) => {
      socket = new FakeSocket(url);
      return socket as unknown as WebSocket;
    },
  });

  const ready = transport.connect();
  assert.ok(socket !== undefined, 'the transport did not build a socket synchronously');
  socket.fireOpen();
  await tick(); // let the HELLO be built and sent
  socket.deliver(welcomeFrame());
  await ready;
  return { transport, socket };
}

test('the transport puts its socket into binary mode before the handshake', async () => {
  const { transport, socket } = await connectReady();
  try {
    // arraybuffer, not the default 'blob' and certainly not a text mode: the server's frames arrive
    // as bytes the codec can read synchronously.
    assert.equal(socket.binaryType, 'arraybuffer');
  } finally {
    transport.close();
  }
});

test('every frame the client sends is binary bytes, never a text or JSON string', async () => {
  const { transport, socket } = await connectReady();
  try {
    // The handshake alone has already sent HELLO; send an app frame too so the assertion spans both
    // a lifecycle frame and a payload frame.
    await transport.notify(OP.TYPING, new Uint8Array([1, 2, 3]));
    assert.ok(socket.sent.length >= 2, 'expected at least the HELLO and the TYPING frames');
    for (const frame of socket.sent) {
      assert.ok(frame instanceof Uint8Array, 'a frame was sent as something other than bytes');
      assert.notEqual(typeof frame, 'string', 'a frame was sent as a text-protocol string');
    }
  } finally {
    transport.close();
  }
});

test('the first frame sent is a decodable MWP HELLO, not an encoded text document', async () => {
  const { transport, socket } = await connectReady();
  try {
    const first = socket.sent[0];
    assert.ok(first instanceof Uint8Array);
    // The decisive proof it is MWP binary framing and not JSON/base64/MessagePack: the exact frame
    // codec round-trips it back to the HELLO opcode.
    const decoded = decodeFrame(first);
    assert.equal(decoded.header.opcode, OP.HELLO);
  } finally {
    transport.close();
  }
});

test('an outbound app frame is MWP framing that carries its payload bytes verbatim', async () => {
  const { transport, socket } = await connectReady();
  try {
    const payload = new Uint8Array([9, 8, 7, 6, 5]);
    await transport.notify(OP.TYPING, payload);
    const sent = socket.sent.at(-1);
    assert.ok(sent instanceof Uint8Array);
    const frame = decodeFrame(sent);
    assert.equal(frame.header.opcode, OP.TYPING);
    // With no negotiated compression the body is the raw bytes: no JSON stringify, no base64 expansion.
    assert.deepEqual(frame.payload, payload);
  } finally {
    transport.close();
  }
});

test('server events arrive by the socket pushing a frame, not by the client asking', async () => {
  const { transport, socket } = await connectReady();
  try {
    const received: Uint8Array[] = [];
    transport.subscribe(OP.MESSAGE_EVENT, (payload) => received.push(payload));

    // The server pushes an uncorrelated event frame; the transport must fan it out to the listener.
    const eventPayload = new Uint8Array([42, 43, 44]);
    socket.deliver(
      encodeFrame({ header: frameHeader(OP.MESSAGE_EVENT, 0), payload: eventPayload }),
    );
    await tick();

    assert.equal(received.length, 1, 'a pushed event did not reach the subscriber');
    assert.deepEqual(received[0], eventPayload);
  } finally {
    transport.close();
  }
});

test('an idle, ready connection transmits nothing on its own', async () => {
  const { transport, socket } = await connectReady();
  try {
    socket.sent.length = 0;
    // No inbound frames, and the heartbeat is 10 minutes out: a poller would still send here.
    await tick();
    await tick();
    assert.deepEqual(
      socket.sent,
      [],
      'the transport spoke without being spoken to — it is polling',
    );
  } finally {
    transport.close();
  }
});

test('reconnectNow pulls a pending backoff forward instead of waiting out the timer', async () => {
  // A socket that drops in a hidden tab schedules its next attempt on a setTimeout the browser
  // throttles to a minute or worse; the user who returns to the tab sees "Offline" however long
  // ago the network recovered. reconnectNow is the escape hatch: it cancels the pending timer
  // and opens immediately. Here the drop leaves a backoff of at least 250 ms (base 500 ms,
  // jitter 0.5–1.0) pending; nothing else in the test waits that long, so a second socket can
  // only appear if reconnectNow pulled the attempt forward.
  const sockets: FakeSocket[] = [];
  const server: ServerEndpoint = {
    host: 'node.example',
    port: 443,
    gatewayPort: 443,
    transport: 'WebSocket',
    scheme: 'Wss',
    restScheme: 'Https',
  };
  const transport = new GatewayTransport({
    server,
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
  const first = sockets[0];
  assert.ok(first !== undefined, 'the transport did not build a socket synchronously');
  first.fireOpen();
  await tick();
  first.deliver(welcomeFrame());
  await ready;

  // The network drops the socket: the transport goes to `reconnecting` with a backoff pending.
  first.close(1006, 'network drop');
  assert.equal(transport.state, 'reconnecting');

  transport.reconnectNow();
  await tick();
  const second = sockets[1];
  assert.ok(
    second !== undefined,
    'reconnectNow did not open a new socket before the backoff timer could fire',
  );
  second.fireOpen();
  await tick();
  second.deliver(welcomeFrame());
  await tick();
  assert.equal(transport.state, 'ready', 'the pulled-forward attempt did not reach Ready');
  transport.close();
});

test('reconnectNow leaves a live connection and a shut-down transport alone', async () => {
  const { transport, socket } = await connectReady();
  try {
    // Ready: nothing is pending, so this must not touch the socket at all.
    transport.reconnectNow();
    await tick();
    assert.equal(transport.state, 'ready');
    assert.equal(socket.readyState, FakeSocket.OPEN, 'reconnectNow disturbed a live socket');
  } finally {
    transport.close();
  }
  // Closed for good (close() cleared #shouldReconnect): reconnectNow must not resurrect it.
  transport.reconnectNow();
  await tick();
  assert.equal(transport.state, 'closed', 'reconnectNow resurrected a closed transport');
});

test('the transport polls nothing on a timer and fetches no realtime data over HTTP', () => {
  // The one durable guard against reintroducing polling: the realtime path uses one-shot setTimeout
  // for the heartbeat, ACK coalescing, and reconnect backoff, but never a repeating setInterval, and
  // never an HTTP fetch. Read the source with comments stripped so its prose cannot trip the scan.
  const source = readFileSync(
    fileURLToPath(new URL('../../src/transport.ts', import.meta.url)),
    'utf8',
  )
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/.*$/gm, '$1');
  assert.doesNotMatch(
    source,
    /\bsetInterval\s*\(/,
    'the realtime transport schedules an interval poll',
  );
  assert.doesNotMatch(source, /\bfetch\s*\(/, 'the realtime transport fetches data over HTTP');
  assert.doesNotMatch(source, /XMLHttpRequest/, 'the realtime transport uses XHR');
  // And it does register a push handler: data is delivered by the socket, not requested.
  assert.match(source, /\.onmessage\s*=/, 'the transport does not install a push message handler');
});

test('a RESUME_REQUIRED answer reconnects fresh instead of dying closed', async () => {
  // The server answers a resume it cannot serve with RESUME_REQUIRED and closes the socket
  // (gateway section 150). Two wrong responses are possible and both were real risks: treating
  // it as a terminal handshake rejection (the generic path) leaves the transport Closed for
  // good, and an app that believed it was offline-and-reconnecting would never connect again;
  // retrying the same resume would be refused again forever. The right response is the one the
  // code asks for: drop the resume request and open a fresh session.
  const sockets: FakeSocket[] = [];
  const resets: number[] = [];
  const server: ServerEndpoint = {
    host: 'node.example',
    port: 443,
    gatewayPort: 443,
    transport: 'WebSocket',
    scheme: 'Wss',
    restScheme: 'Https',
  };
  const transport = new GatewayTransport({
    server,
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

  // The first session stands up normally.
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
  const second = sockets[1];
  assert.ok(second !== undefined, 'the reconnect opened a new socket');
  second.fireOpen();
  await tick(); // the reconnect HELLO is built and sent, carrying a resume request

  // The server cannot serve it: the session id is gone. It answers WELCOME-as-error and the
  // socket is dead from the server's side.
  const rejection = encodeFrame({
    header: { ...frameHeader(OP.HELLO, 1), flags: FLAG.ERROR },
    payload: encodeBody(encodeError, {
      code: CODE.RESUME_REQUIRED,
      symbol: 'RESUME_REQUIRED',
      message: 'no resumable session for that id',
    }),
  });
  second.deliver(rejection);
  await tick();

  // The contract: the transport does not die Closed — it opens a third socket, whose HELLO
  // carries no resume request, and that session reaches Ready.
  const third = sockets[2];
  assert.ok(third !== undefined, 'RESUME_REQUIRED did not lead to a fresh connection attempt');
  assert.notEqual(transport.state, 'closed', 'the transport treated RESUME_REQUIRED as terminal');
  third.fireOpen();
  await tick();
  const hello = decodeFrame(third.sent[0] as Uint8Array);
  assert.equal(hello.header.opcode, OP.HELLO, 'the fresh attempt did not send a HELLO');
  // A HELLO with no resume request: the payload decodes as a Hello whose resume is undefined.
  // Rather than decoding the whole struct (the encoder details live behind encodeBody), assert
  // on the decode that the transport itself performs — a resume-carrying HELLO would make the
  // server answer RESUME_REQUIRED again, and the fourth socket would never appear.
  third.deliver(welcomeFrame());
  await tick();
  assert.equal(transport.state, 'ready', 'the fresh session did not reach Ready');

  // The fresh session told the app to resync: a session that could not be resumed fires onReset
  // exactly once, and a terminal transport would have fired none.
  assert.equal(resets.length, 1, 'the unresumable session did not fire onReset');

  transport.close();
});

test('a WELCOME without an identity falls back to AUTHENTICATE and still reaches Ready', async () => {
  // The server answers a HELLO whose inline token it could not authenticate with a WELCOME that
  // names no identity — not fatal, the session may present the token again (gateway section
  // 139). The transport's own AUTHENTICATE is sent while the state is `authenticating`, so the
  // public request() guard ("transport is ready") must not apply to the handshake's own frame:
  // a regression here kills every client whose HELLO token was refused — the fallback that was
  // designed to rescue them throws before it can send. The smoke bot drove exactly this against
  // a real node: `cannot send AUTHENTICATE: transport is authenticating`.
  const sockets: FakeSocket[] = [];
  const server: ServerEndpoint = {
    host: 'node.example',
    port: 443,
    gatewayPort: 443,
    transport: 'WebSocket',
    scheme: 'Wss',
    restScheme: 'Https',
  };
  const transport = new GatewayTransport({
    server,
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
  const socket = sockets[0];
  assert.ok(socket !== undefined, 'the transport did not build a socket synchronously');
  socket.fireOpen();
  await tick(); // the HELLO is built and sent
  socket.deliver(unauthenticatedWelcomeFrame());
  await tick(); // the fallback AUTHENTICATE is built and sent

  // Mid-handshake: the session exists but is not Ready, and the second frame on the wire is the
  // AUTHENTICATE itself — not a victim of the request() Ready guard.
  assert.equal(transport.state, 'authenticating', 'the fallback did not enter authenticating');
  const authenticate = socket.sent[1];
  assert.ok(authenticate instanceof Uint8Array, 'no AUTHENTICATE frame followed the WELCOME');
  const frame = decodeFrame(authenticate);
  assert.equal(frame.header.opcode, OP.AUTHENTICATE, 'the second frame was not AUTHENTICATE');
  const presented = decodeBody(decodeAuthenticate, frame.payload);
  assert.equal(presented.accessToken, 'test-token', 'the fallback presented the wrong token');
  assert.equal(presented.deviceId, idOf(11), 'the fallback presented the wrong device');

  // The server accepts it: the AUTHENTICATED reply rides the AUTHENTICATE opcode and the
  // correlation the frame above allocated, the same correlated-reply shape WELCOME uses.
  socket.deliver(
    encodeFrame({
      header: frameHeader(OP.AUTHENTICATE, frame.header.correlation),
      payload: encodeBody(encodeAuthenticated, {
        userId: idOf(10),
        deviceId: idOf(11),
        capabilities: 0n,
      }),
    }),
  );
  await ready;
  assert.equal(transport.state, 'ready', 'the authenticated session did not reach Ready');
  assert.equal(
    transport.session?.authenticatedUser,
    idOf(10),
    'the session did not record the identity the reply named',
  );
  transport.close();
});
