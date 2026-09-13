# N-node federation harness

Brings up 2–4 `migod` nodes on one host, each with its own directory (config,
media, log), its own PostgreSQL database, and a real mesh link between them —
formed purely by each node's `federation.peers` configuration, with no
programmatic admission anywhere. On top of the running stack it runs a sync
check that proves which cross-node paths the link actually carries, and
reports the ones it does not.

## Running

```sh
tools/nnode/run.sh              # 2 nodes (default)
NODES=4 tools/nnode/run.sh      # 4 nodes; the sync check still exercises nodes 1 and 2
SKIP_SYNC=1 tools/nnode/run.sh  # bring the stack up and wait; no sync check
```

Requirements on the host:

- a `migod` binary. The default is the released binary at
  `/home/dev/migo-prod/migod` (copied into the run directory before running —
  the original is never executed in place, and nothing in that directory is
  touched otherwise). Override with `MIGOD_BIN`. **The harness never builds a
  binary**: if none exists it stops and says so. The binary must be built from
  a tree that has configuration-driven peer admission (`federation.peers`);
  the harness checks for it and refuses an older binary rather than letting
  every node die at boot with an unknown-field error.
- PostgreSQL on `localhost:15432` (`migo`/`migo`; override with `PG_HOST`,
  `PG_PORT`, `PG_USER`, `PG_PASSWORD`). Databases `migo_nnode1`…`migo_nnode4`
  are created if missing and **never dropped** — a re-run converges, because
  peer admission is idempotent and every run registers fresh accounts.
- Node.js ≥ 23 (the sync check runs the TypeScript source directly) and one
  `pnpm install` at the repo root to link `@migo/sdk` into the workspace.

Ports: node _i_ listens on `BASE+2(i-1)` (mesh) and `BASE+2(i-1)+1` (HTTP and
WebSocket), bound on `127.0.0.1` only. `BASE` defaults to 18200 and must keep
every port inside 18200–18299. The harness **refuses to bind 8080, 18081,
18443, and 19992** — those belong to the production node on this host — and
refuses any port already in use.

Each node's mesh identity comes from a fixed 32-character signing key
(`000000000000000<i>-nnode-mesh-seed`): the node id is the first 16 bytes of
that key read as an `Id`, and the public key its peers carry is the Ed25519
public key of the whole seed. The seeds are fixed, so the same nodes exist on
every run.

## What it proves

The run is a success only if every PROVED path passes; a failure exits
non-zero and prints everything observed, including both nodes' logs and
`/metrics` counters.

1. **The link forms from configuration alone.** Each node's
   `federation.peers` names every other node completely — node id, public
   key, mesh address, region — and the runner verifies that each node
   admitted its peers at boot, then that every node completed at least one
   mesh handshake once the sync check has generated cross-node traffic.
   The mesh is lazy by design: a node only dials a peer when its outbox
   holds an event for that peer, so handshake counters are meaningfully
   non-zero only after traffic has flowed, never on an idle mesh.
   `migo_federation_peers_added_total` is reported per node (it is
   `NODES-1` on a fresh database; on a reused one it is lower, because
   admission is idempotent and the counter only ticks for fresh
   admissions — the handshake count is the live proof).

2. **A room join crosses the link.** bob joins a room on node 2 whose home
   node is node 1; alice's subscriber on node 1 sees the join event. This
   exercises the room relay's forward half: a non-home node sends the event
   to the home node, which publishes it to its own subscribers.

3. **Room messages cross the link, both directions, and decrypt.** alice
   sends into the room's conversation on node 1; bob's subscriber on node 2
   receives _and decrypts_ the message — including the sender-key
   distribution sealed for his device — and his reply comes back the same
   way. This exercises section 170's tiered fan-out: bob's room-topic
   subscription on node 2 asks node 1 (the room's home, matched by the
   `home_region` column) to watch the room, and node 1 forwards one copy per
   watching node.

4. **A typing signal crosses the link.** bob's typing start in the room's
   conversation reaches alice's subscriber on node 1.

5. **A 1:1 direct message crosses the link, both directions, and decrypts —
   when the running binary carries the conversation tier.** alice opens a
   direct conversation with bob on node 1 — node 1 is its home, stamped into
   the row's `home_region` at creation. How the row and its membership reach
   node 2 depends on the binary (see 7 below): with the row-replication tier
   the nodes pull them across the mesh themselves; without it they are
   fixtured the way the room's were. bob learns the conversation from his
   conversation list on node 2 (which proves the far node serves a
   conversation homed elsewhere) and watches it there (the conversation
   tier's subscribe half: node 2 asks the home node to watch it), and each
   side's sealed message reaches and decrypts on the other: alice's via the
   home node's tiered fan-out, bob's reply handed by node 2 to the home node
   and served from its own hub. The stamp is also the harness's version tell:
   a binary that stamps `home_region` must also carry the messages (a failure
   there fails the run), while a binary that predates the tier writes no
   stamp and the check reports the gap instead — the same stance the presence
   tier's check takes.

6. **A presence change crosses the link — when the running binary carries the
   user-topic tier.** bob subscribes to alice's user topic on node 2; the
   granted watch is the tier's subscribe half, which asks every allowed peer
   to watch her, and alice's change on node 1 is then forwarded as one
   federated copy per watching node. Presence is an edge of a session, never
   re-published, so the check gives the subscribe the same drain the
   conversation tier's subscribe gets before it changes presence: a change
   that publishes before the home node recorded the watcher is lost, not
   retried. Both tiers shipped in the same release, so the DM check's
   `home_region` stamp is this check's version tell too — stamped, and the
   crossing is demanded; absent, and the gap is reported.

7. **Account and conversation rows cross the link on their own — when the
   running binary carries the row-replication tier.** The tier's tell is the
   `/metrics` counter `migo_mesh_rows_replicated_total`, which only exists
   once the binary registers it (a zero value is not the same fact as
   absence, so the tell reads whether the counter is named at all). When it
   is present, the direct-message check writes **no** account, profile,
   friendship-cross-edge, conversation, or membership fixture for the direct
   path: alice's conversation create must pull bob's account, profile, and
   their edges across the mesh through node 1's privacy gate (asserted
   straight from node 1's store), bob's subscribe must pull the conversation
   row and both member rows through node 2's membership check (asserted the
   same way), and bob's reply must pull alice's account the same way. The
   applied-answer counters on both nodes are demanded non-zero at the end.
   When the counter is absent, the fixtures return exactly as before.

## What it does not prove — reported, not hidden

These are reported by the sync check as known gaps, and they are the design
today, not harness limitations:

- **Neither tier's crossings are demanded of a binary that predates the
  tier-bearing release.** The server tree carries both the user-topic
  presence tier (FED_USER_SUBSCRIBE / FED_USER_EVENT, proven in-process by
  `server/crates/migod/tests/cross_node_presence.rs`) and the conversation
  tier (FED_CONVERSATION_SUBSCRIBE / FED_CONVERSATION_EVENT, proven by
  `dm_federation.rs`). The sync check here runs released `migod` binaries,
  and it cannot know a release's feature set beyond what the binary itself
  shows: the conversation row's `home_region` stamp is the tell both checks
  share. A binary that stamps it (v0.24.10 and later) must carry the direct
  message and the presence change across — both crossings are demanded and a
  miss fails the run. A binary that predates the release writes no stamp,
  and both checks report the gaps honestly instead of failing.

The harness also stands in, deliberately, for replication that does not exist
yet. What still does not replicate in any binary, and is fixtured in both
regimes: **device and key-bundle rows** (each node must serve the other
account's devices for the clients to seal against — no device-federation tier
exists) and **every room row** (the room tier fans events out but does not
replicate the room, its membership, or its conversation; bob joins the room ON
node 2, which only works if the rows are already there). What the
row-replication tier now carries — account, profile, and friendship rows, and
conversation rows with their membership — is fixtured only for binaries that
predate the tier; on a tier-bearing binary the checks demand the nodes pull
those rows themselves, and the one hand-seeded row left on such binaries is
each node's own-side friendship edge, because the friend handshake itself does
not federate (the cross edges must and do arrive inside the replication
answers). Without the fixtures the cross-node paths would fail on missing rows
before they ever reached the mesh; with them, the check proves exactly the
part that exists: the configuration-formed link and the tiered room,
conversation, presence, and row-replication paths over it.

## Files

- `run.sh` — the runner: ports, identities, databases, per-node config,
  startup, link verification, teardown (only the PIDs it started).
- `src/sync-check.ts` — the prover (also runnable directly against any two
  linked nodes: `NODE1_HTTP=… NODE2_HTTP=… node src/sync-check.ts`).
- `README.md` — this file.
