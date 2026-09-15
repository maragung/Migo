/**
 * §171's loadgen byte accounting, tested at the seams that can silently lie.
 *
 * The arithmetic seam: `summarizeWireBytes` must be exact, pure arithmetic — the report built on it
 * is asserted deterministic elsewhere, and a per-minute rate that rounded, drifted, or divided by
 * zero would either hide bytes or fail an interrupted run on an infinite rate. The gate seam: a
 * budget fails only strictly past budget + 10 percent, every registered scenario must have a
 * budget (a scenario added without one would run ungated, the exact quiet regression §171 forbids
 * for opcodes), and an unbudgeted name never fails. The measurement seam: the counters the summary
 * consumes are the SDK transport's own, proven here by driving a real {@link GatewayTransport}
 * over fake sockets, dropping it mid-session, reconnecting, and demanding the reading equal every
 * byte both sockets carried — the loadgen's number is only as honest as the counter under it.
 *
 * This suite never opens a real socket and never connects to anything.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { BandwidthMode, GatewayTransport, Platform, encodeBody } from '@migo/sdk';
import { protocol, wire } from '@migo/sdk';
import type { ServerEndpoint, WireBytes } from '@migo/sdk';

import { scenarioNames } from '../scenarios.js';
import {
  BYTE_BUDGET_HEADROOM,
  byteBudgetVerdict,
  scenarioByteBudget,
  summarizeWireBytes,
} from '../wire-bytes.js';

const { OP, encodeWelcome } = protocol;
const { encodeFrame, frameHeader, idFromBytes } = wire;
type Welcome = protocol.Welcome;

/** A single user's per-minute rate equals its total when the window is exactly one minute. */
const ONE_MINUTE_MS = 60_000;

// ---------------------------------------------------------------------------
// summarizeWireBytes: exact arithmetic, no divide-by-zero
// ---------------------------------------------------------------------------

test('summarizeWireBytes reduces readings to exact per-user per-minute figures', () => {
  const summary = summarizeWireBytes(
    [
      { sent: 100, received: 250 },
      { sent: 50, received: 0 },
    ],
    30_000,
  );
  assert.equal(summary.users, 2);
  assert.equal(summary.minutes, 0.5);
  assert.equal(summary.sentBytes, 150);
  assert.equal(summary.receivedBytes, 250);
  assert.equal(summary.totalBytes, 400);
  assert.equal(summary.perUserBytes, 200);
  // 200 B/user over half a minute is 400 B/user/min.
  assert.equal(summary.bytesPerUserPerMinute, 400);
});

test('a run with no sessions reads as zero, not NaN', () => {
  const summary = summarizeWireBytes([], ONE_MINUTE_MS);
  assert.equal(summary.users, 0);
  assert.equal(summary.totalBytes, 0);
  assert.equal(summary.perUserBytes, 0);
  assert.ok(Number.isFinite(summary.bytesPerUserPerMinute));
  assert.equal(summary.bytesPerUserPerMinute, 0);
});

test('a run with no elapsed steady window has a zero rate, however many bytes flowed', () => {
  // An interrupted run can accrue handshake bytes before the steady-state clock starts. The
  // verdict for such a run comes from the interrupt flag and the error budget, not from an
  // infinite byte rate, so the summary must stay finite and zero.
  const summary = summarizeWireBytes([{ sent: 10_000, received: 10_000 }], 0);
  assert.equal(summary.totalBytes, 20_000);
  assert.equal(summary.bytesPerUserPerMinute, 0);
  assert.ok(Number.isFinite(summary.bytesPerUserPerMinute));
});

// ---------------------------------------------------------------------------
// budgets and the 10 percent gate
// ---------------------------------------------------------------------------

test('every registered scenario has a byte budget', () => {
  // The registry and the budget table are two lists that must not drift apart: a scenario
  // without a budget runs ungated, and §171's rule for opcodes (every addition carries its
  // measurement) is the rule here too.
  for (const name of scenarioNames()) {
    const budget = scenarioByteBudget(name);
    assert.ok(budget !== undefined, `scenario "${name}" has no byte budget`);
    assert.ok(
      Number.isInteger(budget.bytesPerUserPerMinute) && budget.bytesPerUserPerMinute > 0,
      `scenario "${name}" budget is not a positive whole number of bytes`,
    );
    assert.ok(budget.anchor.length > 0, `scenario "${name}" budget cites no anchor`);
  }
});

test('the gate fails only strictly past budget plus the section-171 headroom', () => {
  const budget = scenarioByteBudget('connect');
  assert.ok(budget !== undefined);
  assert.equal(BYTE_BUDGET_HEADROOM, 0.1);

  // Exactly on the budget: comfortably within.
  const onBudget = summarizeWireBytes(
    [{ sent: budget.bytesPerUserPerMinute, received: 0 }],
    ONE_MINUTE_MS,
  );
  assert.equal(byteBudgetVerdict('connect', onBudget).exceeded, false);

  // Exactly on the limit (budget * 1.1): still within — §171 fails a scenario that exceeds the
  // budget by *more than* 10 percent, so landing on the line passes. 90112 bytes over ten
  // minutes is exactly 8192 * 1.1 per minute, bit for bit.
  const limit = budget.bytesPerUserPerMinute * (1 + BYTE_BUDGET_HEADROOM);
  const onLimit = summarizeWireBytes([{ sent: limit * 10, received: 0 }], 10 * ONE_MINUTE_MS);
  assert.equal(onLimit.bytesPerUserPerMinute, limit);
  assert.equal(byteBudgetVerdict('connect', onLimit).exceeded, false);

  // One byte per minute past the limit: exceeded.
  const pastLimit = summarizeWireBytes(
    [{ sent: limit * 10 + 10, received: 0 }],
    10 * ONE_MINUTE_MS,
  );
  const verdict = byteBudgetVerdict('connect', pastLimit);
  assert.equal(verdict.exceeded, true);
  assert.equal(verdict.limitBytesPerUserPerMinute, limit);
  assert.equal(verdict.measuredBytesPerUserPerMinute, pastLimit.bytesPerUserPerMinute);
});

test('a scenario without a budget is never failed, whatever it spent', () => {
  const summary = summarizeWireBytes([{ sent: 10 ** 9, received: 10 ** 9 }], ONE_MINUTE_MS);
  const verdict = byteBudgetVerdict('no-such-scenario', summary);
  assert.equal(verdict.budget, undefined);
  assert.equal(verdict.limitBytesPerUserPerMinute, null);
  assert.equal(verdict.exceeded, false);
});

// ---------------------------------------------------------------------------
// the counters under the summary: a real transport, fake sockets, a reconnect
// ---------------------------------------------------------------------------

/**
 * A stand-in WebSocket that records what the transport sends, so a test can demand the byte
 * counters equal what actually crossed the wire. Only the surface the transport touches.
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

/** Lets pending microtasks settle so the transport's async frame builds finish. */
function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** A WELCOME that authenticates inline, so the handshake reaches Ready without AUTHENTICATE. */
function welcomeFrame(): Uint8Array {
  const welcome: Welcome = {
    sessionId: idFromBytes(new Uint8Array(16).fill(1)),
    node: { nodeId: 'node-1', region: 'eu', country: 'DE' },
    // No feature bits, so nothing the transport sends is compressed and each frame counts plainly.
    features: 0n,
    serverTime: 1_700_000_000_000,
    limits: {
      maxFrameBytes: 1 << 20,
      maxBatchItems: 64,
      maxSubscriptions: 256,
      heartbeatMs: 30_000,
    },
    authenticatedUser: idFromBytes(new Uint8Array(16).fill(10)),
  };
  return encodeFrame({
    header: frameHeader(OP.HELLO, 1),
    payload: encodeBody(encodeWelcome, welcome),
  });
}

test('the summary consumes exactly the bytes a real transport carried across a reconnect', async () => {
  // The loadgen's byte report is only as honest as the reading it summarizes, and the reading
  // comes from the SDK transport a virtual user owns. So: drive the real transport through a
  // handshake, an app frame, an inbound event, a drop, and a reconnect — then demand the
  // summary built from its final counters equals, byte for byte, what the two sockets carried.
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
      deviceId: idFromBytes(new Uint8Array(16).fill(11)),
      features: 0n,
    },
    // Far enough out that the heartbeat never fires during the test.
    heartbeatMs: 600_000,
    webSocketFactory: (url: string) => {
      const socket = new FakeSocket(url);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    },
  });

  try {
    const ready = transport.connect();
    const first = sockets[0];
    assert.ok(first !== undefined, 'the transport did not build a socket synchronously');
    first.fireOpen();
    await tick();
    first.deliver(welcomeFrame());
    await ready;

    // An app frame out, an event in — both directions counted before the drop.
    await transport.notify(OP.TYPING, new Uint8Array(32));
    const event = encodeFrame({
      header: frameHeader(OP.MESSAGE_EVENT, 0),
      payload: new Uint8Array([1, 2, 3]),
    });
    first.deliver(event);
    await tick();

    // The network drops the socket; reconnectNow pulls the next attempt past the backoff, and
    // the reconnect's own HELLO/WELCOME is counted too, because those bytes were really paid.
    first.close(1006, 'network drop');
    transport.reconnectNow();
    await tick();
    const second = sockets[1];
    assert.ok(second !== undefined, 'the reconnect did not open a new socket');
    second.fireOpen();
    await tick();
    second.deliver(welcomeFrame());
    await tick();
    assert.equal(transport.state, 'ready', 'the reconnect did not reach Ready');

    const carried = (socket: FakeSocket): number =>
      socket.sent.reduce<number>(
        (total, frame) => total + (frame instanceof Uint8Array ? frame.byteLength : 0),
        0,
      );
    const expectedSent = carried(first) + carried(second);
    const expectedReceived = 2 * welcomeFrame().byteLength + event.byteLength;
    const reading: WireBytes = transport.wireBytes;
    assert.equal(reading.sent, expectedSent);
    assert.equal(reading.received, expectedReceived);

    // The loadgen's figure for one such session held one minute: the totals, unchanged.
    const summary = summarizeWireBytes([reading], ONE_MINUTE_MS);
    assert.equal(summary.users, 1);
    assert.equal(summary.totalBytes, expectedSent + expectedReceived);
    assert.equal(summary.bytesPerUserPerMinute, expectedSent + expectedReceived);
    // And the connect gate, whose budget sits far above a single session's handshake-plus-
    // heartbeat spend, reads it as within.
    assert.equal(byteBudgetVerdict('connect', summary).exceeded, false);
  } finally {
    transport.close();
  }
});
