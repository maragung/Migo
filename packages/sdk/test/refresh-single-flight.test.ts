/**
 * `refreshSession` is single-flight.
 *
 * A refresh token is single-use: the server's replay protection revokes the whole
 * token family when a second exchange presents a token it has already rotated. Two
 * concurrent `refreshSession` calls — a proactive timer firing while a caller
 * refreshes by hand, a page's bootstrap racing its own keepalive — would each send
 * the same refresh token and the second exchange would invalidate the session for
 * both callers. That is not an exotic race: both triggers fire around the same
 * expiry boundary, which is exactly when they are most likely to overlap.
 *
 * These tests drive the real `MigoClient` — real transport handshake against a
 * controlled socket, real `BootstrapClient` against a scripted fetch — and pin the
 * exchange count. The fake REST answers `/v1/auth/refresh` with a fresh grant
 * behind a deferred the test resolves by hand, so the concurrency window is real:
 * the first refresh is genuinely still in flight when the second call arrives.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { MigoClient } from '../src/client.js';
import type { Grant } from '../src/index.js';
import { decodeBody, encodeBody } from '../src/codec.js';
import {
  BandwidthMode,
  OP,
  Platform,
  decodeSubscribeRequest,
  encodeKeyPublishResult,
  encodePong,
  encodeSubscribeResponse,
  encodeWelcome,
} from '@migo/protocol';
import type { Welcome } from '@migo/protocol';
import { decodeFrame, encodeFrame, frameHeader, idFromBytes } from '@migo/wire';

/** Lets pending microtasks (handshake builds, promise chains) settle without real time. */
function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** A deterministic id: the number rendered as the low bytes of a 128-bit id. */
function idOf(n: number): ReturnType<typeof idFromBytes> {
  const bytes = new Uint8Array(16);
  bytes[15] = n & 0xff;
  bytes[14] = (n >>> 8) & 0xff;
  return idFromBytes(bytes);
}

/**
 * A stand-in WebSocket the test controls frame by frame.
 *
 * The transport attaches its handlers inside `connect()`, so every socket the
 * factory mints is opened only when the test calls {@link ControlledSocket.fireOpen}
 * and fed only the frames the test delivers. That is what makes the refresh's
 * reauthenticate observable: nothing happens until the test allows it.
 */
class ControlledSocket {
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
    this.readyState = ControlledSocket.CLOSED;
    this.onclose?.({ code, reason });
  }

  fireOpen(): void {
    this.readyState = ControlledSocket.OPEN;
    this.onopen?.();
  }

  deliver(bytes: Uint8Array): void {
    this.onmessage?.({ data: bytes });
  }
}

/** A WELCOME that authenticates inline, so the handshake reaches Ready with no AUTHENTICATE. */
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

/** A grant whose tokens are distinguishable across refreshes by their serial number. */
function grantWith(serial: number): Grant {
  return {
    accountId: idOf(100),
    deviceId: idOf(11),
    sessionId: idOf(200),
    accessToken: `access-${serial}`,
    refreshToken: `refresh-${serial}`,
    accessExpiresAtMs: 10_000_000_000,
    refreshExpiresAtMs: 20_000_000_000,
    capabilities: 0n,
    isNewAccount: false,
  };
}

/**
 * The realtime half of the server: every request the client sends gets the reply its
 * opcode declares.
 *
 * `#establish` does not stop at the handshake — it publishes this device's key
 * material (KEY_PUBLISH) and subscribes to the account's user topic (SUBSCRIBE)
 * before it settles, so a session that fakes only the WELCOME never finishes
 * connecting. Both replies are answered here from the generated codecs, with the
 * correlation the client sent, which is the only thing that resolves its pending
 * request.
 */
class ReplyServer {
  readonly socket: ControlledSocket;

  constructor(socket: ControlledSocket) {
    this.socket = socket;
  }

  /** Answers one client request frame with the protocol's reply for its opcode. */
  replyTo(raw: unknown): void {
    assert.ok(raw instanceof Uint8Array, 'the client sent a frame that is not bytes');
    const frame = decodeFrame(raw);
    const { opcode, correlation } = frame.header;
    // HELLO is answered by the test delivering the WELCOME by hand — a synthetic
    // reply here would be decoded as the WELCOME and fail the handshake.
    if (opcode === OP.HELLO) {
      return;
    }
    if (opcode === OP.KEY_PUBLISH) {
      this.socket.deliver(
        encodeFrame({
          header: frameHeader(OP.KEY_PUBLISH, correlation),
          payload: encodeBody(encodeKeyPublishResult, {
            acceptedPrekeys: 0,
            identityFingerprint: 'probe-fingerprint',
          }),
        }),
      );
      return;
    }
    if (opcode === OP.SUBSCRIBE) {
      // The topics echo back as accepted; decode the request so the reply is honest
      // about what was subscribed rather than hard-coding one topic kind.
      const request = decodeBody(decodeSubscribeRequest, frame.payload);
      this.socket.deliver(
        encodeFrame({
          header: frameHeader(OP.SUBSCRIBE, correlation),
          payload: encodeBody(encodeSubscribeResponse, { accepted: request.topics }),
        }),
      );
      return;
    }
    // PING (heartbeat) and anything else: a Pong keeps the session alive.
    this.socket.deliver(
      encodeFrame({
        header: frameHeader(OP.PING, correlation),
        payload: encodeBody(encodePong, { clientTime: 0, serverTime: 1_700_000_000_000 }),
      }),
    );
  }
}

/**
 * The REST half of the server: a fetch that answers `/v1/auth/refresh` with the
 * next serial's grant only when the test resolves the deferred in flight.
 *
 * Every exchange is counted and its presented refresh token recorded, so the
 * single-flight assertion is about what actually crossed the wire, not what the
 * client's state happened to be.
 */
class RefreshCounter {
  readonly presentedTokens: string[] = [];

  /**
   * When set, the refresh endpoint answers with a rejection — the token family
   * was revoked server-side, say — instead of a grant behind a deferred. The
   * non-refresh rejection ("unexpected call") is not enough for a failure test:
   * the refresh path is exactly the path under test, and a deferred that nobody
   * releases would hang the exchange rather than fail it.
   */
  failing = false;

  #serial = 0;

  get serial(): number {
    return this.#serial;
  }

  #deferred: { resolve: (grant: Grant) => void } | null = null;

  /** The grant the in-flight exchange will resolve with, or null when none is in flight. */
  get pending(): boolean {
    return this.#deferred !== null;
  }

  /** Resolves the in-flight exchange, if any. A no-op when nothing is pending. */
  release(): void {
    this.#deferred?.resolve(grantWith(this.#serial));
  }

  fetch: (input: string, init?: RequestInit) => Promise<Response> = (input, init) => {
    if (input.endsWith('/v1/auth/refresh')) {
      this.#serial += 1;
      const token = JSON.parse(init?.body as string) as { refresh_token: string };
      this.presentedTokens.push(token.refresh_token);
      if (this.failing) {
        this.#deferred = null;
        return Promise.reject(new TypeError('token family revoked'));
      }
      const promise = new Promise<Grant>((resolve) => {
        const deferred = { resolve };
        this.#deferred = deferred;
      });
      return promise.then((grant) => grantResponse(grant));
    }
    return Promise.reject(new TypeError('unexpected call'));
  };
}

/**
 * The JSON body the server's grant endpoint returns for one SDK grant: snake_case
 * fields, and `capabilities` as text — the wire form, never the SDK's `Grant`
 * (whose `capabilities` is a `bigint`, which `JSON.stringify` refuses to serialise).
 */
function grantResponse(grant: Grant): Response {
  return new Response(
    JSON.stringify({
      account_id: grant.accountId,
      device_id: grant.deviceId,
      session_id: grant.sessionId,
      access_token: grant.accessToken,
      refresh_token: grant.refreshToken,
      access_expires_at_ms: grant.accessExpiresAtMs,
      refresh_expires_at_ms: grant.refreshExpiresAtMs,
      capabilities: grant.capabilities.toString(),
      is_new_account: grant.isNewAccount,
    }),
    {
      status: 200,
      headers: { 'content-type': 'application/json' },
    },
  );
}

/** A client with a live (fake) session, its fetch and socket under the test's control. */
async function connectedClient(): Promise<{
  client: MigoClient;
  rest: RefreshCounter;
  socket: ControlledSocket;
}> {
  let socket: ControlledSocket | undefined;
  const rest = new RefreshCounter();
  const client = MigoClient.create({
    server: {
      host: 'node.example',
      port: 443,
      gatewayPort: 443,
      transport: 'WebSocket',
      scheme: 'Wss',
      restScheme: 'Https',
    },
    hello: {
      platform: Platform.Web,
      appVersion: 'test',
      locale: 'en',
      bandwidthMode: BandwidthMode.Normal,
      features: 0n,
    },
    deviceDisplayName: 'test device',
    webSocketFactory: () => {
      socket = new ControlledSocket('wss://node.example/ws');
      return socket as unknown as WebSocket;
    },
    // The far edge of the heartbeat so an idle session is truly idle.
    heartbeatMs: 600_000,
    fetch: rest.fetch,
  });

  // Every request the client sends past the handshake gets the reply its opcode
  // declares — KEY_PUBLISH and SUBSCRIBE are what #establish awaits before it
  // settles, so without these the resume never completes.
  const established = client.resume(grantWith(0));
  assert.ok(socket !== undefined, 'the transport did not build a socket');
  const server = new ReplyServer(socket);
  const originalSend = socket.send.bind(socket);
  socket.send = (data: unknown): void => {
    originalSend(data);
    server.replyTo(data);
  };

  socket.fireOpen();
  await tick(); // the HELLO is built and sent
  socket.deliver(welcomeFrame());
  await established;
  return { client, rest, socket: socket };
}

test('concurrent refreshSession calls share one exchange, not a token replay', async () => {
  const { client, rest, socket } = await connectedClient();
  try {
    // Two callers race the same refresh: the REST answer is held back by the
    // deferred, so the first exchange is provably in flight when the second
    // call arrives — the window in which a second presented token would revoke
    // the family server-side.
    const first = client.refreshSession();
    const second = client.refreshSession();
    await tick();
    await tick();
    assert.equal(rest.presentedTokens.length, 1, 'a second exchange presented the same token');

    rest.release();
    const [grantA, grantB] = await Promise.all([first, second]);
    assert.equal(grantA.accessToken, 'access-1');
    assert.equal(grantB.accessToken, 'access-1');
    assert.ok(grantA === grantB, 'the two callers resolved with the same grant object');

    // The client's live credential is the exchanged one.
    assert.equal(client.grant.refreshToken, 'refresh-1');
    // And the transport was reauthenticated with it: the credential that rides
    // the next HELLO on a reconnect is the new access token. The transport holds
    // it internally, so the proof is indirect — a refresh that failed before the
    // reauthenticate would have left the promise rejected, and this line asserts
    // the whole chain ran.
    assert.equal(rest.serial, 1, 'exactly one exchange crossed the wire');
    assert.ok(socket.sent.length >= 1, 'the session is live');
  } finally {
    await client.disconnect();
  }
});

test('a failed refresh does not wedge the next one behind a dead promise', async () => {
  const { client, rest } = await connectedClient();
  try {
    // The exchange rejects — the token family was revoked server-side, say. The
    // in-flight slot must clear on failure, or every later refresh would await a
    // promise that already settled and can never carry a new exchange.
    rest.failing = true;
    const failing = client.refreshSession();
    // Re-raise: the failing exchange must reject for this test to be meaningful.
    await assert.rejects(failing, /token family revoked/i);
    assert.equal(rest.presentedTokens.length, 1, 'the failing exchange did cross the wire');

    // The slot is free: a second refresh starts a new exchange rather than
    // sharing the rejected one.
    const next = client.refreshSession();
    await assert.rejects(next, /token family revoked/i);
    assert.equal(
      rest.presentedTokens.length,
      2,
      'the second refresh replayed the failed exchange instead of starting a new one',
    );
  } finally {
    await client.disconnect();
  }
});

test('sequential refreshes are not collapsed: each starts its own exchange', async () => {
  const { client, rest } = await connectedClient();
  try {
    const first = client.refreshSession();
    rest.release();
    const grantA = await first;
    assert.equal(grantA.accessToken, 'access-1');

    const second = client.refreshSession();
    rest.release();
    const grantB = await second;
    // The second exchange presented the *rotated* token, which is what makes it
    // legitimate: a collapse here would mean the client replayed refresh-0.
    assert.deepEqual(rest.presentedTokens, ['refresh-0', 'refresh-1']);
    assert.equal(grantB.accessToken, 'access-2');
    assert.equal(client.grant.refreshToken, 'refresh-2');
  } finally {
    await client.disconnect();
  }
});
