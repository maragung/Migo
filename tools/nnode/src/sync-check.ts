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
 *        a release that has the tier from one that does not;
 *     5. a presence change on node 1 reaches a watcher on node 2 — demanded
 *        by the same version tell, since the user-topic tier shipped in the
 *        same release as the conversation tier;
 *     6. when the running binary carries the row-replication tier (its tell
 *        is the /metrics counter migo_mesh_rows_replicated_total, which only
 *        exists once the tier does), the account and conversation rows the
 *        direct-message path needs are demanded to cross the link themselves:
 *        alice's create pulls bob's account over the mesh through her privacy
 *        gate, bob's watch pulls the conversation row and its membership
 *        through his subscribe, and bob's reply pulls alice's account the same
 *        way — no account, profile, friendship-cross-edge, conversation, or
 *        membership fixture is written for the direct path at all.
 *
 *   REPORTED (expected NOT to cross when the running binary predates the
 *   tiers; the report is the point, not a failure):
 *     7. direct messages and presence changes do not federate when the
 *        running binary predates the tier-bearing release (no home_region).
 *
 * What still does not replicate, and is fixtured in BOTH regimes because no
 * tier carries it yet: device and key-bundle rows (the clients seal for each
 * other's devices, so each node must serve the other account's device and key
 * rows — a device-federation tier does not exist), and every room row (the
 * room tier fans events out but does not replicate the room, its membership,
 * or its conversation — bob joins the room ON node 2, which only works if the
 * rows are already there, and his own membership row on node 2 is exactly the
 * row a join on that node would have written). The account and profile rows
 * are fixtured ONLY when the running binary predates the row-replication
 * tier; a binary that has the tier must pull them itself — forced at fixture
 * time by the direct-conversation creates whose privacy gate routes the
 * account query to the home node, since no traffic has flowed yet to pull
 * one and the device fixtures cannot be written until the account rows land
 * — and the direct-message check fails if it does not. Every fixture is
 * logged; nothing is papered over — if a PROVED path fails, the run exits
 * non-zero with everything observed.
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
  RemoteError,
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
  const lines = await metricLines(origin, name);
  let total = 0;
  for (const line of lines) {
    total += Number.parseFloat(line.slice(line.lastIndexOf(' ') + 1));
  }
  return total;
}

/**
 * The exposition lines naming `name`, empty when the node does not export it.
 *
 * A counter that exists but is still zero is not the same fact as a counter
 * that does not exist: the row-replication tier's counter is the version tell
 * for whether the running binary can pull rows at all, and `metric()` returns
 * 0 for both. This reads the raw exposition so the two cases stay distinct.
 */
async function metricLines(origin: string, name: string): Promise<string[]> {
  const response = await fetch(`${origin}/metrics`);
  if (!response.ok) {
    throw new Error(`/metrics on ${origin} answered ${response.status}`);
  }
  const text = await response.text();
  return text
    .split('\n')
    .filter((line) => line.startsWith(`${name}{`) || line.startsWith(`${name} `));
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

/**
 * Waits until one database row exists, polling at a human pace.
 *
 * A row pulled across the mesh lands asynchronously — the ask rides the outbox
 * to the owning node, the answer rides it back — so a fixture that references
 * the far account (a device row's foreign key above all) cannot be written
 * until the pull has actually seated it. Fails the run naming what never
 * arrived, because a fixture written against a missing row is not a fixture,
 * it is a foreign-key error.
 */
async function waitForRow(db: string, sql: string, what: string): Promise<void> {
  const deadline = Date.now() + DELIVERY_TIMEOUT_MS;
  for (;;) {
    if (rowJson<Record<string, unknown>>(db, sql) !== null) {
      return;
    }
    if (Date.now() > deadline) {
      fail('fixture', `${what} never appeared on ${db}`);
    }
    await sleep(500);
  }
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

  // The row-replication tier's version tell. The counter is registered at
  // boot by binaries that carry the tier and absent from ones that predate
  // it — and a zero reading is not the same fact as absence, which is why
  // the tell reads the exposition lines rather than the value. A binary with
  // the counter must pull account and conversation rows across the mesh by
  // itself, and the checks below demand it does; a binary without it gets
  // the stand-in fixtures, exactly as before.
  const rowsReplicate =
    (await metricLines(NODE1_HTTP, 'migo_mesh_rows_replicated_total')).length > 0;
  log(
    'boot',
    `row-replication tier: ${rowsReplicate ? 'present (no account/conversation fixtures will be written)' : 'absent (stand-in fixtures in use)'}`,
  );

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

  // 3. Fixtures. Two regimes, decided by the row-replication tell above:
  //
  //   - Binary without the tier: the account, profile, and device/key rows of
  //     each account are copied verbatim to the other node, and all four
  //     friendship rows are seeded — the same stand-in as always.
  //   - Binary with the tier: nothing is copied. The account and profile rows
  //     must be pulled across the mesh by the nodes themselves — the checks
  //     demand it — and the pull's only client-visible trigger is a
  //     direct-conversation create: its privacy gate fail-closes on a
  //     recipient profile the node does not hold, and the tier turns that
  //     into a mesh ask (FED_ACCOUNT_QUERY) before it answers. The creates
  //     run here, at fixture time, because everything that references the far
  //     account comes after them — the device rows' foreign key above all,
  //     since a device row cannot be written for an account the destination
  //     node does not hold yet and no cross-node traffic has flowed to pull
  //     one.

  // Devices and key bundles cross in both regimes: no tier carries them, and
  // each node must serve the other account's devices for the clients to seal
  // against. The account row must already be seated on `to` — the foreign key
  // on `device.account_id` is the whole reason this is its own step in the
  // tier regime below.
  const fixtureDevices = (from: string, to: string, uuid: string, note: string): void => {
    for (const table of ['device', 'identity_key', 'signed_prekey', 'one_time_prekey']) {
      const rows = rowsJson<Record<string, unknown>>(
        from,
        `select * from ${table} t where account_id = '${uuid}'`,
      );
      for (const row of rows) {
        insertJson(to, table, row);
      }
    }
    log(
      'fixture',
      `${note}'s device and key bundles copied ${from} → ${to} (no device federation tier yet)`,
    );
  };

  // A friendship so the direct-message check is refused by neither privacy
  // policy — it then measures federation, not permissions. The far node's
  // gate is fail-closed: it answers from its own rows, and without the edges
  // there it can only refuse PRIVACY_RESTRICTED. The relationship table's
  // foreign key runs both ways (`account_id` and `other_id` each reference
  // `account`), so an edge toward an account the node does not hold cannot be
  // seeded at all — which is what forces the tier regime below to interleave
  // pulls with own-side edges rather than seeding everything up front. The
  // cross edges are never seeded in that regime: they are the tier's to
  // carry, they arrive inside the account-rows answers the gates pull, and
  // the checks below assert each far edge is there before relying on it.
  const friendshipAt = new Date().toISOString();
  const friendship = (db: string, from: string, to: string): void => {
    insertJson(db, 'relationship', {
      account_id: from,
      other_id: to,
      kind: RelationshipKind.Friend,
      created_at: friendshipAt,
      accepted_at: friendshipAt,
    });
  };

  // One pull trigger: the direct-conversation create whose gate asks the mesh.
  // The first two runs are expected to end in a refusal — no friendship exists
  // anywhere yet, because seeding one takes the far account's row, which is
  // exactly what the pull brings — and the pull has already happened by the
  // time the gate refuses, so the refusal is caught and logged rather than
  // allowed to fail the run. Anything else the server might say rethrows.
  const triggerPull = async (note: string, create: () => Promise<unknown>): Promise<void> => {
    try {
      await create();
      log('fixture', `${note} was accepted; the pull it forced stands either way`);
    } catch (error) {
      if (error instanceof RemoteError && error.symbol === 'PRIVACY_RESTRICTED') {
        log('fixture', `${note} was refused by privacy (expected: no friendship is seeded yet)`);
        return;
      }
      throw error;
    }
  };

  if (!rowsReplicate) {
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
      fixtureDevices(from, to, uuid, note);
    };
    fixtureAccount(PG.db2, PG.db1, bobUuid, 'bob');
    fixtureAccount(PG.db1, PG.db2, aliceUuid, 'alice');
    const pairs: [string, string][] = [
      [aliceUuid, bobUuid],
      [bobUuid, aliceUuid],
    ];
    for (const [a, b] of pairs) {
      friendship(PG.db1, a, b);
      friendship(PG.db2, a, b);
    }
    log('fixture', 'friendship rows written on both nodes');
  } else {
    log(
      'fixture',
      'account and profile rows NOT copied either way: this binary carries the row-replication tier and must pull them itself',
    );
    // Trigger 1 — alice's create makes node 1 ask about bob. The answer seats
    // bob's account and profile on node 1 (no edges: node 2 holds none toward
    // alice yet, for the same foreign-key reason), and the create is then
    // refused, because alice's own edge toward bob cannot be seeded before
    // bob's row lands. Only once the row is there may the device rows follow —
    // their foreign key is the bug this regime used to have — and alice's
    // own-side edge, whose `other_id` the pull just seated.
    await triggerPull("alice's direct-conversation create", () =>
      alice.startConversation(ConversationKind.Direct, [bobId]),
    );
    await waitForRow(
      PG.db1,
      `select row_to_json(t) from account t where account_id = '${bobUuid}'`,
      "bob's account row, pulled to node 1 by the create's privacy gate",
    );
    log(
      'fixture',
      "bob's account row seated on node 1; alice's own-side edge and bob's devices may follow",
    );
    friendship(PG.db1, aliceUuid, bobUuid);
    fixtureDevices(PG.db2, PG.db1, bobUuid, 'bob');

    // Trigger 2 — bob's create makes node 2 ask about alice, and this answer
    // carries the edge alice → bob seeded above, because that is the edge
    // between the queried account and the asker. Node 2 seats alice's
    // account, profile, and that cross edge — the cross edge check 4 demands
    // on node 2 — and refuses the create itself, because bob's own edge
    // toward alice was unseedable until alice's row landed, which the same
    // pull just did.
    await triggerPull("bob's direct-conversation create", () =>
      bob.startConversation(ConversationKind.Direct, [aliceId]),
    );
    await waitForRow(
      PG.db2,
      `select row_to_json(t) from account t where account_id = '${aliceUuid}'`,
      "alice's account row, pulled to node 2 by the create's privacy gate",
    );
    log(
      'fixture',
      "alice's account row seated on node 2; bob's own-side edge and alice's devices may follow",
    );
    friendship(PG.db2, bobUuid, aliceUuid);
    fixtureDevices(PG.db1, PG.db2, aliceUuid, 'alice');

    // Trigger 3 — node 1 already holds bob's profile, so its gate would never
    // ask about him again (the ask fires only when the profile read comes
    // back empty), yet the cross edge bob → alice has not crossed. The
    // replica profile the first pull seated is dropped — a row the tier
    // wrote, not a fixture, and dropping it is the node-lost-its-replica
    // shape the pull-on-demand design recovers from — so the next create
    // re-asks, and the answer now carries bob's edge toward alice. This
    // create is accepted: the ask re-seats the profile, and alice's own-side
    // edge is seeded. It also founds the direct conversation check 4
    // exercises; the create there resolves idempotently to this one.
    psql(PG.db1, `delete from profile where account_id = '${bobUuid}'`);
    log(
      'fixture',
      "bob's replica profile dropped on node 1 so its gate re-asks (the cross edge has not crossed yet)",
    );
    await alice.startConversation(ConversationKind.Direct, [bobId]);
    await waitForRow(
      PG.db1,
      `select row_to_json(t) from relationship t where account_id = '${bobUuid}' and other_id = '${aliceUuid}'`,
      "bob's friendship edge toward alice, carried to node 1 by the re-ask's answer",
    );
    log('fixture', "bob's cross friendship edge seated on node 1 by the re-ask's answer");
  }

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
  // stamped into the row's home_region at creation. How the row plus the
  // whole membership reach node 2 depends on the binary: with the
  // row-replication tier, node 2 pulls them itself when bob's subscribe finds
  // no local membership; without it, they are fixtured the same way the
  // room's were. Bob learns it from his conversation list there and watches
  // it, which is the conversation tier's subscribe half: node 2 asks node 1
  // to watch the conversation. Alice's sealed message then rides the tiered
  // fan-out (one federated copy per watching node), and bob's reply is handed
  // by node 2 to the home node, which publishes it to alice's session from
  // its own hub. Both directions must deliver *and decrypt*: the sealed
  // envelope crosses every node on the way unopened.
  //
  // The harness runs released binaries, and the tiers are in the tree before
  // they are in a release. The stamp is the tell: a binary that has the
  // conversation tier stamps home_region at creation and must also carry the
  // message across (a failure there is a real failure, exit non-zero); a
  // binary that predates the tier writes no home_region at all, and the
  // honest outcome is a reported gap, the same stance the presence tier's
  // check takes. Within the stamped binaries, the replication counter is the
  // second tell: a binary with the counter must pull the rows itself, and a
  // binary between the two releases (stamped but counterless) gets the
  // fixtures — which is why the fixture decision reads the counter and not
  // the stamp.
  const direct = await alice.startConversation(ConversationKind.Direct, [bobId]);
  const directUuid = idToUuid(direct.conversationId);
  const directRow = mustRow<Record<string, unknown>>(
    PG.db1,
    `select row_to_json(t) from conversation t where conversation_id = '${directUuid}'`,
    'the direct conversation row',
  );
  const homeRegionRaw = directRow.home_region;
  const homeRegion = typeof homeRegionRaw === 'string' ? homeRegionRaw : '';
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
    if (rowsReplicate) {
      // CHECK 4, replication regime: no conversation fixtures at all. The
      // fixture-time trigger create already forced node 1's privacy gate to
      // pull bob's account across the mesh (nothing else seats the profile
      // the gate reads), and the create here resolves idempotently to the
      // conversation that create founded — so demand the proof straight from
      // node 1's store: bob's account row, and the cross friendship edge the
      // re-ask's answer carried, both seated by pulls, not fixtures.
      const bobOnNode1 = rowJson<Record<string, unknown>>(
        PG.db1,
        `select row_to_json(t) from account t where account_id = '${bobUuid}'`,
      );
      if (bobOnNode1 === null) {
        fail(
          'check-4',
          "alice's create did not pull bob's account row across the mesh (node 1's store still has no such row)",
        );
      }
      const bobEdgeOnNode1 = rowJson<Record<string, unknown>>(
        PG.db1,
        `select row_to_json(t) from relationship t where account_id = '${bobUuid}' and other_id = '${aliceUuid}'`,
      );
      if (bobEdgeOnNode1 === null) {
        fail(
          'check-4',
          "the replication answer did not seat bob's friendship edge toward alice on node 1",
        );
      }
      log(
        'check-4',
        "bob's account row and friendship edge sit on node 1, pulled by the create's privacy gate",
      );
      proved.push(
        'row replication: node 2 → node 1 (account, profile, and edges pulled through the gate)',
      );
      // Bob's watch is the pull's trigger on this side: the subscribe finds
      // no local membership, node 2 asks the mesh, node 1 answers with the
      // row and both member rows, and only then is the topic granted. The
      // watch precedes the list on purpose — the list cannot show a
      // conversation the store does not hold yet, and the pull is what seats
      // it.
      await bob.watchConversation(direct.conversationId);
      const directOnNode2 = rowJson<Record<string, unknown>>(
        PG.db2,
        `select row_to_json(t) from conversation t where conversation_id = '${directUuid}'`,
      );
      if (directOnNode2 === null) {
        fail(
          'check-4',
          "bob's watch did not pull the direct conversation row across the mesh (node 2's store still has no such row)",
        );
      }
      const membersOnNode2 = rowsJson<Record<string, unknown>>(
        PG.db2,
        `select * from conversation_member t where conversation_id = '${directUuid}'`,
      );
      if (membersOnNode2.length !== 2) {
        fail(
          'check-4',
          `the pulled conversation seats exactly two, found ${membersOnNode2.length} member rows on node 2`,
        );
      }
      log(
        'check-4',
        'the direct conversation row and its membership sit on node 2, pulled by the watch',
      );
      proved.push(
        'row replication: node 1 → node 2 (conversation row and membership pulled by the subscribe)',
      );
      // Bob's client still learns the conversation the way a real client on
      // the far node does — his conversation list — which now serves it only
      // because the pull seated the rows, and which primes his membership
      // cache so his reply can choose its audience. The one honest gap this
      // leaves: pull-on-demand answers a gate that already knows the id, so
      // discovery — a client learning a new conversation exists — is still
      // the fixture-shaped hole a push tier will have to fill.
      const bobList = await bob.loadConversations(50);
      if (!bobList.conversations.some((c) => c.conversationId === direct.conversationId)) {
        fail('check-4', 'node 2 does not serve bob the direct conversation the pull seated');
      }
      log('check-4', "bob's conversation list on node 2 serves the pulled direct conversation");
    } else {
      insertJson(PG.db2, 'conversation', directRow);
      // The whole membership crosses, not bob's row alone: node 2's store
      // answers the roster read his client makes when it chooses the reply's
      // audience, and an audience without alice is an envelope alice can never
      // open. The dm_federation e2e crosses the same rows for the same reason.
      const directMembers = rowsJson<Record<string, unknown>>(
        PG.db1,
        `select * from conversation_member t where conversation_id = '${directUuid}'`,
      );
      if (directMembers.length !== 2) {
        fail(
          'check-4',
          `a direct conversation seats exactly two, found ${directMembers.length} member rows`,
        );
      }
      for (const member of directMembers) {
        insertJson(PG.db2, 'conversation_member', member);
      }
      // Bob's client learns the conversation the way a real client on the far
      // node does: his conversation list. That both proves node 2 serves a
      // conversation homed on node 1 and primes his membership cache, without
      // which his reply bails with "membership is unknown" before it is sealed.
      const bobList = await bob.loadConversations(50);
      if (!bobList.conversations.some((c) => c.conversationId === direct.conversationId)) {
        fail('check-4', 'node 2 does not serve bob the direct conversation homed on node 1');
      }
      log('check-4', "bob's conversation list on node 2 serves the direct conversation");
      await bob.watchConversation(direct.conversationId);
    }
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

    if (rowsReplicate) {
      // Node 2's pull of alice's account happened at fixture time — bob's
      // trigger create forced it, and its answer carried alice's cross
      // friendship edge — so demand both proofs: that edge, and the
      // applied-answer counters on both nodes. These close the loop the
      // checks opened — the direct path crossed on replicated rows and
      // nothing else.
      const aliceEdgeOnNode2 = rowJson<Record<string, unknown>>(
        PG.db2,
        `select row_to_json(t) from relationship t where account_id = '${aliceUuid}' and other_id = '${bobUuid}'`,
      );
      if (aliceEdgeOnNode2 === null) {
        fail(
          'check-4',
          "the replication answer did not seat alice's friendship edge toward bob on node 2",
        );
      }
      for (const [label, origin] of [
        ['node 1', NODE1_HTTP],
        ['node 2', NODE2_HTTP],
      ] as const) {
        const replicated = await metric(origin, 'migo_mesh_rows_replicated_total');
        if (replicated < 1) {
          fail(
            'check-4',
            `${label} never applied a replicated row (migo_mesh_rows_replicated_total is still ${replicated})`,
          );
        }
        log('check-4', `${label} applied ${replicated} replicated row answer(s)`);
      }
      proved.push('row replication: the direct-message path crossed on pulled rows, not fixtures');
    }
  }

  // CHECK 5: a presence change crosses to a watcher on the peer node. Bob's
  // watch of alice's user topic on node 2 is authorized by her profile row on
  // that node — fixtured when the binary predates the row-replication tier,
  // pulled across the mesh by bob's fixture-time trigger create when it has
  // the tier (so it is seated long before this subscribe asks about it) —
  // and by the friendship the rows carry either way; the granted
  // watch is the user-topic tier's subscribe half — node 2 asks its peers to
  // watch alice — and alice then changes presence on node 1, whose forward
  // half carries one federated copy per watching node.
  //
  // Both tiers (user-topic presence, conversation) shipped in the same
  // release, so the DM check's home_region stamp is this check's version tell
  // too: a binary that stamps it carries both tiers and must carry the
  // presence change across (a miss is a real failure, exit non-zero); a
  // binary that predates them reports the gap honestly, the same stance the
  // DM check takes.
  //
  // The drain matters more here than anywhere else: presence is an edge of a
  // session, not a stored event — a change is never re-published, so a change
  // that publishes before node 1 has recorded node 2's ask is lost, not
  // retried. The ask gets the same drain the conversation tier's subscribe
  // gets before the first send.
  const presenceHeard: { userId: Id; state: PresenceState }[] = [];
  bob.presence.onPresence((event) => {
    presenceHeard.push({ userId: event.userId, state: event.state });
    log('bob', `presence event from ${event.userId} state=${event.state}`);
  });
  const subscribeResponse = await bob.subscribe([{ kind: TopicKind.User, id: aliceId }]);
  const watchGranted = subscribeResponse.accepted.some(
    (topic) => topic.kind === TopicKind.User && topic.id === aliceId,
  );
  log('check-5', `bob's watch of alice's user topic on node 2: granted=${watchGranted}`);
  if (!watchGranted) {
    fail(
      'check-5',
      "node 2 refused bob the watch of alice's user topic, so the presence crossing cannot be observed",
    );
  }
  await sleep(3_000);
  await alice.presence.setPresence(PresenceState.Away);
  log('check-5', 'alice set presence Away on node 1');
  const check5 = await waitFor(
    () => presenceHeard.some((e) => e.userId === aliceId && e.state === PresenceState.Away),
    DELIVERY_TIMEOUT_MS,
  );
  let presenceReported = false;
  if (homeRegion === '') {
    log(
      'gap-presence',
      'CONFIRMED GAP: presence did not cross the link (the user-topic tier ships with the release that stamps home_region)',
    );
    presenceReported = true;
  } else if (!check5) {
    fail(
      'check-5',
      `the presence change did not cross the mesh link; bob observed: ${JSON.stringify(presenceHeard)}`,
    );
  } else {
    proved.push('presence change: node 1 → node 2');
    log('check-5', 'PROVED: bob, watching alice from node 2, saw her presence change to Away');
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
  if (presenceReported) {
    console.log(
      '  REPORTED presence does not cross the link (the user-topic tier ships with the release that stamps home_region)',
    );
  }
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
