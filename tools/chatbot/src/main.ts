/* eslint-disable no-console */
/**
 * Migo two-account chat bot.
 *
 * Registers two fresh accounts on the configured node, opens a 1:1
 * conversation between them, and sends N round-trip messages (default
 * 10) so each side sees every message land and decrypt. The bot is
 * intentionally tiny: one round trip is a single message from alice
 * followed by a single reply from bob, and we count every inbound
 * frame on both ends to confirm the path through the gateway.
 *
 * Environment variables:
 *   MIGOD_API_URL        REST origin (e.g. http://localhost:18080)
 *   MIGOD_GATEWAY_URL    WebSocket origin (e.g. ws://localhost:18080/ws)
 *   BOT_ALICE_USERNAME   username for the first account (default: alice-<ts>)
 *   BOT_ALICE_PASSWORD   password for the first account
 *   BOT_BOB_USERNAME     username for the second account
 *   BOT_BOB_PASSWORD     password for the second account
 *   BOT_ROUNDS           how many round-trip messages (default: 10)
 */

import { setTimeout as sleep } from 'node:timers/promises';
import { randomBytes } from 'node:crypto';

import {
  MigoClient,
  ContentType,
  ConversationKind,
  EncryptionMode,
  type IncomingMessage,
  type Id,
} from '@migo/sdk';

const API_URL = process.env.MIGOD_API_URL ?? 'http://localhost:18080';
const GATEWAY_URL = process.env.MIGOD_GATEWAY_URL ?? 'ws://localhost:18080/ws';
const ROUNDS = Number.parseInt(process.env.BOT_ROUNDS ?? '10', 10);
const APP_VERSION = '0.1.0';
const LOCALE = 'en-US';

function ts() {
  return new Date().toISOString();
}

function log(scope: string, message: string) {
  console.log(`[${ts()}] [${scope}] ${message}`);
}

interface AccountSpec {
  username: string;
  password: string;
  displayName: string;
}

function envAccount(label: 'alice' | 'bob'): AccountSpec {
  // Username must be ASCII lowercase, digits, '.' or '_'. The random suffix
  // here is therefore hex (lowercase) plus a base36 timestamp; together the
  // string stays inside that alphabet.
  const stamp = Date.now().toString(36);
  const suffix = randomBytes(2).toString('hex');
  const username =
    (label === 'alice' ? process.env.BOT_ALICE_USERNAME : process.env.BOT_BOB_USERNAME) ??
    `${label}_${stamp}_${suffix}`;
  const password =
    (label === 'alice' ? process.env.BOT_ALICE_PASSWORD : process.env.BOT_BOB_PASSWORD) ??
    `correct-horse-battery-staple-${randomBytes(4).toString('hex')}`;
  return { username, password, displayName: `${label}_bot` };
}

interface InboundCollector {
  received: IncomingMessage[];
  install(client: MigoClient): () => void;
}

function makeCollector(scope: string): InboundCollector {
  const received: IncomingMessage[] = [];
  return {
    received,
    install(client) {
      return client.messaging.onMessage((m) => {
        const text = m.content.type === ContentType.Text ? m.content.text : '<non-text>';
        log(scope, `inbound seq=${m.seq} text="${text}"`);
        received.push(m);
      });
    },
  };
}

async function openOrCreateDirect(alice: MigoClient, bob: MigoClient, bobId: Id): Promise<Id> {
  const existing = await alice.loadConversations(20);
  for (const summary of existing.conversations) {
    if (summary.kind === ConversationKind.Direct && summary.members?.includes(bobId)) {
      log('alice', `reusing existing conversation ${summary.conversationId}`);
      // Bob still has to subscribe on his own connection — the gateway's
      // fan-out is keyed by per-session SUBSCRIBE, so a missed subscribe
      // here would mean his MESSAGE_EVENT never arrives.
      await bob.watchConversation(summary.conversationId);
      return summary.conversationId;
    }
  }
  const summary = await alice.startConversation(ConversationKind.Direct, [bobId]);
  // Bob must subscribe to the conversation topic on his own connection —
  // the gateway's fan-out only reaches sessions that have an active
  // SUBSCRIBE for the topic, and only alice's `startConversation` has
  // wired that up so far. Without this, bob's first MESSAGE_EVENT would
  // never reach him and the round-trip would deadlock.
  await bob.watchConversation(summary.conversationId);
  log('alice', `created conversation ${summary.conversationId}`);
  return summary.conversationId;
}

async function main(): Promise<void> {
  const alice = envAccount('alice');
  const bob = envAccount('bob');
  log('boot', `target ${API_URL}, ${ROUNDS} rounds`);

  // Build the two clients. Each gets its own key material (the SDK mints
  // a fresh keystore on construction), so the sender-key distribution that
  // `messaging.send` performs on first use is a real-world shape.
  const aliceClient = MigoClient.create({
    baseUrl: API_URL,
    gatewayUrl: GATEWAY_URL,
    deviceDisplayName: alice.displayName,
    hello: {
      platform: 4, // Desktop — Platform enum
      appVersion: APP_VERSION,
      locale: LOCALE,
      bandwidthMode: 0, // Auto
    },
  });
  const bobClient = MigoClient.create({
    baseUrl: API_URL,
    gatewayUrl: GATEWAY_URL,
    deviceDisplayName: bob.displayName,
    hello: {
      platform: 4,
      appVersion: APP_VERSION,
      locale: LOCALE,
      bandwidthMode: 0,
    },
  });

  const aliceReceived = makeCollector('alice');
  const bobReceived = makeCollector('bob');
  let unsubAlice: (() => void) | null = null;
  let unsubBob: (() => void) | null = null;
  let bobSawAliceFirst = false;
  let aliceSawBobFirst = false;

  // Register the two accounts. The server allows registration in
  // development; production refuses. We install the inbound listeners
  // *after* the gate opens, because the `messaging` getter throws before
  // the client is connected.
  log('boot', `registering ${alice.username} and ${bob.username}`);
  await aliceClient.register({
    username: alice.username,
    password: alice.password,
    locale: LOCALE,
  });
  await bobClient.register({
    username: bob.username,
    password: bob.password,
    locale: LOCALE,
  });
  log('boot', `accounts ready: alice=${aliceClient.accountId} bob=${bobClient.accountId}`);

  // Now both clients are connected — install the inbound listeners.
  unsubAlice = aliceReceived.install(aliceClient);
  unsubBob = bobReceived.install(bobClient);
  aliceClient.messaging.onMessage((m) => {
    if (m.senderId === bobClient.accountId && !aliceSawBobFirst) {
      aliceSawBobFirst = true;
      log('alice', `first message from bob arrived at seq=${m.seq}`);
    }
  });
  bobClient.messaging.onMessage((m) => {
    if (m.senderId === aliceClient.accountId && !bobSawAliceFirst) {
      bobSawAliceFirst = true;
      log('bob', `first message from alice arrived at seq=${m.seq}`);
    }
  });

  // Open (or reuse) a 1:1 conversation between the two.
  const conversationId = await openOrCreateDirect(aliceClient, bobClient, bobClient.accountId);

  // Each side needs its own membership cache for the sender-key distribution
  // that `messaging.send` performs. Alice's cache is primed by
  // `startConversation`; bob's is primed by reading the same conversation
  // summary through his own session.
  bobClient.rememberConversation({
    conversationId,
    kind: ConversationKind.Direct,
    encryption: EncryptionMode.EndToEnd,
    lastSeq: 0,
    readSeq: 0,
    members: [aliceClient.accountId, bobClient.accountId],
  });

  // Send N round-trip messages. Each side sends one, the other receives
  // it, the loop counts the round trip.
  let aliceSent = 0;
  let bobSent = 0;
  for (let i = 0; i < ROUNDS; i++) {
    const aliceText = `alice round ${i + 1} of ${ROUNDS}`;
    await aliceClient.messaging.send(conversationId, {
      type: ContentType.Text,
      text: aliceText,
    });
    aliceSent += 1;
    log('alice', `sent "${aliceText}"`);

    // Give the server a moment to fan the frame out before bob replies.
    // In a tighter setup we would await the inbound; the small sleep keeps
    // the script readable and exercises the real-time path end to end.
    await sleep(50);
    while (bobReceived.received.length < aliceSent) {
      await sleep(10);
    }

    const bobText = `bob round ${i + 1} of ${ROUNDS}`;
    await bobClient.messaging.send(conversationId, {
      type: ContentType.Text,
      text: bobText,
    });
    bobSent += 1;
    log('bob', `sent "${bobText}"`);

    await sleep(50);
    while (aliceReceived.received.length < bobSent) {
      await sleep(10);
    }
  }

  // Sanity-check: the two clients should have received the same number
  // of inbound messages, and that number equals the rounds sent by the
  // other side.
  const expectedAlice = bobSent;
  const expectedBob = aliceSent;
  if (aliceReceived.received.length !== expectedAlice) {
    throw new Error(
      `alice expected ${expectedAlice} inbound messages, got ${aliceReceived.received.length}`,
    );
  }
  if (bobReceived.received.length !== expectedBob) {
    throw new Error(
      `bob expected ${expectedBob} inbound messages, got ${bobReceived.received.length}`,
    );
  }

  log('result', `alice sent ${aliceSent}, received ${aliceReceived.received.length}`);
  log('result', `bob sent ${bobSent}, received ${bobReceived.received.length}`);
  log('result', `${ROUNDS} round trips completed across two clients on one node`);

  unsubAlice?.();
  unsubBob?.();
  await aliceClient.disconnect();
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
