# 01 — Architecture

> Status: living document. Decisions with lasting consequences are recorded as
> [ADRs](adr/); this document explains how the pieces fit together today.

## 1. Shape of the system

```
                    ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
   clients          │  Web (PWA)   │   │   Android    │   │  Bots / SDK  │
                    └──────┬───────┘   └──────┬───────┘   └──────┬───────┘
                           │  WSS / HTTP3            │                  │
              ═════════════╪═════════════════════════╪══════════════════╪══════
                           ▼                         ▼                  ▼
                 ┌───────────────────────────────────────────────────────────┐
   edge          │  Anycast / GeoDNS  →  regional gateway (TLS 1.3, QUIC)    │
                 └───────────────────────────┬───────────────────────────────┘
                                             ▼
                 ┌───────────────────────────────────────────────────────────┐
                 │                        migod                              │
                 │   role-composed process; one binary, N roles              │
                 │                                                           │
                 │  gateway   api      room      game      federation        │
                 │  ───────   ─────    ──────    ──────    ──────────        │
                 │  socket    REST     fanout    engine    mesh peer         │
                 │  sessions  upload   presence  bots      routing table     │
                 │  backpres. rate     sharding  anticheat replication       │
                 └───┬─────────────┬────────────┬────────────┬───────────────┘
                     ▼             ▼            ▼            ▼
              ┌───────────┐ ┌───────────┐ ┌──────────┐ ┌──────────────┐
   state      │PostgreSQL │ │   Redis   │ │ S3/MinIO │ │ peer migod   │
              │ (truth)   │ │(ephemeral)│ │ (media)  │ │ nodes (mesh) │
              └───────────┘ └───────────┘ └──────────┘ └──────────────┘
```

Client → gateway is the only public realtime path. Node → node is the mesh
([06-federation.md](06-federation.md)). Nothing internal is exposed to the Internet
(brief §118).

## 2. Modular monolith, role-composed

`migod` is a single binary. Which subsystems it activates is configuration:

```bash
MIGO_NODE__ROLES=api,gateway              # edge node
MIGO_NODE__ROLES=room,game                # room/fanout node
MIGO_NODE__ROLES=federation               # mesh relay
MIGO_NODE__ROLES=api,gateway,room,game,federation   # dev laptop: everything
```

Why this and not microservices (brief §92):

- **One deploy artefact, N topologies.** The same code runs as a laptop all-in-one and
  as a 40-node multi-region cluster. There is no "works on my machine" topology gap.
- **Boundaries are enforced at compile time.** Each subsystem is its own crate and can
  only reach its declared dependencies. When traffic justifies a split, the crate
  already _is_ the service: swap the in-process call for a mesh RPC.
- **Cost.** Premature microservices buy you distributed tracing bills and 5 ms of
  network per hop, before you have users.

The split-out rule: extract a role into its own deployable when it (a) needs a different
scaling axis, (b) needs a different failure domain, or (c) has a different release
cadence. Not before.

## 3. Crate map

Dependencies point **downward only**. A cycle is a build error, by design.

```
                       ┌────────────┐   ┌──────────┐
  layer 5  binary      │   migod    │   │  loadgen │
                       └──────┬─────┘   └────┬─────┘
                              ▼              ▼
  layer 4  transports  ┌────────────┐  ┌──────────┐  ┌──────────────┐
                       │migo-gateway│  │ migo-api │  │migo-federation│
                       └──────┬─────┘  └────┬─────┘  └───────┬──────┘
                              └──────┬──────┴────────────────┘
                                     ▼
  layer 3  domain   auth · messaging · rooms · presence · social · media
                    economy · games · bots · moderation · notify
                                     │
                                     ▼
  layer 2  platform    migo-store · migo-cache · migo-ratelimit
                                     │
                                     ▼
  layer 1  kernel   migo-core · migo-wire · migo-protocol · migo-crypto
```

| Crate             | Responsibility                                                                                                      | Phase |
| ----------------- | ------------------------------------------------------------------------------------------------------------------- | ----- |
| `migo-core`       | IDs, time, config loading/validation, error taxonomy, telemetry init                                                | 1     |
| `migo-wire`       | Frame codec: varints, length prefixes, opcode framing, limits                                                       | 1     |
| `migo-protocol`   | Generated message types + envelope, version & feature negotiation                                                   | 1     |
| `migo-crypto`     | Node identity, session tokens, Argon2id, AEAD, X3DH + Double Ratchet                                                | 1     |
| `migo-store`      | Storage traits; Postgres (SeaORM, entities generated from the migrations) and in-memory implementations; migrations | 1     |
| `migo-cache`      | Ephemeral state traits; Redis and in-memory implementations                                                         | 1     |
| `migo-ratelimit`  | Cost-based token buckets, adaptive limits, abuse counters                                                           | 1     |
| `migo-auth`       | Registration, login, device sessions, refresh rotation + theft detection                                            | 1     |
| `migo-messaging`  | Conversations, sequencing, dedup, delivery/read receipts, offline queue                                             | 1     |
| `migo-presence`   | Presence state machine, heartbeats, aggregation                                                                     | 2     |
| `migo-rooms`      | Public/Managed rooms, membership, roles, permissions, fanout topology                                               | 2–3   |
| `migo-social`     | Profiles, friends, follows, blocks, discovery                                                                       | 2     |
| `migo-media`      | Signed upload URLs, validation, thumbnails, quotas                                                                  | 2     |
| `migo-moderation` | Reports, actions, audit log, automated detection                                                                    | 3     |
| `migo-notify`     | Push/local notification fan-out with content redaction                                                              | 2     |
| `migo-economy`    | Double-entry ledger, currency, gifts, XP, badges                                                                    | 4     |
| `migo-games`      | Server-authoritative game engine + built-in mini games                                                              | 5     |
| `migo-bots`       | Bot registry, permissions, sandboxed dispatch, bot API                                                              | 5     |
| `migo-federation` | Node identity, mesh handshake, routing table, replication                                                           | 7     |
| `migo-gateway`    | WebSocket sessions, subscription hub, backpressure, fanout                                                          | 1     |
| `migo-api`        | HTTP/REST surface, OpenAPI, upload endpoints                                                                        | 1     |
| `migod`           | Composition root: config → wiring → role startup → graceful shutdown                                                | 1     |

## 4. The hot path

Everything about latency and bandwidth is decided in one code path: _a message arriving
at a gateway and reaching N subscribers_. It is written to these rules.

1. **Encode once, send N times.** The outbound frame is built into a `bytes::Bytes`
   and cloned by reference-count per subscriber. A 30 000-member room encodes the
   frame once, not 30 000 times.
2. **No allocation per subscriber on the fanout path.** Subscriber lists are
   pre-sized; send is a channel push of a refcounted buffer.
3. **Bounded queues, everywhere.** Every session has a bounded outbound queue.
   Full queue is not a reason to grow memory — it is a reason to apply the
   drop policy (§5).
4. **Never block the fanout task on I/O.** Persistence happens through an outbox;
   fanout does not await the database.
5. **Ordering is per-conversation, not global.** A monotonic per-conversation
   sequence is assigned by the owning shard. Cross-conversation order is irrelevant
   and expensive to guarantee.

## 5. Backpressure and the drop policy

The failure that kills chat servers is the slow consumer: a phone on a train, holding a
socket, while the server buffers megabytes for it. Migo classifies every outbound frame:

| Class         | Examples                                                 | When the queue is full                                                                                                                                               |
| ------------- | -------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Critical`    | message, receipt, auth, error, resume                    | Never dropped. Session is marked _lagging_; if the queue stays full past the deadline the session is closed with `RESUME_REQUIRED` and the client re-syncs by cursor |
| `Coalescable` | presence, typing, member count, room stats               | Newest replaces oldest for the same key. Queue depth cannot grow                                                                                                     |
| `Droppable`   | animations, "user is looking at chat", decorative events | Dropped silently, counted in metrics                                                                                                                                 |

Closing a lagging socket is _cheaper and more correct_ than buffering: the client
already knows how to resume from a cursor (§6), so a reconnect costs a few hundred
bytes and loses nothing.

## 6. Sync model: cursors, not full refreshes

Every conversation and room carries a monotonic `seq`. A client stores the highest
contiguous `seq` it has, and sends it on connect:

```
client → SYNC_REQUEST { conversation_id, have_seq: 1000 }
server → SYNC_RESPONSE { from: 1001, to: 1012, more: false }   # 12 messages, not 40 000
```

- Gap detection is the client's job: receiving 1001, 1002, 1004 triggers a **ranged**
  fetch of 1003, never a full resync (brief §67).
- Deduplication uses the client-generated message ID (ULID). Retrying a send after a
  flaky reconnect is safe and idempotent (brief §68).
- When history is compacted or the client is too far behind, the server answers
  `SYNC_TRUNCATED { from_seq }` and the client shows a "load older" boundary rather
  than silently missing messages.

## 7. Reliable fanout: outbox + at-least-once + dedup

```
send → validate → persist(message + outbox rows)  ← one transaction
                       │
                       ├─ local subscribers        (immediate, in-process)
                       └─ outbox dispatcher ──► peer nodes / push / bots
                                                  retries with backoff, idempotent
```

At-least-once delivery plus idempotent application by message ID gives _effectively
once_ semantics without distributed transactions. This is the boring, correct answer.

## 8. Storage split

| Data                                              | Store                                                     | Consistency                       |
| ------------------------------------------------- | --------------------------------------------------------- | --------------------------------- |
| Accounts, devices, rooms, membership, permissions | PostgreSQL                                                | Strong                            |
| Economy ledger, gift and currency transactions    | PostgreSQL, double-entry, append-only                     | Strong, serialisable              |
| Messages (ciphertext), receipts                   | PostgreSQL, partitioned by month, sharded by conversation | Strong per conversation           |
| Presence, typing, session routing, rate counters  | Redis (or in-memory in dev)                               | Eventual, lossy by design         |
| Media blobs                                       | S3-compatible                                             | Read-after-write on new keys only |
| Search / discovery indexes                        | Derived, rebuildable                                      | Eventual                          |

Two rules: **ephemeral state is never in Postgres**, and **anything in Redis must be
reconstructible**, because Redis will be lost at some point and that must be a
non-event ([09-observability-ops.md](09-observability-ops.md)).

### 8.1 The Redis keyspace

Every key `migo-cache` writes is built by one type, `CacheKey`, and looks like
`m:<scope>:<tail>`:

| Scope  | Holds                                                 | Redis type      |
| ------ | ----------------------------------------------------- | --------------- |
| `kv`   | Opaque values: idempotency markers, short-lived locks | string          |
| `cnt`  | Fixed-window counters                                 | string + `PTTL` |
| `tb`   | Token buckets, one per rate-limited surface           | hash + `PTTL`   |
| `pres` | Presence, one hash field per device                   | hash            |
| `typ`  | Typing, one hash field per account                    | hash            |
| `rt`   | Session routes, `device → {node, encoded route}`      | hash            |
| `rti`  | Reverse index, `account → device → node`              | hash            |

Five decisions in that layout are worth stating, because each of them is the kind
that is expensive to change later:

**The prefix is one character.** It is sent on every command, and the problem it is
often stretched to solve — two deployments sharing one Redis — is already solved by
the database number in the URL.

**Tails are escaped, not trusted.** Some tails arrive from the network (an
idempotency key is client-chosen). Anything outside `[A-Za-z0-9._/-]` becomes `%XX`,
so a tail of `m:rt:0198…` lands at `m:kv:m%3Art%3A0198…` and cannot reach the routing
namespace. The escaping is injective, so two distinct tails never collide, and the
result is truncated at 256 bytes.

**Presence and typing are hashes, keyed by the owner.** Reading one account's
presence is then one `HGETALL` rather than a `SCAN`, and a device's field expires by
the deadline stored inside its own value — the key's TTL only governs storage.

**A route is a two-field hash.** The node id is stored in plain text alongside the
wire-encoded route so that the unbind script can compare _who owns this device_
without decoding anything. Comparing the whole encoded value instead would reject a
legitimate unbind whose only difference was a refreshed heartbeat.

**A token bucket stores only what cannot be recomputed.** Two hash fields: the level
in milli-tokens, and the millisecond at which that level was accurate. The bucket's
_shape_ — capacity and refill rate — is passed in on every call rather than stored, so
retuning a limit takes effect on the next request with no migration and no stale copy
in Redis. The level is in milli-tokens because a bucket refilling at `r` tokens per
second gains exactly `r` milli-tokens per millisecond, which makes refill one
multiplication with no division and therefore no rounding error to accumulate. The
key's TTL is the time the bucket needs to refill from empty, plus a second of clock
slack: an absent bucket reads as a full one, so state that has expired says nothing a
missing key would not. Buckets therefore clean themselves, the largest keyspace in the
system needs no sweeper, and an idle subject costs nothing. Nothing is written on a
refusal, which halves the limiter's own Redis traffic exactly when refusals are the
common case and stops a flood from repeatedly extending the TTL of the bucket bouncing
it.

Read-modify-write goes through Lua rather than `WATCH`/`MULTI`: compare-and-set,
increment-within-a-window, spend-from-a-bucket, and both route operations are each one
round trip, atomic by construction. That matters most for the buckets: a
compare-and-set loop on a hot subject — a busy room, a large NAT — succeeds about one
time in N with N concurrent writers, so it would start refusing traffic it should have
allowed, and get _less_ accurate as load rose. TTLs are capped at 7 days — anything
that wants to live longer is not ephemeral state and belongs in Postgres — and values
at 256 KiB.

The caller's rule is one sentence: **a cache error must never fail a request that
could have succeeded without the cache.** The traits still return `Result`, because
swallowing the error inside the backend would hide a Redis outage from the metrics
whose job is to reveal it. Callers log and degrade; they do not propagate.

## 9. Configuration and startup

Layered, in increasing precedence: built-in defaults → `config/{env}.toml` →
environment (`MIGO_SECTION__KEY`) → CLI flags. The whole config is deserialised and
**validated once at boot**; an invalid value is a startup failure, never a surprise at
3 a.m. (brief §102).

Startup sequence, and nothing may be reordered casually:

```
load config → validate → init telemetry → load/derive node identity
→ open stores → run health checks → bind listeners → announce to mesh → accept traffic
```

Health checks precede traffic (brief §100). Shutdown reverses it, draining first
(brief §101): stop accepting, tell connected clients to reconnect elsewhere with a
jittered deadline, flush outbox, hand over room ownership, close.

## 10. Failure domains

| Failure             | Behaviour                                                                                                                                          |
| ------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| One gateway process | Clients reconnect (backoff+jitter) to another gateway in-region; sessions resume by cursor                                                         |
| Whole region        | GeoDNS/anycast withdraws; clients attach to next-nearest region; rooms whose home region is down become read-only from cache until ownership moves |
| PostgreSQL primary  | Writes fail fast with a retryable error code; reads served from replica; clients queue outbound messages locally                                   |
| Redis               | Presence degrades to "unknown", typing indicators disappear, rate limits fall back to conservative local buckets. Chat keeps working               |
| Object storage      | Media upload/download fails with a retryable error; text chat unaffected                                                                           |
| Mesh partition      | Each side keeps serving its local users; cross-region delivery queues in the outbox and drains on heal                                             |

Design test: _if this component vanishes, can a user still send a text message to
someone in the same region?_ For everything except Postgres and the gateway, the answer
must be yes.

## 11. Multi-tenancy of abuse

Rate limiting is **cost-based**, not request-count-based: each operation declares a
cost, and a caller spends from token buckets keyed by IP, user, device, room, and bot
(brief §120). Sending a message costs 1; creating a room costs 200; uploading media
costs 50. New and untrusted accounts get smaller buckets. This is one mechanism instead
of forty ad-hoc counters, and it makes the abuse surface auditable.

## 12. Feature flags and kill switches

Every subsystem that can be hot is registered with a runtime flag
(`features.social_feed`, `features.translation`, `features.media_upload`). Flags are
per-region. Regret costs one config push, not a rollback and a redeploy. A kill switch
that has never been exercised does not exist — they are tested in staging every release.

## 13. Client architecture (shared shape, per-platform implementation)

Both web and Android implement the same five layers, so behaviour is identical and bugs
are reproducible across platforms:

```
UI  ──▶ Store (observable app state)
          ▲            │
          │            ▼
      Local DB ◀── Sync engine ◀── Connection manager ──▶ Wire codec ──▶ Crypto
   (IndexedDB /       (cursors,     (backoff+jitter,       (binary)      (E2E, local keys)
     Room)             queues,       resume, feature
                       conflicts)    negotiation)
```

Rules that keep the clients honest:

- The UI reads **only** from the local database. The network fills the database; it
  never feeds the UI directly. Offline is therefore the default code path, not a mode.
- An outbound message is written to the local DB as `pending` _before_ any network call,
  so it survives an app kill (brief §17).
- Every screen implements the full state set: loading, empty, error, offline,
  permission-denied, retry, success (brief §133).
- Private keys never leave the platform keystore boundary (Keystore / WebCrypto
  non-extractable / Keychain).

## 14. Adding a feature — the checklist we actually enforce

1. Extend the protocol IDL in `shared/protocol/schema` → `make protocol`.
2. Domain crate: types, validation, permission check, persistence, tests.
3. Transport: opcode handler (gateway) and/or route (api), rate-limit cost, error codes.
4. Client: store, local DB migration, sync path, all seven UI states, i18n strings.
5. Observability: at least one metric, one log event, one trace span.
6. Tests: unit, protocol conformance (both languages), integration, offline/reconnect,
   permission-denied, rate-limit.
7. Docs: update the relevant doc; add an ADR if you made a lasting decision.
