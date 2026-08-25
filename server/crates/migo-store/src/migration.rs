//! The migration set, and the one thing SeaORM's migrator does not do for us.
//!
//! # Why the SQL stays SQL
//!
//! `sea-orm-migration` is usually driven by `SchemaManager` builder calls. This
//! crate does not use them, and the reason is not inertia: `server/migrations/*.sql`
//! is read by three parties that all need it in that form.
//!
//! * `tools/entity-codegen` parses it to generate [`crate::entity`]. A schema
//!   expressed as Rust builder calls would have to be executed against a live
//!   database before anything could be generated from it, which is exactly the
//!   dependency that keeps a codegen gate out of a fast CI job.
//! * A reviewer reads it. `create index ... where deleted_at is null` and
//!   `partition by range (created_at)` are one line each in SQL and a paragraph
//!   each in a builder API, and the paragraph is where a mistake hides.
//! * `docs/04-data-model.md` cites it by line.
//!
//! So each migration here is a thin wrapper that hands one file to the server, and
//! the file is `include_str!`-ed rather than read at runtime: a deployment cannot
//! ship a `migod` without the schema it expects, and touching the SQL rebuilds the
//! binary that carries it.
//!
//! # What is lost, and what is gained
//!
//! `sqlx`'s migrator recorded a checksum of every applied file and refused to run
//! again if one had changed. `sea-orm-migration` records only the name and the time
//! (`seaql_migrations` has no checksum column), so editing an applied migration is
//! no longer caught by the tool. It is still forbidden — see the header of
//! `0001_initial.sql` and `docs/04-data-model.md` §5 — but it is forbidden by review
//! now, which is weaker, and saying so is better than discovering it.
//!
//! Against that, `sqlx` took a session-level advisory lock around the *set* and
//! committed each file separately. This crate takes a transaction-scoped advisory
//! lock and runs the whole set in one transaction (see `Store::migrate`), so two
//! replicas starting at once still serialise, the lock cannot outlive a crashed
//! connection, and a set that fails halfway leaves no half-migrated schema behind.
//! That part is strictly better than what it replaces.
//!
//! # One-off note about databases created before this change
//!
//! The two migrators keep their bookkeeping in different tables: `sqlx` used
//! `_sqlx_migrations`, SeaORM uses `seaql_migrations`. A database that was migrated
//! by the old one therefore looks *empty* to the new one, which will try to create
//! every table again and fail on the first `create table`. There is no upgrade path
//! written here because there is nothing to upgrade — nothing has been deployed —
//! and a conversion routine kept for a case that never existed is a routine nobody
//! ever tests. Drop the development database and migrate again.

use sea_orm::{ConnectionTrait, DbErr};
use sea_orm_migration::{MigrationName, MigrationTrait, MigratorTrait, SchemaManager};

/// The advisory lock every replica takes before migrating.
///
/// An arbitrary constant, and it only has to be stable and unlikely: two `migod`
/// replicas starting together must pick the same number, and nothing else sharing
/// this database should pick it by accident. Transaction-scoped
/// (`pg_advisory_xact_lock`), so a replica that dies mid-migration releases it when
/// its connection drops rather than blocking every other replica until an operator
/// notices.
pub(crate) const MIGRATION_LOCK_KEY: i64 = 0x4d49_474f_5f44_4231; // "MIGO_DB1"

/// The ordered migration set.
pub(crate) struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Initial)]
    }
}

/// `0001_initial` — the schema Migo starts from.
struct Initial;

impl MigrationName for Initial {
    fn name(&self) -> &str {
        // The file's stem, so the bookkeeping table reads like the directory listing
        // and `select version from seaql_migrations` answers "which files ran".
        "0001_initial"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Initial {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(include_str!("../../../migrations/0001_initial.sql"))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Not "unimplemented": refused. Rolling back the initial migration means
        // dropping every table in the database, and a command that does that is a
        // command somebody will eventually run against production believing it does
        // something smaller. Tests that want an empty database create a new one.
        Err(DbErr::Migration(
            "0001_initial cannot be rolled back: create a new database instead".to_owned(),
        ))
    }
}
