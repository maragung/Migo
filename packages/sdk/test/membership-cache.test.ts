/**
 * The membership cache is not a list preview.
 *
 * The server caps each conversation-list row at `MEMBER_PREVIEW` members — enough to render a
 * row's stacked avatars, not enough to choose an encryption audience. A client that cached the
 * preview as the membership and built its sender-key distribution from it would seal a
 * nine-member group's key for eight members: the ninth could never decrypt, and the sender
 * would believe the group smaller than it is. These tests pin the two halves of the fix:
 *
 * * A membership cached from a list row is *incomplete*, and the first send's
 *   `recipientDevices` reads the roster before choosing the audience — once, not per send.
 * * Membership movement (`CONVERSATION_MEMBER_EVENT`) is applied onto the cache, so an invite
 *   or a departure the live stream reports is reflected by the next send without a roster
 *   re-read.
 *
 * They drive the real `MigoClient` — real transport handshake against a controlled socket —
 * so the client's own listener wiring, not a hand-called private method, is what updates the
 * cache.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { MigoClient } from '../src/client.js';
import type { Grant } from '../src/index.js';
import { decodeBody, encodeBody } from '../src/codec.js';
import { identity } from '@migo/crypto';
import {
  BandwidthMode,
  OP,
  Platform,
  ConversationKind,
  EncryptionMode,
  MemberChange,
  decodeKeyBundleRequest,
  decodeRosterReq,
  decodeSubscribeRequest,
  encodeConversationListResponse,
  encodeConversationMemberEvent,
  encodeConversationRosterResponse,
  encodeKeyBundleResponse,
  encodeKeyPublishResult,
  encodePong,
  encodeRoomMemberEvent,
  encodeRosterResponse,
  encodeSubscribeResponse,
  encodeWelcome,
} from '@migo/protocol';
import type {
  ConversationRosterEntry,
  ConversationSummary,
  KeyBundle,
  RosterEntry,
  Welcome,
} from '@migo/protocol';
import { decodeFrame, encodeFrame, frameHeader, idFromBytes } from '@migo/wire';
import type { Id } from '@migo/wire';

/** Lets pending microtasks (handshake builds, promise chains) settle without real time. */
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
 * A stand-in WebSocket the test controls frame by frame.
 *
 * The transport attaches its handlers inside `connect()`, so every socket the factory mints
 * is opened only when the test calls {@link ControlledSocket.fireOpen} and fed only the
 * frames the test delivers.
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

/** A grant whose tokens are placeholders; this test never refreshes. */
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
 * The scripted server: every request the client sends gets the reply its opcode declares, and
 * the roster answers name more members than the list row does — which is exactly how the real
 * server behaves, `MEMBER_PREVIEW` being the list's cap and the roster being the whole truth.
 */
class ScriptedServer {
  readonly socket: ControlledSocket;
  /** Every decoded request, in order, so the tests can assert call counts. */
  readonly asked: number[] = [];
  /** The accounts the roster names, as the numbers the ids were minted from. */
  roster: number[] = [];
  /** The room roster's accounts, for the room-side tests. */
  roomRoster: number[] = [];

  constructor(socket: ControlledSocket) {
    this.socket = socket;
  }

  /** Delivers a member event onto the wire, as the server's fan-out would. */
  memberEvent(userId: number, change: MemberChange): void {
    this.socket.deliver(
      encodeFrame({
        header: frameHeader(OP.CONVERSATION_MEMBER_EVENT, 0),
        payload: encodeBody(encodeConversationMemberEvent, {
          conversationId: idOf(0x5eed),
          userId: idOf(userId),
          change,
          memberCount: 2,
        }),
      }),
    );
  }

  /** Delivers a *room* member event, as the rooms fan-out would — naming the room, not the conversation. */
  roomMemberEvent(userId: number, joined: boolean): void {
    this.socket.deliver(
      encodeFrame({
        header: frameHeader(OP.ROOM_MEMBER_EVENT, 0),
        payload: encodeBody(encodeRoomMemberEvent, {
          roomId: idOf(0x100d),
          userId: idOf(userId),
          joined,
        }),
      }),
    );
  }

  /** Answers one client request frame with the protocol's reply for its opcode. */
  replyTo(raw: unknown): void {
    assert.ok(raw instanceof Uint8Array, 'the client sent a frame that is not bytes');
    const frame = decodeFrame(raw);
    const { opcode, correlation } = frame.header;
    const reply = (payload: Uint8Array): void => {
      this.socket.deliver(encodeFrame({ header: frameHeader(opcode, correlation), payload }));
    };
    // HELLO is answered by the test delivering the WELCOME by hand — a synthetic reply here
    // would be decoded as the WELCOME and fail the handshake.
    if (opcode === OP.HELLO) {
      return;
    }
    this.asked.push(opcode);
    if (opcode === OP.KEY_PUBLISH) {
      reply(
        encodeBody(encodeKeyPublishResult, {
          acceptedPrekeys: 0,
          identityFingerprint: 'probe-fingerprint',
        }),
      );
      return;
    }
    if (opcode === OP.SUBSCRIBE) {
      const request = decodeBody(decodeSubscribeRequest, frame.payload);
      reply(encodeBody(encodeSubscribeResponse, { accepted: request.topics }));
      return;
    }
    if (opcode === OP.CONVERSATION_LIST) {
      // One group row whose members field is a *preview*: the first eight of the roster.
      const summary: ConversationSummary = {
        conversationId: idOf(0x5eed),
        kind: ConversationKind.Group,
        encryption: EncryptionMode.EndToEnd,
        lastSeq: 0,
        readSeq: 0,
        title: 'The Nine',
        members: this.roster.slice(0, 8).map((n) => idOf(n)),
      };
      reply(encodeBody(encodeConversationListResponse, { conversations: [summary] }));
      return;
    }
    if (opcode === OP.CONVERSATION_ROSTER) {
      const entries: ConversationRosterEntry[] = this.roster.map((n) => ({
        accountId: idOf(n),
        role: 0,
        joinedAt: 1_700_000_000_000,
      }));
      reply(encodeBody(encodeConversationRosterResponse, { entries }));
      return;
    }
    if (opcode === OP.ROOM_ROSTER) {
      const request = decodeBody(decodeRosterReq, frame.payload);
      const limit = request.limit ?? this.roomRoster.length;
      const start =
        request.after === undefined
          ? 0
          : this.roomRoster.findIndex((n) => idOf(n) === request.after) + 1;
      const members: RosterEntry[] = this.roomRoster
        .slice(start, start + limit)
        .map((n) => ({ accountId: idOf(n), role: 0, joinedAt: 1_700_000_000_000 }));
      reply(encodeBody(encodeRosterResponse, { members }));
      return;
    }
    if (opcode === OP.KEY_BUNDLE_FETCH) {
      const request = decodeBody(decodeKeyBundleRequest, frame.payload);
      const bundle: KeyBundle = {
        userId: request.userId,
        deviceId: idOf(0x9000 | (request.userId.charCodeAt(15) & 0xff)),
        // A real generated identity's public half: the SDK parses the bundle on arrival, so
        // the key must be a valid pair of points, not filler bytes. Nothing here seals, so
        // the same identity serves every scripted account.
        identityKey: identity.IdentitySecret.generate().public().toBytes(),
        signedPrekeyId: 1,
        signedPrekey: new Uint8Array(32),
        signedPrekeySignature: new Uint8Array(64),
      };
      reply(encodeBody(encodeKeyBundleResponse, { bundles: [bundle] }));
      return;
    }
    // PING (heartbeat) and anything else: a Pong keeps the session alive.
    reply(encodeBody(encodePong, { clientTime: 0, serverTime: 1_700_000_000_000 }));
  }
}

/** A client with a live (fake) session over a scripted server. */
async function connectedClient(): Promise<{
  client: MigoClient;
  server: ScriptedServer;
}> {
  let socket: ControlledSocket | undefined;
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
    fetch: () => Promise.reject(new TypeError('unexpected call')),
  });

  const established = client.resume(grantWith());
  assert.ok(socket !== undefined, 'the transport did not build a socket');
  const server = new ScriptedServer(socket);
  const originalSend = socket.send.bind(socket);
  socket.send = (data: unknown): void => {
    originalSend(data);
    server.replyTo(data);
  };

  socket.fireOpen();
  await tick(); // the HELLO is built and sent
  socket.deliver(welcomeFrame());
  await established;
  await tick();
  return { client, server };
}

test('a list preview is not the audience: the roster is read before the first send', async () => {
  const { client, server } = await connectedClient();
  try {
    // Nine members; the list row will name only the first eight.
    server.roster = [20, 21, 22, 23, 24, 25, 26, 27, 28];
    await client.loadConversations(30);

    // The audience includes member nine, who the preview never named.
    const audience = await client.recipientDevices(idOf(0x5eed));
    const tags = new Set(audience.map((device) => device.userId));
    assert.ok(tags.has(idOf(28)), 'member nine — beyond the preview cap — is in the audience');
    // Exactly one roster read for the promotion, not one per enumeration.
    assert.equal(
      server.asked.filter((opcode) => opcode === OP.CONVERSATION_ROSTER).length,
      1,
      'the preview was promoted by exactly one roster read',
    );

    // The steady state: no further roster reads, the same full audience every time.
    const again = await client.recipientDevices(idOf(0x5eed));
    assert.equal(again.length, audience.length);
    assert.equal(
      server.asked.filter((opcode) => opcode === OP.CONVERSATION_ROSTER).length,
      1,
      'a complete cache is not re-read on every send',
    );
  } finally {
    await client.disconnect();
  }
});

test('a member event updates the cache, so the audience follows the group', async () => {
  const { client, server } = await connectedClient();
  try {
    // Three members; the list row names them all (under the cap), so the only cache updates
    // under test are the ones the member events apply.
    server.roster = [20, 21, 22];
    await client.loadConversations(30);

    // Promote the cache to complete first, so the event's patch is the only change.
    await client.recipientDevices(idOf(0x5eed));
    assert.equal(server.asked.filter((opcode) => opcode === OP.CONVERSATION_ROSTER).length, 1);

    // An invite lands: member 29 joins. The next audience includes them, with no roster
    // re-read — the live event is the cheaper truth, and the fix is that it is applied.
    server.memberEvent(29, MemberChange.Joined);
    await tick();
    const joined = await client.recipientDevices(idOf(0x5eed));
    assert.ok(
      joined.some((device) => device.userId === idOf(29)),
      'the joined member is in the next audience',
    );
    assert.equal(
      server.asked.filter((opcode) => opcode === OP.CONVERSATION_ROSTER).length,
      1,
      'a join the live stream reported needs no roster re-read',
    );

    // A departure: member 20 leaves. The next audience drops them — a sender key sealed
    // after this must not reach a member the group has lost.
    server.memberEvent(20, MemberChange.Left);
    await tick();
    const departed = await client.recipientDevices(idOf(0x5eed));
    assert.ok(
      !departed.some((device) => device.userId === idOf(20)),
      'the departed member is out of the next audience',
    );
  } finally {
    await client.disconnect();
  }
});

test('an incomplete cache is not patched into a wrong answer', async () => {
  const { client, server } = await connectedClient();
  try {
    // Ten members, the list row names eight: the cache is a preview, and a join applied
    // onto it must not promote it — the roster read still happens, and it is the roster's
    // membership that answers, never the preview patched with one event.
    server.roster = [20, 21, 22, 23, 24, 25, 26, 27, 28, 29];
    await client.loadConversations(30);

    server.memberEvent(30, MemberChange.Joined);
    await tick();
    const audience = await client.recipientDevices(idOf(0x5eed));
    const tags = new Set(audience.map((device) => device.userId));
    // Ten roster members plus this account's own other devices — the assertion is that the
    // answer is the roster whole, not the eight the preview named plus one.
    for (const n of [20, 21, 22, 23, 24, 25, 26, 27, 28, 29]) {
      assert.ok(tags.has(idOf(n)), `roster member ${n} is in the audience`);
    }
    assert.ok(
      !tags.has(idOf(30)),
      'the event arrives before the roster the promotion reads, and the roster is the answer',
    );
  } finally {
    await client.disconnect();
  }
});

test('a joined room is primed: the roster pages in, and both topics are watched', async () => {
  const { client, server } = await connectedClient();
  try {
    // A room of five, paged two at a time: the page cap forces the loop to walk three pages.
    server.roomRoster = [40, 41, 42, 43, 44];
    await client.startRoomConversation(idOf(0x5eed), idOf(0x100d), 2);

    const audience = await client.recipientDevices(idOf(0x5eed));
    const tags = new Set(audience.map((device) => device.userId));
    for (const n of [40, 41, 42, 43, 44]) {
      assert.ok(tags.has(idOf(n)), `room member ${n} is in the audience with no roster re-read`);
    }
    // Three pages asked (2+2+1), and then the audience answered from the cache: the fourth
    // CONVERSATION_ROSTER the list row's preview might have caused never happens, because the
    // room's own roster already primed the membership complete.
    assert.equal(
      server.asked.filter((opcode) => opcode === OP.ROOM_ROSTER).length,
      3,
      'the roster paged until a short page',
    );
    assert.equal(
      server.asked.filter((opcode) => opcode === OP.CONVERSATION_ROSTER).length,
      0,
      'a membership primed from the room roster needs no conversation roster read',
    );
  } finally {
    await client.disconnect();
  }
});

test('a room member event folds the joiner into the audience, so old members seal for them', async () => {
  const { client, server } = await connectedClient();
  try {
    server.roomRoster = [40, 41];
    await client.startRoomConversation(idOf(0x5eed), idOf(0x100d), 50);

    // A new member joins the room: the event names the room, and the cache it must reach is
    // the conversation's. Without the bridge the next audience would omit the joiner — the
    // exact "new user sends, old members never receive" shape, in miniature.
    server.roomMemberEvent(42, true);
    await tick();
    const audience = await client.recipientDevices(idOf(0x5eed));
    assert.ok(
      audience.some((device) => device.userId === idOf(42)),
      'the room joiner is in the next audience',
    );

    // And a departure drops them again, with no roster re-read either way.
    server.roomMemberEvent(42, false);
    await tick();
    const after = await client.recipientDevices(idOf(0x5eed));
    assert.ok(
      !after.some((device) => device.userId === idOf(42)),
      'the room leaver is out of the next audience',
    );
  } finally {
    await client.disconnect();
  }
});
