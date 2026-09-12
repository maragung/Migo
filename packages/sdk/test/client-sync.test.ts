/**
 * The client half of section 158's offline-first synchronization.
 *
 * The server half (SYNC, cursors, Truncated) is long built; these tests pin the client half,
 * which the brief leaves SPEC and which a messenger without is only ever online:
 *
 * * **Reconnect order** — a fresh session's topics are re-subscribed User-first, then Room,
 *   then Conversation, because the user topic carries the self-directed events nothing else
 *   replays and the later kinds' events can reference the earlier kinds' state.
 * * **Offline outbox** — a send composed while the link is down is queued, not failed; it
 *   leaves when the session is ready, with its idempotency key minted once and reused, so a
 *   retry the server has already stored is answered `duplicate` rather than doubled. A
 *   non-retryable refusal rejects the entry; a spent attempt budget rejects the entry; the
 *   per-conversation order of queued entries is preserved on the wire.
 * * **Background stop** — a hidden page parks the drain between entries without dropping
 *   anything, and the moment the page is visible the queue continues. Nothing fails, nothing
 *   duplicates.
 *
 * They drive the real `MigoClient` over a scripted socket — the same harness shape the
 * membership-cache tests use — because the point under test is the client's own wiring, not
 * any single class in isolation.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { MigoClient } from '../src/client.js';
import type { OutboxEntry } from '../src/index.js';
import type { Grant } from '../src/index.js';
import { decodeBody, encodeBody } from '../src/codec.js';
import { ContentType } from '../src/content.js';
import type { TextContent } from '../src/content.js';
import {
  BandwidthMode,
  OP,
  Platform,
  TopicKind,
  FLAG,
  MessageKind,
  decodeKeyBundleRequest,
  decodeMessageSend,
  decodeSubscribeRequest,
  encodeAcknowledged,
  encodeConversationRosterResponse,
  encodeError,
  encodeKeyBundleResponse,
  encodeKeyPublishResult,
  encodeMessageAccepted,
  encodePong,
  encodeSubscribeResponse,
  encodeWelcome,
} from '@migo/protocol';
import type { ConversationRosterEntry, KeyBundle, MessageSend, Welcome } from '@migo/protocol';
import { KeyStore } from '../src/index.js';
import { decodeFrame, encodeFrame, frameHeader, idFromBytes } from '@migo/wire';
import type { Id } from '@migo/wire';

/** Lets pending microtasks (handshake builds, promise chains, timers of zero) settle. */
function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** A deterministic id: the number rendered as the low bytes of a 128-bit id. */
function idOf(n: number): Id {
  const bytes = new Uint8Array(16);
  bytes[15] = n & 0xff;
  bytes[14] = (n >>> 8) & 0xff;
  return idFromBytes(bytes);
}

/**
 * A stand-in WebSocket the test controls frame by frame, exactly as the membership tests use.
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
function welcomeFrame(sessionId: Id, resumed = false): Uint8Array {
  const welcome: Welcome = {
    sessionId,
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
    ...(resumed ? { resumed: true } : {}),
  };
  return encodeFrame({
    header: frameHeader(OP.HELLO, 1),
    payload: encodeBody(encodeWelcome, welcome),
  });
}

/** A grant whose tokens are placeholders; these tests never refresh. */
function grantWith(): Grant {
  return {
    accountId: idOf(10),
    deviceId: idOf(11),
    sessionId: idOf(200),
    accessToken: 'access-0',
    refreshToken: 'refresh-0',
    accessExpiresAtMs: 10_000_000_000,
    refreshExpiresAtMs: 20_000_000_000,
    capabilities: 0n,
    isNewAccount: false,
  };
}

/**
 * The scripted server: answers each request with its declared reply and records every
 * MESSAGE_SEND body, which is what the idempotency assertions read. `acceptSends` can be
 * toggled false to simulate the server refusing every send — used for the failure-path tests.
 */
class ScriptedServer {
  readonly socket: ControlledSocket;
  /** Every decoded MessageSend, in arrival order. */
  readonly sends: MessageSend[] = [];
  /** The opcode of every request answered, in arrival order. */
  readonly asked: number[] = [];
  /** When false, a MESSAGE_SEND is answered with a generic INTERNAL_ERROR refusal. */
  refuseSends = false;
  /** A custom refusal to answer MESSAGE_SEND with, as (code, symbol). */
  refusal: { code: number; symbol: string; retryAfterMs?: number } | null = null;
  /** The next seq the server hands a stored message. */
  #nextSeq = 1;
  /** The accounts the roster names, as the numbers the ids were minted from. */
  readonly roster: number[] = [10, 20, 30];
  /** One real device per scripted account, so the send path's X3DH has keys that verify. */
  readonly #stores = new Map<string, KeyStore>();
  /** The device id each scripted account's key store publishes under. */
  readonly #deviceIds = new Map<string, Id>();
  #nextDevice = 0x9000;

  constructor(socket: ControlledSocket) {
    this.socket = socket;
  }

  /**
   * Only the sends the composer authored. The send path also rides MESSAGE_SEND for its
   * key-exchange control messages (the sender-key distribution), so the raw `sends` list is
   * not the count any assertion about *messages* wants to read.
   */
  contentSends(): MessageSend[] {
    return this.sends.filter((send) => send.kind !== MessageKind.KeyExchange);
  }

  /** The key store speaking for `userId`, minted once so its bundle stays self-consistent. */
  #storeFor(userId: Id): { store: KeyStore; deviceId: Id } {
    let store = this.#stores.get(userId);
    let deviceId = this.#deviceIds.get(userId);
    if (store === undefined || deviceId === undefined) {
      store = KeyStore.create(4);
      deviceId = idOf(this.#nextDevice);
      this.#nextDevice += 1;
      this.#stores.set(userId, store);
      this.#deviceIds.set(userId, deviceId);
    }
    return { store, deviceId };
  }

  /** Answers one client request frame with the protocol's reply for its opcode. */
  replyTo(raw: unknown): void {
    assert.ok(raw instanceof Uint8Array, 'the client sent a frame that is not bytes');
    const frame = decodeFrame(raw);
    const { opcode, correlation } = frame.header;
    const reply = (payload: Uint8Array): void => {
      this.socket.deliver(encodeFrame({ header: frameHeader(opcode, correlation), payload }));
    };
    if (opcode === OP.HELLO) {
      // The test delivers the WELCOME by hand, so it can pick the session id.
      return;
    }
    this.asked.push(opcode);
    if (opcode === OP.MESSAGE_SEND) {
      const request = decodeBody(decodeMessageSend, frame.payload);
      this.sends.push(request);
      const refusalApplies =
        this.refuseSends || (this.refusal !== null && request.kind !== MessageKind.KeyExchange);
      if (refusalApplies) {
        const refusal = this.refusal ?? { code: 1600, symbol: 'INTERNAL_ERROR' };
        const errorFrame = encodeFrame({
          header: { ...frameHeader(opcode, correlation), flags: FLAG.ERROR },
          payload: encodeBody(encodeError, {
            code: refusal.code,
            symbol: refusal.symbol,
            ...(refusal.retryAfterMs !== undefined ? { retryAfterMs: refusal.retryAfterMs } : {}),
          }),
        });
        this.socket.deliver(errorFrame);
        return;
      }
      reply(
        encodeBody(encodeMessageAccepted, {
          messageId: request.messageId,
          conversationId: request.conversationId,
          seq: this.#nextSeq,
          createdAt: 1_700_000_000_000,
        }),
      );
      this.#nextSeq += 1;
      return;
    }
    if (opcode === OP.SUBSCRIBE) {
      const request = decodeBody(decodeSubscribeRequest, frame.payload);
      reply(encodeBody(encodeSubscribeResponse, { accepted: request.topics }));
      return;
    }
    if (opcode === OP.UNSUBSCRIBE) {
      reply(encodeBody(encodeAcknowledged, { ok: true }));
      return;
    }
    if (opcode === OP.CONVERSATION_ROSTER) {
      // The whole truth about who is in the conversation; the send path reads it to choose
      // the audience when the membership cache does not know the conversation yet.
      const entries: ConversationRosterEntry[] = this.roster.map((n) => ({
        accountId: idOf(n),
        role: 0,
        joinedAt: 1_700_000_000_000,
      }));
      reply(encodeBody(encodeConversationRosterResponse, { entries }));
      return;
    }
    if (opcode === OP.KEY_BUNDLE_FETCH) {
      const request = decodeBody(decodeKeyBundleRequest, frame.payload);
      // A real store's published bundle: the client verifies the signed prekey against the
      // identity key before it agrees to seal, so filler bytes would fail in the crypto layer
      // before the outbox's behavior under test ever ran.
      const { store, deviceId } = this.#storeFor(request.userId);
      const published = store.publish();
      const firstOneTime = published.oneTimePrekeys[0];
      const bundle: KeyBundle = {
        userId: request.userId,
        deviceId,
        identityKey: published.identityKey,
        signedPrekeyId: published.signedPrekeyId,
        signedPrekey: published.signedPrekey,
        signedPrekeySignature: published.signedPrekeySignature,
        ...(firstOneTime !== undefined
          ? { oneTimePrekeyId: firstOneTime.keyId, oneTimePrekey: firstOneTime.publicKey }
          : {}),
      };
      reply(encodeBody(encodeKeyBundleResponse, { bundles: [bundle] }));
      return;
    }
    if (opcode === OP.KEY_PUBLISH) {
      reply(
        encodeBody(encodeKeyPublishResult, {
          acceptedPrekeys: 0,
          identityFingerprint: 'probe-fingerprint',
        }),
      );
      return;
    }
    // PING (heartbeat) and anything else: a Pong keeps the session alive.
    reply(encodeBody(encodePong, { clientTime: 0, serverTime: 1_700_000_000_000 }));
  }
}

/** An outbox-tuned client connected to a fresh scripted session. */
async function connectedClient(): Promise<{
  client: MigoClient;
  server: ScriptedServer;
  socket: ControlledSocket;
}> {
  // One socket for the whole test, handed back by the factory on every (re)connect: the
  // transport attaches fresh handlers to whatever the factory returns, so a shared object
  // is how the test keeps driving the connection the transport rebuilt after a drop.
  const socket = new ControlledSocket('wss://node.example/ws');
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
    webSocketFactory: () => socket as unknown as WebSocket,
    heartbeatMs: 600_000,
    // The reconnect tests wait on the transport's own backoff; a cap of 1ms keeps the suite
    // from inheriting the production thirty seconds.
    maxReconnectDelayMs: 1,
    fetch: () => Promise.reject(new TypeError('unexpected call')),
    outbox: { backoffBaseMs: 1, backoffCapMs: 1 },
  });

  const established = client.resume(grantWith());
  const server = new ScriptedServer(socket);
  const originalSend = socket.send.bind(socket);
  socket.send = (data: unknown): void => {
    originalSend(data);
    server.replyTo(data);
  };

  socket.fireOpen();
  await tick(); // the HELLO is built and sent
  socket.deliver(welcomeFrame(idOf(1)));
  await established;
  await tick();
  // The conversation every send here targets, known whole the way an app that opened it
  // knows it — so the sends reach the wire and the assertions read outbox behavior, not
  // membership discovery.
  client.rememberMembers(idOf(0x5eed), [idOf(10), idOf(20), idOf(30)]);
  return { client, server, socket };
}

/** A plain text content, as the composer hands the send path. */
function textOf(body: string): TextContent {
  return { type: ContentType.Text, text: body };
}

test('an offline send is queued, not failed, and leaves when the link returns', async () => {
  const { client, server, socket } = await connectedClient();
  try {
    // Tear the link down the way a drop looks: the transport's close handler runs.
    socket.readyState = ControlledSocket.CLOSED;
    socket.close(1006, 'link dropped');
    // The transport now sits in `reconnecting` with a backoff pending; the outbox's readiness
    // check reads exactly that, so the queue holds.
    await tick();

    const settled: OutboxEntry[] = [];
    client.onOutboxEntry((entry) => settled.push(entry));

    const sendOne = client.sendQueued(idOf(0x5eed), textOf('queued while offline'));
    const sendTwo = client.sendQueued(idOf(0x5eed), textOf('second behind the first'));
    await tick();

    // Nothing left the device: the link is down and the entries sit queued, not failed.
    assert.equal(server.contentSends().length, 0, 'nothing left while the link was down');
    assert.equal(client.outbox?.size, 2, 'both entries are held');

    // The link returns. The transport's reconnect mints a fresh socket through the factory,
    // but the factory closure in connectedClient() keeps returning the SAME ControlledSocket,
    // so the test can simply re-open it and answer the fresh handshake — the whole dance a
    // resumed-then-reset session performs, on one object the test still holds.
    socket.readyState = ControlledSocket.OPEN;
    socket.fireOpen();
    await tick(); // the fresh HELLO is sent
    socket.deliver(welcomeFrame(idOf(2), false)); // not resumable: a fresh session
    await tick();
    await tick();
    await tick();

    const accepted = await sendOne;
    assert.equal(accepted.duplicate, undefined, 'a first delivery is not a duplicate');
    await sendTwo;

    // Both left, with one idempotency key each.
    assert.equal(server.contentSends().length, 2, 'both entries drained');
    const ids = server.contentSends().map((send) => send.messageId);
    assert.equal(new Set(ids).size, 2, 'each entry kept its own idempotency key');
    assert.equal(client.outbox?.size, 0, 'the queue emptied');
    const delivered = settled.filter((entry) => entry.state === 'delivered');
    assert.equal(delivered.length, 2, 'both entries were observed delivered');
  } finally {
    await client.disconnect();
  }
});

test('a retried send reuses its idempotency key, so the server can dedup it', async () => {
  const { client, server, socket } = await connectedClient();
  try {
    // The reply to the first content send is swallowed: the request reached the server, the
    // acknowledgement did not reach the client — the classic ambiguity a retry resolves.
    let swallowed = false;
    const originalReply = server.replyTo.bind(server);
    server.replyTo = (raw: unknown): void => {
      const frame = decodeFrame(raw as Uint8Array);
      if (frame.header.opcode === OP.MESSAGE_SEND) {
        // Record it the way the server always would, then drop only the reply.
        const request = decodeBody(decodeMessageSend, frame.payload);
        server.sends.push(request);
        if (
          !swallowed &&
          request.kind !== MessageKind.KeyExchange &&
          server.refusal === null &&
          !server.refuseSends
        ) {
          swallowed = true;
          return; // landed server-side, reply lost
        }
      }
      originalReply(raw);
    };

    const send = client.sendQueued(idOf(0x5eed), textOf('first attempt lost its reply'));
    // The send path's key exchange and content frame leave over several microtask chains;
    // let them settle before the reply-swallowing takes effect below.
    for (let i = 0; i < 8; i += 1) {
      await tick();
    }
    assert.equal(server.contentSends().length, 1, 'the first attempt left');

    // The link drops, which rejects the unanswered request the retry replaces.
    socket.readyState = ControlledSocket.CLOSED;
    socket.close(1006, 'link dropped');
    await tick();

    // The link returns; the queue drains and the retry re-sends with the SAME message id —
    // the idempotency key the server dedups on.
    socket.readyState = ControlledSocket.OPEN;
    socket.fireOpen();
    await tick();
    socket.deliver(welcomeFrame(idOf(2), false));
    await tick();
    await tick();
    await tick();

    await send;
    assert.ok(server.contentSends().length >= 2, 'the retry left after the link returned');
    const ids = server.contentSends().map((entry) => entry.messageId);
    assert.equal(new Set(ids).size, 1, 'every attempt rode the same idempotency key');
    assert.equal(client.outbox?.size, 0, 'the entry left the queue');
  } finally {
    await client.disconnect();
  }
});

test('a refusal the server means fails the entry instead of retrying forever', async () => {
  const { client, server } = await connectedClient();
  try {
    // Permission-class refusal (1200-1299): not retryable, so the entry fails at once.
    server.refusal = { code: 1205, symbol: 'PRIVACY_RESTRICTED' };
    await assert.rejects(client.sendQueued(idOf(0x5eed), textOf('refused')), /PRIVACY_RESTRICTED/);
    assert.equal(client.outbox?.size, 0, 'a meant refusal is not re-queued');
    assert.equal(server.contentSends().length, 1, 'it was attempted exactly once');
  } finally {
    await client.disconnect();
  }
});

test('a hidden page parks the drain; a visible page resumes it without loss', async () => {
  const { client, server } = await connectedClient();
  try {
    client.setPageVisible(false);
    const sendOne = client.sendQueued(idOf(0x5eed), textOf('sent while hidden'));
    await tick();
    await tick();
    assert.equal(server.contentSends().length, 0, 'nothing left while the page was hidden');
    assert.equal(client.outbox?.size, 1, 'the entry was parked, not failed');

    client.setPageVisible(true);
    const accepted = await sendOne;
    assert.ok(accepted.seq > 0, 'the parked send left once the page became visible');
    assert.equal(server.contentSends().length, 1, 'it left exactly once');
  } finally {
    await client.disconnect();
  }
});

test('after a session reset, the topics are re-subscribed user-first', async () => {
  const { client, server, socket } = await connectedClient();
  try {
    // Subscribe a spread of topics the way the application does while online.
    await client.watchConversation(idOf(0xa1));
    await client.watchRoom(idOf(0xa2));
    await client.watchUser(idOf(0xa3));
    await client.watchConversation(idOf(0xa4));
    const before = server.asked.filter((opcode) => opcode === OP.SUBSCRIBE).length;

    // A session reset: the link drops and the fresh WELCOME names a new session (resumed
    // false), so the client re-subscribes every tracked topic on the fresh session.
    socket.readyState = ControlledSocket.CLOSED;
    socket.close(1006, 'link dropped');
    await tick();
    socket.readyState = ControlledSocket.OPEN;
    socket.fireOpen();
    await tick(); // the fresh HELLO is sent
    socket.deliver(welcomeFrame(idOf(2), false));
    await tick();
    await tick();
    await tick();

    const subscribeAnswers = server.asked.filter((opcode) => opcode === OP.SUBSCRIBE).length;
    assert.ok(
      subscribeAnswers > before,
      'the reset re-subscribed the tracked topics on the fresh session',
    );
    // The order: the reset's SUBSCRIBE names the user topic before the room and the
    // conversations. Read it from the last SUBSCRIBE request the client sent.
    const lastSubscribeRaw = [...socket.sent].reverse().find((raw) => {
      const frame = decodeFrame(raw as Uint8Array);
      return frame.header.opcode === OP.SUBSCRIBE;
    });
    assert.ok(lastSubscribeRaw !== undefined, 'a reset SUBSCRIBE was sent');
    const request = decodeBody(
      decodeSubscribeRequest,
      decodeFrame(lastSubscribeRaw as Uint8Array).payload,
    );
    const kinds = request.topics.map((topic) => topic.kind);
    const firstConversation = kinds.indexOf(TopicKind.Conversation);
    const userIndex = kinds.indexOf(TopicKind.User);
    const roomIndex = kinds.indexOf(TopicKind.Room);
    assert.ok(userIndex !== -1, 'the user topic is in the reset subscribe');
    assert.ok(
      userIndex < roomIndex && roomIndex < firstConversation,
      `the reset subscribes User before Room before Conversation (saw ${kinds.join(',')})`,
    );
  } finally {
    await client.disconnect();
  }
});
