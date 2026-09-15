/**
 * §172's bad-network stress slice, as deterministic tests: the lossy half-second path and the
 * repeated network switch.
 *
 * The brief's Stress list asks two things of the client that only show up when the network is
 * hostile: a path with 20 percent packet loss and 500 ms RTT must still resume without
 * duplicating or losing a message, and repeated network switches must reconnect immediately
 * without a reconnect storm. Both are stated as behaviours of the transport over MWP, so both are
 * proven here by driving the real {@link GatewayTransport} — the same code a browser, desktop, or
 * Android client runs — through a link that is deliberately terrible in a seeded, reproducible
 * way (ADR-0009: transport injected, randomness seeded, time virtual).
 *
 * The link is a fake socket pair with a virtual clock. Every frame handed to the wire is queued
 * for a crossing due half an RTT away; the test advances the clock in steps, and each step
 * settles after one macrotask so the transport's own async frame builds run between deliveries.
 * Loss is drawn from a seeded PRNG at send time — the same `SIM_SEED` variable the Rust fuzz
 * suites read, defaulting to 1234 — and a lost frame kills the socket when it would have
 * arrived, taking every frame still in flight behind it, which is the only honest model of loss
 * over an ordered WebSocket: the client cannot know a frame is missing, it can only notice the
 * connection is gone and resume. The node on the far side mirrors the gateway's documented
 * resume contract (§150): Critical frames are retained in a ring, and a resume replays the
 * retained frames past the client's watermark, original bytes intact.
 *
 * The transport's own timers are virtual here too (see {@link TransportTimers}): its ACK
 * coalescing and reconnect backoff otherwise ride real timers whose firing moments race the
 * harness's tick-driven clock, so which frames the seeded PRNG judges lost — and whether a
 * trailing ACK kills the socket after the last message has landed — depended on runner load.
 * That is exactly how this test flaked on CI while its tree was green twice around it: a real
 * 5 ms coalescing timer fired a beat later under load, the draw sequence shifted, and the final
 * state assert met a backoff only real time could fire.
 *
 * Exactly-once is asserted where the brief states it: every message event the node ever
 * sequenced reaches the transport's event listener precisely once, in order, and the node ends
 * up knowing the client's full position — the last resume watermark or the last cumulative ACK
 * covers every frame. A transport whose `frame_seq` accounting drifted by one would ask for the
 * wrong replay point and fail this immediately.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { GatewayTransport, encodeBody, decodeBody } from '../src/index.js';
import type { ServerEndpoint } from '../src/index.js';
import { decodeFrame, encodeFrame, frameHeader, idFromBytes } from '@migo/wire';
import type { Id } from '@migo/wire';
import {
  BandwidthMode,
  FLAG,
  MessageKind,
  OP,
  Platform,
  decodeAck,
  decodeHello,
  decodeMessageEvent,
  encodeMessageEvent,
  encodeWelcome,
} from '@migo/protocol';
import type { Hello, Welcome } from '@migo/protocol';

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
// small time helpers
// ---------------------------------------------------------------------------

/**
 * The platform's real timers, captured at load so the harness keeps settling on real
 * macrotasks even while a test has swapped `globalThis.setTimeout` for a virtual clock.
 */
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;

/** Lets pending microtasks and the transport's async frame builds settle. */
function tick(): Promise<void> {
  return new Promise((resolve) => realSetTimeout(resolve, 0));
}

/** Waits in real milliseconds — only for waits whose length is never asserted on. */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => realSetTimeout(resolve, ms));
}

/** Polls until `condition` holds, bounded by `budgetMs` of real time. */
async function waitFor(what: string, condition: () => boolean, budgetMs: number): Promise<void> {
  for (let waited = 0; waited < budgetMs; waited += 10) {
    if (condition()) {
      return;
    }
    await sleep(10);
  }
  assert.ok(condition(), `timed out waiting for ${what}`);
}

// ---------------------------------------------------------------------------
// the transport's virtual timers
// ---------------------------------------------------------------------------

/**
 * A virtual timer queue the transport's own timers run on while it is installed: `setTimeout`
 * and `clearTimeout` land here instead of the platform's, and the timers fire only when the
 * harness advances the clock, which every `elapse` does in step with the link. Without this the
 * transport's ACK coalescing (5 ms) and reconnect backoff (250–500 ms) rode real timers whose
 * firing moments raced the tick-driven link, so the seeded PRNG was applied to a frame sequence
 * that shifted with runner load — a "deterministic" test that flaked on CI (run 34938135872).
 *
 * One invariant keeps the run independent of the backoff's unseeded jitter: no armed backoff
 * ever survives enough clock to fire. Every elapse in this suite advances the transport clock
 * by at most 120 ms, and every kill is followed by a `driveReady` (whose `reconnectNow` clears
 * the pending timer) before the next elapse runs, while the smallest backoff window is 250 ms —
 * so the backoff's *value* can never decide anything; only its being cleared does.
 */
class TransportTimers {
  #now = 0;
  #nextId = 1;
  readonly #tasks = new Map<number, { at: number; handler: TimerHandler; args: unknown[] }>();

  /** The `setTimeout` the transport sees while this clock is installed. */
  readonly setTimeout = ((handler: TimerHandler, ms?: number, ...args: unknown[]) => {
    const id = this.#nextId++;
    this.#tasks.set(id, {
      at: this.#now + Math.max(0, Math.trunc(Number(ms ?? 0))),
      handler,
      args,
    });
    return id as unknown as ReturnType<typeof setTimeout>;
  }) as unknown as typeof globalThis.setTimeout;

  /** The `clearTimeout` the transport sees while this clock is installed. */
  readonly clearTimeout = ((handle: unknown) => {
    this.#tasks.delete(handle as number);
  }) as unknown as typeof globalThis.clearTimeout;

  /**
   * Fires every task due within `ms` of the clock, earliest first, one real macrotask apart so
   * the transport's async work settles between callbacks exactly as it would between real
   * timers. Tasks scheduled while advancing (a re-armed coalescing window) fire in the same
   * step when they come due within it.
   */
  async advance(ms: number): Promise<void> {
    const end = this.#now + ms;
    for (;;) {
      let dueId: number | undefined;
      let dueAt = Number.POSITIVE_INFINITY;
      for (const [id, task] of this.#tasks) {
        if (task.at <= end && task.at < dueAt) {
          dueId = id;
          dueAt = task.at;
        }
      }
      if (dueId === undefined) {
        break;
      }
      const task = this.#tasks.get(dueId) as { at: number; handler: TimerHandler; args: unknown[] };
      this.#tasks.delete(dueId);
      this.#now = task.at;
      if (typeof task.handler === 'function') {
        (task.handler as (...args: unknown[]) => void)(...task.args);
      }
      await tick();
    }
    this.#now = end;
  }
}

/**
 * Runs `body` with the transport's timers on a virtual clock the harness advances in `elapse`.
 *
 * The harness's own settling (`tick`, `sleep`) keeps using the real timers captured at load, so
 * only what the transport schedules becomes virtual — and everything is restored before the
 * next test, whose backoff assertions read the real timer delays by design.
 */
async function withVirtualTransportTimers(
  body: (clock: TransportTimers) => Promise<void>,
): Promise<void> {
  const clock = new TransportTimers();
  globalThis.setTimeout = clock.setTimeout;
  globalThis.clearTimeout = clock.clearTimeout;
  try {
    await body(clock);
  } finally {
    globalThis.setTimeout = realSetTimeout;
    globalThis.clearTimeout = realClearTimeout;
  }
}

// ---------------------------------------------------------------------------
// the node behind the lossy link
// ---------------------------------------------------------------------------

/** One in-flight crossing of the link: bytes or a kill, due at a virtual timestamp. */
interface Crossing {
  dueAt: number;
  /** Insertion order, so crossings due together leave in the order they were sent. */
  order: number;
  socket: LinkedSocket;
  direction: 'client-to-server' | 'server-to-client';
  kind: 'bytes' | 'kill';
  bytes?: Uint8Array;
}

/**
 * A stand-in WebSocket whose wire runs through the {@link LossyNode}: `send` queues the bytes for
 * a virtual-latency crossing instead of arriving instantly, and the node can push bytes back or
 * kill the connection. Only the surface the transport touches.
 */
class LinkedSocket {
  static readonly OPEN = 1;
  static readonly CLOSED = 3;

  binaryType = 'blob';
  readyState = 0;
  /** True once the link has completed the connection attempt. */
  opened = false;
  /** True once the link (or the transport) has closed this connection for good. */
  dead = false;
  readonly url: string;
  readonly #node: LossyNode;
  #closed = false;

  constructor(node: LossyNode, url: string) {
    this.#node = node;
    this.url = url;
  }

  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;

  send(data: unknown): void {
    this.#node.clientSent(this, data as Uint8Array);
  }

  close(code = 1000, reason = ''): void {
    if (this.#closed) {
      return;
    }
    this.#closed = true;
    this.dead = true;
    this.readyState = LinkedSocket.CLOSED;
    this.#node.socketClosed(this);
    this.onclose?.({ code, reason });
  }

  /** The network completes the connection attempt. */
  fireOpen(): void {
    this.opened = true;
    this.readyState = LinkedSocket.OPEN;
    this.onopen?.();
  }

  /** The node delivers server bytes that survived the crossing. */
  deliver(bytes: Uint8Array): void {
    this.onmessage?.({ data: bytes });
  }
}

/** Options for the fake node and its link. */
interface LossyNodeOptions {
  /** The share of frames lost in each direction; 0 is a clean link. */
  lossRate: number;
  /** One-way latency in virtual milliseconds; the RTT the client experiences is twice this. */
  oneWayLatencyMs: number;
}

/**
 * The far end: a node that speaks enough MWP to stand a session up, keep a resume ring, and
 * replay it on resume — the contract §150 states for the gateway — behind a link that loses and
 * delays.
 *
 * The node retains every sequenced frame it ever sent (its ring is sized for the test), answers a
 * HELLO without a resume with a fresh authenticated WELCOME, and answers a HELLO with a resume by
 * replaying the retained frames past the client's watermark, original bytes intact, exactly as
 * the gateway's `seed_resume` re-queues the ring's tail.
 */
class LossyNode {
  readonly #lossRate: number;
  readonly #latencyMs: number;
  readonly #rng: SeededRandom;
  /** The transport's virtual timers, advanced in lockstep with the link; absent for a real-time rig. */
  readonly #timers: TransportTimers | undefined;
  #crossings: Crossing[] = [];
  #order = 0;

  /** Virtual clock, in milliseconds; advanced only by {@link elapse} and {@link drain}. */
  #now = 0;

  readonly sessionId: Id;
  readonly sockets: LinkedSocket[] = [];

  /** The socket the transport currently speaks through, or undefined while between attempts. */
  #current: LinkedSocket | undefined;

  /** The sequenced frames ever sent, in order: the resume ring (never trimmed here). */
  readonly #retained: Uint8Array[] = [];

  /** Kills of established connections: the number of times loss struck an open socket. */
  kills = 0;
  /** Resume requests served with a resumed WELCOME. */
  resumes = 0;
  /** Fresh (non-resume) handshakes served. */
  freshHandshakes = 0;
  /** The highest cumulative ACK watermark received. */
  ackWatermark = 0;
  /** The highest watermark any resume request has carried. */
  lastResumeSeq = 0;

  constructor(options: LossyNodeOptions, seed: number, timers?: TransportTimers) {
    this.#lossRate = options.lossRate;
    this.#latencyMs = options.oneWayLatencyMs;
    this.#rng = new SeededRandom(seed);
    this.#timers = timers;
    this.sessionId = idFromBytes(new Uint8Array(16).fill(7));
  }

  /** The most recent socket the transport opened, whatever its state. */
  latest(): LinkedSocket | undefined {
    return this.sockets.at(-1);
  }

  /** How many sockets the transport has opened in total. */
  get socketCount(): number {
    return this.sockets.length;
  }

  /** The socket messages are pushed on; only meaningful while the transport is Ready. */
  get liveSocket(): LinkedSocket | undefined {
    return this.#current;
  }

  /** The WebSocket factory the transport is built with. */
  readonly webSocketFactory = (url: string): WebSocket => {
    const socket = new LinkedSocket(this, url);
    this.sockets.push(socket);
    this.#current = socket;
    return socket as unknown as WebSocket;
  };

  /** Called by a socket that closed, from either end of the wire. */
  socketClosed(socket: LinkedSocket): void {
    if (this.#current === socket) {
      this.#current = undefined;
    }
    // A closed connection takes everything still in flight on it with it.
    this.#crossings = this.#crossings.filter((crossing) => crossing.socket !== socket);
  }

  /** Queues a client-to-server crossing, drawn against loss at send time. */
  clientSent(socket: LinkedSocket, bytes: Uint8Array): void {
    this.#queue(socket, 'client-to-server', bytes);
  }

  /** Queues a server-to-client crossing. */
  serverSend(socket: LinkedSocket, bytes: Uint8Array): void {
    this.#queue(socket, 'server-to-client', bytes);
  }

  #queue(socket: LinkedSocket, direction: Crossing['direction'], bytes: Uint8Array): void {
    if (socket.dead) {
      return;
    }
    const lost = this.#rng.next() < this.#lossRate;
    this.#crossings.push({
      dueAt: this.#now + this.#latencyMs,
      order: this.#order++,
      socket,
      direction,
      kind: lost ? 'kill' : 'bytes',
      ...(lost ? {} : { bytes }),
    });
  }

  /** Advances virtual time by `ms`, firing every crossing that comes due, in order. */
  async elapse(ms: number): Promise<void> {
    const end = this.#now + ms;
    for (;;) {
      const due = this.#takeDue(end);
      if (due === undefined) {
        break;
      }
      this.#fire(due);
      // One macrotask per crossing: the transport's async frame builds settle between events.
      await tick();
    }
    this.#now = end;
    // The transport's timers ride the same virtual step: the crossings this elapse fired may
    // have armed the 5 ms ACK coalescing window, which comes due within this same step.
    await this.#timers?.advance(ms);
  }

  /**
   * Fires everything still queued, however far into the virtual future it is due. The
   * transport's clock deliberately does not move with it: a drain empties the wire, it does not
   * pass time, so nothing the drain armed (an ACK window, a reconnect backoff) fires here —
   * the next elapse advances the clock in the open, where the harness can act between steps.
   */
  async drain(): Promise<void> {
    for (;;) {
      const due = this.#takeDue(Number.MAX_SAFE_INTEGER);
      if (due === undefined) {
        return;
      }
      this.#fire(due);
      await tick();
    }
  }

  /** Removes and returns the earliest crossing due by `ceiling`, or undefined when none is. */
  #takeDue(ceiling: number): Crossing | undefined {
    let best: Crossing | undefined;
    for (const crossing of this.#crossings) {
      if (crossing.dueAt > ceiling) {
        continue;
      }
      if (
        best === undefined ||
        crossing.dueAt < best.dueAt ||
        (crossing.dueAt === best.dueAt && crossing.order < best.order)
      ) {
        best = crossing;
      }
    }
    if (best !== undefined) {
      this.#crossings.splice(this.#crossings.indexOf(best), 1);
    }
    return best;
  }

  #fire(crossing: Crossing): void {
    const socket = crossing.socket;
    if (socket.dead) {
      return;
    }
    if (crossing.kind === 'kill') {
      if (socket.opened) {
        this.kills += 1;
      }
      // A frame the far end never received means the path is gone: the socket dies now, and
      // everything still behind it on the wire died with it (socketClosed drops those).
      socket.close(1006, 'link lost');
      return;
    }
    if (crossing.direction === 'server-to-client') {
      socket.deliver(crossing.bytes as Uint8Array);
    } else {
      this.handleClientBytes(socket, crossing.bytes as Uint8Array);
    }
  }

  // ---------------------------------------------------------------------------
  // the protocol brain
  // ---------------------------------------------------------------------------

  /** Handles one client frame: HELLO (fresh or resume), ACK, or nothing the assertions need. */
  handleClientBytes(socket: LinkedSocket, bytes: Uint8Array): void {
    let frame;
    try {
      frame = decodeFrame(bytes);
    } catch {
      return;
    }
    if (frame.header.opcode === OP.HELLO) {
      this.#handleHello(socket, frame.header.correlation, decodeBody(decodeHello, frame.payload));
      return;
    }
    if (frame.header.opcode === OP.ACK) {
      const ack = decodeBody(decodeAck, frame.payload);
      this.ackWatermark = Math.max(this.ackWatermark, ack.frameSeq);
    }
  }

  #handleHello(socket: LinkedSocket, correlation: number, hello: Hello): void {
    if (hello.resume !== undefined && hello.resume.sessionId === this.sessionId) {
      this.resumes += 1;
      this.lastResumeSeq = Math.max(this.lastResumeSeq, hello.resume.lastFrameSeq);
      this.serverSend(socket, this.#welcomeBytes(correlation, true));
      // The resume contract: every retained frame past the client's watermark, original bytes.
      for (const bytes of this.#retained.slice(hello.resume.lastFrameSeq)) {
        this.serverSend(socket, bytes);
      }
      return;
    }
    this.freshHandshakes += 1;
    this.serverSend(socket, this.#welcomeBytes(correlation, false));
  }

  #welcomeBytes(correlation: number, resumed: boolean): Uint8Array {
    const welcome: Welcome = {
      sessionId: this.sessionId,
      node: { nodeId: 'node-1', region: 'eu', country: 'DE' },
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
      header: frameHeader(OP.HELLO, correlation),
      payload: encodeBody(encodeWelcome, welcome),
    });
  }

  /** Sequences one Critical message event and pushes it down the live socket. */
  pushMessageEvent(seq: number): void {
    const socket = this.#current;
    assert.ok(socket !== undefined && !socket.dead, 'pushed a message with no live socket');
    const bytes = encodeFrame({
      header: { ...frameHeader(OP.MESSAGE_EVENT, 0), flags: FLAG.ACK_REQUIRED },
      payload: encodeBody(encodeMessageEvent, {
        messageId: messageEventId(seq),
        conversationId: idFromBytes(new Uint8Array(16).fill(3)),
        seq,
        senderId: idFromBytes(new Uint8Array(16).fill(20)),
        senderDevice: idFromBytes(new Uint8Array(16).fill(9)),
        kind: MessageKind.Text,
        envelope: new Uint8Array([seq]),
        createdAt: 1_700_000_000_000,
      }),
    });
    this.#retained.push(bytes);
    this.serverSend(socket, bytes);
  }
}

/** A 16-byte id for a message event seq, distinct from every other id in the suite. */
function messageEventId(seq: number): Id {
  const bytes = new Uint8Array(16);
  bytes[15] = seq & 0xff;
  bytes[14] = (seq >>> 8) & 0xff;
  bytes[13] = 0x10;
  return idFromBytes(bytes);
}

// ---------------------------------------------------------------------------
// the rig
// ---------------------------------------------------------------------------

const SERVER: ServerEndpoint = {
  host: 'node.example',
  port: 443,
  gatewayPort: 443,
  transport: 'WebSocket',
  scheme: 'Wss',
  restScheme: 'Https',
};

interface Rig {
  node: LossyNode;
  transport: GatewayTransport;
  seen: number[];
  resumedCount: () => number;
}

/** Builds a transport wired to a lossy node and a message-event listener that records seqs. */
function rig(options: LossyNodeOptions, timers?: TransportTimers): Rig {
  const node = new LossyNode(options, simSeed(), timers);
  let resumedTimes = 0;
  const seen: number[] = [];
  const transport = new GatewayTransport({
    server: SERVER,
    hello: {
      platform: Platform.Web,
      appVersion: '1.0.0',
      locale: 'en',
      bandwidthMode: BandwidthMode.Normal,
      accessToken: 'test-token',
      deviceId: messageEventId(11),
      features: 0n,
    },
    // Far enough out that the heartbeat never fires during a test.
    heartbeatMs: 600_000,
    onResumed: () => {
      resumedTimes += 1;
    },
    webSocketFactory: node.webSocketFactory,
  });
  transport.subscribe(OP.MESSAGE_EVENT, (payload) => {
    seen.push(decodeBody(decodeMessageEvent, payload).seq);
  });
  return { node, transport, seen, resumedCount: () => resumedTimes };
}

/**
 * Drives the link's virtual clock until the transport is Ready, reconnecting through whatever
 * the seeded link throws at it: a first connect whose handshake dies, a resume whose HELLO is
 * lost, an attempt that fires into a still-dead network. The loop never waits a real backoff
 * out — it pulls each pending attempt forward with `reconnectNow`, which is the product's own
 * mechanism for exactly this moment (§149: a network switch reconnects immediately, without
 * waiting out a backoff whose cause is already known). Under a virtual transport clock this is
 * not merely impatience but the invariant that keeps the run seed-independent: each round
 * advances the clock 50 ms while the smallest backoff window is 250 ms, so an armed backoff is
 * always pulled forward (and cleared) long before it could fire.
 */
async function driveReady(
  node: LossyNode,
  transport: GatewayTransport,
  rounds = 400,
): Promise<void> {
  for (let round = 0; round < rounds; round++) {
    if (transport.state === 'ready') {
      return;
    }
    if (transport.state === 'idle' || transport.state === 'closed') {
      // Never stood up, or the first connect's handshake died: start again from the top.
      transport.connect().catch(() => {
        // A lossy first hop can refuse the attempt; the next round tries again.
      });
    } else if (transport.state === 'reconnecting') {
      const pending = node.latest();
      if (pending === undefined || pending.dead) {
        // The backoff is pending and no attempt is in flight: pull it forward.
        transport.reconnectNow();
      }
    }
    const latest = node.latest();
    if (latest !== undefined && !latest.opened && !latest.dead) {
      latest.fireOpen();
    }
    await node.elapse(50);
  }
  assert.fail(`the transport never reached Ready (${transport.state})`);
}

/** The node's knowledge of the client's position: the last resume watermark or cumulative ACK. */
function knownPosition(node: LossyNode): number {
  return Math.max(node.ackWatermark, node.lastResumeSeq);
}

// ---------------------------------------------------------------------------
// the lossy, half-second path
// ---------------------------------------------------------------------------

test('a lossy half-second path cannot duplicate or lose a message: every drop resumes to exactly-once', async () => {
  // §172: "packet loss 20 percent and RTT 500 ms. Expected: resume works, no duplicated
  // message, no lost message." The link below draws loss at exactly 20 percent per frame in
  // both directions and delays every crossing by 250 virtual ms, so a round trip costs the
  // promised 500. A lost frame kills the socket when it would have arrived — over an ordered
  // WebSocket that is the only shape loss can take — and the client's whole recovery is the
  // resume path this test exists to hold to exactly-once.
  //
  // The transport's own timers run on the virtual clock for this run: with real timers, the
  // moment the 5 ms ACK coalescing fired against the harness's ticks decided how many ACK
  // frames the seeded PRNG judged, so the "seeded" run was a different run under load.
  await withVirtualTransportTimers(async (clock) => {
    const { node, transport, seen, resumedCount } = rig(
      { lossRate: 0.2, oneWayLatencyMs: 250 },
      clock,
    );
    try {
      await driveReady(node, transport);
      assert.equal(transport.state, 'ready');

      // Push the whole conversation in small batches, letting the link kill sockets between
      // them: every kill forces a resume, and every resume must replay exactly what fell behind.
      const total = 36;
      let pushed = 0;
      while (pushed < total) {
        if (transport.state !== 'ready') {
          await driveReady(node, transport);
        }
        for (let batch = 0; batch < 3 && pushed < total; batch++) {
          pushed += 1;
          node.pushMessageEvent(pushed);
        }
        await node.elapse(120);
        if (transport.state !== 'ready') {
          await driveReady(node, transport);
        }
        await node.drain();
      }

      // Whatever is still in flight (a trailing loss, a kill mid-replay) settles here: drive to
      // Ready, drain, and let the client's position reach the node, until every seq is seen.
      for (let round = 0; round < 60 && seen.length < total; round++) {
        if (transport.state !== 'ready') {
          await driveReady(node, transport);
        }
        await node.drain();
        await node.elapse(50);
      }

      // No loss, no duplication, and in order: the seqs are exactly 1..N, once each.
      assert.deepEqual(
        seen,
        Array.from({ length: total }, (_unused, index) => index + 1),
        'the lossy path duplicated, lost, or reordered a message',
      );

      // The settling loop's exit condition is the seen count, not the connection: a trailing
      // loss in its last drain can kill the socket after the final message has already landed,
      // leaving a backoff pending that only more driving clears. Drive back to Ready — the
      // resume that follows carries the complete watermark, so its replay is empty and the
      // exactly-once count above is untouched.
      if (transport.state !== 'ready') {
        await driveReady(node, transport);
      }

      // The seeded run really did lose frames and really did resume through them — otherwise the
      // assertions above proved nothing about a bad network at all.
      assert.ok(node.kills >= 2, 'the seeded link never killed an established connection');
      assert.ok(node.resumes >= 1, 'the transport never resumed after a kill');
      assert.ok(resumedCount() >= 1, 'the client never observed a successful resume');

      // Resume kept the session: same id throughout, and the final state is a resumed Ready.
      assert.equal(transport.session?.sessionId, node.sessionId);
      assert.equal(transport.session?.resumed, true);
      assert.equal(transport.state, 'ready');

      // The node ends up knowing the client's full position: the last resume watermark, or the
      // cumulative ACK if the final stretch of the run was clean, covers every sequenced frame.
      // A trailing loss here can take the final ACK (or the socket) with it, so the loop also
      // drives the transport back to Ready — the resume that follows carries the full watermark.
      // The client's ACK rides the 5 ms coalescing window, which each elapse's clock advance
      // fires in step, so no real time is waited on anywhere in this loop.
      for (let round = 0; round < 100 && knownPosition(node) < total; round++) {
        if (transport.state !== 'ready') {
          await driveReady(node, transport);
        }
        await node.elapse(50);
      }
      assert.equal(knownPosition(node), total, 'the node never learned the client had every frame');
    } finally {
      transport.close();
    }
  });
});

// ---------------------------------------------------------------------------
// repeated network switches
// ---------------------------------------------------------------------------

/**
 * Records every setTimeout delay scheduled while `body` runs, leaving the real timers untouched.
 *
 * For reading the backoff the transport schedules: the recorded values are the exact delays
 * handed to the timer, so assertions on them are assertions on the transport's arithmetic, not
 * on wall-clock behaviour.
 */
async function scheduledDelaysWhile(body: () => Promise<void>): Promise<number[]> {
  const platformSetTimeout = globalThis.setTimeout;
  const delays: number[] = [];
  globalThis.setTimeout = ((handler: TimerHandler, ms?: number, ...rest: unknown[]) => {
    delays.push(ms ?? 0);
    return platformSetTimeout(handler, ms, ...rest);
  }) as typeof globalThis.setTimeout;
  try {
    await body();
  } finally {
    globalThis.setTimeout = platformSetTimeout;
  }
  return delays;
}

test('repeated network switches reconnect immediately without a reconnect storm', async () => {
  // §172: "repeated network switching. Expected: prompt reconnect, no reconnect storm." A
  // clean link (no loss, no latency) so the only thing exercised is the switch itself: the old
  // network's socket dies, the operating system reports a new network, and §149's rule for
  // that moment is an immediate attempt — no waiting out a backoff whose cause is known.
  const { node, transport, seen, resumedCount } = rig({ lossRate: 0, oneWayLatencyMs: 0 });
  try {
    await driveReady(node, transport);
    assert.equal(transport.state, 'ready');

    node.pushMessageEvent(1);
    node.pushMessageEvent(2);
    node.pushMessageEvent(3);
    await node.drain();
    assert.deepEqual(seen, [1, 2, 3]);

    // Four switches: the socket dies, a new network appears, the client must be on it at once.
    for (let switchCount = 1; switchCount <= 4; switchCount++) {
      const before = node.socketCount;
      node.liveSocket?.close(1006, 'network switched away');
      assert.equal(transport.state, 'reconnecting', 'a switch did not enter reconnecting');

      // No storm while the backoff is pending: the transport opens nothing on its own before
      // its one backoff timer fires, and that timer's minimum is far beyond a few ticks.
      await tick();
      await tick();
      await tick();
      assert.equal(
        node.socketCount,
        before,
        'the transport opened a socket while its backoff was still pending',
      );

      // The new network is reported: the next attempt leaves synchronously, inside the same
      // turn as the reconnectNow call — a backoff-waiting client could not do that.
      transport.reconnectNow();
      assert.equal(
        node.socketCount,
        before + 1,
        'a reported network switch did not attempt immediately',
      );

      await driveReady(node, transport);
      assert.equal(transport.state, 'ready', `switch ${switchCount} did not reach Ready`);
      assert.equal(resumedCount(), switchCount, `switch ${switchCount} did not resume the session`);
      assert.equal(transport.session?.sessionId, node.sessionId, 'a switch changed the session id');

      // The conversation continues exactly where it left off: no gap, no replay of the old.
      node.pushMessageEvent(3 + 2 * switchCount - 1);
      node.pushMessageEvent(3 + 2 * switchCount);
      await node.drain();
    }
    // One more event in steady state, on the network that never went away, so the switch
    // loop's exactly-once count ends at 12 where the outage section picks up.
    node.pushMessageEvent(12);
    await node.drain();
    assert.deepEqual(
      seen,
      [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
      'the switches duplicated, lost, or reordered a message',
    );

    // --- and then the network goes down with nowhere to switch to --------------------------
    //
    // The storm half of the bullet: while the network is genuinely gone the client retries on
    // the exponential, jittered schedule — one attempt per backoff round, never a burst — and
    // a failed attempt must schedule the next round rather than give up. A transport that
    // died Closed here would never reconnect on its own when the network returned; that
    // regression is exactly what the rounds below pin.
    const beforeDown = node.socketCount;
    // Round 0's backoff: the drop schedules it synchronously inside the close, so the recorder
    // is in place before the socket dies. Exactly one timer in the whole 250–1000 ms backoff
    // span, sitting in the first attempt's jitter window (base 500 ms, jitter half-to-full of
    // the capped value).
    const roundZero = await scheduledDelaysWhile(async () => {
      node.liveSocket?.close(1006, 'network gone');
      await tick();
      await tick();
    });
    assert.equal(transport.state, 'reconnecting', 'the drop did not enter reconnecting');
    const zeroInSpan = roundZero.filter((ms) => ms >= 250 && ms <= 1000);
    assert.equal(zeroInSpan.length, 1, 'round 0 did not schedule exactly one backoff timer');
    assert.ok(
      zeroInSpan[0] !== undefined && zeroInSpan[0] >= 250 && zeroInSpan[0] <= 500,
      'round 0 backoff left the first attempt window [250, 500] ms',
    );

    // Nothing opens while that one timer is pending.
    await sleep(20);
    assert.equal(node.socketCount, beforeDown, 'a backoff round opened more than one attempt');

    // The timer fires naturally (its delay is waited for, never asserted on): one attempt.
    await waitFor('the round 0 attempt', () => node.socketCount === beforeDown + 1, 2000);
    assert.equal(node.socketCount, beforeDown + 1, 'round 0 produced more than one attempt socket');

    // That network is dead too: the attempt's socket is refused before a HELLO is answered.
    // The transport must schedule the next round — not die Closed. The refusal's next-round
    // backoff is scheduled from the attempt's rejection (a microtask after the close), so the
    // recorder again wraps the close itself.
    const roundOne = await scheduledDelaysWhile(async () => {
      node.latest()?.close(1002, 'connection refused');
      await tick();
      await tick();
    });
    assert.equal(
      transport.state,
      'reconnecting',
      'a refused reconnect attempt ended the retry loop instead of scheduling the next round',
    );
    const oneInSpan = roundOne.filter((ms) => ms >= 250 && ms <= 1000);
    assert.equal(oneInSpan.length, 1, 'round 1 did not schedule exactly one backoff timer');
    assert.ok(
      oneInSpan[0] !== undefined && oneInSpan[0] >= 500 && oneInSpan[0] <= 1000,
      'round 1 backoff left the doubled attempt window [500, 1000] ms',
    );

    // The network returns: the pending round is pulled forward and the session resumes.
    transport.reconnectNow();
    assert.equal(node.socketCount, beforeDown + 2, 'the returning network did not attempt at once');
    await driveReady(node, transport);
    assert.equal(
      transport.state,
      'ready',
      'the transport did not recover when the network returned',
    );
    assert.equal(resumedCount(), 5, 'the post-outage reconnect did not resume the session');

    // And the conversation is still exactly-once after the whole outage.
    node.pushMessageEvent(13);
    await node.drain();
    assert.deepEqual(
      seen,
      [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13],
      'the outage duplicated, lost, or reordered a message',
    );
  } finally {
    transport.close();
  }
});
