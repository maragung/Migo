# 04 — Data model

Source of truth: PostgreSQL. Migrations in [`../server/migrations`](../server/migrations),
applied in order, never edited after merge (brief §126).

## 1. Identifiers

| Kind                        | Format                                                                           | Why                                                                              |
| --------------------------- | -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| Internal PK                 | 128-bit **UUIDv7** stored as `uuid`                                              | Time-ordered → index locality; globally unique → no coordination between regions |
| Client-generated message id | **ULID** (same 128 bits on the wire)                                             | Client can create an id offline; dedup is free (brief §68)                       |
| Public user id              | `MGO-` + 12 hex digits of a mix of the id's random half, e.g. `MGO-7F82A91CBBAD` | Short, human-speakable, immutable, derived not stored (brief §79)                |
| Public room id              | `MGO-ROOM-82F91AB402`                                                            | Immutable; renaming a room never breaks links (brief §81)                        |

Immutable ids everywhere; display names are mutable and never a key.

## 2. Domain map

```
account ─┬─ device ── session ── refresh_token_chain
         ├─ profile ── privacy_settings
         ├─ identity_key ── signed_prekey ── one_time_prekey   (public halves only)
         ├─ relationship (friend | follow | block | favorite)
         ├─ conversation_member ── conversation ── message ── receipt
         ├─ room_member ── room ── room_role ── room_permission
         │                    └── room_moderation_action ── audit_entry
         ├─ ledger_account ── ledger_entry (double-entry)  ── gift / purchase
         ├─ progression (xp, level) ── badge_award ── achievement
         └─ report (as reporter / as subject)
```

## 3. Tables that carry the load

### `messages` — append-only, partitioned

```sql
message_id      uuid    primary key        -- ULID from the client
conversation_id uuid    not null
seq             bigint  not null           -- monotonic per conversation
sender_id       uuid    not null
sender_device   uuid    not null
kind            smallint not null          -- text | media | system | game | gift
envelope        bytea   not null           -- E2E ciphertext, or MSE room payload
created_at      timestamptz not null
edited_at       timestamptz
deleted_at      timestamptz               -- tombstone, so clients can converge
unique (conversation_id, seq)
```

- **Partitioned monthly** by `created_at`: retention and archival become
  `DETACH PARTITION`, not a 40-million-row `DELETE` that eats the primary.
- Hash-sharded by `conversation_id` when a single primary is no longer enough. Sharding
  by conversation keeps every hot-path query single-shard.
- `envelope` is opaque for private conversations. The database **cannot** read them.
- Deletes are tombstones. A hard delete that offline clients never learn about produces
  permanent ghost messages.

Indexes: `(conversation_id, seq desc)` for paging, `(conversation_id, created_at desc)`
for time queries, partial index on `deleted_at is null`. Nothing else — every extra index
is write amplification on the hottest table in the system.

### `conversation_cursor` — the sync primitive

```sql
conversation_id uuid, member_id uuid,
delivered_seq bigint, read_seq bigint, notified_seq bigint,
primary key (conversation_id, member_id)
```

Unread counts are `last_seq − read_seq`, computed, never stored as a counter that drifts.

### `ledger_transaction` / `ledger_entry` — double-entry, immutable

Two tables, because the _why_ belongs to the transaction and the _how much_ belongs to
each leg. Putting the reason on every leg would let two legs of one transfer disagree
about what it was for.

```sql
-- ledger_transaction
tx_id           uuid primary key,
reason          smallint not null,   -- gift | purchase | reward | refund | …
ref_id          uuid,                -- gift, purchase, event, game
idempotency_key text not null unique,
created_at      timestamptz not null,
created_by      uuid                 -- null for system-issued value

-- ledger_entry
tx_id      uuid not null,       -- groups the debit/credit legs
leg_index  smallint not null,   -- position within the transaction; legs are ordered
account_id uuid not null,       -- ledger_account, not user: users have several
amount     bigint not null,     -- signed minor units, never zero; a tx sums to zero
currency   smallint not null,   -- coins | gems | points
created_at timestamptz not null,
primary key (tx_id, leg_index)
```

An entry has no id of its own. Nothing outside the ledger ever needs to name a single
leg, and `(tx_id, leg_index)` says something a surrogate id does not: which leg this
is. A debit followed by a credit is a transfer; the other order is a refund.

Balances are the sum of entries (with periodic snapshots for speed), never a mutable
`balance` column. This is how finance has done it for centuries and the reason is simple:
an append-only log with a zero-sum invariant is auditable, and a mutable counter is not.
Combined with `idempotency_key`, a retried gift can never double-charge (brief §29, §87).

### `room_member`

```sql
room_id uuid, user_id uuid, role smallint,
permissions_grant bigint, permissions_deny bigint,   -- overrides on top of the role
joined_at timestamptz, muted_until timestamptz, banned_until timestamptz,
primary key (room_id, user_id)
```

Effective permission = `role_default | grant` then `& ~deny`. Deny wins — moderation must
never be overridable by an inherited grant.

### `prekeys` — public halves only

```sql
identity_key   (user_id, device_id, public_key, created_at, revoked_at)
signed_prekey  (user_id, device_id, key_id, public_key, signature, expires_at)
one_time_prekey(user_id, device_id, key_id, public_key, consumed_at)
```

The server hands out a bundle and marks the one-time prekey consumed. It holds no private
key material for any user, ever.

### `audit_entry` — append-only

Actor, action, target, before/after summary (never message content), reason, request id,
IP class, timestamp. Written in the same transaction as the action, so an action without
an audit row is impossible.

## 4. Consistency choices (brief §66)

| Strong (transactional)          | Eventual (cache/derived)       |
| ------------------------------- | ------------------------------ |
| Accounts, credentials, sessions | Presence, online counts        |
| Permissions, roles, bans        | Room popularity, trending      |
| Economy ledger and balances     | Leaderboards (snapshotted)     |
| Message insert + seq assignment | Unread badges on other devices |
| Ownership transfer              | Search/discovery indexes       |

If it can be recomputed, it does not deserve a transaction. If money or safety depends on
it, it gets one.

## 5. Migrations

Numbered `NNNN_description.sql`, forward-only in production with an explicit `-- down`
section for local development. Rules:

1. Additive first: add nullable column → backfill in batches → start writing →
   backfill remainder → add constraint. Never one long-running `ALTER` on a hot table.
2. `CREATE INDEX CONCURRENTLY` for anything over a million rows.
3. Renames are two releases (add new, dual-write, migrate readers, drop old).
4. No destructive migration without a tested restore (brief §104, §126).
5. Every migration is exercised by CI against a real Postgres from empty **and** from
   the previous release's snapshot.
6. **An applied migration is never edited.** SeaORM's migrator records only that a name
   ran, not what it contained (ADR-0012), so an edit means "already applied" on old
   databases and something else on new ones — and no gate can catch it. This is the one
   rule on this list that is enforced by nothing but discipline, which is why it is also
   written at the head of every migration file.
7. Migrations are the **source of truth for the entities**, not the other way round.
   `tools/entity-codegen` reads `server/migrations/*.sql` and writes one SeaORM entity
   module per table; `make entity-check` fails CI when they disagree. A schema change is
   therefore two files in one commit — the migration and the regenerated entities — and
   never a hand-edit of the latter.

`migod migrate` takes a `pg_advisory_lock` around the whole set before applying anything,
because SeaORM's migrator does not and two replicas starting together would otherwise
race into a half-applied schema.

## 6. Retention

| Data               | Retention                                                                                                             |
| ------------------ | --------------------------------------------------------------------------------------------------------------------- |
| Message ciphertext | Per user/room policy; default indefinite, tombstoned on delete                                                        |
| Delivery metadata  | 30 days                                                                                                               |
| Presence           | Ephemeral (Redis only)                                                                                                |
| IP-derived data    | Truncated, 7 days                                                                                                     |
| Audit / moderation | 1 year (longer where legally required)                                                                                |
| Ledger             | Indefinite, immutable                                                                                                 |
| Deleted accounts   | Personal data purged; ledger and moderation records retained as legally required, unlinked from identity (brief §125) |
