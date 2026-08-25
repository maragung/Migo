//! The PostgreSQL backend (ADR-0004), on SeaORM (ADR-0012).
//!
//! Every statement here is written against `server/migrations/0001_initial.sql`.
//! The behaviour it has to reproduce is not defined by this file: it is defined
//! by the trait documentation in [`crate::traits`] and pinned by the tests in
//! `src/memory/tests.rs`. Two backends behind one contract are only
//! interchangeable if one set of statements is true of both, so when the two
//! disagree it is this file that is wrong.
//!
//! # Where the ORM is used, and where it stops
//!
//! Every table has a generated entity in `crate::entity` — private, deliberately,
//! because the traits are the API and an entity in a public signature would pin the
//! ORM in place — derived from the migrations by `tools/entity-codegen`. That is the
//! part worth having: a renamed column is a compile error across this whole file at
//! once, instead of a runtime "column does not exist" in whichever query nobody
//! happened to exercise.
//!
//! So no column list is written by hand anywhere below. Rows are materialised by the
//! entity models, or by `into_tuple` for a projection that is not a row; the `select
//! *` of the previous implementation, and the two hand-maintained `&[&str]` column
//! lists it needed, are gone. That is where column drift actually bites, and it is
//! now closed by construction rather than by review.
//!
//! Writes use the entity API where the entity API says what the statement means, and
//! `sea_query` where it does not. A `case when` patch that has to leave untouched
//! fields alone, an `on conflict ... do update` that reads `excluded`, a `for update`
//! on one column of one row, `greatest(stored, proposed)` inside an upsert: those all
//! stay one statement, because splitting them into read-then-write would need a lock
//! to be correct and would still lose a concurrent change to a field the caller never
//! mentioned.
//!
//! Exactly three statements are still written as SQL text, and each says why in
//! place: the expiry sweep (`delete ... using` a CTE), the balance rollup (one CTE
//! referenced three times), and the migration advisory lock. A builder spelling of
//! those would be longer, no safer, and harder to check against the plan. Their
//! results are still read by name, never by column position, because a projection
//! that gains a column would otherwise start decoding the wrong one in silence.
//!
//! The compile-time-checked macro alternative (`sqlx::query!`) is not used, for the
//! reason it never was: it needs a reachable database or a checked-in offline cache
//! at *build* time, which turns `cargo build` into something that fails on a laptop
//! with no Postgres and makes the cache a file that must be regenerated in the same
//! commit as every query change. The entities buy the same column checking off a
//! file that is already in the repository.
//!
//! # Conversions
//!
//! Three shapes cross the boundary and each has exactly one definition here:
//!
//! * [`Id`] is a UUIDv7 in both worlds, so it converts by bytes with no parsing.
//! * [`Timestamp`] counts milliseconds from the *Migo* epoch (2024-01-01) while
//!   `timestamptz` is absolute, so the conversion goes through Unix milliseconds.
//!   Anything else would silently shift every stored instant by 54 years.
//! * Enumerations are `smallint` matching the protocol's numbering, decoded
//!   fail-closed: an unknown status reads as suspended, an unknown visibility as
//!   nobody. A row written by a newer deployment must not widen access when an
//!   older one reads it.
//!
//! # Time
//!
//! Callers inject every timestamp, for the reasons in ADR-0009. The single
//! exception is `consumed_at` on a one-time prekey, which the trait gives no
//! `at` to carry: it is written with the database clock and is an audit column
//! that nothing compares against injected time.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use migo_core::config::StoreConfig;
use migo_core::{Error, Id, Result, Secret, Timestamp};
use migo_protocol::{
    fault, ConversationKind, EncryptionMode, MessageKind, Platform, RelationshipKind, RoomKind,
    RoomRole,
};
use sea_orm::sea_query::{
    Alias, Expr, ExprTrait, Func, IntoCondition, LikeExpr, LockBehavior, LockType, NullOrdering,
    OnConflict, Order, Query,
};
use sea_orm::sqlx::error::DatabaseError;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, ConnectOptions, ConnectionTrait, DatabaseConnection,
    DatabaseTransaction, DbBackend, DbErr, DerivePartialModel, EntityTrait, JoinType,
    PaginatorTrait, QueryFilter, QueryOrder, QueryResult, QuerySelect, QueryTrait, RelationTrait,
    RuntimeErr, Select, SqlxPostgresConnector, Statement, TransactionTrait, Value,
};
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::entity;
use crate::migration::{Migrator, MIGRATION_LOCK_KEY};
use crate::model::{
    game_status, notification_kind, Account, AccountStatus, AdvanceGame, Appended, AuditEntry,
    BadgeAward, Bot, Conversation, ConversationMember, ConversationPosition, ConversationSummary,
    Currency, Cursor, Device, Entitlement, GameSession, GiftSent, KeyBundle, LedgerAccount,
    LedgerAccountKind, LedgerLeg, LedgerTransaction, MediaObject, NewAccount, NewBot, NewDevice,
    NewGame, NewMessage, NewOutboxEvent, NewPeer, NewRoom, NewSession, NewTransaction, NewXpAward,
    Notification, OutboxRecord, Patch, PeerRecord, Posted, Profile, ProfilePatch, Progression,
    PublishedKeys, PushRegistration, PushTarget, Receipt, Relationship, Report, RevokeReason, Room,
    RoomMember, Scope, Session, Standing, StoredMessage, Visibility, XpChange,
};
use crate::traits::{
    canonical_country, clamp_limit, AccountStore, BotStore, DeviceStore, EconomyStore,
    FederationStore, GameStore, KeyStore, MediaStore, MessagingStore, NotifyStore,
    ProgressionStore, RoomKindFilter, RoomStore, SafetyStore, SessionStore, SocialStore, Store,
    MAX_LEDGER_LEGS,
};

/// A duplicate key. Postgres reports which index, and the caller needs to know:
/// "username taken" and "you already sent this message" are the same class of
/// database event and completely different answers to a user.
const UNIQUE_VIOLATION: &str = "23505";
/// A reference to a row that does not exist.
const FOREIGN_KEY_VIOLATION: &str = "23503";

/// The latest instant `timestamptz` and [`OffsetDateTime`] can both hold, in
/// Unix milliseconds: 9999-12-31T23:59:59.999Z.
///
/// This exact value is reserved as the encoding of [`Timestamp::MAX`], which is how
/// Migo spells "forever": a permanent ban, a hold with no expiry. [`stamp_of`]
/// clamps to it and [`instant_of`] maps it back, so the two are exact inverses at
/// the boundary. The cost is that a genuine instant in the last millisecond of the
/// year 9999 reads back as forever — a debt nobody will ever collect. Without the
/// reservation the cost is a permanent ban that quietly expires, which somebody
/// *would* collect, the first time a banned account compared its own deadline.
const MAX_PG_UNIX_MS: i64 = 253_402_300_799_999;
/// The earliest such instant: 0001-01-01T00:00:00Z. Not reserved, because
/// [`Timestamp`] has no "since forever" to reserve it for.
const MIN_PG_UNIX_MS: i64 = -62_135_596_800_000;

// --- conversions ----------------------------------------------------------

/// [`Id`] to `uuid`. Both are 16 bytes in the same order, so this is a move.
fn uuid_of(id: Id) -> Uuid {
    Uuid::from_bytes(id.into_bytes())
}

/// `uuid` to [`Id`].
fn id_of(value: Uuid) -> Id {
    Id::from_bytes(*value.as_bytes())
}

/// [`Timestamp`] to `timestamptz`, clamped to the representable range.
fn stamp_of(value: Timestamp) -> OffsetDateTime {
    let unix_ms = value.as_unix_ms().clamp(MIN_PG_UNIX_MS, MAX_PG_UNIX_MS);
    // Nanoseconds because that is the only lossless constructor; the input is
    // milliseconds, so the multiplication cannot overflow i128.
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(unix_ms) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// `timestamptz` to [`Timestamp`], the inverse of [`stamp_of`].
fn instant_of(value: OffsetDateTime) -> Timestamp {
    let unix_ms = (value.unix_timestamp_nanos() / 1_000_000) as i64;
    // The top of the range is a sentinel, not a date — see [`MAX_PG_UNIX_MS`].
    if unix_ms >= MAX_PG_UNIX_MS {
        return Timestamp::MAX;
    }
    Timestamp::from_unix_ms(unix_ms)
}

/// A [`Timestamp`] as a bindable value, for the hand-written statements.
///
/// [`Id`] needs no equivalent: a [`Uuid`] converts to a [`Value`] on its own, and
/// this one exists only because the Migo epoch means a timestamp does not.
fn stamp_value(value: Timestamp) -> Value {
    stamp_of(value).into()
}

/// A hand-written statement, bound positionally.
///
/// The only place a SQL string is built in this file. Values are always bound —
/// there is no code path here that formats a value into `text`, which is what makes
/// "the SQL is hand-written" a statement about clarity rather than about safety.
fn sql(text: &str, values: impl IntoIterator<Item = Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, text, values)
}

// --- error plumbing -------------------------------------------------------

/// Attaches a human-readable operation name to a database failure.
///
/// Every fallible statement goes through this, so a storage error in a log always
/// says which operation produced it. The message is internal-only: [`fault`]
/// keeps it out of anything a client sees.
trait DbContext<T> {
    /// Converts a SeaORM failure into a Migo storage error.
    fn context(self, what: &str) -> Result<T>;
}

impl<T> DbContext<T> for std::result::Result<T, DbErr> {
    fn context(self, what: &str) -> Result<T> {
        self.map_err(|error| fault::storage(format!("{what}: {error}")))
    }
}

/// The driver error behind a [`DbErr`], when there is one.
///
/// SeaORM offers a portable classification in [`DbErr::sql_err`], and it is not
/// enough here: it reports *that* a unique constraint was violated and carries the
/// driver's sentence, not the name of the index. Which index it was is the whole
/// question, so this reaches through to the Postgres error — using the `sqlx`
/// SeaORM itself re-exports, rather than a second direct dependency whose version
/// could drift away from the one actually doing the work.
fn driver_error(error: &DbErr) -> Option<&(dyn DatabaseError + 'static)> {
    let runtime = match error {
        DbErr::Conn(runtime) | DbErr::Exec(runtime) | DbErr::Query(runtime) => runtime,
        _ => return None,
    };
    let RuntimeErr::SqlxError(sqlx_error) = runtime else {
        return None;
    };
    match sqlx_error.as_ref() {
        sea_orm::sqlx::Error::Database(db) => Some(db.as_ref()),
        _ => None,
    }
}

/// The Postgres SQLSTATE of a failure, when it has one.
fn sqlstate(error: &DbErr) -> Option<String> {
    driver_error(error).and_then(|db| db.code().map(|code| code.to_string()))
}

/// The index or constraint a failure names, when it names one.
fn constraint(error: &DbErr) -> Option<String> {
    driver_error(error).and_then(|db| db.constraint().map(str::to_string))
}

/// Whether a failure is a duplicate key.
fn is_unique_violation(error: &DbErr) -> bool {
    sqlstate(error).as_deref() == Some(UNIQUE_VIOLATION)
}

/// Whether a failure is a dangling reference.
fn is_foreign_key_violation(error: &DbErr) -> bool {
    sqlstate(error).as_deref() == Some(FOREIGN_KEY_VIOLATION)
}

/// Maps a write failure through a table's own uniqueness rules.
///
/// The closure receives the constraint name so each caller can turn "duplicate
/// key on `account_email_lower_key`" into "email", which is the word the user
/// needs. Anything the closure does not recognise stays a storage error rather
/// than becoming a wrong-but-friendly message.
fn on_conflict(error: DbErr, what: &str, name: impl Fn(&str) -> Option<Error>) -> Error {
    if is_unique_violation(&error) {
        if let Some(mapped) = constraint(&error).as_deref().and_then(&name) {
            return mapped;
        }
        return fault::already_exists(what);
    }
    if is_foreign_key_violation(&error) {
        if let Some(mapped) = constraint(&error).as_deref().and_then(&name) {
            return mapped;
        }
    }
    fault::storage(format!("{what}: {error}"))
}

// --- reading a computed column --------------------------------------------

/// Reads one column of a computed projection, naming it if the decode fails.
///
/// Table columns are read by the generated entities and scalar projections by
/// `into_tuple`, so this is only for the one query whose shape neither covers: the
/// snapshot-plus-entries sum in [`EconomyStore::balance`], which is a CTE referenced
/// three times and has no entity to derive the decode from. The column is named
/// rather than positional because a projection that gains a column should not
/// silently start decoding the wrong one.
fn field<T: sea_orm::TryGetable>(row: &QueryResult, column: &str) -> Result<T> {
    row.try_get_by::<T, &str>(column)
        .map_err(|error| fault::storage(format!("column {column}: {error}")))
}

// --- small shared rules ---------------------------------------------------

/// Case-insensitive index key, matching [`crate::memory`] and the `lower(...)`
/// indexes in the schema. One definition, so the write path and the read path
/// cannot disagree about what "the same username" means.
fn fold(value: &str) -> String {
    value.trim().to_lowercase()
}

/// Escapes the `like` metacharacters in a user-supplied needle.
///
/// Without this, searching for `100%` matches every account and searching for
/// `a_b` matches `axb`. A search box is user input, and user input that reaches a
/// pattern language unescaped is the same class of bug as SQL injection even when
/// the query itself is parameterised.
fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// The value a patch would store: `Some` for [`Patch::Set`], `None` for
/// [`Patch::Clear`] and for [`Patch::Keep`].
///
/// Keep and Clear are told apart by binding [`Patch::is_keep`] alongside this, so
/// all three states fit one `case when $keep then column else $value end` and a
/// patch stays one statement.
fn patch_value<T>(patch: &Patch<T>) -> Option<&T> {
    match patch {
        Patch::Set(value) => Some(value),
        Patch::Keep | Patch::Clear => None,
    }
}

/// A protocol enum discriminant as `smallint`.
///
/// `try_from` rather than `as`: every discriminant in use today is single-digit,
/// but a cast that silently wraps is a bug waiting for the enum to grow. Out of
/// range reads as 0, which every generated `from_wire` maps to `Unknown`.
fn wire_i16(value: u32) -> i16 {
    i16::try_from(value).unwrap_or(0)
}

/// A `smallint` back to a protocol enum discriminant.
fn wire_u32(value: i16) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

// --- entity models to domain models ---------------------------------------
//
// One conversion per table, and they are infallible: the entity has already
// decoded every column, so there is no longer a decode error to propagate. What
// used to be `Result<Account>` from a bag of `try_get` calls is now `Account`,
// which is the difference between a read path with error handling in it and a read
// path without.

impl From<entity::account::Model> for Account {
    fn from(row: entity::account::Model) -> Self {
        Self {
            account_id: id_of(row.account_id),
            username: row.username,
            email: row.email,
            phone: row.phone,
            password_hash: Secret::new(row.password_hash),
            status: AccountStatus::from_i16(row.status),
            country: row.country,
            locale: row.locale,
            created_at: instant_of(row.created_at),
            updated_at: instant_of(row.updated_at),
            last_login_at: row.last_login_at.map(instant_of),
            suspended_until: row.suspended_until.map(instant_of),
            deleted_at: row.deleted_at.map(instant_of),
        }
    }
}

impl From<entity::profile::Model> for Profile {
    fn from(row: entity::profile::Model) -> Self {
        Self {
            account_id: id_of(row.account_id),
            display_name: row.display_name,
            bio: row.bio,
            avatar_media_id: row.avatar_media_id.map(id_of),
            birth_year: row.birth_year,
            show_last_seen: Visibility::from_i16(row.show_last_seen),
            who_can_message: Visibility::from_i16(row.who_can_message),
            who_can_add: Visibility::from_i16(row.who_can_add),
            searchable: row.searchable,
            updated_at: instant_of(row.updated_at),
        }
    }
}

/// The `device` columns this backend reads.
///
/// `push_token` and `push_provider` are absent, and that is the point: notification
/// delivery owns them, and a credential the query never selects cannot be logged by
/// accident from a struct that never held it. It is a partial model rather than a
/// comment beside a full one, so the omission is in the SQL SeaORM generates and
/// not in a reviewer's memory.
#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "entity::device::Entity")]
struct DeviceRow {
    /// Primary key.
    device_id: Uuid,
    /// Owning account.
    account_id: Uuid,
    /// Platform discriminant, protocol numbering.
    platform: i16,
    /// Human-chosen name for the device list.
    display_name: String,
    /// Client build, for compatibility decisions.
    app_version: String,
    /// Operating system version, when the client reports one.
    os_version: Option<String>,
    /// Hardware model, when the client reports one.
    device_model: Option<String>,
    /// Registration instant.
    created_at: OffsetDateTime,
    /// Last activity, coarse.
    last_seen_at: OffsetDateTime,
    /// Set when the device was revoked.
    revoked_at: Option<OffsetDateTime>,
}

impl From<DeviceRow> for Device {
    fn from(row: DeviceRow) -> Self {
        Self {
            device_id: id_of(row.device_id),
            account_id: id_of(row.account_id),
            platform: Platform::from_wire(wire_u32(row.platform)),
            display_name: row.display_name,
            app_version: row.app_version,
            os_version: row.os_version,
            device_model: row.device_model,
            created_at: instant_of(row.created_at),
            last_seen_at: instant_of(row.last_seen_at),
            revoked_at: row.revoked_at.map(instant_of),
        }
    }
}

impl From<entity::session::Model> for Session {
    fn from(row: entity::session::Model) -> Self {
        Self {
            session_id: id_of(row.session_id),
            account_id: id_of(row.account_id),
            device_id: id_of(row.device_id),
            family_id: id_of(row.family_id),
            refresh_hash: row.refresh_hash,
            generation: row.generation,
            created_at: instant_of(row.created_at),
            authenticated_at: instant_of(row.authenticated_at),
            rotated_at: row.rotated_at.map(instant_of),
            access_expires_at: instant_of(row.access_expires_at),
            refresh_expires_at: instant_of(row.refresh_expires_at),
            revoked_at: row.revoked_at.map(instant_of),
            revoked_reason: row.revoked_reason.and_then(RevokeReason::from_i16),
            ip_class: row.ip_class,
            user_agent: row.user_agent,
        }
    }
}

impl From<entity::conversation::Model> for Conversation {
    fn from(row: entity::conversation::Model) -> Self {
        Self {
            conversation_id: id_of(row.conversation_id),
            kind: ConversationKind::from_wire(wire_u32(row.kind)),
            encryption: EncryptionMode::from_wire(wire_u32(row.encryption)),
            room_id: row.room_id.map(id_of),
            last_seq: row.last_seq,
            created_by: id_of(row.created_by),
            created_at: instant_of(row.created_at),
            last_message_at: row.last_message_at.map(instant_of),
            archived_at: row.archived_at.map(instant_of),
        }
    }
}

impl From<entity::conversation_member::Model> for ConversationMember {
    fn from(row: entity::conversation_member::Model) -> Self {
        Self {
            conversation_id: id_of(row.conversation_id),
            account_id: id_of(row.account_id),
            role: row.role,
            joined_at: instant_of(row.joined_at),
            left_at: row.left_at.map(instant_of),
            muted_until: row.muted_until.map(instant_of),
            pinned: row.pinned,
        }
    }
}

impl From<entity::message::Model> for StoredMessage {
    fn from(row: entity::message::Model) -> Self {
        Self {
            message_id: id_of(row.message_id),
            conversation_id: id_of(row.conversation_id),
            seq: row.seq,
            sender_id: id_of(row.sender_id),
            sender_device: row.sender_device.map(id_of),
            kind: MessageKind::from_wire(wire_u32(row.kind)),
            envelope: row.envelope,
            reply_to: row.reply_to.map(id_of),
            expires_at: row.expires_at.map(instant_of),
            created_at: instant_of(row.created_at),
            edited_at: row.edited_at.map(instant_of),
            deleted_at: row.deleted_at.map(instant_of),
            deleted_by: row.deleted_by.map(id_of),
        }
    }
}

impl From<entity::conversation_cursor::Model> for Cursor {
    fn from(row: entity::conversation_cursor::Model) -> Self {
        Self {
            delivered_seq: row.delivered_seq,
            read_seq: row.read_seq,
            notified_seq: row.notified_seq,
        }
    }
}

impl From<entity::room::Model> for Room {
    fn from(row: entity::room::Model) -> Self {
        Self {
            room_id: id_of(row.room_id),
            conversation_id: id_of(row.conversation_id),
            slug: row.slug,
            name: row.name,
            topic: row.topic,
            kind: RoomKind::from_wire(wire_u32(row.kind)),
            owner_id: id_of(row.owner_id),
            home_region: row.home_region,
            member_count: row.member_count,
            max_members: row.max_members,
            slow_mode_seconds: row.slow_mode_seconds,
            join_policy: row.join_policy,
            encryption: EncryptionMode::from_wire(wire_u32(row.encryption)),
            created_at: instant_of(row.created_at),
            updated_at: instant_of(row.updated_at),
            archived_at: row.archived_at.map(instant_of),
        }
    }
}

/// Permissions are `bigint` in the schema and `u64` in the model: the
/// reinterpretation is deliberate, because a permission set is a bit field and its
/// top bit is a permission, not a sign.
impl From<entity::room_member::Model> for RoomMember {
    fn from(row: entity::room_member::Model) -> Self {
        Self {
            room_id: id_of(row.room_id),
            account_id: id_of(row.account_id),
            role: RoomRole::from_wire(wire_u32(row.role)),
            permissions_grant: row.permissions_grant as u64,
            permissions_deny: row.permissions_deny as u64,
            joined_at: instant_of(row.joined_at),
            left_at: row.left_at.map(instant_of),
            muted_until: row.muted_until.map(instant_of),
            banned_until: row.banned_until.map(instant_of),
            ban_reason: row.ban_reason,
            invited_by: row.invited_by.map(id_of),
        }
    }
}

impl From<entity::relationship::Model> for Relationship {
    fn from(row: entity::relationship::Model) -> Self {
        Self {
            account_id: id_of(row.account_id),
            other_id: id_of(row.other_id),
            kind: RelationshipKind::from_wire(wire_u32(row.kind)),
            created_at: instant_of(row.created_at),
            accepted_at: row.accepted_at.map(instant_of),
        }
    }
}

impl From<entity::media_object::Model> for MediaObject {
    fn from(row: entity::media_object::Model) -> Self {
        Self {
            media_id: id_of(row.media_id),
            owner_id: id_of(row.owner_id),
            kind: row.kind,
            mime: row.mime,
            byte_size: row.byte_size,
            width: row.width,
            height: row.height,
            duration_ms: row.duration_ms,
            storage_key: row.storage_key,
            conversation_id: row.conversation_id.map(id_of),
            checksum: row.checksum,
            scan_status: row.scan_status,
            created_at: instant_of(row.created_at),
            deleted_at: row.deleted_at.map(instant_of),
        }
    }
}

impl From<entity::report::Model> for Report {
    fn from(row: entity::report::Model) -> Self {
        Self {
            report_id: id_of(row.report_id),
            reporter_id: id_of(row.reporter_id),
            subject_kind: row.subject_kind,
            subject_id: id_of(row.subject_id),
            room_id: row.room_id.map(id_of),
            reason: row.reason,
            note: row.note,
            evidence_ref: row.evidence_ref.map(id_of),
            status: row.status,
            created_at: instant_of(row.created_at),
            resolved_at: row.resolved_at.map(instant_of),
            resolved_by: row.resolved_by.map(id_of),
            resolution: row.resolution,
        }
    }
}

impl From<entity::audit_entry::Model> for AuditEntry {
    fn from(row: entity::audit_entry::Model) -> Self {
        Self {
            audit_id: id_of(row.audit_id),
            actor_id: row.actor_id.map(id_of),
            actor_kind: row.actor_kind,
            action: row.action,
            target_kind: row.target_kind,
            target_id: row.target_id.map(id_of),
            summary: row.summary,
            reason: row.reason,
            request_id: row.request_id,
            ip_class: row.ip_class,
            created_at: instant_of(row.created_at),
        }
    }
}

/// Maps a `ledger_account` row. The only fallible conversion of the set: an
/// unrecognised kind or currency is a hard error rather than a default, because
/// guessing which currency a balance is in is worse than refusing to answer.
fn ledger_account_of(row: entity::ledger_account::Model) -> Result<LedgerAccount> {
    Ok(LedgerAccount {
        ledger_account_id: id_of(row.ledger_account_id),
        owner_id: row.owner_id.map(id_of),
        kind: LedgerAccountKind::from_i16(row.kind)
            .ok_or_else(|| fault::storage(format!("unknown ledger account kind {}", row.kind)))?,
        currency: Currency::from_i16(row.currency)
            .ok_or_else(|| fault::storage(format!("unknown currency {}", row.currency)))?,
        created_at: instant_of(row.created_at),
    })
}

// --- the store ------------------------------------------------------------

/// The PostgreSQL-backed store.
///
/// Cloning is cheap and shares the pool: [`DatabaseConnection`] is a pool handle
/// behind an `Arc`, so handing a clone to each domain crate costs nothing.
#[derive(Clone, Debug)]
pub struct PostgresStore {
    db: DatabaseConnection,
}

impl PostgresStore {
    /// Builds a store from configuration, opening the pool lazily.
    ///
    /// Lazily on purpose: `migod` must be able to start while the database is
    /// still coming up, and report unhealthy until it is, rather than crash-loop
    /// in a container orchestrator. [`Store::health`] is what tells the truth
    /// about reachability.
    ///
    /// The statement timeout is set as a connection option rather than through a
    /// post-connect hook, so it applies to the very first statement on every
    /// connection including the ones the pool opens to replace a dead one.
    pub async fn connect(config: &StoreConfig) -> Result<Self> {
        let url = config.url.as_ref().ok_or_else(|| {
            fault::validation("store.url", "is required when store.backend is postgres")
        })?;

        // Two things about this check. It is a string comparison rather than a URL
        // parse because `DbBackend::is_prefix_of` panics on a URL it cannot parse, and
        // a malformed value in a config file must produce an error, not a crashed
        // process. And it is here at all because the Postgres driver's own URL parser
        // does not look at the scheme: `mysql://host/db` would be accepted and would
        // quietly try to speak Postgres to a MySQL port.
        let raw = url.expose();
        if !(raw.starts_with("postgres://") || raw.starts_with("postgresql://")) {
            return Err(fault::validation(
                "store.url",
                "must begin with postgres:// or postgresql://",
            ));
        }

        let mut options = ConnectOptions::new(raw.to_owned());
        options
            .max_connections(config.max_connections)
            .acquire_timeout(Duration::from_millis(config.acquire_timeout_ms))
            .statement_timeout(Duration::from_millis(config.statement_timeout_ms))
            // The server instruments its own queries. A second copy of every
            // statement, emitted by the driver at its own level and outside our spans,
            // is noise that also has to be audited for what it might print.
            .sqlx_logging(false);
        options.connect_lazy(true);

        // `SqlxPostgresConnector::connect` rather than `Database::connect`, which is
        // the generic entry point that dispatches to it. The generic one formats the
        // connection string into two of its own error messages, and this connection
        // string carries a password: a URL parse failure would put the credential in
        // whatever log caught the startup error. The message below deliberately
        // reports the driver's complaint and not the URL.
        let db = SqlxPostgresConnector::connect(options)
            .await
            .map_err(|error| {
                fault::validation(
                    "store.url",
                    &format!("is not a valid PostgreSQL URL: {error}"),
                )
            })?;

        Ok(Self { db })
    }

    /// Wraps an existing connection. For tests and for a process that already owns
    /// one.
    #[must_use]
    pub fn from_connection(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// The underlying connection, for maintenance commands that need statements this
    /// trait deliberately does not expose (partition creation, snapshotting).
    #[must_use]
    pub fn connection(&self) -> &DatabaseConnection {
        &self.db
    }

    /// Starts a transaction.
    async fn begin(&self, what: &str) -> Result<DatabaseTransaction> {
        self.db.begin().await.context(what)
    }
}

#[async_trait]
impl AccountStore for PostgresStore {
    async fn create_account(&self, new: NewAccount) -> Result<Account> {
        // `username_lower` and `email_lower` are written here rather than
        // generated by the database, so `fold` is the single definition of
        // case-insensitive identity across both backends. The unique indexes are
        // on those columns, so a race between two signups is decided by Postgres
        // and not by a check-then-insert that has a window between the two.
        let row = entity::account::Entity::insert(entity::account::ActiveModel {
            account_id: Set(uuid_of(new.account_id)),
            username_lower: Set(fold(&new.username)),
            username: Set(new.username),
            email_lower: Set(new.email.as_deref().map(fold)),
            email: Set(new.email),
            phone: Set(new.phone),
            password_hash: Set(new.password_hash.expose().to_owned()),
            status: Set(AccountStatus::Active.to_i16()),
            country: Set(canonical_country(new.country.as_deref())?),
            locale: Set(new.locale),
            created_at: Set(stamp_of(new.created_at)),
            updated_at: Set(stamp_of(new.created_at)),
            ..Default::default()
        })
        .exec_with_returning(&self.db)
        .await
        .map_err(|error| {
            on_conflict(error, "account", |name| match name {
                "account_username_lower_key" => Some(fault::already_exists("username")),
                "account_email_lower_key" => Some(fault::already_exists("email")),
                "account_phone_key" => Some(fault::already_exists("phone")),
                "account_pkey" => Some(fault::already_exists("account id")),
                _ => None,
            })
        })?;
        Ok(row.into())
    }

    async fn account_by_id(&self, account_id: Id) -> Result<Option<Account>> {
        Ok(entity::account::Entity::find_by_id(uuid_of(account_id))
            .one(&self.db)
            .await
            .context("account_by_id")?
            .map(Into::into))
    }

    async fn account_by_username(&self, username: &str) -> Result<Option<Account>> {
        Ok(entity::account::Entity::find()
            .filter(entity::account::Column::UsernameLower.eq(fold(username)))
            .one(&self.db)
            .await
            .context("account_by_username")?
            .map(Into::into))
    }

    async fn account_by_email(&self, email: &str) -> Result<Option<Account>> {
        Ok(entity::account::Entity::find()
            .filter(entity::account::Column::EmailLower.eq(fold(email)))
            .one(&self.db)
            .await
            .context("account_by_email")?
            .map(Into::into))
    }

    async fn set_password_hash(&self, account_id: Id, hash: &str, at: Timestamp) -> Result<()> {
        let result = entity::account::Entity::update_many()
            .filter(entity::account::Column::AccountId.eq(uuid_of(account_id)))
            .set(entity::account::ActiveModel {
                password_hash: Set(hash.to_owned()),
                updated_at: Set(stamp_of(at)),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("set_password_hash")?;
        if result.rows_affected == 0 {
            return Err(fault::not_found("account"));
        }
        Ok(())
    }

    async fn record_login(&self, account_id: Id, at: Timestamp) -> Result<()> {
        let result = entity::account::Entity::update_many()
            .filter(entity::account::Column::AccountId.eq(uuid_of(account_id)))
            .set(entity::account::ActiveModel {
                last_login_at: Set(Some(stamp_of(at))),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("record_login")?;
        if result.rows_affected == 0 {
            return Err(fault::not_found("account"));
        }
        Ok(())
    }

    async fn set_status(
        &self,
        account_id: Id,
        status: AccountStatus,
        until: Option<Timestamp>,
        at: Timestamp,
    ) -> Result<()> {
        // `deleted_at` is a `case` over its own old value: the first deletion
        // stamps the time and later status writes leave it alone, so the retention
        // clock cannot be reset by a second call. `col_expr` rather than `set`,
        // because the new value is a function of the row and an `ActiveModel` can
        // only carry a constant.
        let mut update = entity::account::Entity::update_many()
            .filter(entity::account::Column::AccountId.eq(uuid_of(account_id)))
            .set(entity::account::ActiveModel {
                status: Set(status.to_i16()),
                suspended_until: Set(until.map(stamp_of)),
                updated_at: Set(stamp_of(at)),
                ..Default::default()
            });
        if status == AccountStatus::Deleted {
            update = update.col_expr(
                entity::account::Column::DeletedAt,
                entity::account::Column::DeletedAt.if_null(stamp_of(at)),
            );
        }
        let result = update.exec(&self.db).await.context("set_status")?;
        if result.rows_affected == 0 {
            return Err(fault::not_found("account"));
        }
        Ok(())
    }

    async fn profile(&self, account_id: Id) -> Result<Option<Profile>> {
        Ok(entity::profile::Entity::find_by_id(uuid_of(account_id))
            .one(&self.db)
            .await
            .context("profile")?
            .map(Into::into))
    }

    async fn create_profile(&self, profile: Profile) -> Result<Profile> {
        let row = entity::profile::Entity::insert(entity::profile::ActiveModel {
            account_id: Set(uuid_of(profile.account_id)),
            display_name: Set(profile.display_name),
            bio: Set(profile.bio),
            avatar_media_id: Set(profile.avatar_media_id.map(uuid_of)),
            birth_year: Set(profile.birth_year),
            show_last_seen: Set(profile.show_last_seen.to_i16()),
            who_can_message: Set(profile.who_can_message.to_i16()),
            who_can_add: Set(profile.who_can_add.to_i16()),
            searchable: Set(profile.searchable),
            updated_at: Set(stamp_of(profile.updated_at)),
        })
        .exec_with_returning(&self.db)
        .await
        .map_err(|error| {
            on_conflict(error, "profile", |name| match name {
                "profile_pkey" => Some(fault::already_exists("profile")),
                "profile_account_id_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?;
        Ok(row.into())
    }

    async fn update_profile(
        &self,
        account_id: Id,
        patch: ProfilePatch,
        at: Timestamp,
    ) -> Result<Profile> {
        // A patch has three states per field and only two of them are a value, so
        // Keep has to be expressed as "write the column back to itself". `set` can
        // carry the Set/Clear cases and `col_expr` carries Keep, which keeps the
        // whole patch in one statement and one round trip. Read-modify-write would
        // need a row lock to be correct and would still lose a concurrent change to
        // a field this patch does not touch.
        let mut update = entity::profile::Entity::update_many()
            .filter(entity::profile::Column::AccountId.eq(uuid_of(account_id)))
            .set(entity::profile::ActiveModel {
                updated_at: Set(stamp_of(at)),
                ..Default::default()
            });

        if let Some(display_name) = patch.display_name {
            update = update.col_expr(
                entity::profile::Column::DisplayName,
                Expr::val(display_name),
            );
        }
        if !patch.bio.is_keep() {
            update = update.col_expr(
                entity::profile::Column::Bio,
                Expr::val(patch_value(&patch.bio).cloned()),
            );
        }
        if !patch.avatar_media_id.is_keep() {
            update = update.col_expr(
                entity::profile::Column::AvatarMediaId,
                Expr::val(patch_value(&patch.avatar_media_id).copied().map(uuid_of)),
            );
        }
        if !patch.birth_year.is_keep() {
            update = update.col_expr(
                entity::profile::Column::BirthYear,
                Expr::val(patch_value(&patch.birth_year).copied()),
            );
        }
        if let Some(value) = patch.show_last_seen {
            update = update.col_expr(
                entity::profile::Column::ShowLastSeen,
                Expr::val(value.to_i16()),
            );
        }
        if let Some(value) = patch.who_can_message {
            update = update.col_expr(
                entity::profile::Column::WhoCanMessage,
                Expr::val(value.to_i16()),
            );
        }
        if let Some(value) = patch.who_can_add {
            update = update.col_expr(
                entity::profile::Column::WhoCanAdd,
                Expr::val(value.to_i16()),
            );
        }
        if let Some(value) = patch.searchable {
            update = update.col_expr(entity::profile::Column::Searchable, Expr::val(value));
        }

        update
            .exec_with_returning(&self.db)
            .await
            .context("update_profile")?
            .into_iter()
            .next()
            .map(Into::into)
            .ok_or_else(|| fault::not_found("profile"))
    }

    async fn search_accounts(&self, query: &str, limit: u16) -> Result<Vec<(Account, Profile)>> {
        let needle = escape_like(&fold(query));
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        // Privacy first: `searchable` and `status = active` are in the where
        // clause, not applied after ranking, so a non-searchable account is never
        // fetched at all. Relevance is deliberately crude — see the trait doc.
        //
        // `like` with an escaped needle rather than `similar to` or a regex: the
        // needle is user input and `%`/`_` in it must match themselves. SeaORM's
        // `starts_with`/`contains` shorthands are not used for exactly that reason —
        // they wrap the pattern without escaping it.
        //
        // The join is written from `profile`'s foreign key, reversed, so the pair of
        // columns it joins on is the one the schema declares rather than one spelled
        // out again here.
        let prefix = LikeExpr::new(format!("{needle}%")).escape('\\');
        let anywhere = LikeExpr::new(format!("%{needle}%")).escape('\\');
        let rows = entity::account::Entity::find()
            .join(
                JoinType::InnerJoin,
                entity::profile::Relation::Account.def().rev(),
            )
            .select_two_required(entity::profile::Entity)
            .filter(entity::profile::Column::Searchable.eq(true))
            .filter(entity::account::Column::Status.eq(AccountStatus::Active.to_i16()))
            .filter(
                Condition::any()
                    .add(entity::account::Column::UsernameLower.like(prefix))
                    .add(
                        Expr::expr(Func::lower(Expr::col(
                            entity::profile::Column::DisplayName.as_column_ref(),
                        )))
                        .like(anywhere),
                    ),
            )
            .order_by(
                Expr::expr(Func::char_length(Expr::col(
                    entity::account::Column::Username.as_column_ref(),
                ))),
                Order::Asc,
            )
            .order_by_asc(entity::account::Column::Username)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("search_accounts")?;

        Ok(rows
            .into_iter()
            .map(|(account, profile)| (account.into(), profile.into()))
            .collect())
    }
}

#[async_trait]
impl DeviceStore for PostgresStore {
    async fn register_device(&self, new: NewDevice) -> Result<Device> {
        // Built first and returned at the end rather than read back with
        // `returning`: a `returning` clause on this table would hand back
        // `push_token` and `push_provider`, and the whole point of [`DeviceRow`] is
        // that those two never enter this process. The insert also has nothing to
        // learn from the database — every column it does not write is null, so the
        // row it would read back is the row it just built.
        let row = DeviceRow {
            device_id: uuid_of(new.device_id),
            account_id: uuid_of(new.account_id),
            platform: wire_i16(new.platform.to_wire()),
            display_name: new.display_name,
            app_version: new.app_version,
            os_version: new.os_version,
            device_model: new.device_model,
            created_at: stamp_of(new.created_at),
            last_seen_at: stamp_of(new.created_at),
            revoked_at: None,
        };
        entity::device::Entity::insert(entity::device::ActiveModel {
            device_id: Set(row.device_id),
            account_id: Set(row.account_id),
            platform: Set(row.platform),
            display_name: Set(row.display_name.clone()),
            app_version: Set(row.app_version.clone()),
            os_version: Set(row.os_version.clone()),
            device_model: Set(row.device_model.clone()),
            created_at: Set(row.created_at),
            last_seen_at: Set(row.last_seen_at),
            ..Default::default()
        })
        .exec_without_returning(&self.db)
        .await
        .map_err(|error| {
            on_conflict(error, "device", |name| match name {
                "device_pkey" => Some(fault::already_exists("device")),
                "device_account_id_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?;
        Ok(row.into())
    }

    async fn device_by_id(&self, device_id: Id) -> Result<Option<Device>> {
        Ok(entity::device::Entity::find_by_id(uuid_of(device_id))
            .into_partial_model::<DeviceRow>()
            .one(&self.db)
            .await
            .context("device_by_id")?
            .map(Into::into))
    }

    async fn devices_for_account(&self, account_id: Id) -> Result<Vec<Device>> {
        Ok(entity::device::Entity::find()
            .filter(entity::device::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::device::Column::RevokedAt.is_null())
            .order_by_asc(entity::device::Column::CreatedAt)
            .order_by_asc(entity::device::Column::DeviceId)
            .into_partial_model::<DeviceRow>()
            .all(&self.db)
            .await
            .context("devices_for_account")?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn touch_device(&self, device_id: Id, at: Timestamp) -> Result<()> {
        // `greatest` rather than a plain assignment: a device whose clock ran
        // backwards must not drag its own last-seen backwards with it. A missing
        // device is not an error — presence writes must not fail a request.
        entity::device::Entity::update_many()
            .filter(entity::device::Column::DeviceId.eq(uuid_of(device_id)))
            .col_expr(
                entity::device::Column::LastSeenAt,
                Expr::expr(Func::greatest([
                    Expr::col(entity::device::Column::LastSeenAt.as_column_ref()),
                    Expr::val(stamp_of(at)),
                ])),
            )
            .exec(&self.db)
            .await
            .context("touch_device")?;
        Ok(())
    }

    async fn revoke_device(&self, device_id: Id, at: Timestamp) -> Result<()> {
        // `is null` in the predicate keeps the first revocation time: when it was
        // revoked is the fact support needs, and a second call must not rewrite it.
        entity::device::Entity::update_many()
            .filter(entity::device::Column::DeviceId.eq(uuid_of(device_id)))
            .filter(entity::device::Column::RevokedAt.is_null())
            .set(entity::device::ActiveModel {
                revoked_at: Set(Some(stamp_of(at))),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("revoke_device")?;
        Ok(())
    }
}

/// The `session` row a [`NewSession`] inserts.
///
/// Shared by `create_session` and the second half of `rotate_session`, which insert
/// the same shape into the same table — once against the pool and once inside a
/// transaction. One builder, so the two cannot drift apart.
fn session_insert(new: NewSession) -> entity::session::ActiveModel {
    entity::session::ActiveModel {
        session_id: Set(uuid_of(new.session_id)),
        account_id: Set(uuid_of(new.account_id)),
        device_id: Set(uuid_of(new.device_id)),
        family_id: Set(uuid_of(new.family_id)),
        refresh_hash: Set(new.refresh_hash),
        generation: Set(new.generation),
        created_at: Set(stamp_of(new.created_at)),
        authenticated_at: Set(stamp_of(new.authenticated_at)),
        access_expires_at: Set(stamp_of(new.access_expires_at)),
        refresh_expires_at: Set(stamp_of(new.refresh_expires_at)),
        ip_class: Set(new.ip_class),
        user_agent: Set(new.user_agent),
        ..Default::default()
    }
}

/// Turns an insert failure on `session` into the error the caller needs.
fn session_conflict(error: DbErr) -> Error {
    on_conflict(error, "session", |name| match name {
        "session_refresh_hash_key" => Some(fault::already_exists("refresh token")),
        "session_pkey" => Some(fault::already_exists("session")),
        "session_account_id_fkey" => Some(fault::not_found("account")),
        "session_device_id_fkey" => Some(fault::not_found("device")),
        _ => None,
    })
}

/// The columns and the value that revoke a session, for the three revoke paths.
fn session_revocation(reason: RevokeReason, at: Timestamp) -> entity::session::ActiveModel {
    entity::session::ActiveModel {
        revoked_at: Set(Some(stamp_of(at))),
        revoked_reason: Set(Some(reason.to_i16())),
        ..Default::default()
    }
}

#[async_trait]
impl SessionStore for PostgresStore {
    async fn create_session(&self, new: NewSession) -> Result<Session> {
        let row = entity::session::Entity::insert(session_insert(new))
            .exec_with_returning(&self.db)
            .await
            .map_err(session_conflict)?;
        Ok(row.into())
    }

    async fn session_by_id(&self, session_id: Id) -> Result<Option<Session>> {
        Ok(entity::session::Entity::find_by_id(uuid_of(session_id))
            .one(&self.db)
            .await
            .context("session_by_id")?
            .map(Into::into))
    }

    async fn session_by_refresh_hash(&self, hash: &[u8]) -> Result<Option<Session>> {
        // Looks up revoked and rotated sessions too. That is the point: the caller
        // has to be able to tell "this token is unknown" from "this token was
        // already exchanged", because the second one means the family was stolen.
        Ok(entity::session::Entity::find()
            .filter(entity::session::Column::RefreshHash.eq(hash.to_vec()))
            .one(&self.db)
            .await
            .context("session_by_refresh_hash")?
            .map(Into::into))
    }

    async fn rotate_session(&self, previous: Id, next: NewSession) -> Result<Session> {
        let transaction = self.begin("rotate_session").await?;

        // `for update` on the predecessor is what makes rotation single-use under
        // concurrency: two requests arriving with the same refresh token serialise
        // here, and the second one finds `rotated_at` already set. Whole entity
        // rather than a projection, because every field below is checked before the
        // rotation is allowed.
        let old: Session = entity::session::Entity::find_by_id(uuid_of(previous))
            .lock(LockType::Update)
            .one(&transaction)
            .await
            .context("rotate_session: lock predecessor")?
            .ok_or_else(|| fault::not_found("session"))?
            .into();

        if old.revoked_at.is_some() {
            return Err(fault::unauthenticated("session revoked"));
        }
        if old.rotated_at.is_some() {
            // Reuse of a token that was already exchanged. The caller kills the
            // family; the store refuses the rotation so a stolen token cannot
            // become a second live generation in the meantime.
            return Err(fault::conflict("session already rotated"));
        }
        if old.family_id != next.family_id {
            return Err(fault::validation(
                "family_id",
                "must match the previous session",
            ));
        }
        if next.generation != old.generation + 1 {
            return Err(fault::validation("generation", "must be the successor"));
        }

        let rotated_at = next.created_at;
        let row = entity::session::Entity::insert(session_insert(next))
            .exec_with_returning(&transaction)
            .await
            .map_err(session_conflict)?;

        entity::session::Entity::update_many()
            .filter(entity::session::Column::SessionId.eq(uuid_of(previous)))
            .set(entity::session::ActiveModel {
                rotated_at: Set(Some(stamp_of(rotated_at))),
                ..Default::default()
            })
            .exec(&transaction)
            .await
            .context("rotate_session: retire predecessor")?;

        transaction
            .commit()
            .await
            .context("rotate_session: commit")?;
        Ok(row.into())
    }

    async fn revoke_session(
        &self,
        session_id: Id,
        reason: RevokeReason,
        at: Timestamp,
    ) -> Result<()> {
        // `revoked_at is null` keeps the first revocation and its reason. Revoking
        // an already-revoked session is a no-op, not an error: it happens whenever
        // a logout races a rotation.
        entity::session::Entity::update_many()
            .filter(entity::session::Column::SessionId.eq(uuid_of(session_id)))
            .filter(entity::session::Column::RevokedAt.is_null())
            .set(session_revocation(reason, at))
            .exec(&self.db)
            .await
            .context("revoke_session")?;
        Ok(())
    }

    async fn revoke_family(
        &self,
        family_id: Id,
        reason: RevokeReason,
        at: Timestamp,
    ) -> Result<u64> {
        let result = entity::session::Entity::update_many()
            .filter(entity::session::Column::FamilyId.eq(uuid_of(family_id)))
            .filter(entity::session::Column::RevokedAt.is_null())
            .set(session_revocation(reason, at))
            .exec(&self.db)
            .await
            .context("revoke_family")?;
        Ok(result.rows_affected)
    }

    async fn revoke_account_sessions(
        &self,
        account_id: Id,
        except: Option<Id>,
        reason: RevokeReason,
        at: Timestamp,
    ) -> Result<u64> {
        // One statement rather than two: "log out my other devices" must not have a
        // window in which the current device is logged out too.
        let mut update = entity::session::Entity::update_many()
            .filter(entity::session::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::session::Column::RevokedAt.is_null())
            .set(session_revocation(reason, at));
        if let Some(keep) = except {
            update = update.filter(entity::session::Column::SessionId.ne(uuid_of(keep)));
        }
        let result = update
            .exec(&self.db)
            .await
            .context("revoke_account_sessions")?;
        Ok(result.rows_affected)
    }

    async fn sessions_for_account(&self, account_id: Id, now: Timestamp) -> Result<Vec<Session>> {
        Ok(entity::session::Entity::find()
            .filter(entity::session::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::session::Column::RevokedAt.is_null())
            .filter(entity::session::Column::RefreshExpiresAt.gt(stamp_of(now)))
            .order_by_asc(entity::session::Column::CreatedAt)
            .order_by_asc(entity::session::Column::SessionId)
            .all(&self.db)
            .await
            .context("sessions_for_account")?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn purge_expired_sessions(&self, before: Timestamp) -> Result<u64> {
        // A real delete, not a tombstone: an expired refresh token is already
        // useless, and the unique index on `refresh_hash` is the one thing that
        // must not keep growing. Nothing converges on a session's absence.
        let result = entity::session::Entity::delete_many()
            .filter(entity::session::Column::RefreshExpiresAt.lt(stamp_of(before)))
            .exec(&self.db)
            .await
            .context("purge_expired_sessions")?;
        Ok(result.rows_affected)
    }
}

#[async_trait]
impl KeyStore for PostgresStore {
    async fn publish_keys(&self, keys: PublishedKeys) -> Result<()> {
        if keys.signed_prekey_expires_at <= keys.created_at {
            // A prekey that has already expired on arrival can only produce
            // sessions that fail later, for a reason nobody will connect back to
            // this moment.
            return Err(fault::validation(
                "signed_prekey_expires_at",
                "must be in the future",
            ));
        }

        let transaction = self.begin("publish_keys").await?;
        let account = uuid_of(keys.account_id);
        let device = uuid_of(keys.device_id);
        let created = stamp_of(keys.created_at);

        // Publishing is a replace, matching the in-memory backend: the device is
        // declaring its current key material, so anything it does not mention is
        // gone. `revoked_at = null` on conflict lets a re-provisioned device come
        // back rather than staying dead forever — and it is written as a literal
        // rather than through `update_columns`, because `excluded.revoked_at`
        // would carry whatever the insert happened to bind and the intent here is
        // specifically "clear it".
        entity::identity_key::Entity::insert(entity::identity_key::ActiveModel {
            account_id: Set(account),
            device_id: Set(device),
            public_key: Set(keys.identity_key),
            created_at: Set(created),
            revoked_at: Set(None),
        })
        .on_conflict(
            OnConflict::columns([
                entity::identity_key::Column::AccountId,
                entity::identity_key::Column::DeviceId,
            ])
            .update_columns([
                entity::identity_key::Column::PublicKey,
                entity::identity_key::Column::CreatedAt,
            ])
            .value(
                entity::identity_key::Column::RevokedAt,
                Expr::val(None::<OffsetDateTime>),
            )
            .to_owned(),
        )
        .exec_without_returning(&transaction)
        .await
        .map_err(|error| {
            on_conflict(error, "identity_key", |name| match name {
                "identity_key_device_id_fkey" => Some(fault::not_found("device")),
                "identity_key_account_id_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?;

        entity::signed_prekey::Entity::delete_many()
            .filter(entity::signed_prekey::Column::AccountId.eq(account))
            .filter(entity::signed_prekey::Column::DeviceId.eq(device))
            .exec(&transaction)
            .await
            .context("publish_keys: clear signed prekeys")?;

        entity::signed_prekey::Entity::insert(entity::signed_prekey::ActiveModel {
            account_id: Set(account),
            device_id: Set(device),
            key_id: Set(keys.signed_prekey_id),
            public_key: Set(keys.signed_prekey),
            signature: Set(keys.signed_prekey_signature),
            created_at: Set(created),
            expires_at: Set(stamp_of(keys.signed_prekey_expires_at)),
        })
        .exec_without_returning(&transaction)
        .await
        .context("publish_keys: insert signed prekey")?;

        entity::one_time_prekey::Entity::delete_many()
            .filter(entity::one_time_prekey::Column::AccountId.eq(account))
            .filter(entity::one_time_prekey::Column::DeviceId.eq(device))
            .exec(&transaction)
            .await
            .context("publish_keys: clear one-time prekeys")?;

        // One statement for the whole batch. A device publishes a hundred prekeys
        // at a time, and a hundred round trips inside a transaction is a hundred
        // chances for the connection to be the slow part.
        entity::one_time_prekey::Entity::insert_many(keys.one_time_prekeys.into_iter().map(
            |(key_id, public_key)| entity::one_time_prekey::ActiveModel {
                account_id: Set(account),
                device_id: Set(device),
                key_id: Set(key_id),
                public_key: Set(public_key),
                created_at: Set(created),
                consumed_at: Set(None),
            },
        ))
        .on_conflict(prekey_conflict())
        .exec_without_returning(&transaction)
        .await
        .context("publish_keys: insert one-time prekeys")?;

        transaction.commit().await.context("publish_keys: commit")
    }

    async fn add_one_time_prekeys(
        &self,
        account_id: Id,
        device_id: Id,
        prekeys: Vec<(i32, Vec<u8>)>,
        at: Timestamp,
    ) -> Result<u64> {
        let transaction = self.begin("add_one_time_prekeys").await?;
        let account = uuid_of(account_id);
        let device = uuid_of(device_id);
        let stamp = stamp_of(at);

        // Counted rather than fetched: the identity key's bytes are of no interest
        // here, only whether the row exists. Prekeys without an identity key are
        // unusable, because there is nothing to verify their signature against.
        let published = entity::identity_key::Entity::find_by_id((account, device))
            .count(&transaction)
            .await
            .context("add_one_time_prekeys: check device")?;
        if published == 0 {
            return Err(fault::not_found("published keys"));
        }

        let added = entity::one_time_prekey::Entity::insert_many(prekeys.into_iter().map(
            |(key_id, public_key)| entity::one_time_prekey::ActiveModel {
                account_id: Set(account),
                device_id: Set(device),
                key_id: Set(key_id),
                public_key: Set(public_key),
                created_at: Set(stamp),
                consumed_at: Set(None),
            },
        ))
        .on_conflict(prekey_conflict())
        .exec_without_returning(&transaction)
        .await
        .context("add_one_time_prekeys: insert")?;

        transaction
            .commit()
            .await
            .context("add_one_time_prekeys: commit")?;
        Ok(added)
    }

    async fn take_key_bundle(&self, account_id: Id, device_id: Id) -> Result<Option<KeyBundle>> {
        let transaction = self.begin("take_key_bundle").await?;
        let Some(bundle) = take_bundle_in(&transaction, account_id, device_id).await? else {
            return Ok(None);
        };
        transaction
            .commit()
            .await
            .context("take_key_bundle: commit")?;
        Ok(Some(bundle))
    }

    async fn take_key_bundles_for_account(&self, account_id: Id) -> Result<Vec<KeyBundle>> {
        // One transaction for the whole fanout: a caller starting conversations
        // with every device of an account either gets a consistent set of bundles
        // or none, rather than a partial set with some prekeys already consumed.
        let transaction = self.begin("take_key_bundles_for_account").await?;
        // `select_only` and one column, not the device rows: this needs identifiers
        // and a full row would carry `push_token` along with them.
        let device_ids: Vec<Uuid> = entity::device::Entity::find()
            .select_only()
            .column(entity::device::Column::DeviceId)
            .filter(entity::device::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::device::Column::RevokedAt.is_null())
            .order_by_asc(entity::device::Column::DeviceId)
            .into_tuple()
            .all(&transaction)
            .await
            .context("take_key_bundles_for_account: list devices")?;

        let mut bundles = Vec::with_capacity(device_ids.len());
        for device_id in device_ids {
            if let Some(bundle) = take_bundle_in(&transaction, account_id, id_of(device_id)).await?
            {
                bundles.push(bundle);
            }
        }
        transaction
            .commit()
            .await
            .context("take_key_bundles_for_account: commit")?;
        Ok(bundles)
    }

    async fn one_time_prekey_count(&self, account_id: Id, device_id: Id) -> Result<u32> {
        let count = entity::one_time_prekey::Entity::find()
            .filter(entity::one_time_prekey::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::one_time_prekey::Column::DeviceId.eq(uuid_of(device_id)))
            .filter(entity::one_time_prekey::Column::ConsumedAt.is_null())
            .count(&self.db)
            .await
            .context("one_time_prekey_count")?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    async fn revoke_device_keys(&self, account_id: Id, device_id: Id, at: Timestamp) -> Result<()> {
        let transaction = self.begin("revoke_device_keys").await?;
        let account = uuid_of(account_id);
        let device = uuid_of(device_id);

        entity::identity_key::Entity::update_many()
            .filter(entity::identity_key::Column::AccountId.eq(account))
            .filter(entity::identity_key::Column::DeviceId.eq(device))
            .set(entity::identity_key::ActiveModel {
                revoked_at: Set(Some(stamp_of(at))),
                ..Default::default()
            })
            .exec(&transaction)
            .await
            .context("revoke_device_keys: revoke identity")?;

        // Unconsumed prekeys go immediately: handing out key material for a device
        // that no longer exists can only produce sessions nobody can read. The
        // consumed ones stay, because they are the record of what was handed out.
        entity::one_time_prekey::Entity::delete_many()
            .filter(entity::one_time_prekey::Column::AccountId.eq(account))
            .filter(entity::one_time_prekey::Column::DeviceId.eq(device))
            .filter(entity::one_time_prekey::Column::ConsumedAt.is_null())
            .exec(&transaction)
            .await
            .context("revoke_device_keys: drop unconsumed prekeys")?;

        transaction
            .commit()
            .await
            .context("revoke_device_keys: commit")
    }
}

/// How a one-time prekey collision is resolved: it is not.
///
/// `do nothing` rather than `do update`, because republishing an id must not
/// replace the key behind it — two peers holding different bytes for the same
/// prekey id would each derive a session the other cannot read. The row count
/// then reflects exactly what landed, which is what
/// [`KeyStore::add_one_time_prekeys`] returns.
///
/// The target names the primary key instead of being left empty. A bare
/// `on conflict do nothing` would also swallow a violation of some *other*
/// constraint added later, and silently inserting nothing is the worst way to
/// find out about one.
fn prekey_conflict() -> OnConflict {
    OnConflict::columns([
        entity::one_time_prekey::Column::AccountId,
        entity::one_time_prekey::Column::DeviceId,
        entity::one_time_prekey::Column::KeyId,
    ])
    .do_nothing()
    .to_owned()
}

/// Reads a bundle and consumes one one-time prekey, inside a caller's transaction.
///
/// Shared by [`KeyStore::take_key_bundle`] and
/// [`KeyStore::take_key_bundles_for_account`] so the consumption rule has one
/// implementation. `for update skip locked` is the whole trick: two peers asking
/// for a bundle at the same moment take different prekeys instead of blocking on
/// each other or both taking the same one.
///
/// Generic over the connection rather than taking a transaction, so the same code
/// serves both callers; every caller does in fact pass a transaction, because
/// consuming a prekey and reporting it are two statements that must not be split.
///
/// `consumed_at` is written with the database clock. It is the one place this
/// backend does that, because the trait passes no timestamp — and it can, because
/// nothing compares that column against injected time; it exists so an operator
/// can see that a key was handed out.
async fn take_bundle_in<C: ConnectionTrait>(
    connection: &C,
    account_id: Id,
    device_id: Id,
) -> Result<Option<KeyBundle>> {
    let account = uuid_of(account_id);
    let device = uuid_of(device_id);

    // The identity key and the signed prekey are read in one statement on purpose.
    // Two statements would let a concurrent `publish_keys` land in between and hand
    // back an identity key that does not match the signature on the prekey, which
    // the receiving device would reject with no way to tell why.
    //
    // The join comes from the schema's own composite foreign key, reversed:
    // `signed_prekey (account_id, device_id) → identity_key`, so there is no second
    // place where the join condition could drift from the constraint.
    let Some((identity, prekey)) = entity::identity_key::Entity::find()
        .join(
            JoinType::InnerJoin,
            entity::signed_prekey::Relation::IdentityKey.def().rev(),
        )
        .select_two_required(entity::signed_prekey::Entity)
        .filter(entity::identity_key::Column::AccountId.eq(account))
        .filter(entity::identity_key::Column::DeviceId.eq(device))
        .filter(entity::identity_key::Column::RevokedAt.is_null())
        .order_by_desc(entity::signed_prekey::Column::CreatedAt)
        .order_by_desc(entity::signed_prekey::Column::KeyId)
        .one(connection)
        .await
        .context("take_key_bundle: read keys")?
    else {
        return Ok(None);
    };

    let claimed = entity::one_time_prekey::Entity::find()
        .filter(entity::one_time_prekey::Column::AccountId.eq(account))
        .filter(entity::one_time_prekey::Column::DeviceId.eq(device))
        .filter(entity::one_time_prekey::Column::ConsumedAt.is_null())
        .order_by_asc(entity::one_time_prekey::Column::KeyId)
        .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
        .one(connection)
        .await
        .context("take_key_bundle: claim one-time prekey")?;

    let one_time_prekey = match claimed {
        Some(prekey) => {
            entity::one_time_prekey::Entity::update_many()
                .filter(entity::one_time_prekey::Column::AccountId.eq(account))
                .filter(entity::one_time_prekey::Column::DeviceId.eq(device))
                .filter(entity::one_time_prekey::Column::KeyId.eq(prekey.key_id))
                // `current_timestamp` rather than a bound value, and on PostgreSQL
                // it is the transaction's start time — the same instant the old
                // `now()` returned, for the same reason: the row is a record for a
                // human, not an input to any comparison.
                .col_expr(
                    entity::one_time_prekey::Column::ConsumedAt,
                    Expr::current_timestamp(),
                )
                .exec(connection)
                .await
                .context("take_key_bundle: consume one-time prekey")?;
            Some((prekey.key_id, prekey.public_key))
        }
        // No prekeys left is not a failure: the bundle goes out with the signed
        // prekey alone and the caller tells the owner to publish more.
        None => None,
    };

    Ok(Some(KeyBundle {
        account_id,
        device_id,
        identity_key: identity.public_key,
        signed_prekey_id: prekey.key_id,
        signed_prekey: prekey.public_key,
        signed_prekey_signature: prekey.signature,
        signed_prekey_expires_at: instant_of(prekey.expires_at),
        one_time_prekey,
    }))
}

/// The keyset predicate for the conversation list: rows that sort strictly after
/// `position`.
///
/// The list is ordered by activity descending with nulls last, then creation
/// descending, then id ascending, so "after" unfolds into one clause per
/// tie-break level. `ConversationPosition::precedes` is the same rule in Rust,
/// and the two are checked against each other by the contract tests running over
/// both backends.
///
/// A note on the null handling, because it is the part that looks wrong and is
/// not: a conversation with no messages sorts *after* every conversation that has
/// them, so when the position still has activity the nulls are all ahead of us
/// and belong in the page, and when the position is itself a null the page can
/// contain nothing else.
fn keyset_after(position: ConversationPosition) -> Condition {
    let created = stamp_of(position.created_at);
    let id = uuid_of(position.conversation_id);
    let tie = Condition::any()
        .add(entity::conversation::Column::CreatedAt.lt(created))
        .add(
            Condition::all()
                .add(entity::conversation::Column::CreatedAt.eq(created))
                .add(entity::conversation::Column::ConversationId.gt(id)),
        );
    match position.last_message_at {
        Some(last) => {
            let last = stamp_of(last);
            Condition::any()
                .add(entity::conversation::Column::LastMessageAt.lt(last))
                .add(entity::conversation::Column::LastMessageAt.is_null())
                .add(
                    Condition::all()
                        .add(entity::conversation::Column::LastMessageAt.eq(last))
                        .add(tie),
                )
        }
        None => Condition::all()
            .add(entity::conversation::Column::LastMessageAt.is_null())
            .add(tie),
    }
}

#[async_trait]
impl MessagingStore for PostgresStore {
    async fn create_conversation(
        &self,
        conversation: Conversation,
        members: Vec<Id>,
    ) -> Result<Conversation> {
        let transaction = self.begin("create_conversation").await?;

        let stored: Conversation =
            entity::conversation::Entity::insert(entity::conversation::ActiveModel {
                conversation_id: Set(uuid_of(conversation.conversation_id)),
                kind: Set(wire_i16(conversation.kind.to_wire())),
                encryption: Set(wire_i16(conversation.encryption.to_wire())),
                room_id: Set(conversation.room_id.map(uuid_of)),
                last_seq: Set(conversation.last_seq),
                created_by: Set(uuid_of(conversation.created_by)),
                created_at: Set(stamp_of(conversation.created_at)),
                last_message_at: Set(conversation.last_message_at.map(stamp_of)),
                archived_at: Set(conversation.archived_at.map(stamp_of)),
            })
            .exec_with_returning(&transaction)
            .await
            .map_err(|error| {
                on_conflict(error, "conversation", |name| match name {
                    "conversation_pkey" => Some(fault::already_exists("conversation")),
                    "conversation_created_by_fkey" => Some(fault::not_found("account")),
                    _ => None,
                })
            })?
            .into();

        insert_members(
            &transaction,
            stored.conversation_id,
            &members,
            stored.created_at,
        )
        .await?;

        transaction
            .commit()
            .await
            .context("create_conversation: commit")?;
        Ok(stored)
    }

    async fn direct_conversation(
        &self,
        a: Id,
        b: Id,
        conversation_id: Id,
        encryption: EncryptionMode,
        at: Timestamp,
    ) -> Result<Conversation> {
        if a == b {
            return Err(fault::validation(
                "peer",
                "a direct conversation needs two accounts",
            ));
        }
        let (low, high) = if a < b { (a, b) } else { (b, a) };

        let transaction = self.begin("direct_conversation").await?;

        // The conversation goes in first because `direct_conversation` references
        // it, and the reference is checked at statement end rather than at commit:
        // the index row cannot be written before the row it points at exists.
        let stored: Conversation =
            entity::conversation::Entity::insert(entity::conversation::ActiveModel {
                conversation_id: Set(uuid_of(conversation_id)),
                kind: Set(wire_i16(ConversationKind::Direct.to_wire())),
                encryption: Set(wire_i16(encryption.to_wire())),
                created_by: Set(uuid_of(a)),
                created_at: Set(stamp_of(at)),
                ..Default::default()
            })
            .exec_with_returning(&transaction)
            .await
            .map_err(|error| {
                on_conflict(error, "conversation", |name| match name {
                    "conversation_pkey" => Some(fault::already_exists("conversation")),
                    "conversation_created_by_fkey" => Some(fault::not_found("account")),
                    _ => None,
                })
            })?
            .into();

        // The pair is the unique key, so the race is decided by the index rather
        // than by a check-then-insert. `do nothing` makes the loser's insert affect
        // no rows, which is the signal to go and read the winner's.
        let claimed =
            entity::direct_conversation::Entity::insert(entity::direct_conversation::ActiveModel {
                low_account_id: Set(uuid_of(low)),
                high_account_id: Set(uuid_of(high)),
                conversation_id: Set(uuid_of(conversation_id)),
            })
            .on_conflict(
                OnConflict::columns([
                    entity::direct_conversation::Column::LowAccountId,
                    entity::direct_conversation::Column::HighAccountId,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(&transaction)
            .await
            .map_err(|error| {
                on_conflict(error, "direct_conversation", |name| match name {
                    "direct_conversation_low_account_id_fkey"
                    | "direct_conversation_high_account_id_fkey" => {
                        Some(fault::not_found("account"))
                    }
                    _ => None,
                })
            })?;

        if claimed == 0 {
            // The loser of the race reads the winner's row. Two devices tapping
            // "message Bob" at the same instant must not produce two threads.
            //
            // The rollback is what disposes of the conversation written above: the
            // loser's row never existed as far as anybody outside this transaction
            // is concerned, so there is nothing to clean up afterwards.
            transaction
                .rollback()
                .await
                .context("direct_conversation: rollback")?;
            let (winner, _) = entity::conversation::Entity::find()
                .join(
                    JoinType::InnerJoin,
                    entity::direct_conversation::Relation::Conversation
                        .def()
                        .rev(),
                )
                .select_two_required(entity::direct_conversation::Entity)
                .filter(entity::direct_conversation::Column::LowAccountId.eq(uuid_of(low)))
                .filter(entity::direct_conversation::Column::HighAccountId.eq(uuid_of(high)))
                .one(&self.db)
                .await
                .context("direct_conversation: read existing")?
                .ok_or_else(|| fault::internal("direct index points at a missing conversation"))?;
            return Ok(winner.into());
        }

        insert_members(&transaction, conversation_id, &[a, b], at).await?;

        transaction
            .commit()
            .await
            .context("direct_conversation: commit")?;
        Ok(stored)
    }

    async fn conversation(&self, conversation_id: Id) -> Result<Option<Conversation>> {
        Ok(
            entity::conversation::Entity::find_by_id(uuid_of(conversation_id))
                .one(&self.db)
                .await
                .context("conversation")?
                .map(Into::into),
        )
    }

    async fn members(&self, conversation_id: Id) -> Result<Vec<ConversationMember>> {
        // Includes members who left. The row is what makes "was this person here
        // when that was said" answerable after the fact.
        let rows = entity::conversation_member::Entity::find()
            .filter(
                entity::conversation_member::Column::ConversationId.eq(uuid_of(conversation_id)),
            )
            .order_by_asc(entity::conversation_member::Column::JoinedAt)
            .order_by_asc(entity::conversation_member::Column::AccountId)
            .all(&self.db)
            .await
            .context("members")?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn is_member(&self, conversation_id: Id, account_id: Id) -> Result<bool> {
        // Counted rather than fetched: the answer is a boolean and the row's other
        // columns would only be thrown away.
        let present = entity::conversation_member::Entity::find_by_id((
            uuid_of(conversation_id),
            uuid_of(account_id),
        ))
        .filter(entity::conversation_member::Column::LeftAt.is_null())
        .count(&self.db)
        .await
        .context("is_member")?;
        Ok(present > 0)
    }

    async fn add_member(&self, member: ConversationMember) -> Result<()> {
        // Rejoining clears the departure but keeps the original join time, so
        // "member since" does not reset every time somebody leaves and comes back:
        // `joined_at` is deliberately absent from the update list.
        entity::conversation_member::Entity::insert(entity::conversation_member::ActiveModel {
            conversation_id: Set(uuid_of(member.conversation_id)),
            account_id: Set(uuid_of(member.account_id)),
            role: Set(member.role),
            joined_at: Set(stamp_of(member.joined_at)),
            left_at: Set(None),
            muted_until: Set(member.muted_until.map(stamp_of)),
            pinned: Set(member.pinned),
        })
        .on_conflict(
            OnConflict::columns([
                entity::conversation_member::Column::ConversationId,
                entity::conversation_member::Column::AccountId,
            ])
            .value(
                entity::conversation_member::Column::LeftAt,
                Expr::val(None::<OffsetDateTime>),
            )
            .update_columns([entity::conversation_member::Column::Role])
            .to_owned(),
        )
        .exec_without_returning(&self.db)
        .await
        .map_err(|error| {
            on_conflict(error, "conversation_member", |name| match name {
                "conversation_member_conversation_id_fkey" => {
                    Some(fault::not_found("conversation"))
                }
                "conversation_member_account_id_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?;
        Ok(())
    }

    async fn remove_member(
        &self,
        conversation_id: Id,
        account_id: Id,
        at: Timestamp,
    ) -> Result<()> {
        entity::conversation_member::Entity::update_many()
            .filter(
                entity::conversation_member::Column::ConversationId.eq(uuid_of(conversation_id)),
            )
            .filter(entity::conversation_member::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::conversation_member::Column::LeftAt.is_null())
            .set(entity::conversation_member::ActiveModel {
                left_at: Set(Some(stamp_of(at))),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("remove_member")?;
        Ok(())
    }

    async fn append_message(&self, new: NewMessage) -> Result<Appended> {
        let transaction = self.begin("append_message").await?;
        let conversation = uuid_of(new.conversation_id);

        // `for update` on the conversation row is the sequencer's lock. Everything
        // that follows is serialised per conversation, which is what makes the
        // sequence gapless: no other appender can read the same `last_seq`.
        //
        // The lock is taken *before* the dedup read, so a retry that arrives
        // concurrently with its own original waits and then sees it, instead of
        // both inserting. One column, because the lock is the point and the value
        // is read again below.
        let locked = entity::conversation::Entity::find_by_id(conversation)
            .select_only()
            .column(entity::conversation::Column::LastSeq)
            .lock(LockType::Update)
            .into_tuple::<i64>()
            .one(&transaction)
            .await
            .context("append_message: lock conversation")?;
        if locked.is_none() {
            // Appending to nothing is a caller bug, not a reason to invent a
            // conversation: a conversation nobody created has no members, so the
            // message would be addressed to no one.
            return Err(fault::not_found("conversation"));
        }

        // No `created_at` bound here, so this cannot prune partitions: it is one
        // index scan per partition on `message_dedup_key`. That is deliberate.
        // Bounding it to the current month would make a retry that straddles a
        // month boundary insert a second copy, and a duplicate message is a worse
        // outcome than a scan over a bounded number of partitions.
        let existing = entity::message::Entity::find()
            .filter(entity::message::Column::ConversationId.eq(conversation))
            .filter(entity::message::Column::MessageId.eq(uuid_of(new.message_id)))
            .one(&transaction)
            .await
            .context("append_message: dedup")?;
        if let Some(row) = existing {
            let message: StoredMessage = row.into();
            transaction
                .commit()
                .await
                .context("append_message: commit duplicate")?;
            return Ok(Appended::Duplicate(message));
        }

        // `update ... returning` rather than a sequence: the sequence has to be
        // per-conversation, gapless, and roll back with the insert. A Postgres
        // sequence is none of those things. `col_expr` supplies the increment and
        // `set` the timestamp; they must name different columns, because both end
        // up as values on the same statement.
        let assigned = entity::conversation::Entity::update_many()
            .filter(entity::conversation::Column::ConversationId.eq(conversation))
            .col_expr(
                entity::conversation::Column::LastSeq,
                Expr::col(entity::conversation::Column::LastSeq).add(1),
            )
            .set(entity::conversation::ActiveModel {
                last_message_at: Set(Some(stamp_of(new.created_at))),
                ..Default::default()
            })
            .exec_with_returning(&transaction)
            .await
            .context("append_message: assign sequence")?;
        let seq = assigned
            .first()
            .ok_or_else(|| fault::not_found("conversation"))?
            .last_seq;

        entity::message::Entity::insert(entity::message::ActiveModel {
            message_id: Set(uuid_of(new.message_id)),
            conversation_id: Set(conversation),
            seq: Set(seq),
            sender_id: Set(uuid_of(new.sender_id)),
            sender_device: Set(new.sender_device.map(uuid_of)),
            kind: Set(wire_i16(new.kind.to_wire())),
            envelope: Set(new.envelope.clone()),
            reply_to: Set(new.reply_to.map(uuid_of)),
            expires_at: Set(new.expires_at.map(stamp_of)),
            created_at: Set(stamp_of(new.created_at)),
            ..Default::default()
        })
        .exec_without_returning(&transaction)
        .await
        .context("append_message: insert")?;

        transaction
            .commit()
            .await
            .context("append_message: commit")?;
        Ok(Appended::Created(StoredMessage {
            message_id: new.message_id,
            conversation_id: new.conversation_id,
            seq,
            sender_id: new.sender_id,
            sender_device: new.sender_device,
            kind: new.kind,
            envelope: new.envelope,
            reply_to: new.reply_to,
            expires_at: new.expires_at,
            created_at: new.created_at,
            edited_at: None,
            deleted_at: None,
            deleted_by: None,
        }))
    }

    async fn message(&self, conversation_id: Id, message_id: Id) -> Result<Option<StoredMessage>> {
        Ok(entity::message::Entity::find()
            .filter(entity::message::Column::ConversationId.eq(uuid_of(conversation_id)))
            .filter(entity::message::Column::MessageId.eq(uuid_of(message_id)))
            .one(&self.db)
            .await
            .context("message")?
            .map(Into::into))
    }

    async fn history_before(
        &self,
        conversation_id: Id,
        before_seq: Option<i64>,
        limit: u16,
    ) -> Result<Vec<StoredMessage>> {
        // Newest-first, which is also the order the page is returned in: a client
        // scrolling up wants the row nearest the viewport first. Tombstones are
        // included, because a client that never sees the tombstone keeps showing
        // the message.
        //
        // The absent cursor is now an absent predicate rather than a bound null
        // compared against itself, which is both what it means and one less thing
        // for the planner to work around.
        let mut query = entity::message::Entity::find()
            .filter(entity::message::Column::ConversationId.eq(uuid_of(conversation_id)));
        if let Some(seq) = before_seq {
            query = query.filter(entity::message::Column::Seq.lt(seq));
        }
        let rows = query
            .order_by_desc(entity::message::Column::Seq)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("history_before")?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn history_after(
        &self,
        conversation_id: Id,
        after_seq: i64,
        limit: u16,
    ) -> Result<Vec<StoredMessage>> {
        let rows = entity::message::Entity::find()
            .filter(entity::message::Column::ConversationId.eq(uuid_of(conversation_id)))
            .filter(entity::message::Column::Seq.gt(after_seq))
            .order_by_asc(entity::message::Column::Seq)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("history_after")?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn edit_message(
        &self,
        conversation_id: Id,
        message_id: Id,
        envelope: Vec<u8>,
        at: Timestamp,
    ) -> Result<Option<StoredMessage>> {
        let transaction = self.begin("edit_message").await?;
        let Some(current) = entity::message::Entity::find()
            .filter(entity::message::Column::ConversationId.eq(uuid_of(conversation_id)))
            .filter(entity::message::Column::MessageId.eq(uuid_of(message_id)))
            .lock(LockType::Update)
            .one(&transaction)
            .await
            .context("edit_message: read")?
        else {
            return Ok(None);
        };
        if current.deleted_at.is_some() {
            // Editing a tombstone would resurrect content the sender asked to be
            // gone. That is a caller bug, so it gets an error rather than a
            // silently ignored write.
            return Err(fault::conflict("message is deleted"));
        }

        let updated = entity::message::Entity::update_many()
            .filter(entity::message::Column::ConversationId.eq(uuid_of(conversation_id)))
            .filter(entity::message::Column::MessageId.eq(uuid_of(message_id)))
            .set(entity::message::ActiveModel {
                envelope: Set(envelope),
                edited_at: Set(Some(stamp_of(at))),
                ..Default::default()
            })
            .exec_with_returning(&transaction)
            .await
            .context("edit_message: update")?;
        let message: StoredMessage = updated
            .into_iter()
            .next()
            .ok_or_else(|| fault::internal("edited message disappeared under its own lock"))?
            .into();

        transaction.commit().await.context("edit_message: commit")?;
        Ok(Some(message))
    }

    async fn delete_message(
        &self,
        conversation_id: Id,
        message_id: Id,
        by: Id,
        at: Timestamp,
    ) -> Result<Option<StoredMessage>> {
        // The row stays so every client converges on the deletion, but the payload
        // goes now: keeping the ciphertext of a deleted message would mean
        // "delete" only removed it from one screen. `if_null` keeps the first
        // deleter and time, so a moderator deleting after the sender does not
        // rewrite who did it.
        let rows = entity::message::Entity::update_many()
            .filter(entity::message::Column::ConversationId.eq(uuid_of(conversation_id)))
            .filter(entity::message::Column::MessageId.eq(uuid_of(message_id)))
            .col_expr(
                entity::message::Column::DeletedAt,
                entity::message::Column::DeletedAt.if_null(stamp_of(at)),
            )
            .col_expr(
                entity::message::Column::DeletedBy,
                entity::message::Column::DeletedBy.if_null(uuid_of(by)),
            )
            .col_expr(
                entity::message::Column::Envelope,
                Expr::val(Vec::<u8>::new()),
            )
            .exec_with_returning(&self.db)
            .await
            .context("delete_message")?;
        Ok(rows.into_iter().next().map(Into::into))
    }

    async fn cursor(&self, conversation_id: Id, account_id: Id) -> Result<Cursor> {
        // No row is a cursor at zero, not an error: every member starts having read
        // nothing, and writing that row on join would be a write per member per
        // conversation for no information.
        Ok(entity::conversation_cursor::Entity::find_by_id((
            uuid_of(conversation_id),
            uuid_of(account_id),
        ))
        .one(&self.db)
        .await
        .context("cursor")?
        .map(Cursor::from)
        .unwrap_or_default())
    }

    async fn advance_cursor(
        &self,
        conversation_id: Id,
        account_id: Id,
        delivered_seq: Option<i64>,
        read_seq: Option<i64>,
        notified_seq: Option<i64>,
        at: Timestamp,
    ) -> Result<Cursor> {
        let transaction = self.begin("advance_cursor").await?;
        let last_seq = entity::conversation::Entity::find_by_id(uuid_of(conversation_id))
            .select_only()
            .column(entity::conversation::Column::LastSeq)
            .into_tuple::<i64>()
            .one(&transaction)
            .await
            .context("advance_cursor: read last_seq")?
            .ok_or_else(|| fault::not_found("conversation"))?;

        // Clamped to the end of the conversation before it reaches the database. A
        // client that reports having read message 9000 in a conversation with 12
        // messages is either confused or probing; either way the stored value stays
        // sane. `None` becomes 0, which `greatest` treats as "leave it alone".
        //
        // `Ord::` spelled out because `sea_query::ExprTrait` is in scope and also has a
        // `min`/`max`, for building a SQL expression. Both apply to an `i64`, so the
        // qualification says which one is meant: this arithmetic happens here, not
        // in the database.
        let ceiling = |asked: Option<i64>| asked.map_or(0, |seq| Ord::min(seq, last_seq));
        let delivered = ceiling(delivered_seq);
        let read = ceiling(read_seq);
        let notified = ceiling(notified_seq);

        // `greatest` in the conflict clause is what makes each field forward-only,
        // and it also makes the write idempotent under retry. Delivery takes the
        // read value into account as well, because a client reporting a read
        // without the delivery that preceded it should not leave the two
        // inconsistent — on the insert path that is plain arithmetic, and on the
        // conflict path it is four terms because both the stored row and the
        // proposed one have a delivery and a read.
        let cursor: Cursor =
            entity::conversation_cursor::Entity::insert(entity::conversation_cursor::ActiveModel {
                conversation_id: Set(uuid_of(conversation_id)),
                account_id: Set(uuid_of(account_id)),
                delivered_seq: Set(Ord::max(delivered, read)),
                read_seq: Set(read),
                notified_seq: Set(notified),
                updated_at: Set(stamp_of(at)),
            })
            .on_conflict(
                OnConflict::columns([
                    entity::conversation_cursor::Column::ConversationId,
                    entity::conversation_cursor::Column::AccountId,
                ])
                .values([
                    (
                        entity::conversation_cursor::Column::DeliveredSeq,
                        Func::greatest([
                            stored_seq(entity::conversation_cursor::Column::DeliveredSeq),
                            proposed_seq(entity::conversation_cursor::Column::DeliveredSeq),
                            stored_seq(entity::conversation_cursor::Column::ReadSeq),
                            proposed_seq(entity::conversation_cursor::Column::ReadSeq),
                        ])
                        .into(),
                    ),
                    (
                        entity::conversation_cursor::Column::ReadSeq,
                        Func::greatest([
                            stored_seq(entity::conversation_cursor::Column::ReadSeq),
                            proposed_seq(entity::conversation_cursor::Column::ReadSeq),
                        ])
                        .into(),
                    ),
                    (
                        entity::conversation_cursor::Column::NotifiedSeq,
                        Func::greatest([
                            stored_seq(entity::conversation_cursor::Column::NotifiedSeq),
                            proposed_seq(entity::conversation_cursor::Column::NotifiedSeq),
                        ])
                        .into(),
                    ),
                ])
                .update_columns([entity::conversation_cursor::Column::UpdatedAt])
                .to_owned(),
            )
            .exec_with_returning(&transaction)
            .await
            .map_err(|error| {
                on_conflict(error, "conversation_cursor", |name| match name {
                    "conversation_cursor_account_id_fkey" => Some(fault::not_found("account")),
                    _ => None,
                })
            })?
            .into();

        transaction
            .commit()
            .await
            .context("advance_cursor: commit")?;
        Ok(cursor)
    }

    async fn conversation_list(
        &self,
        account_id: Id,
        limit: u16,
        member_preview: u16,
        after: Option<ConversationPosition>,
    ) -> Result<Vec<ConversationSummary>> {
        let limit = clamp_limit(limit);
        let preview = clamp_limit(member_preview);
        let account = uuid_of(account_id);

        // Four small queries rather than one with two lateral joins. The page is at
        // most `MAX_PAGE` conversations, so the follow-ups are three indexed reads
        // over a bounded id list — and each one is legible on its own, which the
        // alias soup of a single heroic statement would not be.
        //
        // `nulls last` is not decoration: `order by ... desc` in Postgres puts
        // nulls first by default, which would float every conversation that has no
        // messages yet to the top of the list.
        let conversations: Vec<Conversation> = entity::conversation::Entity::find()
            .join(
                JoinType::InnerJoin,
                entity::conversation_member::Relation::Conversation
                    .def()
                    .rev(),
            )
            .filter(entity::conversation_member::Column::AccountId.eq(account))
            .filter(entity::conversation_member::Column::LeftAt.is_null())
            .order_by_with_nulls(
                entity::conversation::Column::LastMessageAt,
                Order::Desc,
                NullOrdering::Last,
            )
            .order_by_desc(entity::conversation::Column::CreatedAt)
            .order_by_asc(entity::conversation::Column::ConversationId)
            .apply_if(after, |query, position| {
                query.filter(keyset_after(position))
            })
            .limit(limit as u64)
            .all(&self.db)
            .await
            .context("conversation_list: conversations")?
            .into_iter()
            .map(Into::into)
            .collect();
        if conversations.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = conversations
            .iter()
            .map(|conversation| uuid_of(conversation.conversation_id))
            .collect();

        // `distinct on` is the cheap way to say "one row per group, the first by
        // this order": the index on (conversation_id, seq desc) supplies it directly.
        let mut last_by_conversation = HashMap::new();
        for row in entity::message::Entity::find()
            .distinct_on([entity::message::Column::ConversationId])
            .filter(entity::message::Column::ConversationId.is_in(ids.clone()))
            .order_by_asc(entity::message::Column::ConversationId)
            .order_by_desc(entity::message::Column::Seq)
            .all(&self.db)
            .await
            .context("conversation_list: last messages")?
        {
            let message: StoredMessage = row.into();
            last_by_conversation.insert(message.conversation_id, message);
        }

        let mut cursor_by_conversation = HashMap::new();
        for row in entity::conversation_cursor::Entity::find()
            .filter(entity::conversation_cursor::Column::ConversationId.is_in(ids.clone()))
            .filter(entity::conversation_cursor::Column::AccountId.eq(account))
            .all(&self.db)
            .await
            .context("conversation_list: cursors")?
        {
            cursor_by_conversation.insert(id_of(row.conversation_id), Cursor::from(row));
        }

        // The caller's own rows, whole: mute state and pin state are rendered on
        // every row of the list, so they are read here in one bounded query
        // instead of once per conversation by whoever renders it.
        let mut member_by_conversation = HashMap::new();
        for row in entity::conversation_member::Entity::find()
            .filter(entity::conversation_member::Column::ConversationId.is_in(ids.clone()))
            .filter(entity::conversation_member::Column::AccountId.eq(account))
            .all(&self.db)
            .await
            .context("conversation_list: membership")?
        {
            let member = ConversationMember::from(row);
            member_by_conversation.insert(member.conversation_id, member);
        }

        // Two columns for the preview, not the member rows: this is a list of who
        // is in each conversation, so the roles and join times would be read and
        // dropped.
        let member_rows: Vec<(Uuid, Uuid)> = entity::conversation_member::Entity::find()
            .select_only()
            .column(entity::conversation_member::Column::ConversationId)
            .column(entity::conversation_member::Column::AccountId)
            .filter(entity::conversation_member::Column::ConversationId.is_in(ids))
            .filter(entity::conversation_member::Column::LeftAt.is_null())
            .order_by_asc(entity::conversation_member::Column::ConversationId)
            .order_by_asc(entity::conversation_member::Column::AccountId)
            .into_tuple()
            .all(&self.db)
            .await
            .context("conversation_list: members")?;
        let mut members_by_conversation: HashMap<Id, Vec<Id>> = HashMap::new();
        for (conversation_id, member_id) in member_rows {
            let entry = members_by_conversation
                .entry(id_of(conversation_id))
                .or_default();
            if entry.len() < preview {
                entry.push(id_of(member_id));
            }
        }

        Ok(conversations
            .into_iter()
            .map(|conversation| {
                let cursor = cursor_by_conversation
                    .get(&conversation.conversation_id)
                    .copied()
                    .unwrap_or_default();
                // The join above already established membership; the fallback
                // keeps the mapping infallible rather than making a list read
                // able to fail on a row that vanished between two queries.
                let member = member_by_conversation
                    .remove(&conversation.conversation_id)
                    .unwrap_or_else(|| ConversationMember {
                        conversation_id: conversation.conversation_id,
                        account_id,
                        role: RoomRole::Member.to_wire() as i16,
                        joined_at: conversation.created_at,
                        left_at: None,
                        muted_until: None,
                        pinned: false,
                    });
                ConversationSummary {
                    last_message: last_by_conversation
                        .get(&conversation.conversation_id)
                        .cloned(),
                    unread: Ord::max(conversation.last_seq - cursor.read_seq, 0),
                    members: members_by_conversation
                        .remove(&conversation.conversation_id)
                        .unwrap_or_default(),
                    cursor,
                    member,
                    conversation,
                }
            })
            .collect())
    }

    async fn conversations_with_unread(&self, account_id: Id) -> Result<Vec<(Id, i64, i64)>> {
        let account = uuid_of(account_id);
        // The cursor is joined on the account as well as the conversation, which is
        // an extra condition on the join and not a `where`: moving it to the `where`
        // would turn the outer join back into an inner one and drop every
        // conversation the account has never read.
        let read = || {
            Expr::col(entity::conversation_cursor::Column::ReadSeq.as_column_ref()).if_null(0_i64)
        };
        let rows: Vec<(Uuid, i64, i64)> = entity::conversation::Entity::find()
            .select_only()
            .column(entity::conversation::Column::ConversationId)
            .column(entity::conversation::Column::LastSeq)
            .expr_as(read(), "read_seq")
            .join(
                JoinType::InnerJoin,
                entity::conversation_member::Relation::Conversation
                    .def()
                    .rev(),
            )
            .join(
                JoinType::LeftJoin,
                entity::conversation_cursor::Relation::Conversation
                    .def()
                    .rev()
                    .on_condition(move |_left, right| {
                        Expr::col((right, entity::conversation_cursor::Column::AccountId))
                            .eq(account)
                            .into_condition()
                    }),
            )
            .filter(entity::conversation_member::Column::AccountId.eq(account))
            .filter(entity::conversation_member::Column::LeftAt.is_null())
            .filter(Expr::col(entity::conversation::Column::LastSeq.as_column_ref()).gt(read()))
            .order_by_asc(entity::conversation::Column::ConversationId)
            .into_tuple()
            .all(&self.db)
            .await
            .context("conversations_with_unread")?;

        Ok(rows
            .into_iter()
            .map(|(conversation_id, last_seq, read_seq)| {
                (id_of(conversation_id), last_seq, read_seq)
            })
            .collect())
    }

    async fn purge_expired_messages(&self, before: Timestamp, limit: u16) -> Result<u64> {
        // A hard delete, and the only one in the messaging path. A disappearing
        // message that leaves a tombstone behind has not disappeared; the client
        // was told when it would go and has already stopped showing it.
        //
        // Hand-written, because the shape is `delete ... using` over a CTE and the
        // entity API has no way to say that. The `order by` inside the CTE is what
        // makes a budgeted run deterministic: hitting the budget twice in a row must
        // make progress, not re-pick a different arbitrary subset each time.
        let result = self
            .db
            .execute_raw(sql(
                "with doomed as ( \
                     select created_at, message_id from message \
                     where expires_at is not null and expires_at <= $1 \
                     order by conversation_id, seq limit $2 \
                 ) \
                 delete from message m using doomed d \
                 where m.created_at = d.created_at and m.message_id = d.message_id",
                [stamp_value(before), (clamp_limit(limit) as i64).into()],
            ))
            .await
            .context("purge_expired_messages")?;
        Ok(result.rows_affected())
    }
}

/// The stored side of an `on conflict do update`, table-qualified.
fn stored_seq(column: entity::conversation_cursor::Column) -> Expr {
    Expr::col(column.as_column_ref())
}

/// The proposed side of an `on conflict do update`.
///
/// `excluded` is the pseudo-table PostgreSQL exposes inside a conflict clause,
/// holding the row that could not be inserted. sea-query writes it for
/// `update_columns`, but there is no helper for naming a column of it inside a
/// larger expression, so it is spelled out here — once, rather than at each of the
/// seven places [`MessagingStore::advance_cursor`] needs it.
fn proposed_seq(column: entity::conversation_cursor::Column) -> Expr {
    Expr::col((Alias::new("excluded"), column))
}

/// Inserts conversation members in one statement.
///
/// One `insert ... values` with a row per member, so a group of fifty is one round
/// trip rather than fifty. `do nothing` tolerates a caller that repeated an id.
///
/// Generic over the connection because both callers pass their own transaction:
/// members appearing without the conversation they belong to, or the other way
/// round, is not a state any reader should be able to observe.
async fn insert_members<C: ConnectionTrait>(
    connection: &C,
    conversation_id: Id,
    members: &[Id],
    at: Timestamp,
) -> Result<()> {
    entity::conversation_member::Entity::insert_many(members.iter().map(|account_id| {
        entity::conversation_member::ActiveModel {
            conversation_id: Set(uuid_of(conversation_id)),
            account_id: Set(uuid_of(*account_id)),
            role: Set(0),
            joined_at: Set(stamp_of(at)),
            ..Default::default()
        }
    }))
    .on_conflict(
        OnConflict::columns([
            entity::conversation_member::Column::ConversationId,
            entity::conversation_member::Column::AccountId,
        ])
        .do_nothing()
        .to_owned(),
    )
    .exec_without_returning(connection)
    .await
    .map_err(|error| {
        on_conflict(error, "conversation_member", |name| match name {
            "conversation_member_account_id_fkey" => Some(fault::not_found("account")),
            "conversation_member_conversation_id_fkey" => Some(fault::not_found("conversation")),
            _ => None,
        })
    })?;
    Ok(())
}

#[async_trait]
impl RoomStore for PostgresStore {
    async fn create_room(&self, new: NewRoom) -> Result<Room> {
        if new.max_members <= 0 {
            return Err(fault::validation("max_members", "must be positive"));
        }

        // The room, its conversation, and the owner's membership are one unit. A
        // room without a conversation is unusable and a room without an owner is
        // unmoderatable, so none of the three may exist alone — which is exactly
        // what a transaction is for.
        let transaction = self.begin("create_room").await?;
        let room_id = uuid_of(new.room_id);
        let conversation_id = uuid_of(new.conversation_id);
        let owner_id = uuid_of(new.owner_id);
        let created_at = stamp_of(new.created_at);
        let encryption = wire_i16(new.encryption.to_wire());
        let owner_role = wire_i16(RoomRole::Owner.to_wire());

        entity::conversation::Entity::insert(entity::conversation::ActiveModel {
            conversation_id: Set(conversation_id),
            kind: Set(wire_i16(ConversationKind::Room.to_wire())),
            encryption: Set(encryption),
            room_id: Set(Some(room_id)),
            created_by: Set(owner_id),
            created_at: Set(created_at),
            ..Default::default()
        })
        .exec_without_returning(&transaction)
        .await
        .map_err(|error| {
            on_conflict(error, "conversation", |name| match name {
                "conversation_pkey" => Some(fault::already_exists("conversation")),
                "conversation_created_by_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?;

        entity::conversation_member::Entity::insert(entity::conversation_member::ActiveModel {
            conversation_id: Set(conversation_id),
            account_id: Set(owner_id),
            role: Set(owner_role),
            joined_at: Set(created_at),
            ..Default::default()
        })
        .exec_without_returning(&transaction)
        .await
        .context("create_room: owner conversation membership")?;

        // `member_count` starts at one rather than at zero and being recounted: the
        // owner's row goes in below, in this same transaction, so one is the true
        // count at commit and a recount here would only read rows we just wrote.
        let room: Room = entity::room::Entity::insert(entity::room::ActiveModel {
            room_id: Set(room_id),
            conversation_id: Set(conversation_id),
            slug: Set(new.slug),
            name: Set(new.name),
            topic: Set(new.topic),
            kind: Set(wire_i16(new.kind.to_wire())),
            owner_id: Set(owner_id),
            home_region: Set(new.home_region),
            member_count: Set(1),
            max_members: Set(new.max_members),
            slow_mode_seconds: Set(0),
            join_policy: Set(crate::model::join_policy::OPEN),
            encryption: Set(encryption),
            created_at: Set(created_at),
            updated_at: Set(created_at),
            archived_at: Set(None),
        })
        .exec_with_returning(&transaction)
        .await
        .map_err(|error| {
            on_conflict(error, "room", |name| match name {
                "room_slug_key" => Some(fault::already_exists("room slug")),
                "room_pkey" => Some(fault::already_exists("room")),
                "room_conversation_id_key" => Some(fault::already_exists("conversation")),
                "room_owner_id_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?
        .into();

        entity::room_member::Entity::insert(entity::room_member::ActiveModel {
            room_id: Set(room_id),
            account_id: Set(owner_id),
            role: Set(owner_role),
            joined_at: Set(created_at),
            ..Default::default()
        })
        .exec_without_returning(&transaction)
        .await
        .context("create_room: owner membership")?;

        transaction.commit().await.context("create_room: commit")?;
        Ok(room)
    }

    async fn room(&self, room_id: Id) -> Result<Option<Room>> {
        Ok(entity::room::Entity::find_by_id(uuid_of(room_id))
            .one(&self.db)
            .await
            .context("room")?
            .map(Into::into))
    }

    async fn room_by_slug(&self, slug: &str) -> Result<Option<Room>> {
        // Written as `lower(slug)` so it uses the expression index of the same
        // shape — the uniqueness of a slug is defined on `lower(slug)`, so this is
        // the only spelling that can find every row the index would reject. `fold`
        // also trims, which is a no-op here: a slug is validated against a pattern
        // upstream and carries no surrounding whitespace.
        Ok(entity::room::Entity::find()
            .filter(
                Expr::expr(Func::lower(Expr::col(
                    entity::room::Column::Slug.as_column_ref(),
                )))
                .eq(fold(slug)),
            )
            .one(&self.db)
            .await
            .context("room_by_slug")?
            .map(Into::into))
    }

    async fn update_room(
        &self,
        room_id: Id,
        name: Option<String>,
        topic: Patch<String>,
        slow_mode_seconds: Option<i32>,
        join_policy: Option<i16>,
        at: Timestamp,
    ) -> Result<Room> {
        if slow_mode_seconds.is_some_and(|seconds| seconds < 0) {
            return Err(fault::validation(
                "slow_mode_seconds",
                "must not be negative",
            ));
        }

        // Absent fields become absent assignments rather than `coalesce(column,
        // column)`, and Keep becomes no assignment at all. Same one round trip, and
        // the same reason as [`AccountStore::update_profile`]: a read-modify-write
        // would need the row's lock to be correct and would still clobber a
        // concurrent change to a field this call does not mention.
        let mut update = entity::room::Entity::update_many()
            .filter(entity::room::Column::RoomId.eq(uuid_of(room_id)))
            .set(entity::room::ActiveModel {
                updated_at: Set(stamp_of(at)),
                ..Default::default()
            });
        if let Some(name) = name {
            update = update.col_expr(entity::room::Column::Name, Expr::val(name));
        }
        if !topic.is_keep() {
            update = update.col_expr(
                entity::room::Column::Topic,
                Expr::val(patch_value(&topic).cloned()),
            );
        }
        if let Some(seconds) = slow_mode_seconds {
            update = update.col_expr(entity::room::Column::SlowModeSeconds, Expr::val(seconds));
        }
        if let Some(policy) = join_policy {
            update = update.col_expr(entity::room::Column::JoinPolicy, Expr::val(policy));
        }

        update
            .exec_with_returning(&self.db)
            .await
            .context("update_room")?
            .into_iter()
            .next()
            .map(Into::into)
            .ok_or_else(|| fault::not_found("room"))
    }

    async fn archive_room(&self, room_id: Id, at: Timestamp) -> Result<()> {
        // Not a delete: links and history keep resolving. The conversation is
        // archived in the same transaction, because a live conversation behind an
        // archived room would still accept messages.
        //
        // `archived_at is null` in the filter makes this idempotent and keeps the
        // first archival time: a second call matches no row, so it neither moves
        // the timestamp nor repeats the conversation write.
        let transaction = self.begin("archive_room").await?;
        let archived = entity::room::Entity::update_many()
            .filter(entity::room::Column::RoomId.eq(uuid_of(room_id)))
            .filter(entity::room::Column::ArchivedAt.is_null())
            .set(entity::room::ActiveModel {
                archived_at: Set(Some(stamp_of(at))),
                updated_at: Set(stamp_of(at)),
                ..Default::default()
            })
            .exec_with_returning(&transaction)
            .await
            .context("archive_room: archive room")?;

        if let Some(room) = archived.first() {
            entity::conversation::Entity::update_many()
                .filter(entity::conversation::Column::ConversationId.eq(room.conversation_id))
                .set(entity::conversation::ActiveModel {
                    archived_at: Set(Some(stamp_of(at))),
                    ..Default::default()
                })
                .exec(&transaction)
                .await
                .context("archive_room: archive conversation")?;
        }

        transaction.commit().await.context("archive_room: commit")?;
        Ok(())
    }

    async fn browse_rooms(&self, kind: Option<RoomKindFilter>, limit: u16) -> Result<Vec<Room>> {
        // Busiest first, which is what a directory is for; then oldest, then by id
        // so the order is total. `archived_at is null` and the kind together are the
        // partial index `room_browse_idx` was built for.
        let mut query =
            entity::room::Entity::find().filter(entity::room::Column::ArchivedAt.is_null());
        if let Some(filter) = kind {
            let wanted = match filter {
                RoomKindFilter::Public => RoomKind::Public,
                RoomKindFilter::Managed => RoomKind::Managed,
            };
            query = query.filter(entity::room::Column::Kind.eq(wire_i16(wanted.to_wire())));
        }
        let rows = query
            .order_by_desc(entity::room::Column::MemberCount)
            .order_by_asc(entity::room::Column::CreatedAt)
            .order_by_asc(entity::room::Column::RoomId)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("browse_rooms")?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn join_room(&self, member: RoomMember) -> Result<RoomMember> {
        let transaction = self.begin("join_room").await?;
        let room_id = uuid_of(member.room_id);
        let account_id = uuid_of(member.account_id);

        // `for update` on the room row is what makes the capacity check safe. A
        // check in the caller is a race: two joins that both read "one seat left"
        // would both take it. Serialising per room also serialises the member_count
        // write below, so the cached count cannot drift by a lost update.
        let room: Room = entity::room::Entity::find_by_id(room_id)
            .lock(LockType::Update)
            .one(&transaction)
            .await
            .context("join_room: lock room")?
            .ok_or_else(|| fault::not_found("room"))?
            .into();
        if room.archived_at.is_some() {
            return Err(fault::conflict("room is archived"));
        }

        let already_active = entity::room_member::Entity::find_by_id((room_id, account_id))
            .filter(entity::room_member::Column::LeftAt.is_null())
            .count(&transaction)
            .await
            .context("join_room: check membership")?
            > 0;

        if !already_active {
            // Counted from the rows rather than read from `member_count`, because a
            // capacity check has to be right even if the cached counter has drifted.
            let active = entity::room_member::Entity::find()
                .filter(entity::room_member::Column::RoomId.eq(room_id))
                .filter(entity::room_member::Column::LeftAt.is_null())
                .count(&transaction)
                .await
                .context("join_room: count members")?;
            if active >= u64::try_from(room.max_members).unwrap_or(0) {
                return Err(fault::conflict("room is full"));
            }
        }

        // Rejoining clears the departure but keeps the sanctions. A ban that could
        // be shed by leaving and coming back would not be a ban; the caller checks
        // `is_banned` before ever getting here. Role and join time are kept for the
        // same reason: neither is something a rejoin should reset. So the conflict
        // clause names the two columns it means, and `update_columns` is not used at
        // all — a list of everything to preserve would have to be edited every time
        // the table grows a column, and forgetting to is how a ban gets cleared.
        let stored: RoomMember =
            entity::room_member::Entity::insert(entity::room_member::ActiveModel {
                room_id: Set(room_id),
                account_id: Set(account_id),
                role: Set(wire_i16(member.role.to_wire())),
                permissions_grant: Set(member.permissions_grant as i64),
                permissions_deny: Set(member.permissions_deny as i64),
                joined_at: Set(stamp_of(member.joined_at)),
                left_at: Set(member.left_at.map(stamp_of)),
                muted_until: Set(member.muted_until.map(stamp_of)),
                banned_until: Set(member.banned_until.map(stamp_of)),
                ban_reason: Set(member.ban_reason),
                invited_by: Set(member.invited_by.map(uuid_of)),
            })
            .on_conflict(
                OnConflict::columns([
                    entity::room_member::Column::RoomId,
                    entity::room_member::Column::AccountId,
                ])
                .value(
                    entity::room_member::Column::LeftAt,
                    Expr::val(None::<OffsetDateTime>),
                )
                .value(
                    entity::room_member::Column::InvitedBy,
                    Expr::col((
                        Alias::new("excluded"),
                        entity::room_member::Column::InvitedBy,
                    ))
                    .if_null(Expr::col(
                        entity::room_member::Column::InvitedBy.as_column_ref(),
                    )),
                )
                .to_owned(),
            )
            .exec_with_returning(&transaction)
            .await
            .map_err(|error| {
                on_conflict(error, "room_member", |name| match name {
                    "room_member_account_id_fkey" => Some(fault::not_found("account")),
                    _ => None,
                })
            })?
            .into();

        if !already_active {
            recount_room_in(&transaction, room_id).await?;
            entity::conversation_member::Entity::insert(entity::conversation_member::ActiveModel {
                conversation_id: Set(uuid_of(room.conversation_id)),
                account_id: Set(account_id),
                role: Set(wire_i16(stored.role.to_wire())),
                joined_at: Set(stamp_of(stored.joined_at)),
                ..Default::default()
            })
            .on_conflict(
                OnConflict::columns([
                    entity::conversation_member::Column::ConversationId,
                    entity::conversation_member::Column::AccountId,
                ])
                .value(
                    entity::conversation_member::Column::LeftAt,
                    Expr::val(None::<OffsetDateTime>),
                )
                .to_owned(),
            )
            .exec_without_returning(&transaction)
            .await
            .context("join_room: conversation membership")?;
        }

        transaction.commit().await.context("join_room: commit")?;
        Ok(stored)
    }

    async fn leave_room(&self, room_id: Id, account_id: Id, at: Timestamp) -> Result<()> {
        let transaction = self.begin("leave_room").await?;
        let room = uuid_of(room_id);
        let account = uuid_of(account_id);

        let Some(conversation_id) = entity::room::Entity::find_by_id(room)
            .select_only()
            .column(entity::room::Column::ConversationId)
            .lock(LockType::Update)
            .into_tuple::<Uuid>()
            .one(&transaction)
            .await
            .context("leave_room: lock room")?
        else {
            // Leaving a room that does not exist is already the state the caller
            // wanted. Nothing to report.
            return Ok(());
        };

        let departed = entity::room_member::Entity::update_many()
            .filter(entity::room_member::Column::RoomId.eq(room))
            .filter(entity::room_member::Column::AccountId.eq(account))
            .filter(entity::room_member::Column::LeftAt.is_null())
            .set(entity::room_member::ActiveModel {
                left_at: Set(Some(stamp_of(at))),
                ..Default::default()
            })
            .exec(&transaction)
            .await
            .context("leave_room: mark departure")?;

        if departed.rows_affected > 0 {
            recount_room_in(&transaction, room).await?;
            entity::conversation_member::Entity::update_many()
                .filter(entity::conversation_member::Column::ConversationId.eq(conversation_id))
                .filter(entity::conversation_member::Column::AccountId.eq(account))
                .filter(entity::conversation_member::Column::LeftAt.is_null())
                .set(entity::conversation_member::ActiveModel {
                    left_at: Set(Some(stamp_of(at))),
                    ..Default::default()
                })
                .exec(&transaction)
                .await
                .context("leave_room: leave conversation")?;
        }

        transaction.commit().await.context("leave_room: commit")?;
        Ok(())
    }

    async fn room_member(&self, room_id: Id, account_id: Id) -> Result<Option<RoomMember>> {
        // No `left_at is null` filter: the caller needs to see a lapsed or banned
        // membership in order to enforce it.
        Ok(
            entity::room_member::Entity::find_by_id((uuid_of(room_id), uuid_of(account_id)))
                .one(&self.db)
                .await
                .context("room_member")?
                .map(Into::into),
        )
    }

    async fn room_members(
        &self,
        room_id: Id,
        limit: u16,
        after: Option<Id>,
    ) -> Result<Vec<RoomMember>> {
        let room = uuid_of(room_id);

        // The cursor is read first rather than expressed as a subquery, so the rule
        // for a cursor that is no longer on the roster is visible in Rust: paging
        // restarts from the top instead of silently returning nothing. It is read
        // from the active roster only, because that is the list being paged. Two
        // columns, because the keyset below is built from them and the rest of the
        // row would be fetched only to be dropped.
        let anchor = match after {
            Some(cursor) => entity::room_member::Entity::find_by_id((room, uuid_of(cursor)))
                .select_only()
                .column(entity::room_member::Column::Role)
                .column(entity::room_member::Column::JoinedAt)
                .filter(entity::room_member::Column::LeftAt.is_null())
                .into_tuple::<(i16, OffsetDateTime)>()
                .one(&self.db)
                .await
                .context("room_members: read cursor")?
                .map(|(role, joined_at)| (role, joined_at, uuid_of(cursor))),
            None => None,
        };

        // Highest role first, then longest-standing, then by id so the order is
        // total and the cursor above is unambiguous. Keyset rather than `offset`: an
        // offset page shifts under a concurrent join and drops or repeats rows.
        //
        // The three-way disjunction is the keyset comparison written out, because
        // the order is not all in one direction — role descends while the two
        // tie-breakers ascend, so a row tuple comparison would say the wrong thing.
        let mut query = entity::room_member::Entity::find()
            .filter(entity::room_member::Column::RoomId.eq(room))
            .filter(entity::room_member::Column::LeftAt.is_null());
        if let Some((role, joined_at, account_id)) = anchor {
            query = query.filter(
                Condition::any()
                    .add(entity::room_member::Column::Role.lt(role))
                    .add(
                        Condition::all()
                            .add(entity::room_member::Column::Role.eq(role))
                            .add(entity::room_member::Column::JoinedAt.gt(joined_at)),
                    )
                    .add(
                        Condition::all()
                            .add(entity::room_member::Column::Role.eq(role))
                            .add(entity::room_member::Column::JoinedAt.eq(joined_at))
                            .add(entity::room_member::Column::AccountId.gt(account_id)),
                    ),
            );
        }
        let rows = query
            .order_by_desc(entity::room_member::Column::Role)
            .order_by_asc(entity::room_member::Column::JoinedAt)
            .order_by_asc(entity::room_member::Column::AccountId)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("room_members")?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn rooms_for_account(&self, account_id: Id) -> Result<Vec<Room>> {
        let rows = entity::room::Entity::find()
            .join(
                JoinType::InnerJoin,
                entity::room_member::Relation::Room.def().rev(),
            )
            .filter(entity::room_member::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::room_member::Column::LeftAt.is_null())
            .order_by_asc(entity::room::Column::CreatedAt)
            .order_by_asc(entity::room::Column::RoomId)
            .all(&self.db)
            .await
            .context("rooms_for_account")?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn set_room_role(
        &self,
        room_id: Id,
        account_id: Id,
        role: RoomRole,
        _at: Timestamp,
    ) -> Result<()> {
        let result = entity::room_member::Entity::update_many()
            .filter(entity::room_member::Column::RoomId.eq(uuid_of(room_id)))
            .filter(entity::room_member::Column::AccountId.eq(uuid_of(account_id)))
            .set(entity::room_member::ActiveModel {
                role: Set(wire_i16(role.to_wire())),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("set_room_role")?;
        if result.rows_affected == 0 {
            return Err(fault::not_found("room member"));
        }
        Ok(())
    }

    async fn transfer_room_ownership(
        &self,
        room_id: Id,
        from: Id,
        to: Id,
        at: Timestamp,
    ) -> Result<()> {
        let transaction = self.begin("transfer_room_ownership").await?;
        let room = uuid_of(room_id);
        let outgoing = uuid_of(from);
        let incoming = uuid_of(to);

        // `FOR UPDATE` on the room row rather than a bare read: two transfers racing
        // would otherwise both see the same current owner and both succeed, leaving
        // the room owned by whichever committed last and demoting the other winner.
        let locked: Option<entity::room::Model> = entity::room::Entity::find_by_id(room)
            .lock_exclusive()
            .one(&transaction)
            .await
            .context("transfer_room_ownership: room")?;
        let locked = locked.ok_or_else(|| fault::not_found("room"))?;
        if locked.owner_id != outgoing {
            return Err(fault::conflict("not the owner of the room"));
        }
        if outgoing == incoming {
            return Ok(());
        }

        let successor: Option<entity::room_member::Model> = entity::room_member::Entity::find()
            .filter(entity::room_member::Column::RoomId.eq(room))
            .filter(entity::room_member::Column::AccountId.eq(incoming))
            .filter(entity::room_member::Column::LeftAt.is_null())
            .one(&transaction)
            .await
            .context("transfer_room_ownership: successor")?;
        if successor.is_none() {
            return Err(fault::not_found("room member"));
        }

        for (account, role) in [(incoming, RoomRole::Owner), (outgoing, RoomRole::Manager)] {
            entity::room_member::Entity::update_many()
                .filter(entity::room_member::Column::RoomId.eq(room))
                .filter(entity::room_member::Column::AccountId.eq(account))
                .set(entity::room_member::ActiveModel {
                    role: Set(wire_i16(role.to_wire())),
                    ..Default::default()
                })
                .exec(&transaction)
                .await
                .context("transfer_room_ownership: role")?;
        }

        entity::room::Entity::update_many()
            .filter(entity::room::Column::RoomId.eq(room))
            .set(entity::room::ActiveModel {
                owner_id: Set(incoming),
                updated_at: Set(stamp_of(at)),
                ..Default::default()
            })
            .exec(&transaction)
            .await
            .context("transfer_room_ownership: owner")?;

        transaction
            .commit()
            .await
            .context("transfer_room_ownership: commit")?;
        Ok(())
    }

    async fn set_room_permissions(
        &self,
        room_id: Id,
        account_id: Id,
        grant: u64,
        deny: u64,
        _at: Timestamp,
    ) -> Result<()> {
        let result = entity::room_member::Entity::update_many()
            .filter(entity::room_member::Column::RoomId.eq(uuid_of(room_id)))
            .filter(entity::room_member::Column::AccountId.eq(uuid_of(account_id)))
            .set(entity::room_member::ActiveModel {
                permissions_grant: Set(grant as i64),
                permissions_deny: Set(deny as i64),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("set_room_permissions")?;
        if result.rows_affected == 0 {
            return Err(fault::not_found("room member"));
        }
        Ok(())
    }

    async fn set_room_sanction(
        &self,
        room_id: Id,
        account_id: Id,
        muted_until: Option<Timestamp>,
        banned_until: Option<Timestamp>,
        reason: Option<String>,
        at: Timestamp,
    ) -> Result<()> {
        let transaction = self.begin("set_room_sanction").await?;
        let room = uuid_of(room_id);
        let account = uuid_of(account_id);

        let Some(conversation_id) = entity::room::Entity::find_by_id(room)
            .select_only()
            .column(entity::room::Column::ConversationId)
            .lock(LockType::Update)
            .into_tuple::<Uuid>()
            .one(&transaction)
            .await
            .context("set_room_sanction: lock room")?
        else {
            return Err(fault::not_found("room"));
        };

        // A ban ends the membership in the same statement that records it. A ban
        // that left `left_at` alone would leave the banned account counted as
        // present, and still receiving the room's messages. `if_null` keeps an
        // earlier departure, so banning somebody who had already left does not
        // rewrite when they went.
        let banned = banned_until.is_some();
        let mut update = entity::room_member::Entity::update_many()
            .filter(entity::room_member::Column::RoomId.eq(room))
            .filter(entity::room_member::Column::AccountId.eq(account))
            .set(entity::room_member::ActiveModel {
                muted_until: Set(muted_until.map(stamp_of)),
                banned_until: Set(banned_until.map(stamp_of)),
                ban_reason: Set(reason),
                ..Default::default()
            });
        if banned {
            update = update.col_expr(
                entity::room_member::Column::LeftAt,
                entity::room_member::Column::LeftAt.if_null(stamp_of(at)),
            );
        }
        let result = update
            .exec(&transaction)
            .await
            .context("set_room_sanction: apply")?;
        if result.rows_affected == 0 {
            return Err(fault::not_found("room member"));
        }

        if banned {
            recount_room_in(&transaction, room).await?;
            entity::conversation_member::Entity::update_many()
                .filter(entity::conversation_member::Column::ConversationId.eq(conversation_id))
                .filter(entity::conversation_member::Column::AccountId.eq(account))
                .col_expr(
                    entity::conversation_member::Column::LeftAt,
                    entity::conversation_member::Column::LeftAt.if_null(stamp_of(at)),
                )
                .exec(&transaction)
                .await
                .context("set_room_sanction: remove from conversation")?;
        }

        transaction
            .commit()
            .await
            .context("set_room_sanction: commit")?;
        Ok(())
    }

    async fn recount_room(&self, room_id: Id) -> Result<i32> {
        let transaction = self.begin("recount_room").await?;
        let room = uuid_of(room_id);
        let known = entity::room::Entity::find_by_id(room)
            .select_only()
            .column(entity::room::Column::RoomId)
            .lock(LockType::Update)
            .into_tuple::<Uuid>()
            .one(&transaction)
            .await
            .context("recount_room: lock room")?;
        if known.is_none() {
            return Err(fault::not_found("room"));
        }
        let count = recount_room_in(&transaction, room).await?;
        transaction.commit().await.context("recount_room: commit")?;
        Ok(count)
    }
}

/// Rebuilds a room's cached member count from its membership rows.
///
/// The count is derived, which is the point: a denormalised counter that cannot be
/// recomputed is a permanent source of numbers nobody trusts. Every caller here
/// already holds the room row's lock, so the read and the write cannot interleave
/// with another join.
///
/// It stays one statement — the count is a subquery of the update rather than a
/// value fetched and then written back — so the number stored is the number counted
/// even if the lock above is ever loosened.
///
/// Generic over the connection because every caller has a transaction in hand.
async fn recount_room_in<C: ConnectionTrait>(connection: &C, room_id: Uuid) -> Result<i32> {
    let updated = entity::room::Entity::update_many()
        .filter(entity::room::Column::RoomId.eq(room_id))
        .col_expr(
            entity::room::Column::MemberCount,
            Expr::from(
                entity::room_member::Entity::find()
                    .select_only()
                    .expr_as(
                        Func::count(Expr::col(
                            entity::room_member::Column::AccountId.as_column_ref(),
                        )),
                        "member_count",
                    )
                    .filter(entity::room_member::Column::RoomId.eq(room_id))
                    .filter(entity::room_member::Column::LeftAt.is_null())
                    .into_query(),
            ),
        )
        .exec_with_returning(connection)
        .await
        .context("recount_room")?;
    updated
        .first()
        .map(|room| room.member_count)
        .ok_or_else(|| fault::not_found("room"))
}

#[async_trait]
impl SocialStore for PostgresStore {
    async fn put_relationship(&self, relationship: Relationship) -> Result<Relationship> {
        if relationship.account_id == relationship.other_id {
            return Err(fault::validation(
                "other_id",
                "an account cannot relate to itself",
            ));
        }
        // The upsert deliberately leaves `created_at` alone: re-sending a friend
        // request should not make an old one look new. `accepted_at` is the only
        // column in the update list, which is the whole difference between the two
        // states this table stores.
        Ok(
            entity::relationship::Entity::insert(entity::relationship::ActiveModel {
                account_id: Set(uuid_of(relationship.account_id)),
                other_id: Set(uuid_of(relationship.other_id)),
                kind: Set(wire_i16(relationship.kind.to_wire())),
                created_at: Set(stamp_of(relationship.created_at)),
                accepted_at: Set(relationship.accepted_at.map(stamp_of)),
            })
            .on_conflict(
                OnConflict::columns([
                    entity::relationship::Column::AccountId,
                    entity::relationship::Column::OtherId,
                    entity::relationship::Column::Kind,
                ])
                .update_column(entity::relationship::Column::AcceptedAt)
                .to_owned(),
            )
            .exec_with_returning(&self.db)
            .await
            .map_err(|error| {
                on_conflict(error, "relationship", |name| match name {
                    "relationship_account_id_fkey" | "relationship_other_id_fkey" => {
                        Some(fault::not_found("account"))
                    }
                    _ => None,
                })
            })?
            .into(),
        )
    }

    async fn relationship(
        &self,
        account_id: Id,
        other_id: Id,
        kind: RelationshipKind,
    ) -> Result<Option<Relationship>> {
        Ok(entity::relationship::Entity::find_by_id((
            uuid_of(account_id),
            uuid_of(other_id),
            wire_i16(kind.to_wire()),
        ))
        .one(&self.db)
        .await
        .context("relationship")?
        .map(Into::into))
    }

    async fn remove_relationship(
        &self,
        account_id: Id,
        other_id: Id,
        kind: RelationshipKind,
    ) -> Result<()> {
        // No row is the state the caller asked for, so a delete that matches
        // nothing is a success.
        entity::relationship::Entity::delete_by_id((
            uuid_of(account_id),
            uuid_of(other_id),
            wire_i16(kind.to_wire()),
        ))
        .exec(&self.db)
        .await
        .context("remove_relationship")?;
        Ok(())
    }

    async fn accept_friend(&self, account_id: Id, other_id: Id, at: Timestamp) -> Result<()> {
        let transaction = self.begin("accept_friend").await?;
        let owner = uuid_of(account_id);
        let peer = uuid_of(other_id);
        let incoming = wire_i16(RelationshipKind::PendingIncoming.to_wire());
        let outgoing = wire_i16(RelationshipKind::PendingOutgoing.to_wire());
        let pending = Condition::any()
            .add(
                Condition::all()
                    .add(entity::relationship::Column::AccountId.eq(owner))
                    .add(entity::relationship::Column::OtherId.eq(peer))
                    .add(entity::relationship::Column::Kind.eq(incoming)),
            )
            .add(
                Condition::all()
                    .add(entity::relationship::Column::AccountId.eq(peer))
                    .add(entity::relationship::Column::OtherId.eq(owner))
                    .add(entity::relationship::Column::Kind.eq(outgoing)),
            );

        // Either half of the pending pair proves the request happened; the locally
        // held incoming row is preferred when both are present, which is what the
        // `case` in the ordering says. `for update` makes a double accept wait rather
        // than race, so the two friend rows below are written once from one agreed
        // request time.
        let locally_held: Expr = Expr::case(
            Expr::col(entity::relationship::Column::AccountId.as_column_ref()).eq(owner),
            0,
        )
        .finally(1)
        .into();
        let requested_at = entity::relationship::Entity::find()
            .select_only()
            .column(entity::relationship::Column::CreatedAt)
            .filter(pending.clone())
            .order_by(locally_held, Order::Asc)
            .lock(LockType::Update)
            .into_tuple::<OffsetDateTime>()
            .one(&transaction)
            .await
            .context("accept_friend: read request")?
            .ok_or_else(|| fault::not_found("friend request"))?;

        entity::relationship::Entity::delete_many()
            .filter(pending)
            .exec(&transaction)
            .await
            .context("accept_friend: clear request")?;

        // Both directions, in one statement. A friendship stored on one side only is
        // how "we are friends but you are not in my list" bugs happen.
        //
        // Here `created_at` *is* in the update list, unlike the upsert above: an
        // earlier rejected-and-forgotten friend row must take the accepted request's
        // date rather than keep its own.
        let friend = wire_i16(RelationshipKind::Friend.to_wire());
        entity::relationship::Entity::insert_many([
            entity::relationship::ActiveModel {
                account_id: Set(owner),
                other_id: Set(peer),
                kind: Set(friend),
                created_at: Set(requested_at),
                accepted_at: Set(Some(stamp_of(at))),
            },
            entity::relationship::ActiveModel {
                account_id: Set(peer),
                other_id: Set(owner),
                kind: Set(friend),
                created_at: Set(requested_at),
                accepted_at: Set(Some(stamp_of(at))),
            },
        ])
        .on_conflict(
            OnConflict::columns([
                entity::relationship::Column::AccountId,
                entity::relationship::Column::OtherId,
                entity::relationship::Column::Kind,
            ])
            .update_columns([
                entity::relationship::Column::CreatedAt,
                entity::relationship::Column::AcceptedAt,
            ])
            .to_owned(),
        )
        .exec_without_returning(&transaction)
        .await
        .context("accept_friend: create friendship")?;

        transaction
            .commit()
            .await
            .context("accept_friend: commit")?;
        Ok(())
    }

    async fn relationships(
        &self,
        account_id: Id,
        kind: RelationshipKind,
        limit: u16,
    ) -> Result<Vec<Relationship>> {
        let rows = entity::relationship::Entity::find()
            .filter(entity::relationship::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::relationship::Column::Kind.eq(wire_i16(kind.to_wire())))
            .order_by_desc(entity::relationship::Column::CreatedAt)
            .order_by_asc(entity::relationship::Column::OtherId)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("relationships")?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn count_relationships(&self, account_id: Id, kind: RelationshipKind) -> Result<u64> {
        // Served by the primary key's leading columns, so this is an index-only count
        // rather than a read of every edge the account owns.
        entity::relationship::Entity::find()
            .filter(entity::relationship::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::relationship::Column::Kind.eq(wire_i16(kind.to_wire())))
            .count(&self.db)
            .await
            .context("count_relationships")
    }

    async fn inbound_relationships(
        &self,
        account_id: Id,
        kind: RelationshipKind,
        limit: u16,
    ) -> Result<Vec<Relationship>> {
        // Served by `relationship_reverse_idx`; without it this is a full scan of
        // every edge in the system to answer "who asked to be my friend".
        let rows = entity::relationship::Entity::find()
            .filter(entity::relationship::Column::OtherId.eq(uuid_of(account_id)))
            .filter(entity::relationship::Column::Kind.eq(wire_i16(kind.to_wire())))
            .order_by_desc(entity::relationship::Column::CreatedAt)
            .order_by_asc(entity::relationship::Column::AccountId)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("inbound_relationships")?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn is_blocked_either_way(&self, a: Id, b: Id) -> Result<bool> {
        // Symmetric on purpose: a block has to stop both directions, or the blocked
        // party could still open the conversation.
        let blocks = entity::relationship::Entity::find()
            .filter(
                entity::relationship::Column::Kind.eq(wire_i16(RelationshipKind::Block.to_wire())),
            )
            .filter(
                Condition::any()
                    .add(
                        Condition::all()
                            .add(entity::relationship::Column::AccountId.eq(uuid_of(a)))
                            .add(entity::relationship::Column::OtherId.eq(uuid_of(b))),
                    )
                    .add(
                        Condition::all()
                            .add(entity::relationship::Column::AccountId.eq(uuid_of(b)))
                            .add(entity::relationship::Column::OtherId.eq(uuid_of(a))),
                    ),
            )
            .count(&self.db)
            .await
            .context("is_blocked_either_way")?;
        Ok(blocks > 0)
    }
}

/// The system owner sentinel, so a null `owner_id` still has a unique key.
///
/// Literal SQL rather than a bound value, deliberately. PostgreSQL matches an `on
/// conflict` target against the index definition by comparing the parsed
/// expressions, and a placeholder is not equal to a constant however it is bound —
/// so binding this would make the upsert in [`EconomyStore::ledger_account`] fail
/// with "no unique or exclusion constraint matching" rather than find the index.
const SYSTEM_OWNER: &str = "'00000000-0000-0000-0000-000000000000'::uuid";

/// `coalesce(<owner>, <system owner>)`, the shape of `ledger_account_owner_key`.
///
/// One definition for the three places that need it — the conflict target, and both
/// sides of the read that follows a lost race — because an index expression written
/// twice is an index expression that will eventually be written two ways, and the
/// symptom is a silent sequential scan.
fn owner_key(owner: Expr) -> Expr {
    owner.if_null(Expr::cust(SYSTEM_OWNER))
}

/// Reads the legs of one transaction, in the order they were posted.
///
/// Two columns rather than the entity, because a leg is an account and an amount:
/// the currency and posting time are the transaction's, already known to every
/// caller here.
async fn legs_of<C: ConnectionTrait>(connection: &C, tx_id: Uuid) -> Result<Vec<LedgerLeg>> {
    let rows: Vec<(Uuid, i64)> = entity::ledger_entry::Entity::find()
        .select_only()
        .column(entity::ledger_entry::Column::AccountId)
        .column(entity::ledger_entry::Column::Amount)
        .filter(entity::ledger_entry::Column::TxId.eq(tx_id))
        .order_by_asc(entity::ledger_entry::Column::LegIndex)
        .into_tuple()
        .all(connection)
        .await
        .context("legs_of")?;
    Ok(rows
        .into_iter()
        .map(|(ledger_account_id, amount)| LedgerLeg {
            ledger_account_id: id_of(ledger_account_id),
            amount,
        })
        .collect())
}

/// One account's balance, on a given connection.
///
/// The snapshot-plus-entries sum, factored out so it can run on the pool for a
/// plain read and inside a transaction for a balance read taken under a
/// `FOR UPDATE` lock. This is the single definition of "what an account holds" in
/// this backend: [`EconomyStore::balance`] and the overdraft floor in
/// [`EconomyStore::post_transaction`] both go through it, so a read and the write
/// that guards against it cannot disagree about the number.
///
/// Hand-written: the shape is a CTE referenced three times, and there is no builder
/// spelling of that which would be easier to check than the SQL. The cast to
/// `bigint` matters — Postgres `sum()` over `bigint` returns `numeric`, and the
/// column is decoded as `i64`.
async fn balance_on<C: ConnectionTrait>(connection: &C, account: Uuid) -> Result<i64> {
    // The snapshot is a starting point, not the truth: it exists only so this sum
    // does not have to start at the beginning of time. Entries stamped at exactly
    // `as_of` are inside the snapshot, so only later ones are added, and dropping
    // every snapshot still yields the same number.
    let row = connection
        .query_one_raw(sql(
            "with base as ( \
                 select balance, as_of from ledger_snapshot \
                 where ledger_account_id = $1 order by as_of desc limit 1 \
             ) \
             select (coalesce((select balance from base), 0) \
                  + coalesce(( \
                        select sum(amount) from ledger_entry \
                        where account_id = $1 \
                        and created_at > coalesce((select as_of from base), '-infinity') \
                    ), 0))::bigint as balance",
            [account.into()],
        ))
        .await
        .context("balance")?
        .ok_or_else(|| fault::internal("balance returned no row"))?;
    field(&row, "balance")
}

fn transaction_of(
    row: entity::ledger_transaction::Model,
    legs: Vec<LedgerLeg>,
) -> LedgerTransaction {
    LedgerTransaction {
        tx_id: id_of(row.tx_id),
        reason: row.reason,
        ref_id: row.ref_id.map(id_of),
        idempotency_key: row.idempotency_key,
        created_by: row.created_by.map(id_of),
        created_at: instant_of(row.created_at),
        legs,
    }
}

fn gift_of(row: entity::gift_sent::Model) -> GiftSent {
    GiftSent {
        gift_id: id_of(row.gift_id),
        tx_id: id_of(row.tx_id),
        sender_id: id_of(row.sender_id),
        recipient_id: id_of(row.recipient_id),
        gift_code: row.gift_code,
        conversation_id: row.conversation_id.map(id_of),
        created_at: instant_of(row.created_at),
    }
}

fn entitlement_of(row: entity::entitlement::Model) -> Entitlement {
    Entitlement {
        account_id: id_of(row.account_id),
        sku: row.sku,
        acquired_at: instant_of(row.acquired_at),
        tx_id: row.tx_id.map(id_of),
    }
}

fn progression_of(row: entity::progression::Model) -> Progression {
    Progression {
        account_id: id_of(row.account_id),
        xp: row.xp,
        level: row.level,
        updated_at: instant_of(row.updated_at),
    }
}

fn game_session_of(row: entity::game_session::Model) -> GameSession {
    // No fallible enum conversions here: `kind` and `status` stay raw `i16`, because
    // their meaning belongs to `migo-games`, and `state` is bytes the store never
    // interprets. So a row always converts — there is nothing in it this layer could
    // find invalid.
    GameSession {
        game_id: id_of(row.game_id),
        kind: row.kind,
        conversation_id: id_of(row.conversation_id),
        state: row.state,
        turn_of: row.turn_of.map(id_of),
        status: row.status,
        stake_currency: row.stake_currency,
        stake_amount: row.stake_amount,
        created_at: instant_of(row.created_at),
        updated_at: instant_of(row.updated_at),
        finished_at: row.finished_at.map(instant_of),
    }
}

/// A `bot` row to a [`Bot`]. Like [`game_session_of`], nothing here can fail: the
/// scope bits stay a raw `i64` because their meaning lives in `migo-bots`, and the
/// token hash is opaque bytes this layer never interprets.
fn bot_of(row: entity::bot::Model) -> Bot {
    Bot {
        bot_id: id_of(row.bot_id),
        owner_id: id_of(row.owner_id),
        account_id: id_of(row.account_id),
        name: row.name,
        token_hash: row.token_hash,
        scopes: row.scopes,
        webhook_url: row.webhook_url,
        created_at: instant_of(row.created_at),
        disabled_at: row.disabled_at.map(instant_of),
    }
}

fn peer_of(row: entity::node_peer::Model) -> PeerRecord {
    PeerRecord {
        node_id: row.node_id,
        public_key: row.public_key,
        base_url: row.base_url,
        region: row.region,
        status: row.status,
        added_at: instant_of(row.added_at),
        last_seen_at: row.last_seen_at.map(instant_of),
    }
}

fn outbox_of(row: entity::federation_outbox::Model) -> OutboxRecord {
    OutboxRecord {
        event_id: id_of(row.event_id),
        target_node: row.target_node,
        opcode: row.opcode,
        payload: row.payload,
        attempts: row.attempts,
        created_at: instant_of(row.created_at),
        next_attempt_at: instant_of(row.next_attempt_at),
        delivered_at: row.delivered_at.map(instant_of),
        last_error: row.last_error,
    }
}

fn badge_of(row: entity::badge_award::Model) -> BadgeAward {
    BadgeAward {
        account_id: id_of(row.account_id),
        badge_code: row.badge_code,
        awarded_at: instant_of(row.awarded_at),
        ref_id: row.ref_id.map(id_of),
    }
}

fn standing_of((account_id, xp, level): (Uuid, i64, i32)) -> Standing {
    Standing {
        account_id: id_of(account_id),
        xp,
        level,
    }
}

/// The three columns every leaderboard reads, ordered the way every leaderboard has
/// to order them.
///
/// Read as three columns rather than whole rows because a `Standing` is narrower than
/// a `progression` row, and `updated_at` is not something anybody is ranked by.
///
/// XP descending, then account id ascending. The second key is not cosmetic: a hundred
/// accounts sitting on the same round number is the normal shape of a leaderboard's
/// tail, and without a tiebreak PostgreSQL is free to return them in a different order
/// every time, so a page-two request can repeat or skip whoever was on the boundary.
/// The memory backend's `rank` sorts by the same two keys for the same reason.
fn leaderboard(limit: u16) -> Select<entity::progression::Entity> {
    entity::progression::Entity::find()
        .select_only()
        .column(entity::progression::Column::AccountId)
        .column(entity::progression::Column::Xp)
        .column(entity::progression::Column::Level)
        .order_by_desc(entity::progression::Column::Xp)
        .order_by_asc(entity::progression::Column::AccountId)
        .limit(clamp_limit(limit) as u64)
}

impl PostgresStore {
    /// Reads back a transaction by its retry key, legs included.
    async fn transaction_by_key(&self, key: &str) -> Result<Option<LedgerTransaction>> {
        let Some(row) = entity::ledger_transaction::Entity::find()
            .filter(entity::ledger_transaction::Column::IdempotencyKey.eq(key))
            .one(&self.db)
            .await
            .context("transaction_by_key")?
        else {
            return Ok(None);
        };
        let legs = legs_of(&self.db, row.tx_id).await?;
        Ok(Some(transaction_of(row, legs)))
    }
}

#[async_trait]
impl EconomyStore for PostgresStore {
    async fn ledger_account(
        &self,
        owner_id: Option<Id>,
        kind: LedgerAccountKind,
        currency: Currency,
        create_with_id: Id,
        at: Timestamp,
    ) -> Result<LedgerAccount> {
        let owner = owner_id.map(uuid_of);
        // Find-or-create as one statement rather than select-then-insert, which
        // races: two first-time gifts to the same account would both find nothing
        // and both insert.
        //
        // The conflict target is the index's expression, not a column list, because
        // uniqueness here is defined over `coalesce(owner_id, ...)` — system accounts
        // have no owner and a plain `(owner_id, kind, currency)` target would let
        // every one of them be created twice. The column references are unqualified
        // for the same reason: that is the form an index definition takes.
        let created = entity::ledger_account::Entity::insert(entity::ledger_account::ActiveModel {
            ledger_account_id: Set(uuid_of(create_with_id)),
            owner_id: Set(owner),
            kind: Set(kind.to_i16()),
            currency: Set(currency.to_i16()),
            created_at: Set(stamp_of(at)),
        })
        .on_conflict(
            OnConflict::new()
                .exprs([
                    owner_key(Expr::col(entity::ledger_account::Column::OwnerId)),
                    Expr::col(entity::ledger_account::Column::Kind),
                    Expr::col(entity::ledger_account::Column::Currency),
                ])
                .do_nothing()
                .to_owned(),
        )
        .exec_with_returning(&self.db)
        .await;

        match created {
            Ok(row) => return ledger_account_of(row),
            // `do nothing` returned no row, so the account already existed. This is
            // the expected outcome of the second caller, not a failure.
            //
            // Both variants are matched because which one arrives is an internal
            // detail of the ORM, not of this schema: sea-orm 2.0 reports an empty
            // `returning` from `exec_with_returning` as `RecordNotFound`, while the
            // `exec_without_returning` path reports `RecordNotInserted`. Neither can
            // mean anything else after an insert — there is no row to fail to find
            // except the one the conflict declined to write — so treating only one of
            // them as "already existed" would make a working upsert depend on which
            // constructor sea-orm happens to route through.
            Err(DbErr::RecordNotInserted | DbErr::RecordNotFound(_)) => {}
            Err(error) => {
                return Err(on_conflict(error, "ledger account", |name| match name {
                    "ledger_account_pkey" => Some(fault::already_exists("ledger account")),
                    "ledger_account_owner_id_fkey" => Some(fault::not_found("account")),
                    _ => None,
                }))
            }
        }

        let row = entity::ledger_account::Entity::find()
            .filter(
                owner_key(Expr::col(
                    entity::ledger_account::Column::OwnerId.as_column_ref(),
                ))
                .eq(owner_key(Expr::val(owner))),
            )
            .filter(entity::ledger_account::Column::Kind.eq(kind.to_i16()))
            .filter(entity::ledger_account::Column::Currency.eq(currency.to_i16()))
            .one(&self.db)
            .await
            .context("ledger_account: read existing")?
            .ok_or_else(|| fault::internal("ledger account conflicted with nothing"))?;
        ledger_account_of(row)
    }

    async fn post_transaction(&self, new: NewTransaction) -> Result<Posted> {
        // The retry key is checked before anything else. A retry of a transaction
        // that was already accepted has to read back the original, not be judged
        // again against rules that may have moved since.
        if let Some(existing) = self.transaction_by_key(&new.idempotency_key).await? {
            return Ok(Posted::Duplicate(existing));
        }
        if new.legs.len() < 2 {
            return Err(fault::validation(
                "legs",
                "a transfer needs at least two legs",
            ));
        }
        if new.legs.len() > MAX_LEDGER_LEGS {
            return Err(fault::validation("legs", "too many legs"));
        }

        let referenced: Vec<Uuid> = new
            .legs
            .iter()
            .map(|leg| uuid_of(leg.ledger_account_id))
            .collect();
        let rows: Vec<(Uuid, i16, i16)> = entity::ledger_account::Entity::find()
            .select_only()
            .column(entity::ledger_account::Column::LedgerAccountId)
            .column(entity::ledger_account::Column::Currency)
            .column(entity::ledger_account::Column::Kind)
            .filter(entity::ledger_account::Column::LedgerAccountId.is_in(referenced))
            .into_tuple()
            .all(&self.db)
            .await
            .context("post_transaction: read accounts")?;
        let mut currencies: HashMap<Id, i16> = HashMap::with_capacity(rows.len());
        let mut kinds: HashMap<Id, i16> = HashMap::with_capacity(rows.len());
        for (ledger_account_id, currency, kind) in rows {
            let id = id_of(ledger_account_id);
            currencies.insert(id, currency);
            kinds.insert(id, kind);
        }

        // Double entry, enforced rather than hoped for. If the legs do not sum to
        // zero then value was created or destroyed, and a currency whose total
        // drifts is a currency nobody can audit.
        let wanted = new.currency.to_i16();
        let mut total: i64 = 0;
        for leg in &new.legs {
            if leg.amount == 0 {
                return Err(fault::validation(
                    "legs",
                    "a zero-amount leg carries no meaning",
                ));
            }
            let currency = currencies
                .get(&leg.ledger_account_id)
                .ok_or_else(|| fault::not_found("ledger account"))?;
            if *currency != wanted {
                return Err(fault::validation(
                    "legs",
                    "every leg must share the currency",
                ));
            }
            total = total
                .checked_add(leg.amount)
                .ok_or_else(|| fault::validation("legs", "amounts overflow"))?;
        }
        if total != 0 {
            return Err(fault::validation("legs", "amounts must sum to zero"));
        }

        // A user account may not be driven below zero. A user cannot spend money
        // they do not have, and a negative user balance would be the ledger
        // asserting the platform extended them credit it never agreed to. System
        // accounts are exempt by design: Mint is negative by construction (its
        // balance is the total ever issued), and Fee and Escrow only accumulate
        // what users have already paid in. Collect the user accounts this
        // transaction debits on net, to be locked and checked once it is open.
        let mut deltas: HashMap<Uuid, i64> = HashMap::new();
        for leg in &new.legs {
            *deltas.entry(uuid_of(leg.ledger_account_id)).or_default() += leg.amount;
        }
        let user_kind = LedgerAccountKind::User.to_i16();
        let mut debited: Vec<(Uuid, i64)> = deltas
            .into_iter()
            .filter(|(account, delta)| {
                *delta < 0 && kinds.get(&id_of(*account)).copied() == Some(user_kind)
            })
            .collect();
        // Deterministic order, so two concurrent posts touching the same pair of
        // accounts cannot deadlock by locking them in opposite orders.
        debited.sort_by_key(|a| a.0);

        let transaction = self.begin("post_transaction").await?;

        // The overdraft floor, taken under a row lock so concurrent debits of one
        // account serialise: whoever locks first spends first, and the next sees the
        // balance the first left behind. Without the lock, two debits could each read
        // the original balance, each find it sufficient, and together overdraw. Only
        // the debited user accounts are locked, in id order; credits cannot cause an
        // overdraft, so they are left unserialised.
        if !debited.is_empty() {
            let locked: Vec<Uuid> = debited.iter().map(|(account, _)| *account).collect();
            entity::ledger_account::Entity::find()
                .select_only()
                .column(entity::ledger_account::Column::LedgerAccountId)
                .filter(entity::ledger_account::Column::LedgerAccountId.is_in(locked))
                .order_by_asc(entity::ledger_account::Column::LedgerAccountId)
                .lock(LockType::Update)
                .into_tuple::<Uuid>()
                .all(&transaction)
                .await
                .context("post_transaction: lock accounts")?;
            for (account, delta) in &debited {
                let current = balance_on(&transaction, *account).await?;
                let projected = current
                    .checked_add(*delta)
                    .ok_or_else(|| fault::validation("legs", "balance overflow"))?;
                if projected < 0 {
                    transaction
                        .rollback()
                        .await
                        .context("post_transaction: rollback")?;
                    return Err(fault::insufficient_balance("account"));
                }
            }
        }

        let tx_id = uuid_of(new.tx_id);
        let created_at = stamp_of(new.created_at);
        let write =
            entity::ledger_transaction::Entity::insert(entity::ledger_transaction::ActiveModel {
                tx_id: Set(tx_id),
                reason: Set(new.reason),
                ref_id: Set(new.ref_id.map(uuid_of)),
                idempotency_key: Set(new.idempotency_key.clone()),
                created_at: Set(created_at),
                created_by: Set(new.created_by.map(uuid_of)),
            })
            .exec_without_returning(&transaction)
            .await;

        if let Err(error) = write {
            // A retry that lost the race to its own original: the key check above
            // found nothing because the winner had not committed yet. Roll back and
            // read the winner, which is the answer the caller wanted either way.
            if constraint(&error).as_deref() == Some("ledger_transaction_idempotency_key_key") {
                transaction
                    .rollback()
                    .await
                    .context("post_transaction: rollback")?;
                return self
                    .transaction_by_key(&new.idempotency_key)
                    .await?
                    .map(Posted::Duplicate)
                    .ok_or_else(|| fault::internal("retry key conflicted with nothing"));
            }
            return Err(on_conflict(error, "transaction", |name| match name {
                "ledger_transaction_pkey" => Some(fault::already_exists("transaction")),
                "ledger_transaction_created_by_fkey" => Some(fault::not_found("account")),
                _ => None,
            }));
        }

        // The legs are keyed by their position in the transaction, so the index is
        // written from the enumeration rather than left to insertion order: a debit
        // followed by a credit is a transfer and the other order is a refund, so the
        // position is data.
        entity::ledger_entry::Entity::insert_many(new.legs.iter().enumerate().map(
            |(index, leg)| entity::ledger_entry::ActiveModel {
                tx_id: Set(tx_id),
                leg_index: Set(index as i16),
                account_id: Set(uuid_of(leg.ledger_account_id)),
                amount: Set(leg.amount),
                currency: Set(wanted),
                created_at: Set(created_at),
            },
        ))
        .exec_without_returning(&transaction)
        .await
        .context("post_transaction: write entries")?;

        // The delivery, inside the same database transaction as the money. A gift
        // that charged and delivered nothing is a support ticket a week later with
        // no evidence left; committing the legs first and the receipt second is
        // exactly how that state is reached.
        match &new.receipt {
            Some(Receipt::Gift(gift)) => {
                let write = entity::gift_sent::Entity::insert(entity::gift_sent::ActiveModel {
                    gift_id: Set(uuid_of(gift.gift_id)),
                    tx_id: Set(tx_id),
                    sender_id: Set(uuid_of(gift.sender_id)),
                    recipient_id: Set(uuid_of(gift.recipient_id)),
                    gift_code: Set(gift.gift_code.clone()),
                    conversation_id: Set(gift.conversation_id.map(uuid_of)),
                    created_at: Set(created_at),
                })
                .exec_without_returning(&transaction)
                .await;
                if let Err(error) = write {
                    // No explicit rollback: dropping the transaction rolls it back,
                    // and returning early is the only path out of here.
                    return Err(on_conflict(error, "gift", |name| match name {
                        "gift_sent_pkey" | "gift_sent_tx_key" => {
                            Some(fault::already_exists("gift"))
                        }
                        "gift_sent_sender_id_fkey" | "gift_sent_recipient_id_fkey" => {
                            Some(fault::not_found("account"))
                        }
                        _ => None,
                    }));
                }
            }
            Some(Receipt::Entitlement { sku }) => {
                let owner = new.created_by.ok_or_else(|| {
                    fault::validation("created_by", "an entitlement needs an owner")
                })?;
                let write = entity::entitlement::Entity::insert(entity::entitlement::ActiveModel {
                    account_id: Set(uuid_of(owner)),
                    sku: Set(sku.clone()),
                    acquired_at: Set(created_at),
                    tx_id: Set(Some(tx_id)),
                })
                .exec_without_returning(&transaction)
                .await;
                if let Err(error) = write {
                    return Err(on_conflict(error, "entitlement", |name| match name {
                        "entitlement_pkey" => Some(fault::already_exists("entitlement")),
                        "entitlement_account_id_fkey" => Some(fault::not_found("account")),
                        _ => None,
                    }));
                }
            }
            None => {}
        }

        transaction
            .commit()
            .await
            .context("post_transaction: commit")?;
        Ok(Posted::Created(LedgerTransaction {
            tx_id: new.tx_id,
            reason: new.reason,
            ref_id: new.ref_id,
            idempotency_key: new.idempotency_key,
            created_by: new.created_by,
            created_at: new.created_at,
            legs: new.legs,
        }))
    }

    async fn balance(&self, ledger_account_id: Id) -> Result<i64> {
        let account = uuid_of(ledger_account_id);
        let known = entity::ledger_account::Entity::find_by_id(account)
            .select_only()
            .column(entity::ledger_account::Column::LedgerAccountId)
            .into_tuple::<Uuid>()
            .one(&self.db)
            .await
            .context("balance: read account")?;
        if known.is_none() {
            return Err(fault::not_found("ledger account"));
        }
        balance_on(&self.db, account).await
    }

    async fn ledger_history(
        &self,
        ledger_account_id: Id,
        limit: u16,
    ) -> Result<Vec<(LedgerTransaction, i64)>> {
        // Three bounded queries rather than one join: the page is at most `MAX_PAGE`
        // entries, and a join that repeats every transaction once per leg would have
        // to be de-duplicated here anyway.
        let statement: Vec<(Uuid, i64)> = entity::ledger_entry::Entity::find()
            .select_only()
            .column(entity::ledger_entry::Column::TxId)
            .column(entity::ledger_entry::Column::Amount)
            .filter(entity::ledger_entry::Column::AccountId.eq(uuid_of(ledger_account_id)))
            .order_by_desc(entity::ledger_entry::Column::CreatedAt)
            .order_by_desc(entity::ledger_entry::Column::TxId)
            .order_by_desc(entity::ledger_entry::Column::LegIndex)
            .limit(clamp_limit(limit) as u64)
            .into_tuple()
            .all(&self.db)
            .await
            .context("ledger_history: read entries")?;
        if statement.is_empty() {
            return Ok(Vec::new());
        }
        let tx_ids: Vec<Uuid> = statement.iter().map(|(tx_id, _)| *tx_id).collect();

        // Every leg of each transaction on the page, not just the ones belonging to
        // this account: a receipt that showed one side of a transfer would not say
        // who the other side was.
        let leg_rows: Vec<(Uuid, Uuid, i64)> = entity::ledger_entry::Entity::find()
            .select_only()
            .column(entity::ledger_entry::Column::TxId)
            .column(entity::ledger_entry::Column::AccountId)
            .column(entity::ledger_entry::Column::Amount)
            .filter(entity::ledger_entry::Column::TxId.is_in(tx_ids.clone()))
            .order_by_asc(entity::ledger_entry::Column::TxId)
            .order_by_asc(entity::ledger_entry::Column::LegIndex)
            .into_tuple()
            .all(&self.db)
            .await
            .context("ledger_history: read legs")?;
        let mut legs: HashMap<Uuid, Vec<LedgerLeg>> = HashMap::new();
        for (tx_id, ledger_account_id, amount) in leg_rows {
            legs.entry(tx_id).or_default().push(LedgerLeg {
                ledger_account_id: id_of(ledger_account_id),
                amount,
            });
        }

        let mut transactions: HashMap<Uuid, LedgerTransaction> = HashMap::new();
        for row in entity::ledger_transaction::Entity::find()
            .filter(entity::ledger_transaction::Column::TxId.is_in(tx_ids))
            .all(&self.db)
            .await
            .context("ledger_history: read transactions")?
        {
            let tx_id = row.tx_id;
            let legs = legs.remove(&tx_id).unwrap_or_default();
            transactions.insert(tx_id, transaction_of(row, legs));
        }

        Ok(statement
            .into_iter()
            .filter_map(|(tx_id, amount)| {
                transactions
                    .get(&tx_id)
                    .cloned()
                    .map(|transaction| (transaction, amount))
            })
            .collect())
    }

    async fn currency_sum(&self, currency: Currency) -> Result<i64> {
        // Clamped rather than cast, because `sum` over `bigint` is numeric and a
        // plain cast would fail the audit with an overflow error instead of a very
        // obviously wrong number. The answer is supposed to be zero either way.
        let clamped = Func::least([
            Expr::from(Func::greatest([
                Expr::from(Func::sum(Expr::col(
                    entity::ledger_entry::Column::Amount.as_column_ref(),
                )))
                .if_null(0_i64),
                Expr::val(i64::MIN),
            ])),
            Expr::val(i64::MAX),
        ]);
        let total = entity::ledger_entry::Entity::find()
            .select_only()
            .expr_as(Expr::from(clamped).cast_as(Alias::new("bigint")), "total")
            .join(
                JoinType::InnerJoin,
                entity::ledger_entry::Relation::LedgerAccount.def(),
            )
            .filter(entity::ledger_account::Column::Currency.eq(currency.to_i16()))
            .into_tuple::<i64>()
            .one(&self.db)
            .await
            .context("currency_sum")?
            .unwrap_or(0);
        Ok(total)
    }

    async fn gifts_received(&self, account_id: Id, limit: u16) -> Result<Vec<GiftSent>> {
        Ok(entity::gift_sent::Entity::find()
            .filter(entity::gift_sent::Column::RecipientId.eq(uuid_of(account_id)))
            .order_by_desc(entity::gift_sent::Column::CreatedAt)
            .order_by_desc(entity::gift_sent::Column::GiftId)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("gifts_received")?
            .into_iter()
            .map(gift_of)
            .collect())
    }

    async fn gifts_in_conversation(
        &self,
        conversation_id: Id,
        limit: u16,
    ) -> Result<Vec<GiftSent>> {
        Ok(entity::gift_sent::Entity::find()
            .filter(entity::gift_sent::Column::ConversationId.eq(uuid_of(conversation_id)))
            .order_by_desc(entity::gift_sent::Column::CreatedAt)
            .order_by_desc(entity::gift_sent::Column::GiftId)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("gifts_in_conversation")?
            .into_iter()
            .map(gift_of)
            .collect())
    }

    async fn gift_tally(&self, account_id: Id) -> Result<Vec<(String, u32)>> {
        // Grouped in the database rather than counted here. The shelf on a popular
        // profile is a few dozen distinct codes over thousands of rows, and reading
        // the rows to count them would move the whole history across the wire to
        // produce a list that fits on one screen.
        let counted = || Func::count(Expr::col(entity::gift_sent::Column::GiftId.as_column_ref()));
        let rows: Vec<(String, i64)> = entity::gift_sent::Entity::find()
            .select_only()
            .column(entity::gift_sent::Column::GiftCode)
            .expr_as(counted(), "tally")
            .filter(entity::gift_sent::Column::RecipientId.eq(uuid_of(account_id)))
            .group_by(entity::gift_sent::Column::GiftCode)
            // Count descending then code ascending. Ordered by the aggregate itself
            // rather than by the alias so the statement says what it sorts on, and
            // with the second key so that two codes on the same count do not swap
            // places between two renders of the same profile.
            .order_by_desc(Expr::from(counted()))
            .order_by_asc(entity::gift_sent::Column::GiftCode)
            .into_tuple()
            .all(&self.db)
            .await
            .context("gift_tally")?;
        Ok(rows
            .into_iter()
            // Saturating rather than fallible: a count that exceeded `u32` would be
            // four billion gifts to one account, and refusing to render the shelf is
            // a worse answer than rendering an implausible number.
            .map(|(code, count)| (code, u32::try_from(count).unwrap_or(u32::MAX)))
            .collect())
    }

    async fn entitlements(&self, account_id: Id) -> Result<Vec<Entitlement>> {
        Ok(entity::entitlement::Entity::find()
            .filter(entity::entitlement::Column::AccountId.eq(uuid_of(account_id)))
            .order_by_asc(entity::entitlement::Column::AcquiredAt)
            .order_by_asc(entity::entitlement::Column::Sku)
            .all(&self.db)
            .await
            .context("entitlements")?
            .into_iter()
            .map(entitlement_of)
            .collect())
    }

    async fn has_entitlement(&self, account_id: Id, sku: &str) -> Result<bool> {
        // A primary-key probe that selects a constant: the question is whether the
        // row exists, and `acquired_at` is not part of the answer. `count` would read
        // the same index and then make the caller compare a number to zero.
        Ok(
            entity::entitlement::Entity::find_by_id((uuid_of(account_id), sku.to_owned()))
                .select_only()
                .expr_as(Expr::val(1_i32), "present")
                .into_tuple::<i32>()
                .one(&self.db)
                .await
                .context("has_entitlement")?
                .is_some(),
        )
    }
}

#[async_trait]
impl ProgressionStore for PostgresStore {
    async fn progression(&self, account_id: Id) -> Result<Option<Progression>> {
        Ok(entity::progression::Entity::find_by_id(uuid_of(account_id))
            .one(&self.db)
            .await
            .context("progression")?
            .map(progression_of))
    }

    async fn award_xp(&self, award: NewXpAward) -> Result<XpChange> {
        if award.amount <= 0 {
            return Err(fault::validation("amount", "must be positive"));
        }
        // Both rows in one transaction. An award that counted towards a daily cap but
        // not towards a rank, or the reverse, is what a crash between two statements
        // would leave behind, and neither half can be reconstructed from the other.
        let transaction = self.begin("award_xp").await?;

        entity::xp_award::Entity::insert(entity::xp_award::ActiveModel {
            award_id: Set(uuid_of(award.award_id)),
            account_id: Set(uuid_of(award.account_id)),
            source: Set(award.source),
            amount: Set(award.amount),
            ref_id: Set(award.ref_id.map(uuid_of)),
            idempotency_key: Set(award.idempotency_key.clone()),
            created_at: Set(stamp_of(award.at)),
        })
        .exec_without_returning(&transaction)
        .await
        .map_err(|error| {
            on_conflict(error, "xp award", |name| match name {
                "xp_award_pkey" | "xp_award_key" => Some(fault::already_exists("xp award")),
                "xp_award_account_id_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?;

        // The addition happens inside PostgreSQL, so two awards landing together sum
        // rather than overwrite. `xp = "progression"."xp" + amount` references the
        // existing row and not `excluded`, because `excluded.xp` is this call's amount
        // and adding it to itself would be the increment twice on a first award and
        // the increment alone on every one after.
        let row = entity::progression::Entity::insert(entity::progression::ActiveModel {
            account_id: Set(uuid_of(award.account_id)),
            xp: Set(award.amount),
            // A first award starts at level one. Recomputing the projection is the
            // caller's job; see `ProgressionStore::set_level`.
            level: Set(1),
            updated_at: Set(stamp_of(award.at)),
        })
        .on_conflict(
            OnConflict::column(entity::progression::Column::AccountId)
                .value(
                    entity::progression::Column::Xp,
                    Expr::col((entity::progression::Entity, entity::progression::Column::Xp))
                        .add(award.amount),
                )
                .value(entity::progression::Column::UpdatedAt, stamp_of(award.at))
                .to_owned(),
        )
        .exec_with_returning(&transaction)
        .await
        .map_err(|error| {
            on_conflict(error, "progression", |name| match name {
                "progression_account_id_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?;

        transaction.commit().await.context("award_xp: commit")?;

        // Exact by construction: the statement above added `amount` to whatever was
        // there, so the total before it is the total after it minus `amount`. Read
        // back rather than recomputed, because a concurrent award may have landed in
        // between and `after` has to be the value the row actually holds.
        //
        // An overflow is the one case where the two backends differ: the memory
        // backend refuses with a validation fault, while PostgreSQL refuses the
        // addition itself and the error arrives as a storage fault. The row is
        // unchanged either way, and reaching it needs nine quintillion XP.
        let after = row.xp;
        Ok(XpChange {
            before: after - award.amount,
            after,
        })
    }

    async fn set_level(&self, account_id: Id, level: i32, at: Timestamp) -> Result<()> {
        if level < 1 {
            return Err(fault::validation("level", "must be at least one"));
        }
        let result = entity::progression::Entity::update_many()
            .filter(entity::progression::Column::AccountId.eq(uuid_of(account_id)))
            .set(entity::progression::ActiveModel {
                level: Set(level),
                updated_at: Set(stamp_of(at)),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("set_level")?;
        if result.rows_affected == 0 {
            // No row means nothing has ever been earned, and a level without XP is
            // not something this table should be able to hold.
            return Err(fault::not_found("progression"));
        }
        Ok(())
    }

    async fn award_badge(&self, award: BadgeAward) -> Result<bool> {
        if award.badge_code.trim().is_empty() {
            return Err(fault::validation("badge_code", "must not be empty"));
        }
        let inserted = entity::badge_award::Entity::insert(entity::badge_award::ActiveModel {
            account_id: Set(uuid_of(award.account_id)),
            badge_code: Set(award.badge_code.clone()),
            awarded_at: Set(stamp_of(award.awarded_at)),
            ref_id: Set(award.ref_id.map(uuid_of)),
        })
        .on_conflict(
            OnConflict::columns([
                entity::badge_award::Column::AccountId,
                entity::badge_award::Column::BadgeCode,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(&self.db)
        .await;
        match inserted {
            Ok(_) => Ok(true),
            // The badge was already held. Both variants are matched for the reason
            // given at `ledger_account`: which one sea-orm raises for a declined
            // insert is an internal detail of the ORM.
            Err(DbErr::RecordNotInserted | DbErr::RecordNotFound(_)) => Ok(false),
            Err(error) => Err(on_conflict(error, "badge", |name| match name {
                "badge_award_account_id_fkey" => Some(fault::not_found("account")),
                _ => None,
            })),
        }
    }

    async fn badges(&self, account_id: Id) -> Result<Vec<BadgeAward>> {
        Ok(entity::badge_award::Entity::find()
            .filter(entity::badge_award::Column::AccountId.eq(uuid_of(account_id)))
            .order_by_desc(entity::badge_award::Column::AwardedAt)
            .order_by_asc(entity::badge_award::Column::BadgeCode)
            .all(&self.db)
            .await
            .context("badges")?
            .into_iter()
            .map(badge_of)
            .collect())
    }

    async fn xp_earned_since(
        &self,
        account_id: Id,
        source: Option<i16>,
        since: Timestamp,
    ) -> Result<i64> {
        // Clamped on the high side only. Every amount is positive -- the schema's check
        // constraint says so -- so there is no lower bound to defend, and the answer is
        // about to be compared against a cap: a total that wrapped would come back
        // small and hand an abuser the very allowance this read exists to deny them.
        let clamped = Func::least([
            Expr::from(Func::sum(Expr::col(
                entity::xp_award::Column::Amount.as_column_ref(),
            )))
            .if_null(0_i64),
            Expr::val(i64::MAX),
        ]);
        let mut query = entity::xp_award::Entity::find()
            .select_only()
            .expr_as(Expr::from(clamped).cast_as(Alias::new("bigint")), "earned")
            .filter(entity::xp_award::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::xp_award::Column::CreatedAt.gte(stamp_of(since)));
        if let Some(source) = source {
            query = query.filter(entity::xp_award::Column::Source.eq(source));
        }
        Ok(query
            .into_tuple::<i64>()
            .one(&self.db)
            .await
            .context("xp_earned_since")?
            .unwrap_or(0))
    }

    async fn leaderboard(
        &self,
        scope: Scope<'_>,
        since: Option<Timestamp>,
        limit: u16,
    ) -> Result<Vec<Standing>> {
        // A subquery per scope rather than a join, and the same one in both branches.
        // `progression` has no relation to `room_member` and inventing one would suggest
        // a room owns XP; it does not, and this ranks a room's current members by what
        // they have earned anywhere. The country arm compares the stored value directly
        // so `account_country_idx` can serve it -- `upper(country)` would turn a top-ten
        // into a scan of every account ever registered -- which is safe because the
        // column is normalised on write and the schema refuses anything else.
        let eligible = match scope {
            Scope::Global => None,
            Scope::Country(code) => {
                let wanted = canonical_country(Some(code))?
                    .ok_or_else(|| fault::validation("country", "must be two ASCII letters"))?;
                Some(
                    Query::select()
                        .column(entity::account::Column::AccountId)
                        .from(entity::account::Entity)
                        .and_where(Expr::col(entity::account::Column::Country).eq(wanted))
                        .to_owned(),
                )
            }
            Scope::Room(room_id) => Some(
                Query::select()
                    .column(entity::room_member::Column::AccountId)
                    .from(entity::room_member::Entity)
                    .and_where(Expr::col(entity::room_member::Column::RoomId).eq(uuid_of(room_id)))
                    .and_where(Expr::col(entity::room_member::Column::LeftAt).is_null())
                    .to_owned(),
            ),
        };

        let Some(since) = since else {
            let mut query = leaderboard(limit);
            if let Some(eligible) = eligible {
                query = query.filter(entity::progression::Column::AccountId.in_subquery(eligible));
            }
            return Ok(query
                .into_tuple()
                .all(&self.db)
                .await
                .context("leaderboard")?
                .into_iter()
                .map(standing_of)
                .collect());
        };

        // Two bounded queries rather than one join, for the reason `ledger_history`
        // gives: the page is at most `MAX_PAGE` rows, and reaching `progression` from
        // `xp_award` means hopping through `account`, which is a join written to fetch
        // one integer.
        let clamped = || {
            Func::least([
                Expr::from(Func::sum(Expr::col(
                    entity::xp_award::Column::Amount.as_column_ref(),
                )))
                .if_null(0_i64),
                Expr::val(i64::MAX),
            ])
        };
        let mut window = entity::xp_award::Entity::find()
            .select_only()
            .column(entity::xp_award::Column::AccountId)
            .expr_as(
                Expr::from(clamped()).cast_as(Alias::new("bigint")),
                "earned",
            )
            .filter(entity::xp_award::Column::CreatedAt.gte(stamp_of(since)))
            .group_by(entity::xp_award::Column::AccountId)
            .order_by_desc(Expr::from(clamped()))
            .order_by_asc(entity::xp_award::Column::AccountId)
            .limit(clamp_limit(limit) as u64);
        if let Some(eligible) = eligible {
            window = window.filter(entity::xp_award::Column::AccountId.in_subquery(eligible));
        }
        let ranked: Vec<(Uuid, i64)> = window
            .into_tuple()
            .all(&self.db)
            .await
            .context("leaderboard: window")?;
        if ranked.is_empty() {
            return Ok(Vec::new());
        }

        // `level` is the account's level now, not the level it held when the window
        // opened. There is no such thing as the level somebody held last Tuesday, and a
        // weekly board that pretended otherwise would disagree with their own profile.
        let levels: HashMap<Uuid, i32> = entity::progression::Entity::find()
            .select_only()
            .column(entity::progression::Column::AccountId)
            .column(entity::progression::Column::Level)
            .filter(
                entity::progression::Column::AccountId
                    .is_in(ranked.iter().map(|(account_id, _)| *account_id)),
            )
            .into_tuple::<(Uuid, i32)>()
            .all(&self.db)
            .await
            .context("leaderboard: levels")?
            .into_iter()
            .collect();

        Ok(ranked
            .into_iter()
            .map(|(account_id, xp)| {
                // Defaulting to one rather than skipping the row: `award_xp` writes both
                // rows in one transaction, so a missing level cannot happen, and if it
                // somehow did, dropping somebody out of a ranking they earned is the
                // worse of the two failures.
                standing_of((
                    account_id,
                    xp,
                    levels.get(&account_id).copied().unwrap_or(1),
                ))
            })
            .collect())
    }
}

#[async_trait]
impl GameStore for PostgresStore {
    async fn create_game(&self, new: NewGame) -> Result<GameSession> {
        let row = entity::game_session::Entity::insert(entity::game_session::ActiveModel {
            game_id: Set(uuid_of(new.game_id)),
            kind: Set(new.kind),
            conversation_id: Set(uuid_of(new.conversation_id)),
            state: Set(new.state),
            turn_of: Set(new.turn_of.map(uuid_of)),
            status: Set(game_status::OPEN),
            stake_currency: Set(new.stake_currency),
            stake_amount: Set(new.stake_amount),
            created_at: Set(stamp_of(new.at)),
            updated_at: Set(stamp_of(new.at)),
            finished_at: Set(None),
        })
        .exec_with_returning(&self.db)
        .await
        .map_err(|error| {
            on_conflict(error, "game", |name| match name {
                "game_session_pkey" => Some(fault::already_exists("game")),
                "game_session_conversation_id_fkey" => Some(fault::not_found("conversation")),
                "game_session_turn_of_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?;
        Ok(game_session_of(row))
    }

    async fn game(&self, game_id: Id) -> Result<Option<GameSession>> {
        Ok(entity::game_session::Entity::find_by_id(uuid_of(game_id))
            .one(&self.db)
            .await
            .context("game")?
            .map(game_session_of))
    }

    async fn active_games(&self, conversation_id: Id, limit: u16) -> Result<Vec<GameSession>> {
        let rows = entity::game_session::Entity::find()
            .filter(entity::game_session::Column::ConversationId.eq(uuid_of(conversation_id)))
            .filter(entity::game_session::Column::Status.eq(game_status::OPEN))
            .order_by_desc(entity::game_session::Column::CreatedAt)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("active_games")?;
        Ok(rows.into_iter().map(game_session_of).collect())
    }

    async fn advance_game(&self, advance: AdvanceGame) -> Result<Option<GameSession>> {
        let finished_at = if advance.status == game_status::OPEN {
            None
        } else {
            Some(stamp_of(advance.at))
        };
        // The compare-and-swap: this update matches a row only while it is still open
        // and still carries the token the move was computed from. A superseded or
        // replayed move matches nothing and writes nothing — section 90 enforced at
        // the storage layer. `finished_at` is a function of the target status, set in
        // the same write so a finishing move and its terminal timestamp cannot part.
        let result = entity::game_session::Entity::update_many()
            .filter(entity::game_session::Column::GameId.eq(uuid_of(advance.game_id)))
            .filter(
                entity::game_session::Column::UpdatedAt.eq(stamp_of(advance.expected_updated_at)),
            )
            .filter(entity::game_session::Column::Status.eq(game_status::OPEN))
            .set(entity::game_session::ActiveModel {
                state: Set(advance.state),
                turn_of: Set(advance.turn_of.map(uuid_of)),
                status: Set(advance.status),
                updated_at: Set(stamp_of(advance.at)),
                finished_at: Set(finished_at),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("advance_game")?;
        if result.rows_affected == 0 {
            return Ok(None);
        }
        // Read the row back so the caller sees exactly what landed. Safe against a
        // racing writer: any concurrent move would have had to name the token this
        // update just replaced, so none can have committed between the write and this
        // read.
        self.game(advance.game_id).await
    }

    async fn abandon_game(&self, game_id: Id, at: Timestamp) -> Result<Option<GameSession>> {
        let result = entity::game_session::Entity::update_many()
            .filter(entity::game_session::Column::GameId.eq(uuid_of(game_id)))
            .filter(entity::game_session::Column::Status.eq(game_status::OPEN))
            .set(entity::game_session::ActiveModel {
                status: Set(game_status::ABANDONED),
                turn_of: Set(None),
                updated_at: Set(stamp_of(at)),
                finished_at: Set(Some(stamp_of(at))),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("abandon_game")?;
        if result.rows_affected == 0 {
            return Ok(None);
        }
        self.game(game_id).await
    }
}

#[async_trait]
impl BotStore for PostgresStore {
    async fn register_bot(&self, new: NewBot) -> Result<Bot> {
        // The backing account, its profile, and the bot row are one unit: a bot
        // account with no bot row can never be signed into and nothing knows how to
        // read it, and a bot row with no account has nothing to post under. So all
        // three go in one transaction, the same reasoning `create_room` uses for a
        // room, its conversation, and its owner's membership.
        let transaction = self.begin("register_bot").await?;
        let account_id = uuid_of(new.account_id);
        let created_at = stamp_of(new.created_at);

        entity::account::Entity::insert(entity::account::ActiveModel {
            account_id: Set(account_id),
            username_lower: Set(fold(&new.username)),
            username: Set(new.username),
            // A bot has no email or phone: it authenticates by bearer token, never by
            // a credential a human would recover.
            email_lower: Set(None),
            email: Set(None),
            phone: Set(None),
            password_hash: Set(new.password_hash.expose().to_owned()),
            status: Set(AccountStatus::Active.to_i16()),
            country: Set(canonical_country(None)?),
            locale: Set(new.locale),
            created_at: Set(created_at),
            updated_at: Set(created_at),
            ..Default::default()
        })
        .exec_without_returning(&transaction)
        .await
        .map_err(|error| {
            on_conflict(error, "bot account", |name| match name {
                "account_username_lower_key" => Some(fault::already_exists("username")),
                "account_pkey" => Some(fault::already_exists("account id")),
                _ => None,
            })
        })?;

        // The profile, private by default, exactly as a human registration builds it,
        // so the memory and Postgres backends agree on a new bot's visibility.
        entity::profile::Entity::insert(entity::profile::ActiveModel {
            account_id: Set(account_id),
            display_name: Set(new.display_name.clone()),
            bio: Set(None),
            avatar_media_id: Set(None),
            birth_year: Set(None),
            show_last_seen: Set(Visibility::Friends.to_i16()),
            who_can_message: Set(Visibility::Friends.to_i16()),
            who_can_add: Set(Visibility::Everyone.to_i16()),
            searchable: Set(true),
            updated_at: Set(created_at),
        })
        .exec_without_returning(&transaction)
        .await
        .map_err(|error| {
            on_conflict(error, "bot profile", |name| match name {
                "profile_pkey" => Some(fault::already_exists("profile")),
                "profile_account_id_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?;

        let bot = entity::bot::Entity::insert(entity::bot::ActiveModel {
            bot_id: Set(uuid_of(new.bot_id)),
            owner_id: Set(uuid_of(new.owner_id)),
            account_id: Set(account_id),
            name: Set(new.display_name),
            token_hash: Set(new.token_hash),
            scopes: Set(new.scopes),
            webhook_url: Set(new.webhook_url),
            created_at: Set(created_at),
            disabled_at: Set(None),
        })
        .exec_with_returning(&transaction)
        .await
        .map_err(|error| {
            on_conflict(error, "bot", |name| match name {
                "bot_pkey" => Some(fault::already_exists("bot")),
                "bot_account_id_key" => Some(fault::already_exists("bot account")),
                "bot_token_hash_key" => Some(fault::already_exists("bot token")),
                "bot_owner_id_fkey" | "bot_account_id_fkey" => Some(fault::not_found("account")),
                _ => None,
            })
        })?;

        transaction.commit().await.context("register_bot: commit")?;
        Ok(bot_of(bot))
    }

    async fn bot(&self, bot_id: Id) -> Result<Option<Bot>> {
        Ok(entity::bot::Entity::find_by_id(uuid_of(bot_id))
            .one(&self.db)
            .await
            .context("bot")?
            .map(bot_of))
    }

    async fn bot_by_account(&self, account_id: Id) -> Result<Option<Bot>> {
        Ok(entity::bot::Entity::find()
            .filter(entity::bot::Column::AccountId.eq(uuid_of(account_id)))
            .one(&self.db)
            .await
            .context("bot_by_account")?
            .map(bot_of))
    }

    async fn bot_by_token_hash(&self, token_hash: &[u8]) -> Result<Option<Bot>> {
        Ok(entity::bot::Entity::find()
            .filter(entity::bot::Column::TokenHash.eq(token_hash.to_vec()))
            .one(&self.db)
            .await
            .context("bot_by_token_hash")?
            .map(bot_of))
    }

    async fn bots_for_owner(&self, owner_id: Id, limit: u16) -> Result<Vec<Bot>> {
        let rows = entity::bot::Entity::find()
            .filter(entity::bot::Column::OwnerId.eq(uuid_of(owner_id)))
            .order_by_desc(entity::bot::Column::CreatedAt)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("bots_for_owner")?;
        Ok(rows.into_iter().map(bot_of).collect())
    }

    async fn set_bot_scopes(&self, bot_id: Id, scopes: i64) -> Result<Option<Bot>> {
        let result = entity::bot::Entity::update_many()
            .filter(entity::bot::Column::BotId.eq(uuid_of(bot_id)))
            .set(entity::bot::ActiveModel {
                scopes: Set(scopes),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("set_bot_scopes")?;
        if result.rows_affected == 0 {
            return Ok(None);
        }
        self.bot(bot_id).await
    }

    async fn set_bot_token_hash(&self, bot_id: Id, token_hash: Vec<u8>) -> Result<Option<Bot>> {
        // A rotation that collides with another bot's tag must fail the whole write,
        // the way `bot_token_hash_key` rejects it — never silently overwrite so two
        // bots share a token. The memory backend guards the same index by hand.
        let result = entity::bot::Entity::update_many()
            .filter(entity::bot::Column::BotId.eq(uuid_of(bot_id)))
            .set(entity::bot::ActiveModel {
                token_hash: Set(token_hash),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .map_err(|error| {
                on_conflict(error, "set_bot_token_hash", |name| match name {
                    "bot_token_hash_key" => Some(fault::already_exists("bot token")),
                    _ => None,
                })
            })?;
        if result.rows_affected == 0 {
            return Ok(None);
        }
        self.bot(bot_id).await
    }

    async fn set_bot_disabled(
        &self,
        bot_id: Id,
        disabled_at: Option<Timestamp>,
    ) -> Result<Option<Bot>> {
        let result = entity::bot::Entity::update_many()
            .filter(entity::bot::Column::BotId.eq(uuid_of(bot_id)))
            .set(entity::bot::ActiveModel {
                disabled_at: Set(disabled_at.map(stamp_of)),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("set_bot_disabled")?;
        if result.rows_affected == 0 {
            return Ok(None);
        }
        self.bot(bot_id).await
    }
}

#[async_trait]
impl FederationStore for PostgresStore {
    async fn add_peer(&self, new: NewPeer) -> Result<PeerRecord> {
        // Both unique constraints named, so the caller learns which identity clashed:
        // the primary key is the node id, and `node_peer_key_key` guards the public
        // key — the key, not the id, is what a handshake is checked against, so a
        // second peer must not quietly claim one already in use.
        let row = entity::node_peer::Entity::insert(entity::node_peer::ActiveModel {
            node_id: Set(new.node_id),
            public_key: Set(new.public_key),
            base_url: Set(new.base_url),
            region: Set(new.region),
            status: Set(new.status),
            added_at: Set(stamp_of(new.added_at)),
            last_seen_at: Set(None),
        })
        .exec_with_returning(&self.db)
        .await
        .map_err(|error| {
            on_conflict(error, "node peer", |name| match name {
                "node_peer_pkey" => Some(fault::already_exists("node peer")),
                "node_peer_key_key" => Some(fault::already_exists("node key")),
                _ => None,
            })
        })?;
        Ok(peer_of(row))
    }

    async fn peer(&self, node_id: &str) -> Result<Option<PeerRecord>> {
        Ok(entity::node_peer::Entity::find_by_id(node_id.to_owned())
            .one(&self.db)
            .await
            .context("peer")?
            .map(peer_of))
    }

    async fn peers(&self, limit: u16) -> Result<Vec<PeerRecord>> {
        let rows = entity::node_peer::Entity::find()
            .order_by_desc(entity::node_peer::Column::AddedAt)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("peers")?;
        Ok(rows.into_iter().map(peer_of).collect())
    }

    async fn set_peer_status(&self, node_id: &str, status: i16) -> Result<Option<PeerRecord>> {
        let result = entity::node_peer::Entity::update_many()
            .filter(entity::node_peer::Column::NodeId.eq(node_id))
            .set(entity::node_peer::ActiveModel {
                status: Set(status),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("set_peer_status")?;
        if result.rows_affected == 0 {
            return Ok(None);
        }
        self.peer(node_id).await
    }

    async fn touch_peer(&self, node_id: &str, seen_at: Timestamp) -> Result<Option<PeerRecord>> {
        let result = entity::node_peer::Entity::update_many()
            .filter(entity::node_peer::Column::NodeId.eq(node_id))
            .set(entity::node_peer::ActiveModel {
                last_seen_at: Set(Some(stamp_of(seen_at))),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("touch_peer")?;
        if result.rows_affected == 0 {
            return Ok(None);
        }
        self.peer(node_id).await
    }

    async fn enqueue_event(&self, new: NewOutboxEvent) -> Result<OutboxRecord> {
        let row =
            entity::federation_outbox::Entity::insert(entity::federation_outbox::ActiveModel {
                event_id: Set(uuid_of(new.event_id)),
                target_node: Set(new.target_node),
                opcode: Set(new.opcode),
                payload: Set(new.payload),
                attempts: Set(0),
                created_at: Set(stamp_of(new.created_at)),
                next_attempt_at: Set(stamp_of(new.next_attempt_at)),
                delivered_at: Set(None),
                last_error: Set(None),
            })
            .exec_with_returning(&self.db)
            .await
            .map_err(|error| {
                on_conflict(error, "federation event", |name| match name {
                    "federation_outbox_pkey" => Some(fault::already_exists("federation event")),
                    _ => None,
                })
            })?;
        Ok(outbox_of(row))
    }

    async fn due_events(&self, now: Timestamp, limit: u16) -> Result<Vec<OutboxRecord>> {
        // A plain read, matching the trait contract: no lock and no mutation, so this
        // backend and the in-memory one return the same set. `next_attempt_at`
        // ascending is the due order, and the partial index `federation_outbox_due_idx`
        // (`where delivered_at is null`) is the one this scan uses.
        let rows = entity::federation_outbox::Entity::find()
            .filter(entity::federation_outbox::Column::DeliveredAt.is_null())
            .filter(entity::federation_outbox::Column::NextAttemptAt.lte(stamp_of(now)))
            .order_by_asc(entity::federation_outbox::Column::NextAttemptAt)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("due_events")?;
        Ok(rows.into_iter().map(outbox_of).collect())
    }

    async fn mark_delivered(
        &self,
        event_id: Id,
        delivered_at: Timestamp,
    ) -> Result<Option<OutboxRecord>> {
        // `delivered_at is null` in the predicate makes this idempotent: the first
        // delivery writes the timestamp, a retry that races it updates nothing and the
        // original instant stands. The row is read back afterwards either way, so an
        // already-delivered event still returns `Some` and only an unknown id is `None`.
        entity::federation_outbox::Entity::update_many()
            .filter(entity::federation_outbox::Column::EventId.eq(uuid_of(event_id)))
            .filter(entity::federation_outbox::Column::DeliveredAt.is_null())
            .set(entity::federation_outbox::ActiveModel {
                delivered_at: Set(Some(stamp_of(delivered_at))),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("mark_delivered")?;
        Ok(
            entity::federation_outbox::Entity::find_by_id(uuid_of(event_id))
                .one(&self.db)
                .await
                .context("mark_delivered: read back")?
                .map(outbox_of),
        )
    }

    async fn mark_failed(
        &self,
        event_id: Id,
        next_attempt_at: Timestamp,
        error: &str,
    ) -> Result<Option<OutboxRecord>> {
        // `attempts + 1` as a column expression rather than a value this process read a
        // moment ago, so the count stays correct even if two failures race: each
        // increment is computed from the row's own current value under the write lock.
        let result = entity::federation_outbox::Entity::update_many()
            .filter(entity::federation_outbox::Column::EventId.eq(uuid_of(event_id)))
            .col_expr(
                entity::federation_outbox::Column::Attempts,
                Expr::col(entity::federation_outbox::Column::Attempts.as_column_ref()).add(1),
            )
            .col_expr(
                entity::federation_outbox::Column::NextAttemptAt,
                Expr::val(stamp_of(next_attempt_at)),
            )
            .col_expr(
                entity::federation_outbox::Column::LastError,
                Expr::val(error.to_owned()),
            )
            .exec(&self.db)
            .await
            .context("mark_failed")?;
        if result.rows_affected == 0 {
            return Ok(None);
        }
        Ok(
            entity::federation_outbox::Entity::find_by_id(uuid_of(event_id))
                .one(&self.db)
                .await
                .context("mark_failed: read back")?
                .map(outbox_of),
        )
    }
}

#[async_trait]
impl MediaStore for PostgresStore {
    async fn create_media(&self, media: MediaObject) -> Result<MediaObject> {
        if media.byte_size <= 0 {
            return Err(fault::validation("byte_size", "must be positive"));
        }
        let row = entity::media_object::Entity::insert(entity::media_object::ActiveModel {
            media_id: Set(uuid_of(media.media_id)),
            owner_id: Set(uuid_of(media.owner_id)),
            kind: Set(media.kind),
            mime: Set(media.mime),
            byte_size: Set(media.byte_size),
            width: Set(media.width),
            height: Set(media.height),
            duration_ms: Set(media.duration_ms),
            storage_key: Set(media.storage_key),
            conversation_id: Set(media.conversation_id.map(uuid_of)),
            checksum: Set(media.checksum),
            scan_status: Set(media.scan_status),
            created_at: Set(stamp_of(media.created_at)),
            deleted_at: Set(media.deleted_at.map(stamp_of)),
        })
        .exec_with_returning(&self.db)
        .await
        .map_err(|error| {
            on_conflict(error, "media object", |name| match name {
                "media_object_pkey" => Some(fault::already_exists("media object")),
                "media_object_owner_id_fkey" => Some(fault::not_found("account")),
                "media_object_conversation_id_fkey" => Some(fault::not_found("conversation")),
                _ => None,
            })
        })?;
        Ok(row.into())
    }

    async fn media(&self, media_id: Id) -> Result<Option<MediaObject>> {
        Ok(entity::media_object::Entity::find_by_id(uuid_of(media_id))
            .one(&self.db)
            .await
            .context("media")?
            .map(Into::into))
    }

    async fn set_media_scan_status(&self, media_id: Id, status: i16, _at: Timestamp) -> Result<()> {
        let result = entity::media_object::Entity::update_many()
            .filter(entity::media_object::Column::MediaId.eq(uuid_of(media_id)))
            .set(entity::media_object::ActiveModel {
                scan_status: Set(status),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("set_media_scan_status")?;
        if result.rows_affected == 0 {
            return Err(fault::not_found("media object"));
        }
        Ok(())
    }

    async fn delete_media(&self, media_id: Id, at: Timestamp) -> Result<()> {
        // A tombstone, not a delete: the sweeper needs the storage key to remove the
        // bytes, and a row deleted before its object would leave the object orphaned
        // in the bucket forever. `coalesce` keeps the first deletion time, so a
        // retried delete does not move the sweeper's deadline.
        entity::media_object::Entity::update_many()
            .filter(entity::media_object::Column::MediaId.eq(uuid_of(media_id)))
            .col_expr(
                entity::media_object::Column::DeletedAt,
                entity::media_object::Column::DeletedAt.if_null(stamp_of(at)),
            )
            .exec(&self.db)
            .await
            .context("delete_media")?;
        Ok(())
    }
}

/// The push columns of `device`, and nothing else.
///
/// The mirror image of [`DeviceRow`]: that one names every column except the two
/// credential ones, this one names only those. Two partial models over one table, so
/// that a query which needs the token cannot accidentally hand back a device, and a
/// query which needs a device cannot accidentally hand back a token.
#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "entity::device::Entity")]
struct PushRow {
    /// Which device holds the registration.
    device_id: Uuid,
    /// Platform, which decides the payload shape.
    platform: i16,
    /// The sealed token. Opaque here; no key for it exists in this process.
    push_token: Option<String>,
    /// The lookup handle.
    push_token_hash: Option<String>,
    /// Which push service.
    push_provider: Option<i16>,
    /// Last refresh, for the staleness sweep.
    push_updated_at: Option<OffsetDateTime>,
}

impl PushRow {
    /// Turns a row into a target, or into nothing.
    ///
    /// Four nullable columns describing one credential means fifteen ways to be
    /// half-registered, and `Option` chaining here is what keeps every one of them
    /// out of the send path. A row missing any part of its registration is not an
    /// error — it is a device that never enabled notifications.
    fn into_target(self) -> Option<PushTarget> {
        Some(PushTarget {
            device_id: id_of(self.device_id),
            platform: Platform::from_wire(wire_u32(self.platform)),
            registration: PushRegistration {
                sealed: self.push_token?,
                hash: self.push_token_hash?,
                provider: self.push_provider?,
            },
            updated_at: instant_of(self.push_updated_at?),
        })
    }
}

impl From<entity::notification::Model> for Notification {
    fn from(row: entity::notification::Model) -> Self {
        Self {
            notification_id: id_of(row.notification_id),
            account_id: id_of(row.account_id),
            kind: row.kind,
            room_id: row.room_id.map(id_of),
            actor_id: row.actor_id.map(id_of),
            subject_id: row.subject_id.map(id_of),
            created_at: instant_of(row.created_at),
            read_at: row.read_at.map(instant_of),
        }
    }
}

#[async_trait]
impl NotifyStore for PostgresStore {
    async fn create_notification(&self, notification: Notification) -> Result<Notification> {
        if !notification_kind::is_storable(notification.kind) {
            return Err(fault::validation("kind", "is not a storable notification"));
        }
        let row = entity::notification::Entity::insert(entity::notification::ActiveModel {
            notification_id: Set(uuid_of(notification.notification_id)),
            account_id: Set(uuid_of(notification.account_id)),
            kind: Set(notification.kind),
            room_id: Set(notification.room_id.map(uuid_of)),
            actor_id: Set(notification.actor_id.map(uuid_of)),
            subject_id: Set(notification.subject_id.map(uuid_of)),
            created_at: Set(stamp_of(notification.created_at)),
            read_at: Set(notification.read_at.map(stamp_of)),
        })
        .exec_with_returning(&self.db)
        .await
        .map_err(|error| {
            on_conflict(error, "notification", |name| match name {
                "notification_pkey" => Some(fault::already_exists("notification")),
                "notification_account_id_fkey" | "notification_actor_id_fkey" => {
                    Some(fault::not_found("account"))
                }
                "notification_room_id_fkey" => Some(fault::not_found("room")),
                _ => None,
            })
        })?;
        Ok(row.into())
    }

    async fn notifications(&self, account_id: Id, limit: u16) -> Result<Vec<Notification>> {
        // Newest first, matching `notification_inbox_idx`. The tie-break on the id is
        // not decoration: two notifications can share a millisecond, and a page
        // boundary that falls between them would otherwise show one of them twice or
        // neither.
        Ok(entity::notification::Entity::find()
            .filter(entity::notification::Column::AccountId.eq(uuid_of(account_id)))
            .order_by_desc(entity::notification::Column::CreatedAt)
            .order_by_desc(entity::notification::Column::NotificationId)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("notifications")?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn unread_notifications(&self, account_id: Id) -> Result<u32> {
        let count = entity::notification::Entity::find()
            .filter(entity::notification::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::notification::Column::ReadAt.is_null())
            .count(&self.db)
            .await
            .context("unread_notifications")?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    async fn mark_notifications_read(
        &self,
        account_id: Id,
        through: Timestamp,
        at: Timestamp,
    ) -> Result<u32> {
        // `is null` in the predicate rather than `coalesce` in the assignment: a
        // notification's read time is when it was first seen, and a second call from
        // a second device must not move it forward. It also keeps the update off every
        // row that was already read, which on an old account is most of them.
        let result = entity::notification::Entity::update_many()
            .filter(entity::notification::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::notification::Column::ReadAt.is_null())
            .filter(entity::notification::Column::CreatedAt.lte(stamp_of(through)))
            .set(entity::notification::ActiveModel {
                read_at: Set(Some(stamp_of(at))),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("mark_notifications_read")?;
        Ok(u32::try_from(result.rows_affected).unwrap_or(u32::MAX))
    }

    async fn purge_notifications(&self, before: Timestamp, limit: u16) -> Result<u64> {
        // A subquery with a limit, because SeaORM's `delete_many` has no `limit` of
        // its own and PostgreSQL has no `delete ... limit`. The point of the bound is
        // that a sweep which has been broken for a month must not take a lock
        // proportional to the month.
        let doomed = entity::notification::Entity::find()
            .select_only()
            .column(entity::notification::Column::NotificationId)
            .filter(entity::notification::Column::ReadAt.is_not_null())
            .filter(entity::notification::Column::CreatedAt.lt(stamp_of(before)))
            .order_by_asc(entity::notification::Column::CreatedAt)
            .limit(clamp_limit(limit) as u64)
            .into_tuple::<Uuid>()
            .all(&self.db)
            .await
            .context("purge_notifications: select")?;
        if doomed.is_empty() {
            return Ok(0);
        }
        let result = entity::notification::Entity::delete_many()
            .filter(entity::notification::Column::NotificationId.is_in(doomed))
            .exec(&self.db)
            .await
            .context("purge_notifications: delete")?;
        Ok(result.rows_affected)
    }

    async fn set_push_registration(
        &self,
        device_id: Id,
        registration: PushRegistration,
        at: Timestamp,
    ) -> Result<()> {
        // One transaction for two statements, because between them the hash belongs to
        // nobody. Do it without and a concurrent `push_targets` for the account that
        // is losing the token can read a row that has already been cleared while the
        // new one is not yet written — or, worse, the clear succeeds and the set fails
        // and a phone stops receiving anything with no error anywhere.
        let transaction = self.begin("set_push_registration").await?;
        entity::device::Entity::update_many()
            .filter(entity::device::Column::PushTokenHash.eq(registration.hash.clone()))
            .filter(entity::device::Column::DeviceId.ne(uuid_of(device_id)))
            .set(entity::device::ActiveModel {
                push_token: Set(None),
                push_token_hash: Set(None),
                push_provider: Set(None),
                push_updated_at: Set(None),
                ..Default::default()
            })
            .exec(&transaction)
            .await
            .context("set_push_registration: displace")?;
        let result = entity::device::Entity::update_many()
            .filter(entity::device::Column::DeviceId.eq(uuid_of(device_id)))
            .filter(entity::device::Column::RevokedAt.is_null())
            .set(entity::device::ActiveModel {
                push_token: Set(Some(registration.sealed)),
                push_token_hash: Set(Some(registration.hash)),
                push_provider: Set(Some(registration.provider)),
                push_updated_at: Set(Some(stamp_of(at))),
                ..Default::default()
            })
            .exec(&transaction)
            .await
            .context("set_push_registration: write")?;
        if result.rows_affected == 0 {
            // No rollback call: dropping the transaction rolls it back, so the
            // displacement above is undone and the losing device keeps its token.
            // Which is right — nothing took it from them.
            return Err(fault::not_found("device"));
        }
        transaction
            .commit()
            .await
            .context("set_push_registration: commit")?;
        Ok(())
    }

    async fn clear_push_registration(&self, device_id: Id) -> Result<()> {
        entity::device::Entity::update_many()
            .filter(entity::device::Column::DeviceId.eq(uuid_of(device_id)))
            .set(entity::device::ActiveModel {
                push_token: Set(None),
                push_token_hash: Set(None),
                push_provider: Set(None),
                push_updated_at: Set(None),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("clear_push_registration")?;
        Ok(())
    }

    async fn retire_push_hash(&self, hash: &str) -> Result<bool> {
        let result = entity::device::Entity::update_many()
            .filter(entity::device::Column::PushTokenHash.eq(hash))
            .set(entity::device::ActiveModel {
                push_token: Set(None),
                push_token_hash: Set(None),
                push_provider: Set(None),
                push_updated_at: Set(None),
                ..Default::default()
            })
            .exec(&self.db)
            .await
            .context("retire_push_hash")?;
        Ok(result.rows_affected > 0)
    }

    async fn push_targets(&self, account_id: Id) -> Result<Vec<PushTarget>> {
        Ok(entity::device::Entity::find()
            .filter(entity::device::Column::AccountId.eq(uuid_of(account_id)))
            .filter(entity::device::Column::RevokedAt.is_null())
            .filter(entity::device::Column::PushTokenHash.is_not_null())
            .order_by_asc(entity::device::Column::DeviceId)
            .into_partial_model::<PushRow>()
            .all(&self.db)
            .await
            .context("push_targets")?
            .into_iter()
            .filter_map(PushRow::into_target)
            .collect())
    }

    async fn stale_push_hashes(&self, before: Timestamp, limit: u16) -> Result<Vec<String>> {
        // One column, not the row. The sweeper retires registrations; it has no
        // business assembling a list of which accounts have stopped using the product,
        // and a query that never selects `account_id` cannot leak one.
        Ok(entity::device::Entity::find()
            .select_only()
            .column(entity::device::Column::PushTokenHash)
            .filter(entity::device::Column::PushTokenHash.is_not_null())
            .filter(entity::device::Column::PushUpdatedAt.lt(stamp_of(before)))
            .order_by_asc(entity::device::Column::PushUpdatedAt)
            .limit(clamp_limit(limit) as u64)
            .into_tuple::<String>()
            .all(&self.db)
            .await
            .context("stale_push_hashes")?)
    }
}

#[async_trait]
impl SafetyStore for PostgresStore {
    async fn create_report(&self, report: Report) -> Result<Report> {
        let row = entity::report::Entity::insert(entity::report::ActiveModel {
            report_id: Set(uuid_of(report.report_id)),
            reporter_id: Set(uuid_of(report.reporter_id)),
            subject_kind: Set(report.subject_kind),
            subject_id: Set(uuid_of(report.subject_id)),
            room_id: Set(report.room_id.map(uuid_of)),
            reason: Set(report.reason),
            note: Set(report.note),
            evidence_ref: Set(report.evidence_ref.map(uuid_of)),
            status: Set(report.status),
            created_at: Set(stamp_of(report.created_at)),
            resolved_at: Set(report.resolved_at.map(stamp_of)),
            resolved_by: Set(report.resolved_by.map(uuid_of)),
            resolution: Set(report.resolution),
        })
        .exec_with_returning(&self.db)
        .await
        .map_err(|error| {
            on_conflict(error, "report", |name| match name {
                "report_pkey" => Some(fault::already_exists("report")),
                "report_reporter_id_fkey" | "report_resolved_by_fkey" => {
                    Some(fault::not_found("account"))
                }
                "report_room_id_fkey" => Some(fault::not_found("room")),
                _ => None,
            })
        })?;
        Ok(row.into())
    }

    async fn report(&self, report_id: Id) -> Result<Option<Report>> {
        Ok(entity::report::Entity::find_by_id(uuid_of(report_id))
            .one(&self.db)
            .await
            .context("report")?
            .map(Into::into))
    }

    async fn open_reports(&self, limit: u16) -> Result<Vec<Report>> {
        // Oldest first, and served by the partial index on open reports only. A queue
        // ordered newest-first starves the reports that have waited longest, which
        // are the ones most likely to matter.
        Ok(entity::report::Entity::find()
            .filter(entity::report::Column::Status.eq(crate::model::report_status::OPEN))
            .order_by_asc(entity::report::Column::CreatedAt)
            .order_by_asc(entity::report::Column::ReportId)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("open_reports")?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn open_report_by_reporter(
        &self,
        reporter_id: Id,
        subject_kind: i16,
        subject_id: Id,
    ) -> Result<Option<Report>> {
        Ok(entity::report::Entity::find()
            .filter(entity::report::Column::ReporterId.eq(uuid_of(reporter_id)))
            .filter(entity::report::Column::SubjectKind.eq(subject_kind))
            .filter(entity::report::Column::SubjectId.eq(uuid_of(subject_id)))
            .filter(entity::report::Column::Status.eq(crate::model::report_status::OPEN))
            .order_by_asc(entity::report::Column::CreatedAt)
            .order_by_asc(entity::report::Column::ReportId)
            .one(&self.db)
            .await
            .context("open_report_by_reporter")?
            .map(Into::into))
    }

    async fn count_reports_about(
        &self,
        subject_kind: i16,
        subject_id: Id,
        since: Timestamp,
    ) -> Result<u32> {
        let count = entity::report::Entity::find()
            .filter(entity::report::Column::SubjectKind.eq(subject_kind))
            .filter(entity::report::Column::SubjectId.eq(uuid_of(subject_id)))
            .filter(entity::report::Column::CreatedAt.gte(stamp_of(since)))
            .count(&self.db)
            .await
            .context("count_reports_about")?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    async fn resolve_report(
        &self,
        report_id: Id,
        status: i16,
        resolution: i16,
        by: Id,
        at: Timestamp,
    ) -> Result<()> {
        // Two moderators opening the same report is normal; both of them acting on it
        // is not. `for update` makes the loser wait and then be told, instead of
        // overwriting the first verdict with the second.
        let transaction = self.begin("resolve_report").await?;
        let current = entity::report::Entity::find_by_id(uuid_of(report_id))
            .select_only()
            .column(entity::report::Column::Status)
            .lock(LockType::Update)
            .into_tuple::<i16>()
            .one(&transaction)
            .await
            .context("resolve_report: lock report")?;
        match current {
            None => return Err(fault::not_found("report")),
            Some(status) if status != crate::model::report_status::OPEN => {
                return Err(fault::conflict("report already resolved"))
            }
            Some(_) => {}
        }

        entity::report::Entity::update_many()
            .filter(entity::report::Column::ReportId.eq(uuid_of(report_id)))
            .set(entity::report::ActiveModel {
                status: Set(status),
                resolution: Set(Some(resolution)),
                resolved_by: Set(Some(uuid_of(by))),
                resolved_at: Set(Some(stamp_of(at))),
                ..Default::default()
            })
            .exec(&transaction)
            .await
            .context("resolve_report: apply")?;

        transaction
            .commit()
            .await
            .context("resolve_report: commit")?;
        Ok(())
    }

    async fn append_audit(&self, entry: AuditEntry) -> Result<()> {
        entity::audit_entry::Entity::insert(entity::audit_entry::ActiveModel {
            audit_id: Set(uuid_of(entry.audit_id)),
            actor_id: Set(entry.actor_id.map(uuid_of)),
            actor_kind: Set(entry.actor_kind),
            action: Set(entry.action),
            target_kind: Set(entry.target_kind),
            target_id: Set(entry.target_id.map(uuid_of)),
            summary: Set(entry.summary),
            reason: Set(entry.reason),
            request_id: Set(entry.request_id),
            ip_class: Set(entry.ip_class),
            created_at: Set(stamp_of(entry.created_at)),
        })
        .exec_without_returning(&self.db)
        .await
        .map_err(|error| {
            on_conflict(error, "audit entry", |name| match name {
                "audit_entry_pkey" => Some(fault::already_exists("audit entry")),
                _ => None,
            })
        })?;
        Ok(())
    }

    async fn audit_for_target(
        &self,
        target_kind: i16,
        target_id: Id,
        limit: u16,
    ) -> Result<Vec<AuditEntry>> {
        // Kind and id together: the same uuid under a different kind is a different
        // thing, and matching on the id alone would leak one target's history into
        // another's. `audit_id` only breaks ties between entries stamped at the same
        // instant, so the order is total.
        Ok(entity::audit_entry::Entity::find()
            .filter(entity::audit_entry::Column::TargetKind.eq(target_kind))
            .filter(entity::audit_entry::Column::TargetId.eq(uuid_of(target_id)))
            .order_by_desc(entity::audit_entry::Column::CreatedAt)
            .order_by_desc(entity::audit_entry::Column::AuditId)
            .limit(clamp_limit(limit) as u64)
            .all(&self.db)
            .await
            .context("audit_for_target")?
            .into_iter()
            .map(Into::into)
            .collect())
    }
}

#[async_trait]
impl Store for PostgresStore {
    fn backend_name(&self) -> &'static str {
        "postgres"
    }

    async fn migrate(&self) -> Result<()> {
        // The migrations are compiled into the binary, so a deployment cannot ship a
        // server without the schema it expects.
        //
        // Two things are added here that `Migrator::up` does not do on its own, and
        // both exist because several replicas start at once.
        //
        // The advisory lock is the first. sqlx's migrator took one; SeaORM's does
        // not, so without this line two replicas can both read an empty
        // `seaql_migrations` and both run `create table`. It is
        // `pg_advisory_xact_lock` rather than the session-scoped form because a
        // transaction-scoped lock is released by the commit or the rollback — there
        // is no path, including a panic, that leaves it held.
        //
        // The outer transaction is the second, and it is a genuine improvement on
        // what came before: the whole migration set commits or none of it does,
        // instead of a failure in the fourth file leaving three applied. SeaORM's
        // migrator opens its own transaction per migration; nested inside this one
        // those become savepoints, which is exactly the semantics wanted.
        let transaction = self.begin("migrate").await?;
        transaction
            .execute_raw(sql(
                "select pg_advisory_xact_lock($1)",
                [MIGRATION_LOCK_KEY.into()],
            ))
            .await
            .context("migrate: lock")?;
        Migrator::up(&transaction, None)
            .await
            .map_err(|error| fault::storage(format!("migrate: {error}")))?;
        transaction.commit().await.context("migrate: commit")
    }

    async fn health(&self) -> Result<()> {
        // Deliberately not a count of anything. A health check whose cost grows with
        // the data takes the service out of rotation exactly when it is busiest.
        // `ping` is the driver's own minimal round trip.
        self.db.ping().await.context("health")
    }
}
