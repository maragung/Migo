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
        vec![
            Box::new(Initial),
            Box::new(CaptchaChallenges),
            Box::new(Recovery),
            Box::new(IdentityLogin),
            Box::new(GlobalAdmins),
            Box::new(ProfileGender),
            Box::new(RoomNetworkBan),
            Box::new(ConversationGroups),
            Box::new(PublicRoomCapacity),
        ]
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

/// `0002_captcha_challenges` -- the captcha challenge table for the public
/// bootstrap surface. See `server/migrations/0002_captcha_challenges.sql` for the
/// shape, and `crates/migo-captcha` for the service that reads and writes it.
struct CaptchaChallenges;

impl MigrationName for CaptchaChallenges {
    fn name(&self) -> &str {
        "0002_captcha_challenges"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CaptchaChallenges {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(include_str!(
                "../../../migrations/0002_captcha_challenges.sql"
            ))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Same posture as Initial: refusing to drop a table that the rest of the
        // server is still writing to is not a missing feature.
        Err(DbErr::Migration(
            "0002_captcha_challenges cannot be rolled back: create a new database instead"
                .to_owned(),
        ))
    }
}

/// `0003_password_recovery` -- the password-recovery token table behind
/// `/v1/auth/recovery/*`. See `server/migrations/0003_password_recovery.sql` for
/// the shape, and the recovery store in this crate for the read and write paths.
struct Recovery;

impl MigrationName for Recovery {
    fn name(&self) -> &str {
        "0003_password_recovery"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Recovery {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(include_str!(
                "../../../migrations/0003_password_recovery.sql"
            ))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Same posture as Initial: refusing to drop a table that the rest of the
        // server is still writing to is not a missing feature.
        Err(DbErr::Migration(
            "0003_password_recovery cannot be rolled back: create a new database instead"
                .to_owned(),
        ))
    }
}

/// `0004_identity_login` -- the ML-DSA account identity, single-use login
/// challenges, the EVM wallet registry, and the device status/credential
/// columns. See `server/migrations/0004_identity_login.sql` for the shape and
/// the naming note beside the pre-existing E2EE `identity_key` table.
struct IdentityLogin;

impl MigrationName for IdentityLogin {
    fn name(&self) -> &str {
        "0004_identity_login"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for IdentityLogin {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(include_str!("../../../migrations/0004_identity_login.sql"))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Same posture as every migration before it.
        Err(DbErr::Migration(
            "0004_identity_login cannot be rolled back: create a new database instead".to_owned(),
        ))
    }
}

/// `0005_global_admins` -- the registry of global admins for public rooms,
/// appointed by the Owner/CEO. See `server/migrations/0005_global_admins.sql`
/// for the shape, and `crates/migo-auth` for the service that gates every
/// write behind the owner check.
struct GlobalAdmins;

impl MigrationName for GlobalAdmins {
    fn name(&self) -> &str {
        "0005_global_admins"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for GlobalAdmins {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(include_str!("../../../migrations/0005_global_admins.sql"))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Same posture as every migration before it.
        Err(DbErr::Migration(
            "0005_global_admins cannot be rolled back: create a new database instead".to_owned(),
        ))
    }
}

/// `0006_profile_gender` -- the gender the account disclosed at registration,
/// on the profile row next to `birth_year`. See
/// `server/migrations/0006_profile_gender.sql` for the shape and the numbering.
struct ProfileGender;

impl MigrationName for ProfileGender {
    fn name(&self) -> &str {
        "0006_profile_gender"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ProfileGender {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(include_str!("../../../migrations/0006_profile_gender.sql"))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Same posture as every migration before it.
        Err(DbErr::Migration(
            "0006_profile_gender cannot be rolled back: create a new database instead".to_owned(),
        ))
    }
}

/// `0007_room_network_ban` -- the one-row-per-account network ban a global
/// admin's third kick escalates to: banned from every chatroom at once. See
/// `server/migrations/0007_room_network_ban.sql` for the shape, and
/// `crates/migo-rooms` for the join-time check and the store paths it calls.
struct RoomNetworkBan;

impl MigrationName for RoomNetworkBan {
    fn name(&self) -> &str {
        "0007_room_network_ban"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for RoomNetworkBan {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(include_str!(
                "../../../migrations/0007_room_network_ban.sql"
            ))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Same posture as every migration before it.
        Err(DbErr::Migration(
            "0007_room_network_ban cannot be rolled back: create a new database instead".to_owned(),
        ))
    }
}

/// `0008_conversation_groups` -- the group title on the conversation row, and
/// the `conversation_member.role` renumber that gives the column its meaning:
/// Member 1, Founder 2, with the pre-group rows moved from the default 0 to
/// Member. See `server/migrations/0008_conversation_groups.sql`.
struct ConversationGroups;

impl MigrationName for ConversationGroups {
    fn name(&self) -> &str {
        "0008_conversation_groups"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ConversationGroups {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(include_str!(
                "../../../migrations/0008_conversation_groups.sql"
            ))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Same posture as every migration before it.
        Err(DbErr::Migration(
            "0008_conversation_groups cannot be rolled back: create a new database instead"
                .to_owned(),
        ))
    }
}

/// `0009_public_room_capacity` -- the one-statement repair that seats every
/// public room at the fixed 33. The rule itself lives in the rooms service
/// (`capacity_for` in `crates/migo-rooms`); this migration catches the rooms
/// created before the rule changed. See
/// `server/migrations/0009_public_room_capacity.sql`.
struct PublicRoomCapacity;

impl MigrationName for PublicRoomCapacity {
    fn name(&self) -> &str {
        "0009_public_room_capacity"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for PublicRoomCapacity {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(include_str!(
                "../../../migrations/0009_public_room_capacity.sql"
            ))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Refused on purpose, and for a reason specific to this one: rolling back
        // would have to pick the old number, and the old numbers were per-room
        // choices the migration does not keep. There is no honest down.
        Err(DbErr::Migration(
            "0009_public_room_capacity cannot be rolled back: the prior capacities are not recorded"
                .to_owned(),
        ))
    }
}
