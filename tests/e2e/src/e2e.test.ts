/**
 * The end-to-end suite: Migo's product flows proven against a real node.
 *
 * What runs under these tests is the whole product, not a piece of it: one `migod`
 * process (started by the harness with a real PostgreSQL database, a real Redis, and a
 * real filesystem media directory), and on the other side of the socket the TypeScript
 * SDK's `MigoClient` — the same code a browser runs, executing here on Node, which is
 * the one client of the four that gives true end-to-end coverage with no new machinery:
 * no display, no emulator, no second protocol stack to keep honest. The Android and
 * desktop clients are builds, not different wire protocols; what this suite proves about
 * the bytes is true for all of them.
 *
 * Every scenario ends in an assertion about observable truth — a message that arrived
 * with the exact plaintext and sequence the sender's acknowledgement named, a media byte
 * that round-tripped, a metric that moved, history that survived a node restart — never
 * merely "the call did not throw".
 *
 * What is deliberately not here: the fuzz, load, stress and security suites of brief
 * section 172 (they run in their own crates and in the loadgen, and duplicating them here
 * would make two owners of one contract), and the multi-node failure scenarios of section
 * 173, which already run as deterministic tests in `server/crates/migod/tests/`. This
 * suite is the single-node product path: everything a person does with one node.
 *
 * The nine tests run in one file on purpose: they share one migod (a RAM-starved host
 * must not pay for nine of them), they run serially, and each creates its own accounts so
 * no scenario depends on another's state — except the node itself, which is the subject
 * under test.
 */

import { after, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { setTimeout as sleep } from 'node:timers/promises';

import {
  BandwidthMode,
  ContentType,
  ConversationKind,
  EncryptionMode,
  KeyStore,
  MediaKind,
  MigoClient,
  Platform,
  ReceiptKind,
  RoomKind,
  TypingState,
  serverEndpointFromUrl,
  type Grant,
  type Id,
  type IncomingMessage,
  type MessageAccepted,
  type MessageReceipt,
  type ServerEndpoint,
  type TypingEvent,
} from '@migo/sdk';

import { NodeHarness } from './harness.js';

const APP_VERSION = '0.1.0';
const LOCALE = 'en-US';
/** One event has this long to cross the wire before the test names it missing. */
const DELIVERY_TIMEOUT_MS = 30_000;

/**
 * A real 1x1 RGBA PNG, 70 bytes, magic bytes intact.
 *
 * The node sniffs media at commit and refuses bytes it cannot identify (brief section
 * 168), so a fixture of arbitrary bytes would pass the client and die at the server;
 * a genuine PNG proves the whole path.
 */
const PNG_BASE64 =
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==';

// Created at module scope on purpose: the database, the run directory and the kill hook
// must exist before any test starts, and a load failure (no psql, no PostgreSQL) is a
// suite failure the runner reports with this file's name against it.
const harness = NodeHarness.create();

before(async () => {
  await harness.start();
});

after(async () => {
  for (const client of clients) {
    await client.disconnect().catch(() => {
      // A client whose socket already died has nothing left to close; the node is
      // torn down next regardless.
    });
  }
  await harness.destroy();
});

// --- shared helpers ---------------------------------------------------------

/** Every client this file creates, so `after` can close them all no matter how a test ended. */
const clients: MigoClient[] = [];

let userSerial = 0;

/**
 * A unique account spec. Usernames stay in the server's alphabet — lowercase, digits, `_` —
 * and inside its 32-byte cap, which is why the label is truncated to a budget rather than
 * trusted: the label is the one part whose length this file does not control (a scenario
 * name grows past the cap without any test noticing, and the failure lands at registration
 * as FIELD_TOO_LONG on a whole scenario). The suffix, not the label, carries uniqueness.
 */
function account(label: string): { username: string; passphrase: string } {
  userSerial += 1;
  const suffix = `${Date.now().toString(36)}${userSerial.toString(36)}${randomBytes(2).toString('hex')}`;
  const budget = 32 - 'e2e_'.length - '_'.length - suffix.length;
  return {
    username: `e2e_${label.slice(0, budget)}_${suffix}`,
    passphrase: `correct-horse-battery-staple-${randomBytes(4).toString('hex')}`,
  };
}

/**
 * A client pointed at the harness node.
 *
 * The gateway port is set explicitly rather than left to the endpoint derivation: on a
 * loopback host the derivation splits the ports (`gateway = rest + 1`, the dev-policy
 * default), while this node — like the repository's own single-port deployment — serves
 * `/ws` on the one port it binds. Without the override, the REST bootstrap would succeed
 * and the WebSocket would land on a port nothing listens on.
 */
function client(displayName: string, keyStore?: KeyStore): MigoClient {
  const endpoint: ServerEndpoint = {
    ...serverEndpointFromUrl(harness.apiUrl),
    gatewayPort: harness.httpPort,
  };
  const created = MigoClient.create({
    server: endpoint,
    deviceDisplayName: displayName,
    ...(keyStore === undefined ? {} : { keyStore }),
    hello: {
      platform: Platform.Desktop,
      appVersion: APP_VERSION,
      locale: LOCALE,
      bandwidthMode: BandwidthMode.Auto,
    },
  });
  clients.push(created);
  return created;
}

/** Registers a fresh account and leaves the client connected, holding its grant. */
async function register(
  label: string,
): Promise<{ client: MigoClient; grant: Grant; username: string; passphrase: string }> {
  const credentials = account(label);
  const created = client(label);
  const grant = await created.register({
    username: credentials.username,
    passphrase: credentials.passphrase,
    locale: LOCALE,
  });
  return { client: created, grant, ...credentials };
}

/**
 * Waits until `condition` holds, or fails with what was waited for.
 *
 * `detail`, when given, adds scenario-specific state to the failure — what a collector
 * actually received, which turns "the message never arrived" into "three other messages
 * did, and here are their ids" — and the node's log tail follows, so a failure names
 * both sides of the socket before a person has to go looking.
 */
async function until(
  what: string,
  condition: () => boolean,
  timeoutMs = DELIVERY_TIMEOUT_MS,
  detail?: () => string,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() > deadline) {
      throw new Error(
        `${what} did not happen within ${timeoutMs}ms` +
          (detail === undefined ? '' : `\n${detail()}`) +
          `\n--- migod log tail ---\n${harness.logTail()}`,
      );
    }
    await sleep(20);
  }
}

/**
 * One scenario: its own accounts and clients, closed whether it passes or fails.
 *
 * The scenarios share one node on purpose — a RAM-starved host must not pay for nine
 * of them — but sharing the node is not sharing sessions. A scenario that fails part
 * way through must not leave its clients connected, because the next scenario reads
 * the same node's gauges and counters, and a leaked session from an earlier failure
 * moves the live-session gauge for reasons that have nothing to do with the scenario
 * being run. Everything a scenario opened is closed in a `finally`, so a timeout
 * cannot poison the scenarios after it.
 */
function scenario(name: string, body: () => Promise<void>): void {
  test(name, async () => {
    const firstOwnClient = clients.length;
    try {
      await body();
    } finally {
      for (const created of clients.slice(firstOwnClient)) {
        await created.disconnect().catch(() => {
          // A client whose socket already died has nothing left to close.
        });
      }
      clients.length = firstOwnClient;
    }
  });
}

/** Collects every inbound message a client decrypts, for exact-after-the-fact assertions. */
function collector(): { received: IncomingMessage[]; install(target: MigoClient): void } {
  const received: IncomingMessage[] = [];
  return {
    received,
    install(target: MigoClient): void {
      target.messaging.onMessage((message) => {
        received.push(message);
      });
    },
  };
}

/**
 * Makes two accounts friends.
 *
 * A fresh account is private by default (`who_can_message` starts at Friends), so a
 * direct conversation between strangers is refused at the door. Friending is the path
 * every real pair of users walks before their first message.
 */
async function befriend(requester: MigoClient, responder: MigoClient): Promise<void> {
  await requester.social.friendRequest(responder.accountId);
  await responder.social.friendRespond(requester.accountId, true);
}

/** Opens a 1:1 conversation between two friends, with both sides subscribed and primed. */
async function openDirect(a: MigoClient, b: MigoClient): Promise<Id> {
  await befriend(a, b);
  const summary = await a.startConversation(ConversationKind.Direct, [b.accountId]);
  const conversationId = summary.conversationId;
  // The creator is subscribed by `startConversation`; the peer must subscribe on its own
  // connection, and prime the membership cache the sender-key distribution reads.
  await b.watchConversation(conversationId);
  b.rememberConversation({
    conversationId,
    kind: ConversationKind.Direct,
    encryption: EncryptionMode.EndToEnd,
    lastSeq: summary.lastSeq,
    readSeq: summary.readSeq,
    members: [a.accountId, b.accountId],
  });
  return conversationId;
}

/** The plaintext of an inbound message, or null when it is not a text message. */
function textOf(message: IncomingMessage): string | null {
  return message.content.type === ContentType.Text ? message.content.text : null;
}

/** The `/v1/config` document, as far as these tests read it. */
interface ConfigDocument {
  node: { id: string };
  features: number;
  limits: { allow_registration: boolean };
  captcha: { enabled: boolean };
}

/** One named value out of the Prometheus text the node renders at `/metrics`. */
async function metric(name: string): Promise<number | undefined> {
  const response = await fetch(`${harness.apiUrl}/metrics`);
  assert.ok(response.ok, `/metrics answered ${response.status}`);
  const text = await response.text();
  // A series renders either bare (`name 0`) or labelled (`name{outcome="ok"} 0`).
  // Most of the gateway's counters are by-reason series and therefore labelled, so
  // matching only the bare form made a series that is registered and zero look
  // absent — the exact failure the first CI run of the contract scenario hit.
  const line = text
    .split('\n')
    .find((candidate) => candidate.startsWith(`${name} `) || candidate.startsWith(`${name}{`));
  if (line === undefined) {
    return undefined;
  }
  const value = line.slice(line.lastIndexOf(' ') + 1);
  return Number.parseInt(value, 10);
}

/**
 * Reads a gauge once it has stopped moving: two consecutive reads, a second apart, agree.
 *
 * A gauge that a moment ago counted sessions being reaped is not a baseline. The previous
 * scenario's disconnects resolve on the client before the node frees the slots — the close
 * frame crosses the wire, the gateway notices, the gauge drops a beat later — and a sample
 * taken inside that beat sees a number that is about to change for reasons that have
 * nothing to do with whatever comes next. The next scenario's session and the late reap
 * then cancel out and a `moved above the baseline` assertion can never fire, even though
 * the gauge is tracking every session perfectly. Settling first makes the value the floor
 * the scenario's own traffic moves from, and nothing else's.
 */
async function settledMetric(name: string): Promise<number | undefined> {
  const deadline = Date.now() + DELIVERY_TIMEOUT_MS;
  let previous = await metric(name);
  for (;;) {
    await sleep(1000);
    const current = await metric(name);
    if (current === previous) {
      return current;
    }
    previous = current;
    if (Date.now() > deadline) {
      throw new Error(
        `the ${name} gauge never settled within ${DELIVERY_TIMEOUT_MS}ms ` +
          `(last value: ${current})`,
      );
    }
  }
}

/**
 * Polls a metric until it satisfies `accepts`, returning the value that did.
 *
 * Gauges move when the node processes the event, which is after the call that caused it
 * resolved — the honest way to wait for that is to watch the metric itself, the same
 * surface an operator's alarm watches.
 */
async function untilMetric(
  name: string,
  accepts: (value: number | undefined) => boolean,
  what: string,
): Promise<number | undefined> {
  const deadline = Date.now() + DELIVERY_TIMEOUT_MS;
  for (;;) {
    const value = await metric(name);
    if (accepts(value)) {
      return value;
    }
    if (Date.now() > deadline) {
      throw new Error(
        `${what} did not happen within ${DELIVERY_TIMEOUT_MS}ms (last value: ${value})`,
      );
    }
    await sleep(100);
  }
}

// --- the scenarios ----------------------------------------------------------

scenario('the node answers its public contract before any client arrives', async () => {
  // /health and /ready: the two doors the harness itself waited on, checked here as a
  // product fact rather than a startup detail.
  const health = await fetch(`${harness.apiUrl}/health`);
  assert.equal(health.status, 200, '/health');
  const ready = await fetch(`${harness.apiUrl}/ready`);
  assert.equal(ready.status, 200, '/ready');

  // /v1/config: the document a client reads before it opens a socket. The node id must
  // be the one this run configured, registration must be open (the suite registers), and
  // the feature bits must be a real u64 rather than an absent field.
  const configResponse = await fetch(`${harness.apiUrl}/v1/config`);
  assert.equal(configResponse.status, 200, '/v1/config');
  const config = (await configResponse.json()) as ConfigDocument;
  assert.equal(
    config.node.id,
    harness.nodeId,
    'the node names the identity it was configured with',
  );
  assert.equal(config.limits.allow_registration, true, 'registration is open on this node');
  assert.equal(typeof config.features, 'number', 'the feature bits are published');
  assert.equal(config.captcha.enabled, true, 'the captcha service is on, as its default says');

  // /metrics: brief section 174 requires every series to be registered at zero at
  // startup, because an alarm waiting on a series that only appears on its first event
  // fails silently instead of wrongly. Before any client has connected is the one
  // moment that rule can be observed directly.
  for (const series of [
    'migo_gateway_sessions_live',
    'migo_gateway_frames_in_total',
    'migo_gateway_frames_out_total',
    'migo_gateway_sessions_opened_total',
    'migo_gateway_resume_total',
  ]) {
    const value = await metric(series);
    assert.notEqual(value, undefined, `${series} is registered at startup`);
    assert.equal(value, 0, `${series} starts at zero before any session exists`);
  }
});

scenario('two accounts register, befriend, and chat over the real gateway', async () => {
  const alice = await register('alice');
  const aliceAccountId = alice.client.accountId;

  // The credential path, exercised separately from registration: alice signs back in
  // on a second device, and the account she lands on is the one she registered.
  await alice.client.disconnect();
  const aliceSecond = client('alice_second_device');
  await aliceSecond.login({ identifier: alice.username, passphrase: alice.passphrase });
  assert.equal(
    aliceSecond.accountId,
    aliceAccountId,
    'login returns the account that was registered',
  );

  const bob = await register('bob');
  const conversationId = await openDirect(aliceSecond, bob.client);

  const aliceHeard = collector();
  const bobHeard = collector();
  aliceHeard.install(aliceSecond);
  bobHeard.install(bob.client);

  // Five round trips. Each send is asserted against its own acknowledgement — the
  // message id and the sequence the server assigned — on the far side, which is a
  // stronger claim than "some messages eventually arrived".
  const acks: MessageAccepted[] = [];
  const rounds = 5;
  for (let round = 1; round <= rounds; round += 1) {
    const aliceText = `alice round ${round} of ${rounds}`;
    const aliceAck = await aliceSecond.messaging.send(conversationId, {
      type: ContentType.Text,
      text: aliceText,
    });
    acks.push(aliceAck);
    await until(`bob receives alice's round ${round}`, () =>
      bobHeard.received.some((message) => message.messageId === aliceAck.messageId),
    );
    const atBob = bobHeard.received.find((message) => message.messageId === aliceAck.messageId);
    assert.ok(atBob !== undefined);
    assert.equal(textOf(atBob), aliceText, 'bob decrypts the exact plaintext alice sealed');
    assert.equal(atBob.senderId, aliceAccountId, 'the message names its sender');
    assert.equal(atBob.seq, aliceAck.seq, 'the seq bob saw is the seq the server acknowledged');

    const bobText = `bob round ${round} of ${rounds}`;
    const bobAck = await bob.client.messaging.send(conversationId, {
      type: ContentType.Text,
      text: bobText,
    });
    acks.push(bobAck);
    await until(`alice receives bob's round ${round}`, () =>
      aliceHeard.received.some((message) => message.messageId === bobAck.messageId),
    );
    const atAlice = aliceHeard.received.find((message) => message.messageId === bobAck.messageId);
    assert.ok(atAlice !== undefined);
    assert.equal(textOf(atAlice), bobText, 'alice decrypts the exact plaintext bob sealed');
    assert.equal(atAlice.seq, bobAck.seq, 'the seq alice saw is the seq the server acknowledged');
  }

  // The sequencer's own promises, not a guess at its starting point. A conversation's
  // sequence space is shared by every event in it: the sender-key distributions that
  // ride ahead of a member's first message are KeyExchange events on the same
  // sequencer (the SDK's watermark counts them as events, exactly like tombstones),
  // so the first text from each member does not land on seq 1 and the sequence of
  // *messages* alone is not gapless. What section 92 actually promises — a sequence
  // number is never reused and never handed out out of order — is asserted here
  // against the acknowledgements themselves, which the per-round checks above already
  // tied one-to-one to what the far side received.
  const seqs = acks.map((ack) => ack.seq);
  assert.equal(new Set(seqs).size, seqs.length, 'no sequence number is ever reused');
  assert.deepEqual(
    seqs,
    [...seqs].sort((a, b) => a - b),
    'acknowledged sequence numbers never go backwards',
  );
});

scenario('a group conversation fans out to every member', async () => {
  const alice = await register('group_alice');
  const bob = await register('group_bob');
  const carol = await register('group_carol');

  // Every pair friends the others: the create door applies each member's privacy
  // settings, and a scenario that depended on which pairs happened to be friends would
  // be testing luck rather than the fanout.
  await befriend(alice.client, bob.client);
  await befriend(alice.client, carol.client);
  await befriend(bob.client, carol.client);

  const summary = await alice.client.startConversation(
    ConversationKind.Group,
    [bob.client.accountId, carol.client.accountId],
    { title: 'e2e group' },
  );
  const conversationId = summary.conversationId;
  const members = summary.members ?? [
    alice.client.accountId,
    bob.client.accountId,
    carol.client.accountId,
  ];

  // The creator is subscribed by `startConversation`; the other two subscribe and prime
  // their membership caches from the create answer, which named the whole membership.
  for (const member of [bob.client, carol.client]) {
    await member.watchConversation(conversationId);
    member.rememberConversation({
      conversationId,
      kind: ConversationKind.Group,
      encryption: summary.encryption,
      lastSeq: summary.lastSeq,
      readSeq: summary.readSeq,
      members,
    });
  }

  const aliceHeard = collector();
  const bobHeard = collector();
  const carolHeard = collector();
  aliceHeard.install(alice.client);
  bobHeard.install(bob.client);
  carolHeard.install(carol.client);

  // One message from the founder must reach both other members with the same seq.
  const aliceText = 'group: hello from the founder';
  const aliceAck = await alice.client.messaging.send(conversationId, {
    type: ContentType.Text,
    text: aliceText,
  });
  for (const [name, heard] of [
    ['bob', bobHeard],
    ['carol', carolHeard],
  ] as const) {
    await until(`${name} receives the founder's message`, () =>
      heard.received.some((message) => message.messageId === aliceAck.messageId),
    );
    const received = heard.received.find((message) => message.messageId === aliceAck.messageId);
    assert.ok(received !== undefined);
    assert.equal(textOf(received), aliceText, `${name} decrypts the founder's plaintext`);
    assert.equal(received.seq, aliceAck.seq, `${name} sees the acknowledged seq`);
  }

  // And a reply from the third member reaches both the founder and the other member —
  // fanout is not a founder-to-members broadcast but a conversation topic.
  const carolText = 'group: reply from the third member';
  const carolAck = await carol.client.messaging.send(conversationId, {
    type: ContentType.Text,
    text: carolText,
  });
  for (const [name, heard] of [
    ['alice', aliceHeard],
    ['bob', bobHeard],
  ] as const) {
    await until(`${name} receives carol's reply`, () =>
      heard.received.some((message) => message.messageId === carolAck.messageId),
    );
    const received = heard.received.find((message) => message.messageId === carolAck.messageId);
    assert.ok(received !== undefined);
    assert.equal(textOf(received), carolText, `${name} decrypts carol's plaintext`);
    assert.equal(received.seq, carolAck.seq, `${name} sees the acknowledged seq`);
  }
});

scenario('a public room carries its own conversation', async () => {
  const dana = await register('room_dana');
  const erin = await register('room_erin');

  const slug = `e2e-room-${Date.now().toString(36)}${randomBytes(2).toString('hex')}`;
  const joined = await dana.client.rooms.create(
    slug,
    'E2E Room',
    RoomKind.Public,
    'room conversation smoke',
  );
  const conversationId = joined.conversationId;

  const erinJoined = await erin.client.rooms.join(joined.room.roomId);
  assert.equal(
    erinJoined.conversationId,
    conversationId,
    'both members see the same conversation behind the room',
  );

  // The roster is the member truth the sender-key audience is built from.
  const roster = await dana.client.rooms.getRoster(joined.room.roomId, 100);
  const members = roster.map((entry) => entry.accountId);
  assert.deepEqual([...members].sort(), [dana.client.accountId, erin.client.accountId].sort());

  for (const member of [dana.client, erin.client]) {
    member.rememberConversation({
      conversationId,
      kind: ConversationKind.Room,
      encryption: joined.encryption,
      lastSeq: joined.lastSeq,
      readSeq: 0,
      members,
    });
    await member.watchConversation(conversationId);
  }

  const danaHeard = collector();
  const erinHeard = collector();
  danaHeard.install(dana.client);
  erinHeard.install(erin.client);

  const danaText = 'room: the founder speaks';
  const danaAck = await dana.client.messaging.send(conversationId, {
    type: ContentType.Text,
    text: danaText,
  });
  await until('erin receives the founder message', () =>
    erinHeard.received.some((message) => message.messageId === danaAck.messageId),
  );
  const atErin = erinHeard.received.find((message) => message.messageId === danaAck.messageId);
  assert.ok(atErin !== undefined);
  assert.equal(textOf(atErin), danaText);
  assert.equal(atErin.seq, danaAck.seq);

  const erinText = 'room: the joiner answers';
  const erinAck = await erin.client.messaging.send(conversationId, {
    type: ContentType.Text,
    text: erinText,
  });
  await until('dana receives the joiner answer', () =>
    danaHeard.received.some((message) => message.messageId === erinAck.messageId),
  );
  const atDana = danaHeard.received.find((message) => message.messageId === erinAck.messageId);
  assert.ok(atDana !== undefined);
  assert.equal(textOf(atDana), erinText);
  assert.equal(atDana.seq, erinAck.seq);
});

scenario('typing events reach the other side of the conversation', async () => {
  const alice = await register('typing_alice');
  const bob = await register('typing_bob');
  const conversationId = await openDirect(alice.client, bob.client);

  const events: TypingEvent[] = [];
  bob.client.typing.onTyping((event) => {
    events.push(event);
  });

  await alice.client.typing.setTyping(conversationId, TypingState.Start);
  await until('bob sees alice start typing', () =>
    events.some(
      (event) =>
        event.conversationId === conversationId &&
        event.state === TypingState.Start &&
        event.userId === alice.client.accountId,
    ),
  );

  await alice.client.typing.setTyping(conversationId, TypingState.Stop);
  await until('bob sees alice stop typing', () =>
    events.some(
      (event) =>
        event.conversationId === conversationId &&
        event.state === TypingState.Stop &&
        event.userId === alice.client.accountId,
    ),
  );
});

scenario('read receipts cross the wire as watermarks', async () => {
  const alice = await register('receipt_alice');
  const bob = await register('receipt_bob');
  const conversationId = await openDirect(alice.client, bob.client);

  const bobHeard = collector();
  bobHeard.install(bob.client);

  const receipts: MessageReceipt[] = [];
  alice.client.messaging.onReceipt((receipt) => {
    receipts.push(receipt);
  });

  // Two messages, both waited for on the far side, then one read receipt naming the
  // second one's seq — the cumulative watermark, not a per-message ping.
  const first = await alice.client.messaging.send(conversationId, {
    type: ContentType.Text,
    text: 'receipt: first',
  });
  await until('bob receives the first message', () =>
    bobHeard.received.some((message) => message.messageId === first.messageId),
  );
  const second = await alice.client.messaging.send(conversationId, {
    type: ContentType.Text,
    text: 'receipt: second',
  });
  await until('bob receives the second message', () =>
    bobHeard.received.some((message) => message.messageId === second.messageId),
  );

  await bob.client.messaging.sendReceipt(conversationId, ReceiptKind.Read, second.seq);
  await until('alice receives the read receipt', () =>
    receipts.some((receipt) => receipt.kind === ReceiptKind.Read && receipt.seq === second.seq),
  );
  const receipt = receipts.find(
    (candidate) => candidate.kind === ReceiptKind.Read && candidate.seq === second.seq,
  );
  assert.ok(receipt !== undefined);
  assert.equal(receipt.conversationId, conversationId, 'the receipt names the conversation');
  assert.equal(receipt.userId, bob.client.accountId, 'the receipt names who read it');
});

scenario('media bytes round-trip through the node storage', async () => {
  const alice = await register('media_alice');
  const bob = await register('media_bob');
  const conversationId = await openDirect(alice.client, bob.client);

  const png = new Uint8Array(Buffer.from(PNG_BASE64, 'base64'));
  const key = randomBytes(32);
  const nonce = randomBytes(24);

  // Bob's listener goes in before anything is sent, not after the send resolves: the
  // SDK delivers to whoever is listening at the moment the frame lands, and a message
  // that arrives between the ack and a late `install` is not replayed to the late
  // comer — it is simply gone. This was the first CI run's failure, and it was a
  // listener race, not a delivery failure.
  const bobHeard = collector();
  bobHeard.install(bob.client);

  // The upload declares a conversation, so the fetch door can membership-check it later.
  const upload = await alice.client.media.upload(
    {
      kind: MediaKind.Image,
      contentType: 'image/png',
      size: png.length,
      conversationId,
      width: 1,
      height: 1,
    },
    png,
  );

  // The pointer travels as a message, sealed like any other content; only the members
  // can read which object it names.
  const messageAck = await alice.client.messaging.send(conversationId, {
    type: ContentType.MediaRef,
    mediaId: upload.mediaId,
    mimeType: 'image/png',
    sizeBytes: png.length,
    key,
    nonce,
  });

  await until(
    'bob receives the media message',
    () => bobHeard.received.some((message) => message.messageId === messageAck.messageId),
    DELIVERY_TIMEOUT_MS,
    () =>
      `bob has received ${bobHeard.received.length} message(s): ` +
      bobHeard.received
        .map((message) => `${message.messageId} (seq ${message.seq}, type ${message.content.type})`)
        .join(', '),
  );
  const atBob = bobHeard.received.find((message) => message.messageId === messageAck.messageId);
  assert.ok(atBob !== undefined);
  assert.equal(atBob.content.type, ContentType.MediaRef, 'the message is a media reference');
  if (atBob.content.type === ContentType.MediaRef) {
    assert.equal(atBob.content.mediaId, upload.mediaId, 'the pointer names the uploaded object');
  }

  // The other member fetches the bytes through a granted URL and gets exactly what was
  // PUT, byte for byte — the storage truth, not just an HTTP 200.
  const grant = await bob.client.media.download(upload.mediaId, conversationId);
  const response = await fetch(grant.url);
  assert.equal(response.status, 200, 'the granted URL serves');
  const served = new Uint8Array(await response.arrayBuffer());
  assert.equal(served.length, png.length, 'the object has the uploaded length');
  assert.deepEqual(served, png, 'the object is the uploaded bytes, byte for byte');
});

scenario('a node restart keeps history and lets the same device resume', async () => {
  const alice = await register('restart_alice');
  const bob = await register('restart_bob');
  const conversationId = await openDirect(alice.client, bob.client);

  const bobHeard = collector();
  bobHeard.install(bob.client);

  // Five messages, alternating senders, every acknowledgement kept: the ids and seqs
  // are the ground truth the history after the restart must still hold, every one of
  // them, whoever sent it.
  const acks: MessageAccepted[] = [];
  for (let round = 1; round <= 5; round += 1) {
    const sender = round % 2 === 0 ? bob : alice;
    const text = `restart round ${round}`;
    const ack = await sender.client.messaging.send(conversationId, {
      type: ContentType.Text,
      text,
    });
    acks.push(ack);
    await until(
      `the peer receives round ${round}`,
      () => bobHeard.received.length >= Math.ceil(round / 2),
    );
  }

  // The device's identity, snapshotted before the restart, and the grant that opens the
  // same session again — the two halves a real client persists between runs.
  const aliceKeys = alice.client.keyStore.snapshot();
  const bobKeys = bob.client.keyStore.snapshot();
  const summary = {
    conversationId,
    kind: ConversationKind.Direct as const,
    encryption: EncryptionMode.EndToEnd,
    lastSeq: acks[acks.length - 1]?.seq ?? 0,
    readSeq: 0,
    members: [alice.client.accountId, bob.client.accountId],
  };
  await alice.client.disconnect();
  await bob.client.disconnect();

  await harness.restart();

  // Alice comes back as the same device: the key store restored from the snapshot, and
  // the grant persisted from before the restart. The gateway's in-memory resume buffer
  // died with the process it lived in — what this proves is the durable half: the
  // session row and the signed token live in storage, so a client holding its grant
  // reconnects as the same device without registering or logging in again, and the
  // history it is entitled to is still there.
  const aliceBack = client('restart_alice', KeyStore.restore(aliceKeys));
  await aliceBack.resume(alice.grant);
  // Listeners before the gate opens any further — the live message below is delivered
  // through the same listener path every live delivery uses.
  const received: IncomingMessage[] = [];
  aliceBack.messaging.onMessage((message) => {
    received.push(message);
  });
  await aliceBack.watchConversation(conversationId);

  // History from zero, asserted on the raw sync response: the server's half of
  // durability, checked without decrypting a byte. Every acknowledged message — both
  // senders' — is still in the log after the restart, at its original sequence, with
  // the log ordered and free of duplication. Decryption from history is deliberately
  // not claimed here, because the product does not grant it: the pairwise sessions
  // that carried the sender-key distributions are in-memory ratchets a key-store
  // snapshot never contained (and this device was live for every message anyway, so
  // the ratchet's replay protection would refuse the re-read); and the sender-key
  // design itself refuses a device the history sealed before its distribution. What
  // a restored device can still prove — from the snapshot's seeds alone, on the wire
  // — is the live half below.
  const history = await aliceBack.catchUp(conversationId, 0);
  for (const ack of acks) {
    const event = history.messages.find((candidate) => candidate.messageId === ack.messageId);
    assert.ok(
      event !== undefined,
      `message ${ack.messageId} (seq ${ack.seq}) survived the restart in history`,
    );
    assert.equal(event.seq, ack.seq, 'history kept the message at its sequence');
  }
  const historySeqs = history.messages.map((event) => event.seq);
  assert.deepEqual(
    historySeqs,
    [...historySeqs].sort((a, b) => a - b),
    'history replays in sequence order',
  );
  assert.equal(new Set(historySeqs).size, historySeqs.length, 'no event appears twice in history');
  assert.equal(history.more, false, 'the whole log came back in one page');
  assert.ok(
    history.toSeq >= (acks[acks.length - 1]?.seq ?? 0),
    'the log reaches past the last acknowledged message',
  );

  // And the restarted node still serves live traffic — the half that proves the
  // restored device's crypto rebuilt from the snapshot's seeds alone. Bob resumes too
  // (his own sender-key state and pairwise sessions died with his process, so his next
  // send starts over: a fresh chain, distributed through a brand-new X3DH prekey
  // envelope against alice's still-published bundle — her one-time prekeys were never
  // consumed, she was the initiator every time before). Alice's restored store opens
  // it, accepts the fresh chain, and decrypts his message over the socket.
  const bobBack = client('restart_bob', KeyStore.restore(bobKeys));
  await bobBack.resume(bob.grant);
  await bobBack.watchConversation(conversationId);
  bobBack.rememberConversation(summary);

  const postRestartText = 'restart: the node still serves';
  const postRestartAck = await bobBack.messaging.send(conversationId, {
    type: ContentType.Text,
    text: postRestartText,
  });
  await until('the restored device receives a live message after restart', () =>
    received.some((message) => message.messageId === postRestartAck.messageId),
  );
  const liveAfterRestart = received.find(
    (message) => message.messageId === postRestartAck.messageId,
  );
  assert.ok(liveAfterRestart !== undefined);
  assert.equal(textOf(liveAfterRestart), postRestartText);
});

scenario('the metrics endpoint observes the session that is live right now', async () => {
  // Brief section 174's runtime half: the gauges and counters are not decoration, they
  // track real sessions. A session opens, the gauge moves; it closes, the gauge moves
  // back — an observability contract asserted against the running node. The baseline is
  // taken settled, not raw: the restart scenario before this one disconnects its clients
  // in its `finally`, and the node reaps those sessions a beat after the client-side
  // disconnect resolves, so a raw sample would freeze the scenario onto a floor the
  // reaps are still carving (see {@link settledMetric}).
  const before = await settledMetric('migo_gateway_sessions_live');
  assert.ok(before !== undefined, 'the sessions gauge is registered');

  const user = await register('metrics_user');

  // The gauge is read by polling because the session is counted the moment the handshake
  // completes, which `register` has already raced past by the time it resolves.
  const during = await untilMetric(
    'migo_gateway_sessions_live',
    (value) => value !== undefined && value > before,
    `the live-session gauge counts the new session (was ${before})`,
  );
  assert.ok(during !== undefined && during > before);

  const framesIn = await metric('migo_gateway_frames_in_total');
  assert.ok(framesIn !== undefined && framesIn > 0, 'frames crossed the wire and were counted');

  await user.client.disconnect();
  await untilMetric(
    'migo_gateway_sessions_live',
    (value) => value !== undefined && value <= before,
    `the live-session gauge drops back after the disconnect (was ${before})`,
  );
});
