# 10 — Testing strategy

> "Do not declare a feature finished just because the build succeeded." (brief §134)

## 1. Definition of done

A feature is done when **all** of these hold:

- [ ] Build clean, `clippy -D warnings` clean, formatted.
- [ ] Unit tests for logic and every error branch.
- [ ] Protocol conformance: new/changed frames pass the cross-language vectors.
- [ ] Integration test through the real transport against a real Postgres.
- [ ] Negative tests: unauthenticated, wrong user, wrong role, rate-limited, replayed,
      malformed, oversized.
- [ ] Offline test: the action queues locally and completes after reconnect.
- [ ] Reconnect test: killing the socket mid-operation loses nothing and duplicates nothing.
- [ ] Bandwidth check: measured bytes within the budget in [05](05-bandwidth-budget.md).
- [ ] Observability: a metric, a log event and a span exist.
- [ ] All seven UI states implemented (loading, empty, error, offline, denied, retry, success).
- [ ] i18n: no hardcoded user-facing string.
- [ ] Docs updated; ADR added if a lasting decision was made.

## 2. Test pyramid, and what each layer is for

| Layer           | Location                         | Speed      | Catches                                                            |
| --------------- | -------------------------------- | ---------- | ------------------------------------------------------------------ |
| Unit            | in-crate `#[cfg(test)]`          | ms         | Logic, boundaries, error paths                                     |
| Property        | `proptest`                       | ms–s       | Codec round-trips, permission algebra, ledger invariants           |
| Contract        | `server/crates/*/tests/contract` | ms–s       | **Two backends disagreeing** about one trait                       |
| Vector / golden | `shared/protocol/vectors`        | ms         | **Cross-language drift** — the highest-value tests in the repo     |
| Fuzz            | `cargo-fuzz` targets             | continuous | Decoder crashes, OOM, panics on hostile input                      |
| Integration     | `server/tests`                   | s          | Real DB, real WebSocket, real auth                                 |
| Simulation      | `server/tests/sim`               | s          | Reconnects, partitions, reordering, clock skew — deterministically |
| E2E             | `tests/e2e` (Playwright)         | s–min      | Web client against a real server                                   |
| Load            | `tools/loadgen`                  | min        | Throughput, fanout cost, bytes/user, memory growth                 |
| Security        | `tests/` + CI audit              | min        | Authz bypass, IDOR, replay, limits, dependency CVEs                |

## 3. Contract tests: one suite, two backends

`migo-store` has two backends behind one trait: `MemoryStore` for tests and the
deterministic simulator, `PostgresStore` for production. A trait with two
implementations is a promise that they behave the same, and the only way to keep
that promise is to run one suite against both.

So the suite is written once, backend-agnostic, and expanded twice:

```
server/crates/migo-store/tests/
├── contract/mod.rs        # the cases: pub async fn NAME(store: &SharedStore)
├── memory_contract.rs     # runner: MemoryStore
└── postgres_contract.rs   # runner: a throwaway database per case
```

Each case takes a `&SharedStore` and touches nothing backend-specific. The list of
cases lives in one `for_each_contract_case!` macro at the bottom of `contract/mod.rs`,
and both runners expand it, so **a case cannot be wired into one backend only** —
that was the whole point. Tests that genuinely belong to one backend (memory's
private `fold`/`pair` helpers, for instance) stay in that backend's own
`#[cfg(test)]` module.

The memory runner always runs. The PostgreSQL runner needs a server, and reads its
address from `MIGO_TEST_DATABASE_URL`; with the variable unset every case
early-returns, so `cargo test` on a laptop with no database still passes and still
means something. CI sets it.

Each PostgreSQL case gets its own database — `migo_contract_<case_name>`, dropped
and recreated on entry, then migrated from `server/migrations`. That is slower than
sharing one schema and worth it: cases run in parallel, and a shared database makes
every failure suspect the neighbours.

The migration is run by the same code production runs, which is the point: it exercises
the embedded SeaORM migrator, the `pg_advisory_lock` around it, and the SQL in
`server/migrations` on every single case. `entity-check` is a separate gate because it
answers a question the contract suite cannot — the suite proves the entities match the
database it just migrated, not that they match the migration files in the tree. A
regenerated entity that nobody committed passes every test here and fails on the next
person's checkout.

```bash
# A throwaway cluster, tuned for tests rather than durability.
initdb -D .pgdata -U migo --auth=trust
pg_ctl -D .pgdata -l .tmp/pg.log -o "-p 55432 -k $PWD/.tmp/pgsock \
  -c listen_addresses=127.0.0.1 -c fsync=off -c full_page_writes=off \
  -c synchronous_commit=off -c max_connections=300" start

cd server
MIGO_TEST_DATABASE_URL="postgres://migo@127.0.0.1:55432/postgres" \
  cargo test -p migo-store
```

The URL names a _maintenance_ database (`postgres`), not the test database: the
harness connects there only to issue `create database`. `max_connections` is high
because every case holds its own small pool. `fsync=off` would be malpractice on a
real server and is exactly right on one whose entire contents are disposable.

When the two backends disagree, the question is which one is wrong — not which one
is easier to change. `create_media` validated its arguments after checking for a
duplicate id, which PostgreSQL cannot reproduce because it learns about a duplicate
from a primary key, and a primary key is only consulted after the row exists. The
memory backend was the one that changed.

### 3.1 Cache contract tests

`migo-cache` is the same shape — `MemoryCache` and `RedisCache` behind six narrow
traits — so it gets the same treatment: 48 cases in `contract/mod.rs`, one
`for_each_contract_case!` macro, two runners. Redis is opt-in through
`MIGO_TEST_REDIS_URL`, unset means every case early-returns.

Two things differ from the storage suite, both because the subject is time.

**Cases share one database and isolate by key, not by flushing.** Every fixture takes
a fresh keyspace scope from an `AtomicU64` and prefixes every key and id with it, so
parallel cases cannot collide. The suite never issues `FLUSHDB`: a flush is exactly what
would destroy an operator's Redis the first time this variable is pointed at a real
one by accident. A counter, not a hash of the case name, because a counter cannot
collide.

**The counter isolates cases from each other, and not runs from each other** — which is
worth writing down because the code claimed otherwise for a while and the claim was
wrong. Every process starts the counter at one, so the second run hands out exactly the
scope numbers the first run used. Most cases write with a sixty-second TTL and never
delete, deliberately, so a re-run inside that minute can read back a value the _previous_
run left under what it believes is its own private key. It presents as flakiness and
reproduces reliably enough to lose an afternoon to. `migo-store` does not have this
problem because it drops and recreates a database per case; Redis cannot be flushed for
the reason above, so each run stamps its scopes with a process-unique prefix and leaves
the low twenty bits to the counter, which keeps the case number legible in a failure
message. The test for the fix is simply to run the suite three times inside one minute.

**Expiry is asserted through a helper that moves two clocks at once.** The memory
backend expires against the `now` it is handed (ADR-0009: backends never read a
clock); Redis expires against its own. So:

```rust
async fn advance(now: &mut Timestamp, millis: u64) {
    *now = now.saturating_add_millis(millis as i64);
    tokio::time::sleep(Duration::from_millis(millis)).await;
}
```

One suite, honest against both. TTLs in expiry cases are short (80 ms) and the sleep
is generous (300 ms), which costs under a second in total and leaves enough margin
that a loaded CI runner does not turn a real assertion into a flaky one.

```bash
cd server
MIGO_TEST_REDIS_URL="redis://127.0.0.1:6379/15" cargo test -p migo-cache
```

Database 15, not 0: the keyspace scoping makes it safe to share, and a high database
number is one more reason a mistyped URL lands somewhere harmless.

Two invariants belong to one backend each and so live in that backend's runner
rather than in the shared suite:

**`sweep` actually reclaims** — asserted on memory. Redis reclaims expired keys
itself and the Redis backend answers `Ok(0)`, so the contract case can only check
that a sweep never removes a live entry. On memory the sweep is the only thing
standing between a long-running simulation and unbounded growth, so its test writes
into all six namespaces, sweeps, and demands all six back.

**Every key carries a deadline** — asserted on Redis. A `SET` that lost its `PX`, or
an `HSET` whose companion `PEXPIRE` was dropped in a refactor, passes every
functional test and quietly turns the cache into a leak. So the Redis runner writes
once through each of the seven paths that create a key, opens a _second_ connection —
the point is to see what the backend left behind, not what it thinks it wrote — and
`SCAN`s for anything under the prefix whose `PTTL` is negative.

### 3.2 Testing a rate limiter

`migo-ratelimit` has no second backend, so it has no contract suite. It has the
opposite problem: the behaviour that matters most is the behaviour under conditions
nobody can arrange on demand.

**The arithmetic is tested against a real backend, not a mock.** `MemoryCache` is the
subject, because the same `BucketState::charge` runs there, inside the Redis Lua script,
and inside the local fallback. A mock would prove the limiter calls the cache; the point
is whether it gets the right answer.

**The outage is tested against a mock, because it has to be.** There is no way to ask a
working Redis to fail on demand, and the degraded path is the one nobody exercises until
the night it matters. A `BrokenCache` that answers every call with `CACHE_UNAVAILABLE`
covers three things that are otherwise untestable: that a first request still gets
through, that the tightened local buckets still _refuse_ (a limiter that opens under
stress is worse than none, because nobody expects it), and that a caller's own bug — a
cost no bucket could ever hold — is reported rather than quietly degraded.

**The full-fallback path is testable because the ceiling is a parameter.**
`LocalBuckets::with_capacity` exists so saturation can be reached with two buckets
instead of a quarter of a million. A ceiling that can only be reached by allocating
twenty megabytes is a ceiling nobody tests, and the code behind it runs when a node is
already having its worst day.

**Two tests assert things about the whole system rather than about a function.**
`the_shipped_configuration_is_usable` fails if the defaults would ever resolve a bucket
too small to pay for an operation that reaches it — a bucket that refuses forever rather
than limits, with a `retry_after` that is a lie. And
`every_rejection_series_exists_before_anything_is_rejected` fails if a counter would
spring into existence on first use, because `rate(...) > 0` on a series that does not
exist yet does not fire: the alert would have to be written after the first incident it
was meant to catch.

### 3.3 Testing authentication

`migo-auth` has no second backend either, and one property it must hold is not a value at
all — it is a _timing_. So the suite is organised around the things that are easy to get
wrong and impossible to notice in a green build.

**Indistinguishability is asserted as an equality of outcomes, not of clocks.** A test
that measures wall-clock time to prove two paths cost the same is a test that fails on a
loaded CI runner. So `an_unknown_account_and_a_wrong_passphrase_are_indistinguishable`
asserts the two paths return the same error code, and the _mechanism_ that makes the times
equal — a placeholder hash verified on the unknown-account path — is a construction-time
field, exercised by every one of those tests rather than sampled by one of them.

**Order of checks is tested, because order is the security property.** Three tests exist
only to pin an ordering that a refactor would happily change: a suspended account is told
so _after_ its passphrase verifies, never before; a replayed refresh kills the family even
when the token is also expired and also from the wrong device; and a weak passphrase is
refused _before_ anything is hashed. Each of these reads as an implementation detail and
is not one.

**Privacy is asserted on the stored row, not on the returned value.**
`a_session_records_a_network_class_and_never_a_full_address` reads the session back out
of the store and asserts both halves: that the class is `203.0.113.0/24`, and that the
string does not contain `113.77`. Asserting only the first half passes on a field that
appends the full address after the class.

**The rate-limit tests assert the arithmetic, not the presence of a limit.**
`a_wrong_passphrase_costs_the_whole_anonymous_budget` pins the derived price against the
shipped anonymous bucket, so lowering `anonymous_burst` in the config breaks the test
rather than silently turning failure-pricing into a no-op. And
`pressure_is_per_network_and_not_per_account` fails if anyone ever adds a per-account
failure counter, which would let a stranger who knows a username lock its owner out.

**Two tests only check that construction fails.** A missing token key and a
32-_character_ key that decodes to 24 bytes both have to be errors at `Auth::new`, not at
first sign-in. The second exists because `decode_key_material` tries base64 before hex, so
a hex literal that _looks_ like 32 bytes is 24 — which is exactly the kind of key an
operator writes by hand.

## 4. Cross-language conformance vectors

The single most dangerous class of bug in a binary protocol is client and server
disagreeing about bytes. So the bytes are the test:

```
shared/protocol/vectors/
├── README.md             # the format, and the provenance rule below
├── wire/varint.json      # LEB128 and zigzag, incl. non-minimal and over-long inputs
├── wire/frames.json      # frame header encodings, flags, limits, malformed inputs
├── wire/mse.json         # primitive + struct encodings, incl. unknown-field skipping
├── crypto/kdf.json       # HKDF-SHA256 per label, plus RFC 5869's own vectors
├── crypto/aead.json      # XChaCha20-Poly1305 sealed envelopes and the tampering to reject
└── crypto/mac.json       # HMAC-SHA256 tokens, multi-part tags, truncation policy
```

Rust and TypeScript both run the same files. Rust encodes → compares to `hex`;
Rust decodes `hex` → compares to the expected value; TypeScript does the same. A new
optional field with no vector does not merge.

**Where the expected bytes come from matters more than the tests.** A vector whose
expected value was captured from this codebase's own output is not a conformance
test — it passes forever and detects nothing but accidental change, while looking
exactly like coverage. So every value is either hand-computed from the
specification or produced by an independent implementation, and each case records
which in a `provenance` field. The generators in `tools/vectors/` are that
independent implementation: the wire one is written from `docs/02-protocol.md` §3–4
and the crypto one from RFC 5869, RFC 2104 and draft-irtf-cfrg-xchacha, and the
crypto generator refuses to emit a file until it has reproduced those documents'
published vectors. See `shared/protocol/vectors/README.md`.

| Command             | What it does                                                                                           |
| ------------------- | ------------------------------------------------------------------------------------------------------ |
| `make vectors`      | Regenerate the files from the independent generators                                                   |
| `make vector-check` | Fail if the committed files are stale — Python only, no Rust toolchain, so it sits in the fast CI gate |
| `make test-vectors` | Run the runners in every language that has one                                                         |

The Rust runners are `server/crates/migo-wire/tests/vectors.rs` and
`server/crates/migo-crypto/tests/vectors.rs`. Both fail on a missing file, an empty
section, or a case that does not parse: a vector suite that silently runs zero cases
is the most expensive kind of green build.

## 5. Deterministic simulation testing

Networks are the hardest thing to test with real networks. So the sync engine and
connection manager are written against injectable `Clock`, `Rng` and `Transport` traits,
and the simulation harness drives them with a virtual clock and a seeded fault injector:
delay, reorder, duplicate, drop, partition, disconnect mid-frame, clock skew.

A failing seed is a **reproducible** bug report — `SIM_SEED=1234 cargo test` replays it
exactly. Flaky reconnect bugs are otherwise found by users, in production, at scale.

## 6. Load scenarios (brief §97–98)

| Scenario           | Users                             | Asserted                                                |
| ------------------ | --------------------------------- | ------------------------------------------------------- |
| Private chat storm | 1k → 100k                         | p99 delivery, bytes/user/min, memory flat               |
| Public room        | 10k → 100k in one room            | Fanout duration, one message crosses a region link once |
| Presence churn     | 50k connect/disconnect cycles     | Coalescing works, no queue growth                       |
| Reconnect storm    | 100 % disconnect, jittered return | No thundering herd, handshake p99 holds                 |
| Slow consumers     | 10 % of sessions read at 1 KB/s   | Drop policy fires, healthy sessions unaffected          |
| Game room          | 500 concurrent games              | Engine tick cost, delta size                            |
| Media              | 1k concurrent uploads             | Chat path unaffected (media never proxied through it)   |

**One room must never be able to kill the cluster.** That is the acceptance criterion,
not a nice-to-have.

## 7. Multi-region failure tests (brief §96)

Region down, network partition, 30 % packet loss, 400 ms added latency, DB failover,
Redis loss, object-storage loss. Expected outcome for each is written down in
[01-architecture.md](01-architecture.md) §10, and the test asserts _that_ outcome —
graceful degradation is a specification, not an aspiration.

## 8. Security tests (brief §95)

Per endpoint and per opcode, automatically enumerated so a new route cannot skip them:
unauthenticated access, valid token for the wrong user, valid token missing the role,
IDOR on every id parameter, replayed frame, oversized payload, malformed payload,
rate-limit bypass attempts, token from a revoked session, and upload of a mislabelled
file type.

## 9. CI gates

```
protocol-check → entity-check → brief-check → fmt → clippy -D warnings → doc-check
  → unit+property
  → contract: store (memory + Postgres) and cache (memory + Redis)
  → vectors (Rust & TS) → integration (Postgres service) → web build + e2e
  → msrv (cargo check on 1.94) → audit (reports, does not gate)
  → [nightly] fuzz + load
```

This lives in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml), and `make ci`
is the single source of truth for what green means — the same command a developer runs
locally. The workflow's own job is to supply the _services_ that turn a laptop run into a
real one, and to fail in a minute when it is going to fail at all: the brief and protocol
gates need only Node, so they never wait behind a Rust build.

The contract gate runs with both `MIGO_TEST_DATABASE_URL` and `MIGO_TEST_REDIS_URL`
set (§3). It has to: with a variable unset that backend's half of the suite passes by
doing nothing, which is the correct behaviour on a laptop and a silent hole in CI. A
new contract suite is therefore not finished when it is written — it is finished when
CI has the service and the variable.

Which is a rule, and a rule needs a mechanism. Both suites report exactly the same count
of passing tests whether they touched a real backend or skipped every case, so deleting a
service from the workflow, renaming a variable, or pointing one at a host that stopped
resolving would all look like a green build. So CI also sets
`MIGO_TEST_REQUIRE_BACKENDS=1`, and each runner ends with one test that fails when that
flag is set and its URL is not. The flag stays unset on a laptop, where a developer who
has not installed PostgreSQL should get a green suite rather than a wall of red for a
service nobody asked them for.

`doc-check` is `cargo doc` with `RUSTDOCFLAGS=-D warnings`, and it earns its place
because `cargo test` compiles doc _examples_ but never resolves doc _links_. A link to an
item that was renamed, or that was made private, is a hole in the published documentation
that every other gate reports as green. Turning it on for the first time found six, which
is the usual result and the reason it is a gate rather than a habit.

Two jobs deliberately do not gate the same way as the rest. The **MSRV** job runs only
`cargo check` on Rust 1.94, because `rust-version = "1.94"` is a promise to anyone who
depends on these crates and a promise nothing verifies is a comment. 1.94 is not a number
this project picked — it is what sea-orm 2.0 declares — so the floor is set by a dependency
and can move without anyone here editing a file, and that is the concrete thing the job
catches: a `cargo update` pulling a sea-orm patch with a higher `rust-version` compiles
fine on stable and fails only here. The **audit** job reports and does not gate:
an advisory published this morning against a transitive dependency is news, not a reason to
stop every unrelated pull request in the repository.

Nothing merges red. A quarantined flaky test gets an owner and a deadline, or it is
deleted — a permanently flaky suite trains everyone to ignore red, which is worse than
having no tests.
