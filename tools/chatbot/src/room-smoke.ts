/**
 * Migo fresh-database smoke test: register → login → create room → chat.
 *
 * The full user path over one node, in the order a person actually meets it:
 * register two accounts, prove the *login* path separately by signing alice
 * back in on a brand-new client, have alice create a public room, have bob
 * join it, and exchange round-trip messages inside the room's conversation
 * so both ends decrypt what the other sent.
 *
 * Environment variables:
 *   MIGOD_API_URL        REST origin (default: http://localhost:8080)
 *   BOT_ROUNDS           round-trip messages in the room (default: 3)
 */

import { setTimeout as sleep } from 'node:timers/promises';
import { randomBytes } from 'node:crypto';

import {
  MigoClient,
  ContentType,
  ConversationKind,
  RoomKind,
  serverEndpointFromUrl,
  type IncomingMessage,
} from '@migo/sdk';

const API_URL = process.env.MIGOD_API_URL ?? 'http://localhost:8080';
const ROUNDS = Number.parseInt(process.env.BOT_ROUNDS ?? '3', 10);
const APP_VERSION = '0.1.0';
const LOCALE = 'en-US';
/** One message has this long to land on the other side before the run fails. */
const DELIVERY_TIMEOUT_MS = 30_000;

function ts() {
  return new Date().toISOString();
}

function log(scope: string, message: string) {
  console.log(`[${ts()}] [${scope}] ${message}`);
}

function fail(scope: string, message: string): never {
  console.error(`[${ts()}] [${scope}] FAIL: ${message}`);
  process.exit(1);
}

function makeClient(displayName: string): MigoClient {
  return MigoClient.create({
    server: serverEndpointFromUrl(API_URL),
    deviceDisplayName: displayName,
    hello: {
      platform: 4, // Desktop — Platform enum
      appVersion: APP_VERSION,
      locale: LOCALE,
      bandwidthMode: 0, // Auto
    },
  });
}

function makeCollector(scope: string) {
  const received: IncomingMessage[] = [];
  return {
    received,
    install(client: MigoClient): () => void {
      return client.messaging.onMessage((m) => {
        const text = m.content.type === ContentType.Text ? m.content.text : '<non-text>';
        log(scope, `inbound seq=${m.seq} text="${text}"`);
        received.push(m);
      });
    },
  };
}

/** Waits until `condition` holds, or fails the run after the delivery timeout. */
async function awaitDelivery(scope: string, what: string, condition: () => boolean): Promise<void> {
  const deadline = Date.now() + DELIVERY_TIMEOUT_MS;
  while (!condition()) {
    if (Date.now() > deadline) {
      fail(scope, `${what} did not arrive within ${DELIVERY_TIMEOUT_MS}ms`);
    }
    await sleep(20);
  }
}

async function main(): Promise<void> {
  const stamp = Date.now().toString(36);
  const suffix = randomBytes(2).toString('hex');
  const aliceUsername = `alice_${stamp}_${suffix}`;
  const bobUsername = `bob_${stamp}_${suffix}`;
  const password = `correct-horse-battery-staple-${randomBytes(4).toString('hex')}`;
  log('boot', `target ${API_URL}, ${ROUNDS} rounds`);

  // 1. Register: two fresh accounts on the node.
  log('register', `registering ${aliceUsername} and ${bobUsername}`);
  const aliceClient = makeClient('alice_bot');
  await aliceClient.register({ username: aliceUsername, password, locale: LOCALE });
  const aliceAccountId = aliceClient.accountId;
  await aliceClient.disconnect();

  const bobClient = makeClient('bob_bot');
  await bobClient.register({ username: bobUsername, password, locale: LOCALE });
  log('register', `accounts ready: alice=${aliceAccountId} bob=${bobClient.accountId}`);

  // 2. Login: alice signs back in on a brand-new client — the credential path,
  //    exercised separately from registration.
  log('login', `signing ${aliceUsername} back in on a fresh client`);
  const aliceAgain = makeClient('alice_second_device');
  await aliceAgain.login({ identifier: aliceUsername, password });
  if (aliceAgain.accountId !== aliceAccountId) {
    fail('login', `expected account ${aliceAccountId}, got ${aliceAgain.accountId}`);
  }
  log('login', `alice back in as ${aliceAgain.accountId}`);

  const aliceReceived = makeCollector('alice');
  const bobReceived = makeCollector('bob');
  aliceReceived.install(aliceAgain);
  bobReceived.install(bobClient);

  // 3. Create room: alice founds a public room; creation is entry.
  const slug = `smoke-${stamp}-${suffix}`;
  log('room', `creating room "${slug}"`);
  const joined = await aliceAgain.rooms.create(
    slug,
    'Smoke Room',
    RoomKind.Public,
    'fresh database smoke test',
  );
  const roomId = joined.room.roomId;
  const conversationId = joined.conversationId;
  log('room', `room ${roomId} → conversation ${conversationId} (encryption=${joined.encryption})`);

  // 4. Bob joins the room.
  log('room', `bob joining ${roomId}`);
  const bobJoined = await bobClient.rooms.join(roomId);
  if (bobJoined.conversationId !== conversationId) {
    fail(
      'room',
      `bob's conversation ${bobJoined.conversationId} differs from alice's ${conversationId}`,
    );
  }
  log('room', 'bob joined the room');

  // Both sides subscribe to the conversation topic and prime the membership
  // cache the sender-key distribution needs. The roster is the member truth.
  const roster = await aliceAgain.rooms.getRoster(roomId, 100);
  const members = roster.map((entry) => entry.accountId);
  log('room', `roster: ${members.length} member(s)`);
  for (const client of [aliceAgain, bobClient]) {
    client.rememberConversation({
      conversationId,
      kind: ConversationKind.Room,
      encryption: joined.encryption,
      lastSeq: joined.lastSeq,
      readSeq: 0,
      members,
    });
    await client.watchConversation(conversationId);
  }

  // 5. Chat: N round trips inside the room, each message verified on the
  //    other side before the reply goes out.
  let aliceSent = 0;
  let bobSent = 0;
  for (let i = 0; i < ROUNDS; i++) {
    const aliceText = `alice round ${i + 1} of ${ROUNDS}`;
    await aliceAgain.messaging.send(conversationId, { type: ContentType.Text, text: aliceText });
    aliceSent += 1;
    log('alice', `sent "${aliceText}"`);
    await awaitDelivery(
      'bob',
      `round ${i + 1} message from alice`,
      () => bobReceived.received.length >= aliceSent,
    );

    const bobText = `bob round ${i + 1} of ${ROUNDS}`;
    await bobClient.messaging.send(conversationId, { type: ContentType.Text, text: bobText });
    bobSent += 1;
    log('bob', `sent "${bobText}"`);
    await awaitDelivery(
      'alice',
      `round ${i + 1} reply from bob`,
      () => aliceReceived.received.length >= bobSent,
    );
  }

  if (aliceReceived.received.length !== bobSent) {
    fail('result', `alice received ${aliceReceived.received.length}, expected ${bobSent}`);
  }
  if (bobReceived.received.length !== aliceSent) {
    fail('result', `bob received ${bobReceived.received.length}, expected ${aliceSent}`);
  }

  log(
    'result',
    `register ✓ login ✓ room "${slug}" ✓ chat: ${ROUNDS} round trips, both sides decrypted every message`,
  );

  await aliceAgain.disconnect();
  await bobClient.disconnect();
}

main().then(
  () => {
    process.exit(0);
  },
  (error) => {
    console.error(`[${ts()}] [fatal]`, error);
    process.exit(1);
  },
);
