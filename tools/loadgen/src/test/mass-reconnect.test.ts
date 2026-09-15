/**
 * §172's mass-reconnect stress bullet, at the scale CI can finish deterministically.
 *
 * The brief asks for ten thousand clients reconnecting at once and expects full jitter to
 * spread the load so the node does not fall. Ten thousand real sockets is a full-scale load
 * scenario that stays SPEC; what this suite proves deterministically is the property the brief
 * is actually about, on the machinery that would carry it: a few hundred in-process virtual
 * connections, each a real {@link GatewayTransport} over a fake socket (the same harness the
 * byte-accounting suite drives), all established, all dropped at once, and then examined on the
 * one number that decides whether a returning wave is a spread or a stampede — the delay each
 * client drew for its first reconnect attempt.
 *
 * Determinism comes from seeding: the transports draw their jitter from `Math.random`, so the
 * test swaps in a seeded SplitMix64 stream (reading `SIM_SEED`, the same variable the Rust fuzz
 * suites and the SDK's bad-network suite read, defaulting to 1234) for the duration of the
 * synchronous drop, and records both the draws and the delays the transports handed to their
 * backoff timers. Every assertion after that is arithmetic on recorded numbers, not on
 * wall-clock behaviour: each client drew exactly the full-jitter transform of its random value,
 * the drawn delays cover the whole first-attempt window rather than piling into one instant,
 * no slice of the window holds a herd, and when the timers fire, every one of the hundreds of
 * simultaneous handshakes completes into a resumed session on a node that never refused one.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { GatewayTransport, encodeBody, protocol, wire } from '@migo/sdk';
import type { ServerEndpoint } from '@migo/sdk';

const { OP, Platform, BandwidthMode, encodeWelcome } = protocol;
const { encodeFrame, frameHeader, idFromBytes } = wire;
type Welcome = protocol.Welcome;

// ---------------------------------------------------------------------------
// seeded randomness — the same variable the Rust fuzz suites read
// ---------------------------------------------------------------------------

/** Reads the seed from `SIM_SEED`, defaulting to 1234, as the wire fuzz suites do. */
function simSeed(): number {
  const parsed = Number.parseInt(process.env.SIM_SEED ?? '', 10);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : 1234;
}

const MASK64 = (1n << 64n) - 1n;

/** SplitMix64 as a [0, 1) double stream: equal seeds produce equal streams on every run. */
class SeededRandom {
  #x: bigint;

  constructor(seed: number) {
    this.#x = BigInt(seed) & MASK64;
  }

  next(): number {
    this.#x = (this.#x + 0x9e37_79b9_7f4a_7c15n) & MASK64;
    let z = this.#x;
    z = ((z ^ (z >> 30n)) * 0xbf58_476d_1ce4_e5b9n) & MASK64;
    z = ((z ^ (z >> 27n)) * 0x94d0_49bb_1331_11ebn) & MASK64;
    z = z ^ (z >> 31n);
    // The top 53 bits, so every double in the stream is exact.
    return Number(z >> 11n) / 9007199254740992;
  }
}

// ---------------------------------------------------------------------------
// the harness: fake sockets, a fake node, real transports
// ---------------------------------------------------------------------------

/** Lets pending microtasks and the transport's async frame builds settle. */
function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** Polls until `condition` holds, bounded by `budgetMs` of real time. */
async function waitFor(what: string, condition: () => boolean, budgetMs: number): Promise<void> {
  for (let waited = 0; waited < budgetMs; waited += 10) {
    if (condition()) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.ok(condition(), `timed out waiting for ${what}`);
}

/**
 * A stand-in WebSocket that records what the transport sends, as the byte-accounting suite
 * drives the transport with. Only the surface the transport touches.
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
    if (this.#closed) return;
    this.#closed = true;
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.({ code, reason });
  }

  fireOpen(): void {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }

  deliver(bytes: Uint8Array): void {
    this.onmessage?.({ data: bytes });
  }
}

const SERVER: ServerEndpoint = {
  host: 'node.example',
  port: 443,
  gatewayPort: 443,
  transport: 'WebSocket',
  scheme: 'Wss',
  restScheme: 'Https',
};

/** The WELCOME for connection `n`: a session of its own, authenticated inline. */
function welcomeFrame(n: number, resumed: boolean): Uint8Array {
  const welcome: Welcome = {
    sessionId: idFromBytes(idBytes(n)),
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
    ...(resumed ? { resumed: true } : {}),
    authenticatedUser: idFromBytes(new Uint8Array(16).fill(10)),
  };
  return encodeFrame({
    header: frameHeader(OP.HELLO, 1),
    payload: encodeBody(encodeWelcome, welcome),
  });
}

/** Distinct 16-byte ids, one per virtual connection. */
function idBytes(n: number): Uint8Array {
  const bytes = new Uint8Array(16);
  bytes[0] = 0x10;
  bytes[15] = n & 0xff;
  bytes[14] = (n >>> 8) & 0xff;
  return bytes;
}

/** How many virtual connections the mass-reconnect scenario runs. */
const CONNECTIONS = 256;

interface Herd {
  sockets: FakeSocket[];
  transports: GatewayTransport[];
  /** A holder, not a bare number: the onResumed closures increment it after `herd` returns. */
  resumptions: { count: number };
}

/** Builds `count` real transports over fake sockets against the one fake node. */
function herd(count: number): Herd {
  const sockets: FakeSocket[] = [];
  const transports: GatewayTransport[] = [];
  const resumptions = { count: 0 };
  for (let n = 0; n < count; n++) {
    transports.push(
      new GatewayTransport({
        server: SERVER,
        hello: {
          platform: Platform.Web,
          appVersion: '1.0.0',
          locale: 'en',
          bandwidthMode: BandwidthMode.Normal,
          accessToken: 'test-token',
          deviceId: idFromBytes(idBytes(0x90_00 + n)),
          features: 0n,
        },
        // Far enough out that no heartbeat fires during the scenario.
        heartbeatMs: 600_000,
        onResumed: () => {
          resumptions.count += 1;
        },
        webSocketFactory: (url: string) => {
          const socket = new FakeSocket(url);
          sockets.push(socket);
          return socket as unknown as WebSocket;
        },
      }),
    );
  }
  return { sockets, transports, resumptions };
}

test('a mass reconnect spreads its first attempts across the backoff window and every handshake lands', async () => {
  const count = CONNECTIONS;
  const { sockets, transports, resumptions } = herd(count);
  try {
    // Every connection is established first: one node, hundreds of live sessions.
    const readies = transports.map((transport) => transport.connect());
    assert.equal(sockets.length, count, 'a transport opened more than one socket to connect');
    for (const socket of sockets) {
      socket.fireOpen();
    }
    await tick(); // every HELLO is built and sent
    for (const [n, socket] of sockets.entries()) {
      socket.deliver(welcomeFrame(n, false));
    }
    await Promise.all(readies);
    assert.ok(
      transports.every((transport) => transport.state === 'ready'),
      'a virtual connection failed to establish',
    );

    // --- the node falls: every socket dies in the same synchronous burst --------------------
    //
    // The transports draw their jitter from Math.random, so the seeded stream stands in for the
    // duration of the burst, and the timer recorder captures the exact delay each transport
    // handed to its backoff. Both swaps are installed around one synchronous loop and removed
    // before anything asynchronous can run, so the recorded pairs are exactly the transports'
    // own draws, in transport order.
    const rng = new SeededRandom(simSeed());
    const draws: number[] = [];
    const delays: number[] = [];
    const realRandom = Math.random;
    const realSetTimeout = globalThis.setTimeout;
    Math.random = (): number => {
      const value = rng.next();
      draws.push(value);
      return value;
    };
    // No DOM lib here, so no TimerHandler: a plain () => void stands in for the handlers the
    // transport schedules, and the cast restores the global's real, rest-argument signature.
    globalThis.setTimeout = ((handler: () => void, ms?: number) => {
      delays.push(ms ?? 0);
      return realSetTimeout(handler, ms);
    }) as typeof globalThis.setTimeout;
    try {
      for (const socket of sockets) {
        socket.close(1006, 'the node fell');
      }
    } finally {
      Math.random = realRandom;
      globalThis.setTimeout = realSetTimeout;
    }

    // One attempt per client — a client that scheduled two backoffs in one drop is the storm.
    assert.equal(
      delays.length,
      count,
      'a transport scheduled more than one backoff after the fall',
    );
    assert.equal(draws.length, count, 'a transport drew jitter more than once after the fall');

    // The SDK's first-attempt window: base 500 ms, full jitter over the upper half, so every
    // delay is 500 * (0.5 + draw * 0.5) — exactly the transform of the transport's own draw.
    for (const [n, draw] of draws.entries()) {
      // A missing delay reads as NaN, which fails both bounds below and names the transport.
      const delay = delays[n] ?? Number.NaN;
      const want = 500 * (0.5 + draw * 0.5);
      assert.ok(delay >= 250 && delay < 500, `delay ${delay} left the first-attempt window`);
      assert.ok(
        Math.abs(delay - want) < 1e-9,
        `delay ${delay} is not the full-jitter transform of draw ${draw}`,
      );
    }

    // The spread: the window divides into ten 25 ms slices, and every slice carries attempts —
    // a synchronized herd (every client on the same delay) would leave nine slices empty. With
    // a seeded uniform spread the expected slice holds ~26 of 256, so the bounds below fail
    // only a genuinely lumpy draw, and the seed makes any failure reproducible.
    const slices = new Array<number>(10).fill(0);
    for (const delay of delays) {
      const slot = Math.min(Math.floor((delay - 250) / 25), 9);
      slices[slot] = (slices[slot] ?? 0) + 1;
    }
    for (const [index, held] of slices.entries()) {
      assert.ok(held >= 8, `backoff slice ${index} held only ${held} of ${count} attempts`);
      assert.ok(held <= 64, `backoff slice ${index} held ${held} of ${count} attempts`);
    }
    // Nearly every delay is distinct: full jitter drew each client its own moment.
    assert.ok(
      new Set(delays).size >= count - 8,
      'the first attempts collapsed onto a handful of delays',
    );

    // No herd anywhere in the window: the busiest 10 ms slice holds a sliver of the clients,
    // where a synchronized retry would put all of them in one.
    const sorted = [...delays].sort((a, b) => a - b);
    let busiest = 1;
    let start = 0;
    for (let end = 0; end < sorted.length; end++) {
      const hi = sorted[end];
      assert.ok(hi !== undefined, 'a recorded first-attempt delay went missing');
      let lo = sorted[start];
      while (lo !== undefined && hi - lo > 10) {
        start += 1;
        lo = sorted[start];
      }
      busiest = Math.max(busiest, end - start + 1);
    }
    assert.ok(
      busiest <= 48,
      `a 10 ms slice of the window held ${busiest} of ${count} first attempts`,
    );

    // --- the node comes back: every timer fires, every handshake lands ---------------------
    //
    // The timers were left running with their recorded delays; each fires within the window and
    // its transport attempts exactly once more. A node that fell would refuse some of these;
    // this one answers all of them, and every session resumes.
    await waitFor('every first reconnect attempt', () => sockets.length === 2 * count, 3000);
    assert.equal(
      sockets.length,
      2 * count,
      'a transport attempted more than once while its first backoff was still spreading',
    );
    const attempts = sockets.slice(count);
    for (const socket of attempts) {
      socket.fireOpen();
    }
    await tick(); // every resume HELLO is built and sent
    for (const [n, socket] of attempts.entries()) {
      socket.deliver(welcomeFrame(n, true));
    }
    await waitFor(
      'every session to resume',
      () => transports.every((transport) => transport.state === 'ready'),
      3000,
    );
    assert.ok(
      transports.every((transport) => transport.state === 'ready'),
      'a virtual connection failed to reconnect after the fall',
    );
    assert.equal(resumptions.count, count, 'not every connection resumed its session');
  } finally {
    for (const transport of transports) {
      transport.close();
    }
  }
});
