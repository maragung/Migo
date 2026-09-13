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

## What it does not prove — reported, not hidden

These are reported by the sync check as known gaps, and they are the design
today, not harness limitations:

- **Presence does not federate — in the released binaries this harness
  runs.** The server tree now carries the user-topic tier (FED_USER_SUBSCRIBE
  / FED_USER_EVENT): bob's watch of alice's user topic on node 2 asks the
  peers to watch her, and her presence change on node 1 is forwarded as one
  copy per watching node, proven in-process by
  `server/crates/migod/tests/cross_node_presence.rs`. The sync check here
  runs against released `migod` binaries, so it keeps reporting the gap until
  a release carries the tier; flipping the check to demand it is the release
  follow-up.
- **A 1:1 direct message does not federate.** Only room conversations ride
  the tiered fan-out; a direct conversation's messages stay on the node they
  were sent to.

The harness also stands in, deliberately, for replication that does not exist
yet: accounts, devices, key bundles, room rows, and membership rows do not
replicate across nodes, so the sync check copies the counterpart rows
directly in PostgreSQL (verbatim copies of what the registering node already
holds, plus the membership rows a join on that node would have written).
Without those fixtures the cross-node paths would fail on missing rows
before they ever reached the mesh. A future replication layer replaces the
fixtures; until then the check proves exactly the part that exists: the
configuration-formed link and the tiered room fan-out over it.

## Files

- `run.sh` — the runner: ports, identities, databases, per-node config,
  startup, link verification, teardown (only the PIDs it started).
- `src/sync-check.ts` — the prover (also runnable directly against any two
  linked nodes: `NODE1_HTTP=… NODE2_HTTP=… node src/sync-check.ts`).
- `README.md` — this file.
