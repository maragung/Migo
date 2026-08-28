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

import { GatewayTransport, encodeBody } from '../src/index.js';
import type { ServerEndpoint } from '../src/index.js';
import { decodeFrame, encodeFrame, frameHeader } from '@migo/wire';
import { BandwidthMode, OP, Platform, encodeWelcome } from '@migo/protocol';
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
