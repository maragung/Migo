# ADR-0012 — SeaORM, with entities generated from the migrations

- **Status:** Accepted · **Date:** 2026-08-20 · **Brief refs:** §181, §46, §174

## Context

The PostgreSQL backend began as hand-written `sqlx` queries: 3,576 lines, ~80 statements,
every `select` spelling out its own column list. Two of those lists had been factored into
`const MESSAGE_COLUMNS: &[&str]` and `const ACCOUNT_COLUMNS: &[&str]` precisely because
they were repeated — which is the tell. A migration that adds a column has to be followed
into every query that reads the table, and nothing in the toolchain notices when it isn't.
The failure mode is not a crash: it is a column that is written and never read, or a
`row.get("mime_type")` against a schema that calls it `mime`, discovered by whichever
query nobody exercised.

`sqlx::query!` would catch exactly that at compile time, and was rejected for the reason
in ADR-0004: it needs a live database or a checked-in offline cache _at build time_, so
`cargo build` fails on a laptop with no Postgres and the cache becomes a file that must be
regenerated in the same commit as every query change.

## Options

| Option                                         | Pros                                                                                                                | Cons                                                                                            |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| Keep hand-written `sqlx`                       | No new dependency; full control of every statement                                                                  | Column lists drift from the schema with no gate; ~350 lines of decode boilerplate               |
| `sqlx::query!` macros                          | Compile-time column checking, no ORM                                                                                | Database or offline cache required to build; rejected in ADR-0004                               |
| Diesel                                         | Mature, strong typing                                                                                               | Schema in a Rust DSL — a _second_ source of truth beside the SQL migrations; async is a bolt-on |
| SeaORM, entities generated from the migrations | Column checking off a file already in the repo; one direction of truth; query builder covers the awkward statements | New dependency; MSRV floor set by it; its migrator is weaker than sqlx's                        |

## Decision

SeaORM 2.0 for all PostgreSQL access, with entities generated from `server/migrations/*.sql`
by `tools/entity-codegen` — one module per table, 29 tables, regenerated with `make entities`
and kept honest by `make entity-check` in CI.

The direction of generation is the whole point and is one-way: **SQL migrations are the source
of truth, entities are derived.** Diesel was rejected for inverting this. Nothing declares a
table twice.

Boundaries, in order of how load-bearing they are:

1. **The traits in `migo-store::traits` remain the only API.** They speak domain models, not
   entities. `entity` is `pub(crate)`, so an entity cannot reach a public signature — which is
   what makes "replaceable ORM" a property of the code rather than an aspiration.
2. **No column list is written by hand.** Rows materialise through entity models, or through
   `into_tuple` for a projection that is not a row. `MESSAGE_COLUMNS` and `ACCOUNT_COLUMNS`
   are deleted.
3. **The entity API where it says what the statement means; `sea_query` where it doesn't.**
   A three-state patch that must leave untouched fields alone, an `on conflict do update`
   reading `excluded`, `greatest(stored, proposed)` inside an upsert, `for update` on one
   column of one row — all still one statement, because the read-then-write alternative needs
   a lock to be correct and still loses a concurrent change to a field the caller never named.
4. **Three statements stay SQL text**, each saying why in place: the message expiry sweep
   (`delete ... using` a CTE), the balance rollup (one CTE referenced three times), and the
   migration advisory lock. Their results are read _by column name_, never by position.

## Consequences

**What got better.** A renamed column is now a compile error across the whole backend at once.
The decode boilerplate — thirteen `fn account_row(&PgRow) -> Result<Account>` functions, each a
hand-written list of `row.try_get("...")` — is replaced by thirteen infallible `From<Model>`
impls, because the generated model has already decoded the row. Several loops that inserted one
row per iteration collapsed into a single `insert_many`, which also retired three
`unnest($n::uuid[])` tricks that existed only to batch from Rust. Net: the file is longer in
lines (4,181 vs 3,576) and shorter in things that can silently be wrong.

**The privacy invariant is now structural.** Brief §46 and §174 forbid push credentials from
reaching a log. Under `select *` that was a rule reviewers had to remember. Now the device read
path uses a `DerivePartialModel` that does not contain `push_token` or `push_provider` at all,
and `register_device` writes without `returning` — so the columns are not in the generated SQL,
and a credential that never enters a struct cannot be logged by accident from it.

**The migrator is weaker, and this is the real cost.** `sqlx`'s migrator hashed every applied
file and refused to run if one had changed. SeaORM's records only that a _name_ ran. So an
edit to an applied migration means "already applied" on old databases and something else on new
ones, and **no gate can catch it** — `make brief-check` cannot read intent. The rule is
therefore written at the head of every migration file and in §181, and it is a human rule:
fix a mistake with the next migration, never by rewriting history.

`sqlx`'s migrator also took a session advisory lock around the migration set, so two `migod`
processes starting together could not apply the same file twice. SeaORM's does not, so
`migo-store::migration` takes `pg_advisory_lock` explicitly before migrating and releases it
after. Without it, a two-replica deploy is a race whose loser leaves a half-applied schema.

The bookkeeping tables differ (`_sqlx_migrations` → `seaql_migrations`), so a database migrated
by the old code would re-run 0001 and fail. Nothing is deployed yet, so no data migration is
needed — but this is recorded because it would otherwise be discovered by someone with data.

**MSRV is no longer ours to choose.** sea-orm 2.0 declares `rust-version = 1.94`, so the
workspace floor moved from 1.85 to 1.94. It can move again on a `cargo update` without any file
here changing, which is exactly what the separate MSRV job in CI exists to catch.

**One dependency, reached through.** SeaORM re-exports `sqlx`, and constraint-name mapping needs
the SQLSTATE and constraint of a Postgres error, so `sea_orm::sqlx::error::DatabaseError` is used
directly. `migo-store` has no `sqlx` dependency of its own; there is exactly one `sqlx` version in
the tree, chosen by SeaORM.
