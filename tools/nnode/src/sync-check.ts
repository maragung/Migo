/**
 * N-node federation sync check: prove, against real running nodes, which
 * cross-node paths a configuration-formed mesh link actually carries.
 *
 * Two migod nodes are linked purely by their `federation.peers` entries (the
 * runner in `run.sh` starts them; no programmatic admission anywhere). This
 * script then walks the paths section 170 promises and the paths it does not,
 * reporting each honestly:
 *
 *   PROVED (a failure here fails the run):
 *     1. bob's room join on node 2 surfaces on alice's node 1 subscriber;
 *     2. a room message alice sends on node 1 arrives and decrypts on bob's
 *        node 2 subscriber — and the reply comes back the other way;
 *     3. bob's typing signal in the room's conversation reaches alice;
 *     4. a 1:1 direct message crosses the link both ways — demanded when the
 *        running binary carries the conversation tier (the conversation row's
 *        home_region is stamped at creation), which is how the harness tells
 *        a release that has the tier from one that does not.
 *
 *   REPORTED (expected NOT to cross until a release carries the tier; the
 *   report is the point, not a failure):
 *     5. presence does not federate — user topics stay local;
 *     6. direct messages do not federate either, when the running binary
 *        predates the conversation tier (no home_region column).
 *
 * Accounts, devices, key bundles, and room membership rows do not replicate
 * across nodes today. To keep the proof about the *link* and the tiered
 * fan-out rather than about that missing replication, this script fixtures the
 * other node's rows directly in PostgreSQL (verbatim copies of what the
 * registering node already holds), the same stand-in a replication layer would
 * eventually provide. Every fixture is logged; nothing is papered over — if a
 * PROVED path fails, the run exits non-zero with everything observed.
 *
 * Environment variables (defaults match tools/nnode/run.sh):
 *   NODE1_HTTP  node 1 REST origin   (default http://127.0.0.1:18201)
 *   NODE2_HTTP  node 2 REST origin   (default http://127.0.0.1:18203)
 *   PGHOST, PGPORT, PGUSER, PGPASSWORD   PostgreSQL the nodes run on
 *   DB1, DB2    node 1 / node 2 database names
 */

import { setTimeout as sleep } from 'node:timers/promises';
import { execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';

import {
  MigoClient,
  ContentType,
  ConversationKind,
  PresenceState,
  RelationshipKind,
  RoomKind,
  RoomRole,
  TopicKind,
  TypingState,
  serverEndpointFromUrl,
  type Id,
} from '@migo/sdk';

const NODE1_HTTP = process.env.NODE1_HTTP ?? 'http://127.0.0.1:18201';
const NODE2_HTTP = process.env.NODE2_HTTP ?? 'http://127.0.0.1:18203';
const PG = {
  host: process.env.PGHOST ?? 'localhost',
  port: process.env.PGPORT ?? '15432',
  user: process.env.PGUSER ?? 'migo',
  password: process.env.PGPASSWORD ?? 'migo',
  db1: process.env.DB1 ?? 'migo_nnode1',
  db2: process.env.DB2 ?? 'migo_nnode2',
};

const APP_VERSION = '0.1.0';
const LOCALE = 'en-US';
/** How long one cross-node delivery has to land before the check fails. */
const DELIVERY_TIMEOUT_MS = 30_000;
/** How long the known-gap checks wait before declaring "did not cross". */
const GAP_TIMEOUT_MS = 10_000;

function ts(): string {
  return new Date().toISOString();
}

function log(scope: string, message: string): void {
  console.log(`[${ts()}] [${scope}] ${message}`);
}

function fail(scope: string, message: string): never {
  console.error(`[${ts()}] [${scope}] FAIL: ${message}`);
  process.exit(1);
}

// --- PostgreSQL fixtures ------------------------------------------------------

function psql(db: string, sql: string): string {
  return execFileSync(
    'psql',
    [
      '-h',
      PG.host,
      '-p',
      PG.port,
      '-U',
      PG.user,
      '-d',
      db,
      '-tAq',
      '-v',
      'ON_ERROR_STOP=1',
      '-c',
      sql,
    ],
    {
      env: { ...process.env, PGPASSWORD: PG.password },
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
    },
  ).trim();
}

/** Runs a query whose result is one `row_to_json` row, parsed; null when absent. */
function rowJson<T>(db: string, sql: string): T | null {
  const out = psql(db, sql);
  return out === '' ? null : (JSON.parse(out) as T);
}

/** Reads every row of `inner` as one JSON array. `inner` selects plain rows —
 * the aggregation wraps each row itself, so an inner `row_to_json` would nest
 * each element one level too deep (`{"row_to_json": {...}}`). */
function rowsJson<T>(db: string, inner: string): T[] {
  const out = psql(db, `select coalesce(json_agg(t), '[]'::json) from (${inner}) t`);
  return (JSON.parse(out) as T[]) ?? [];
}

/** Reads one row that must exist, or fails the run naming what is missing. */
function mustRow<T>(db: string, sql: string, what: string): T {
  const row = rowJson<T>(db, sql);
  if (row === null) {
    fail('fixture', `${what} not found on ${db}`);
  }
  return row;
}

/** Inserts one JSON object into `table`, column-for-column via the table type. */
function insertJson(db: string, table: string, row: Record<string, unknown>): void {
  const json = JSON.stringify(row).replaceAll("'", "''");
  psql(db, `insert into ${table} select * from json_populate_record(null::${table}, '${json}')`);
}

/**
 * The 26-character canonical text form of an {@link Id} as PostgreSQL holds it.
 *
 * Ids are Crockford base32 over 128 bits (5 bits per character, most
 * significant first); the store maps them to uuid by reading the same 128 bits
 * as bytes. Nothing on the SQL side accepts the text form, so every fixture
 * that references an account or a conversation by id goes through this.
 */
function idToUuid(id: Id): string {
  const alphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
  // Crockford's human confusions, absorbed on parse: O→0, I/L→1.
  const confusion: Record<string, string> = { O: '0', I: '1', L: '1' };
  let n = 0n;
  for (const raw of id.toUpperCase()) {
    const ch = confusion[raw] ?? raw;
    const value = alphabet.indexOf(ch);
    if (value < 0) {
      throw new Error(`not a canonical id character: ${raw}`);
    }
    n = (n << 5n) | BigInt(value);
  }
  const hex = n.toString(16).padStart(32, '0');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

// --- node HTTP helpers --------------------------------------------------------

/** One metric's value off a node's `/metrics`, summed over label variants. */
async function metric(origin: string, name: string): Promise<number> {
  const response = await fetch(`${origin}/metrics`);
  if (!response.ok) {
    throw new Error(`/metrics on ${origin} answered ${response.status}`);
  }
  const text = await response.text();
  let total = 0;
  for (const line of text.split('\n')) {
    if (line.startsWith(`${name}{`) || line.startsWith(`${name} `)) {
      total += Number.parseFloat(line.slice(line.lastIndexOf(' ') + 1));
    }
  }
  return total;
}

/** Waits until `condition` holds, or gives up after `timeout` milliseconds. */
async function waitFor(condition: () => boolean, timeout: number): Promise<boolean> {
  const deadline = Date.now() + timeout;
  while (!condition()) {
    if (Date.now() > deadline) {
      return false;
    }
    await sleep(50);
  }
  return true;
}

// --- clients ------------------------------------------------------------------

/**
 * A client for one node. The gateway WebSocket is served by the same listener
 * as REST (`/ws` on the HTTP bind), so the endpoint's gateway port is pinned to
 * the REST port rather than left at the loopback default of port+1.
 */
function makeClient(origin: string, displayName: string): MigoClient {
  const base = serverEndpointFromUrl(origin);
  return MigoClient.create({
    server: { ...base, gatewayPort: base.port },
    deviceDisplayName: displayName,
    hello: {
      platform: 4, // Desktop
      appVersion: APP_VERSION,
      locale: LOCALE,
      bandwidthMode: 0, // Auto
    },
  });
}

// --- the run ------------------------------------------------------------------

async function main(): Promise<void> {
  const stamp = Date.now().toString(36);
  const suffix = randomBytes(2).toString('hex');
  const aliceUsername = `alice_${stamp}_${suffix}`;
  const bobUsername = `bob_${stamp}_${suffix}`;
  const passphrase = `correct-horse-battery-staple-${randomBytes(4).toString('hex')}`;
  log('boot', `node 1 ${NODE1_HTTP} (${PG.db1}), node 2 ${NODE2_HTTP} (${PG.db2})`);

  // 1. Register: alice on node 1, bob on node 2 — distinct accounts on
  //    distinct nodes, exactly the split the mesh is supposed to bridge.
  const alice = makeClient(NODE1_HTTP, 'nnode_alice');
  await alice.register({ username: aliceUsername, passphrase, locale: LOCALE });
  const aliceId = alice.accountId;
  const bob = makeClient(NODE2_HTTP, 'nnode_bob');
  await bob.register({ username: bobUsername, passphrase, locale: LOCALE });
  const bobId = bob.accountId;
  log('register', `alice=${aliceId} on node 1, bob=${bobId} on node 2`);

  const aliceUuid = idToUuid(aliceId);
  const bobUuid = idToUuid(bobId);

  // 2. Alice founds a public room on node 1. Node 1 is the room's home — its
  //    `home_region` is node 1's region, which is what the tiered fan-out keys
  //    the cross-node copies on.
  const slug = `nnode-${stamp}-${suffix}`;
  const joined = await alice.rooms.create(
    slug,
    'NNode Sync Room',
    RoomKind.Public,
    'cross-node sync check',
  );
  const roomId = joined.room.roomId;
  const conversationId = joined.conversationId;
  const roomUuid = idToUuid(roomId);
  const conversationUuid = idToUuid(conversationId);
  log('room', `room ${roomId} → conversation ${conversationId} (encryption=${joined.encryption})`);

  // 3. Fixtures: stand in for the account/device/key replication a full mesh
  //    does not have yet. Everything is a verbatim copy of rows the other node
  //    already owns, except one policy tweak that makes cross-node visibility
  //    meaningful and the membership rows a join on that node would have
  //    written.
  const fixtureAccount = (from: string, to: string, uuid: string, note: string): void => {
    const account = mustRow<Record<string, unknown>>(
      from,
      `select row_to_json(t) from account t where account_id = '${uuid}'`,
      `${note}'s account row`,
    );
    insertJson(to, 'account', account);
    const profile = mustRow<Record<string, unknown>>(
      from,
      `select row_to_json(t) from profile t where account_id = '${uuid}'`,
      `${note}'s profile row`,
    );
    // `show_last_seen = 2` (everyone) so the other node's watch of this
    // account's presence topic is authorized without a friendship graph.
    profile.show_last_seen = 2;
    insertJson(to, 'profile', profile);
    for (const table of ['device', 'identity_key', 'signed_prekey', 'one_time_prekey']) {
      const rows = rowsJson<Record<string, unknown>>(
        from,
        `select * from ${table} t where account_id = '${uuid}'`,
      );
      for (const row of rows) {
        insertJson(to, table, row);
      }
    }
    log('fixture', `${note}'s account, profile, device, and key bundles copied ${from} → ${to}`);
  };

  fixtureAccount(PG.db2, PG.db1, bobUuid, 'bob');
  fixtureAccount(PG.db1, PG.db2, aliceUuid, 'alice');

  // The room and its conversation, verbatim: node 2 must know the room exists
  // (its client joins there) and that the conversation is a room conversation
  // whose home is elsewhere. `home_region` rides along unchanged. The
  // conversation goes first: `room.conversation_id` references it.
  const conversationRow = mustRow<Record<string, unknown>>(
    PG.db1,
    `select row_to_json(t) from conversation t where conversation_id = '${conversationUuid}'`,
    'the room conversation row',
  );
  insertJson(PG.db2, 'conversation', conversationRow);
  const roomRow = mustRow<Record<string, unknown>>(
    PG.db1,
    `select row_to_json(t) from room t where room_id = '${roomUuid}'`,
    'the room row',
  );
  insertJson(PG.db2, 'room', roomRow);
  log(
    'fixture',
    `room ${roomId} (home_region=${String(roomRow.home_region)}) and its conversation copied to node 2`,
  );

  // bob's conversation membership on node 2, modeled on alice's own row: the
  // one piece a join there would have written that `rooms.join` itself does not.
  const aliceMembership = mustRow<Record<string, unknown>>(
    PG.db1,
    `select row_to_json(t) from conversation_member t
      where conversation_id = '${conversationUuid}' and account_id = '${aliceUuid}'`,
    "alice's conversation membership",
  );
  insertJson(PG.db2, 'conversation_member', { ...aliceMembership, account_id: bobUuid });

  // The roster each node serves: alice's roster on node 1 must include bob (so
  // her first send seals a sender-key distribution for his device), and bob's
  // roster on node 2 must include alice (so his reply does the same for hers).
  const now = new Date().toISOString();
  const membershipRow = (uuid: string, role: RoomRole): Record<string, unknown> => ({
    room_id: roomUuid,
    account_id: uuid,
    role,
    permissions_grant: 0,
    permissions_deny: 0,
    joined_at: now,
    left_at: null,
    muted_until: null,
    banned_until: null,
    ban_reason: null,
    invited_by: null,
  });
  insertJson(PG.db1, 'room_member', membershipRow(bobUuid, RoomRole.Member));
  insertJson(PG.db2, 'room_member', membershipRow(aliceUuid, RoomRole.Owner));

  // A friendship on node 1 so the direct-message check is refused by neither
  // privacy policy — it then measures federation, not permissions.
  for (const [a, b] of [
    [aliceUuid, bobUuid],
    [bobUuid, aliceUuid],
  ]) {
    insertJson(PG.db1, 'relationship', {
      account_id: a,
      other_id: b,
      kind: RelationshipKind.Friend,
      created_at: now,
      accepted_at: now,
    });
  }
  log('fixture', 'room membership and friendship rows written on both nodes');

  // 4. Subscriptions, before anything is sent. Alice's roster (node 1) now
  //    names bob, so her membership cache — the audience the first send seals
  //    for — is built after the fixture, not before it.
  const memberEvents: { userId: Id; joined: boolean }[] = [];
  alice.rooms.onMember((event) => {
    memberEvents.push({ userId: event.userId, joined: event.joined });
    log('alice', `room member event: ${event.userId} ${event.joined ? 'joined' : 'left'}`);
  });
  const bobMessages: { senderId: Id; text: string; conversationId: Id }[] = [];
  bob.messaging.onMessage((message) => {
    const text = message.content.type === ContentType.Text ? message.content.text : '<non-text>';
    bobMessages.push({ senderId: message.senderId, text, conversationId: message.conversationId });
    log('bob', `inbound seq=${message.seq} text="${text}"`);
  });
  const aliceMessages: { senderId: Id; text: string; conversationId: Id }[] = [];
  alice.messaging.onMessage((message) => {
    const text = message.content.type === ContentType.Text ? message.content.text : '<non-text>';
    aliceMessages.push({
      senderId: message.senderId,
      text,
      conversationId: message.conversationId,
    });
    log('alice', `inbound seq=${message.seq} text="${text}"`);
  });
  const aliceTyping: { userId: Id | undefined; state: TypingState }[] = [];
  alice.typing.onTyping((event) => {
    aliceTyping.push({ userId: event.userId, state: event.state });
    log('alice', `typing event from ${event.userId ?? '?'} state=${event.state}`);
  });

  await alice.startRoomConversation(conversationId, roomId);
  log('alice', 'room conversation started on node 1 (roster, conversation topic, room topic)');

  // 5. Bob joins the room ON NODE 2. The join itself is the first federated
  //    event: it must cross the link and surface on alice's node 1 subscriber.
  const bobJoined = await bob.rooms.join(roomId);
  if (bobJoined.conversationId !== conversationId) {
    fail('room', `bob's conversation ${bobJoined.conversationId} differs from ${conversationId}`);
  }
  await bob.startRoomConversation(conversationId, roomId);
  log('bob', 'joined the room on node 2 and started its conversation there');

  const proved: string[] = [];

  // CHECK 1: the join event crossed the link (room topic, home node fan-in).
  const check1 = await waitFor(
    () => memberEvents.some((e) => e.userId === bobId && e.joined),
    DELIVERY_TIMEOUT_MS,
  );
  if (!check1) {
    fail(
      'check-1',
      `the room join event did not cross the mesh link; events observed: ${JSON.stringify(memberEvents)}`,
    );
  }
  proved.push('room join event: node 2 → node 1');
  log('check-1', "PROVED: alice's subscriber saw bob join");

  // Let the FED_ROOM_SUBSCRIBE (bob's room-topic grant on node 2 asked node 1
  // to watch the room) drain through the outbox before sending into it.
  await sleep(3_000);

  // CHECK 2a: a room message alice sends on node 1 arrives — and decrypts —
  // on bob's node 2 subscriber. The first send carries the sender-key
  // distribution sealed for bob's device, both frames cross the link, and
  // bob's client opens them with the keys it holds.
  const aliceText = `cross-node hello from node 1 (${stamp})`;
  log('alice', `sending "${aliceText}"`);
  await alice.messaging.send(conversationId, { type: ContentType.Text, text: aliceText });
  const check2a = await waitFor(
    () =>
      bobMessages.some(
        (m) =>
          m.senderId === aliceId && m.text === aliceText && m.conversationId === conversationId,
      ),
    DELIVERY_TIMEOUT_MS,
  );
  if (!check2a) {
    fail(
      'check-2a',
      `the room message did not arrive on node 2; bob observed: ${JSON.stringify(bobMessages)}`,
    );
  }
  proved.push('room message: node 1 → node 2 (delivered and decrypted)');
  log('check-2a', `PROVED: bob received and decrypted "${aliceText}"`);

  // CHECK 2b: the reply, the other direction — node 2 → node 1.
  const bobText = `cross-node reply from node 2 (${stamp})`;
  log('bob', `sending "${bobText}"`);
  await bob.messaging.send(conversationId, { type: ContentType.Text, text: bobText });
  const check2b = await waitFor(
    () =>
      aliceMessages.some(
        (m) => m.senderId === bobId && m.text === bobText && m.conversationId === conversationId,
      ),
    DELIVERY_TIMEOUT_MS,
  );
  if (!check2b) {
    fail(
      'check-2b',
      `the reply did not arrive on node 1; alice observed: ${JSON.stringify(aliceMessages)}`,
    );
  }
  proved.push('room message: node 2 → node 1 (delivered and decrypted)');
  log('check-2b', `PROVED: alice received and decrypted "${bobText}"`);

  // CHECK 3: a typing signal in the room's conversation crosses the link.
  // Typing is fire-and-forget and coalescable, so this is lossy by design —
  // one Start signal with a generous window is the fair test.
  log('bob', 'typing start in the room conversation');
  await bob.typing.setTyping(conversationId, TypingState.Start);
  const check3 = await waitFor(
    () => aliceTyping.some((e) => e.userId === bobId && e.state === TypingState.Start),
    DELIVERY_TIMEOUT_MS,
  );
  if (!check3) {
    fail(
      'check-3',
      `the typing signal did not cross the mesh link; alice observed: ${JSON.stringify(aliceTyping)}`,
    );
  }
  proved.push('typing signal: node 2 → node 1');
  log('check-3', "PROVED: alice's subscriber saw bob start typing");

  // CHECK 4: the 1:1 direct message, both directions. Alice opens a direct
  // conversation with bob on node 1 — node 1 becomes the conversation's home,
  // stamped into the row's home_region at creation — and the row plus bob's
  // membership are fixtured into node 2 the same way the room's were. Bob
  // watches it on node 2, which is the conversation tier's subscribe half:
  // node 2 asks node 1 to watch the conversation. Alice's sealed message then
  // rides the tiered fan-out (one federated copy per watching node), and
  // bob's reply is handed by node 2 to the home node, which publishes it to
  // alice's session from its own hub. Both directions must deliver *and
  // decrypt*: the sealed envelope crosses every node on the way unopened.
  //
  // The harness runs released binaries, and the conversation tier is in the
  // tree before it is in a release. The stamp is the tell: a binary that has
  // the tier stamps home_region at creation and must also carry the message
  // across (a failure there is a real failure, exit non-zero); a binary that
  // predates the tier writes no home_region at all, and the honest outcome is
  // a reported gap, the same stance the presence tier's check takes.
  const direct = await alice.startConversation(ConversationKind.Direct, [bobId]);
  const directUuid = idToUuid(direct.conversationId);
  const directRow = mustRow<Record<string, unknown>>(
    PG.db1,
    `select row_to_json(t) from conversation t where conversation_id = '${directUuid}'`,
    'the direct conversation row',
  );
  const homeRegion =
    directRow.home_region === undefined || directRow.home_region === null
      ? ''
      : String(directRow.home_region);
  let dmReported = false;
  if (homeRegion === '') {
    log(
      'gap-dm',
      'the conversation row has no home_region, so this binary predates the conversation federation tier',
    );
    log(
      'gap-dm',
      'CONFIRMED GAP: direct messages do not cross the link in these released binaries (the tier ships with the next release; flipping this check to demand them is the release follow-up)',
    );
    dmReported = true;
  } else {
    if (homeRegion !== 'nnode-1') {
      fail(
        'check-4',
        `the direct conversation's home_region is ${homeRegion}, not node 1's region`,
      );
    }
    insertJson(PG.db2, 'conversation', directRow);
    insertJson(
      PG.db2,
      'conversation_member',
      mustRow<Record<string, unknown>>(
        PG.db1,
        `select row_to_json(t) from conversation_member t
          where conversation_id = '${directUuid}' and account_id = '${bobUuid}'`,
        "bob's direct conversation membership",
      ),
    );
    await bob.watchConversation(direct.conversationId);
    log(
      'check-4',
      `direct conversation ${direct.conversationId} created on node 1 (home), bob watching it on node 2`,
    );

    // Let the FED_CONVERSATION_SUBSCRIBE drain through the outbox before the
    // first send, the same drain the room's subscribe got: a send that leaves
    // before the home node recorded the watcher is simply not fanned out.
    await sleep(3_000);

    const dmText = `direct hello across the mesh (${stamp})`;
    log('alice', `sending "${dmText}" into the direct conversation`);
    await alice.messaging.send(direct.conversationId, { type: ContentType.Text, text: dmText });
    const check4a = await waitFor(
      () =>
        bobMessages.some(
          (m) =>
            m.senderId === aliceId &&
            m.text === dmText &&
            m.conversationId === direct.conversationId,
        ),
      DELIVERY_TIMEOUT_MS,
    );
    if (!check4a) {
      fail(
        'check-4a',
        `the direct message did not cross the mesh link; bob observed: ${JSON.stringify(bobMessages)}`,
      );
    }
    proved.push('direct message: node 1 → node 2 (delivered and decrypted)');
    log('check-4a', `PROVED: bob received and decrypted "${dmText}"`);

    const dmReply = `direct reply across the mesh (${stamp})`;
    log('bob', `sending "${dmReply}" back`);
    await bob.messaging.send(direct.conversationId, { type: ContentType.Text, text: dmReply });
    const check4b = await waitFor(
      () =>
        aliceMessages.some(
          (m) =>
            m.senderId === bobId &&
            m.text === dmReply &&
            m.conversationId === direct.conversationId,
        ),
      DELIVERY_TIMEOUT_MS,
    );
    if (!check4b) {
      fail(
        'check-4b',
        `the direct reply did not reach node 1; alice observed: ${JSON.stringify(aliceMessages)}`,
      );
    }
    proved.push('direct message: node 2 → node 1 (delivered and decrypted)');
    log('check-4b', `PROVED: alice received and decrypted "${dmReply}"`);
  }

  // KNOWN GAP A: presence. The tree carries the user-topic tier
  // (presence_relay, FED_USER_SUBSCRIBE / FED_USER_EVENT), but this harness
  // runs released binaries, so until a release carries the tier the honest
  // observation is that the event does not cross. Bob's watch of alice's user
  // topic on node 2 is authorized by the fixtured account row; alice then
  // changes presence on node 1. If it crosses, that is a release note, not a
  // failure — the demand flip is the release follow-up.
  const presenceHeard: { userId: Id; state: PresenceState }[] = [];
  bob.presence.onPresence((event) => {
    presenceHeard.push({ userId: event.userId, state: event.state });
    log('bob', `presence event from ${event.userId} state=${event.state}`);
  });
  const subscribeResponse = await bob.subscribe([{ kind: TopicKind.User, id: aliceId }]);
  const watchGranted = subscribeResponse.accepted.some(
    (topic) => topic.kind === TopicKind.User && topic.id === aliceId,
  );
  log('gap-presence', `bob's watch of alice's user topic on node 2: granted=${watchGranted}`);
  if (!watchGranted) {
    log(
      'gap-presence',
      'the watch was not granted, so the presence gap cannot be observed as a federation fact',
    );
  } else {
    await alice.presence.setPresence(PresenceState.Away);
    log('gap-presence', 'alice set presence Away on node 1; waiting to see if node 2 hears it');
    await sleep(GAP_TIMEOUT_MS);
    const crossed = presenceHeard.some(
      (event) => event.userId === aliceId && event.state === PresenceState.Away,
    );
    log(
      'gap-presence',
      crossed
        ? 'NOTE: a presence event crossed the link — the released binaries carry the user-topic tier; flip this check to demand it'
        : 'CONFIRMED GAP: presence did not cross the link (the user-topic tier is in the tree but not in the released binaries yet)',
    );
  }

  // 6. What the link itself did, straight off both nodes' /metrics.
  for (const [label, origin] of [
    ['node 1', NODE1_HTTP],
    ['node 2', NODE2_HTTP],
  ] as const) {
    const peers = await metric(origin, 'migo_federation_peers_added_total');
    const handshakes = await metric(origin, 'migo_federation_handshakes_total');
    const enqueued = await metric(origin, 'migo_federation_outbox_enqueued_total');
    const delivered = await metric(origin, 'migo_federation_outbox_delivered_total');
    log(
      'metrics',
      `${label}: peers_added=${peers} handshakes=${handshakes} outbox_enqueued=${enqueued} outbox_delivered=${delivered}`,
    );
  }

  console.log('');
  console.log('=== cross-node sync check: what the link proved ===');
  for (const line of proved) {
    console.log(`  PROVED   ${line}`);
  }
  console.log(
    '  REPORTED presence does not cross the link (the user-topic tier is in the tree, not in the released binaries yet)',
  );
  if (dmReported) {
    console.log(
      '  REPORTED direct messages do not cross the link (the conversation tier is in the tree, not in the released binaries yet)',
    );
  }
  console.log('');
  log(
    'result',
    'all proved paths passed; see the REPORTED lines for the paths that do not federate',
  );

  await alice.disconnect();
  await bob.disconnect();
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
