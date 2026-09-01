//! The storage contract.
//!
//! Split by domain rather than expressed as one giant trait, because the split is
//! what lets a domain crate declare exactly what it touches: `migo-auth` takes
//! `Arc<dyn AccountStore>` and cannot reach the ledger even by accident. The
//! [`Store`] supertrait exists only for the composition root, which does need all
//! of it in one object.
//!
//! Every method takes explicit timestamps and caller-generated ids rather than
//! reading a clock or minting a uuid inside the backend. That is not ceremony: it
//! is what makes the deterministic simulator (ADR-0009) able to replay a run
//! exactly, and it means a caller can reference an id before the insert lands.
//!
//! # Conventions
//!
//! * A lookup that finds nothing returns `Ok(None)`. Absence is not an error; the
//!   caller decides whether it is.
//! * A write that is a no-op returns `Ok(())`, not an error. Idempotence is a
//!   feature of a system that retries.
//! * A write whose retry must not duplicate an effect returns an outcome type
//!   ([`Appended`], [`Posted`]) that says whether this call did the writing.
//! * Paged reads take an explicit limit and the backend clamps it. A caller that
//!   asks for a million rows gets the clamp, not an out-of-memory kill.

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::{fault, MlDsaPurpose, RelationshipKind};

use crate::model::{
    Account, AdvanceGame, Appended, AuditEntry, BadgeAward, Bot, Conversation, ConversationMember,
    ConversationPosition, ConversationSummary, Cursor, Device, Entitlement, GameSession, GiftSent,
    KeyBundle, LedgerAccount, LedgerAccountKind, LedgerTransaction, MediaObject, NewAccount,
    NewBot, NewDevice, NewGame, NewMessage, NewOutboxEvent, NewPeer, NewRoom, NewSession,
    NewTransaction, NewXpAward, Notification, OutboxRecord, PeerRecord, Posted, Profile,
    ProfilePatch, Progression, PublishedKeys, PushRegistration, PushTarget, Relationship, Report,
    Room, RoomMember, Scope, Session, Standing, StoredMessage, XpChange,
};

/// Largest page any read will return, whatever the caller asks for.
///
/// A limit exists so that one bad request cannot become a memory incident. 200 is
/// chosen to match the client's screenful-plus-prefetch, so the clamp is never
/// reached during normal use and always reached during abuse.
pub const MAX_PAGE: u16 = 200;

/// Clamps a caller-supplied limit into the allowed range.
#[must_use]
pub fn clamp_limit(limit: u16) -> usize {
    limit.clamp(1, MAX_PAGE) as usize
}

/// Puts a country code into the one form the column is allowed to hold.
///
/// Every backend calls this before writing `account.country`, and section 32's country
/// leaderboard is the reason. That query has to compare the stored value to a code the
/// client supplied, and it has to do it through `account_country_idx` or it becomes a
/// scan of every account that ever registered. An index on a raw column can only serve
/// an exact comparison, so either the column holds exactly one spelling of "Indonesia"
/// or the leaderboard has to wrap it in `upper()` and give the index up.
///
/// So the normalising happens once, on the way in, and the schema's check constraint
/// refuses anything else — which also means the two backends cannot disagree about
/// whether an account written as `id` belongs in `ID`'s ranking.
///
/// `None` stays `None`: not stating a country is allowed, and section 32's leaderboard
/// simply has nobody to rank.
///
/// # Errors
///
/// `VALIDATION_FAILED` when the value is not two ASCII letters. Refused rather than
/// dropped: an account created with `country: Some("Indonesia")` and stored with no
/// country at all is a silent data loss the caller has no way to notice, and the column
/// is `char(2)`, so PostgreSQL would refuse it a moment later anyway with a message
/// about a value too long for type character(2).
pub fn canonical_country(value: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = value.map(str::trim).filter(|code| !code.is_empty()) else {
        return Ok(None);
    };
    if raw.len() != 2 || !raw.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(fault::validation("country", "must be two ASCII letters"));
    }
    Ok(Some(raw.to_ascii_uppercase()))
}

/// Largest number of legs one ledger transaction may carry.
///
/// Real transactions have two to four legs (payer, payee, and sometimes a fee
/// and a mint). The cap exists because the PostgreSQL backend derives each
/// entry's primary key from the transaction id and the leg index, and because an
/// unbounded leg list is an unbounded write inside one database transaction.
pub const MAX_LEDGER_LEGS: usize = 32;

/// Accounts and profiles.
#[async_trait]
pub trait AccountStore: Send + Sync {
    /// Creates an account. Fails with `ALREADY_EXISTS` when the username, email,
    /// or phone is taken.
    async fn create_account(&self, new: NewAccount) -> Result<Account>;

    /// Looks up by primary key.
    async fn account_by_id(&self, account_id: Id) -> Result<Option<Account>>;

    /// Looks up by username, case-insensitively.
    async fn account_by_username(&self, username: &str) -> Result<Option<Account>>;

    /// Looks up by email, case-insensitively.
    async fn account_by_email(&self, email: &str) -> Result<Option<Account>>;

    /// Looks up by phone, exactly (the E.164 form is canonical).
    async fn account_by_phone(&self, phone: &str) -> Result<Option<Account>>;

    /// Replaces the password hash. Callers revoke sessions separately, in the
    /// same request, so that a password change logs other devices out.
    async fn set_password_hash(&self, account_id: Id, hash: &str, at: Timestamp) -> Result<()>;

    /// Sets the recoverable contact on the account — the email or phone that
    /// account recovery and security notifications are addressed to.
    ///
    /// `contact` is parsed as one of the two. An email is stored on both
    /// `email` and `email_lower` so case-insensitive lookups keep working; a
    /// phone is stored on `phone` exactly (E.164 is the canonical form). A
    /// value that is neither, or an account id that is unknown, is
    /// `VALIDATION_FAILED` or `NOT_FOUND` respectively; callers should not
    /// treat the two as interchangeable.
    async fn set_contact(&self, account_id: Id, contact: &str, at: Timestamp) -> Result<()>;

    /// Records a successful sign-in.
    async fn record_login(&self, account_id: Id, at: Timestamp) -> Result<()>;

    /// Sets lifecycle state, with an optional suspension expiry.
    async fn set_status(
        &self,
        account_id: Id,
        status: crate::model::AccountStatus,
        until: Option<Timestamp>,
        at: Timestamp,
    ) -> Result<()>;

    /// Reads a profile.
    async fn profile(&self, account_id: Id) -> Result<Option<Profile>>;

    /// Creates the profile row that accompanies a new account.
    async fn create_profile(&self, profile: Profile) -> Result<Profile>;

    /// Applies a patch and returns the result.
    async fn update_profile(
        &self,
        account_id: Id,
        patch: ProfilePatch,
        at: Timestamp,
    ) -> Result<Profile>;

    /// Searches accounts that opted into being searchable.
    ///
    /// A prefix match on username or display name. Not a ranked full-text search:
    /// discovery on a social product needs to be governed by privacy settings
    /// first and relevance second, and a real search index is a later problem
    /// with its own ADR.
    async fn search_accounts(&self, query: &str, limit: u16) -> Result<Vec<(Account, Profile)>>;
}

/// Devices belonging to accounts.
#[async_trait]
pub trait DeviceStore: Send + Sync {
    /// Registers a device.
    async fn register_device(&self, new: NewDevice) -> Result<Device>;

    /// Looks up one device.
    async fn device_by_id(&self, device_id: Id) -> Result<Option<Device>>;

    /// Lists an account's devices, revoked ones excluded.
    async fn devices_for_account(&self, account_id: Id) -> Result<Vec<Device>>;

    /// Updates the last-seen stamp. Called often, so it must stay a single-row
    /// write with no read first.
    async fn touch_device(&self, device_id: Id, at: Timestamp) -> Result<()>;

    /// Activates a pending device. Called when an add-device challenge is
    /// consumed; a no-op on an already-active device, an error on an unknown
    /// or revoked one, because both mean the caller's picture of the world is
    /// wrong and that should not pass silently.
    async fn activate_device(&self, device_id: Id, at: Timestamp) -> Result<()>;

    /// Records or replaces a device's login-credential public key. Idempotent:
    /// the legacy upgrade path may retry with the same key, and the second
    /// write must not be an error.
    async fn set_device_credential(&self, device_id: Id, public_key: &[u8]) -> Result<()>;

    /// Revokes a device. Its sessions must be revoked by the caller in the same
    /// operation; the store does not do it implicitly, because a silent cascade
    /// is the kind of behaviour that surprises people during an incident.
    async fn revoke_device(&self, device_id: Id, at: Timestamp) -> Result<()>;
}

/// Login sessions and refresh-token families.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Opens a session.
    async fn create_session(&self, new: NewSession) -> Result<Session>;

    /// Looks up by id.
    async fn session_by_id(&self, session_id: Id) -> Result<Option<Session>>;

    /// Looks up by the hash of a presented refresh token.
    ///
    /// The token is never stored, so this is the only way to find a session from
    /// what the client sent. A hit on a session that is already rotated or
    /// revoked is the reuse signal that kills the family.
    async fn session_by_refresh_hash(&self, hash: &[u8]) -> Result<Option<Session>>;

    /// Marks a session rotated and opens its successor in one operation.
    ///
    /// One call because the two must not be separable: a crash between them
    /// either leaves a client holding a token nothing accepts, or leaves two live
    /// generations, and the second is a security hole.
    async fn rotate_session(&self, previous: Id, next: NewSession) -> Result<Session>;

    /// Revokes one session.
    async fn revoke_session(
        &self,
        session_id: Id,
        reason: crate::model::RevokeReason,
        at: Timestamp,
    ) -> Result<()>;

    /// Revokes every generation of a family. Used on reuse detection.
    async fn revoke_family(
        &self,
        family_id: Id,
        reason: crate::model::RevokeReason,
        at: Timestamp,
    ) -> Result<u64>;

    /// Revokes every session of an account, except optionally the current one.
    /// This is "log out my other devices".
    async fn revoke_account_sessions(
        &self,
        account_id: Id,
        except: Option<Id>,
        reason: crate::model::RevokeReason,
        at: Timestamp,
    ) -> Result<u64>;

    /// Lists live sessions, for the security screen in the client.
    async fn sessions_for_account(&self, account_id: Id, now: Timestamp) -> Result<Vec<Session>>;

    /// Deletes sessions whose refresh window closed before `before`.
    ///
    /// Housekeeping, run by `migod maintain`. Expired sessions are already
    /// useless; keeping them forever only grows an index.
    async fn purge_expired_sessions(&self, before: Timestamp) -> Result<u64>;
}

/// Published public key material. Private halves never appear here, in any form,
/// under any circumstances.
#[async_trait]
pub trait KeyStore: Send + Sync {
    /// Publishes or replaces a device's key material.
    async fn publish_keys(&self, keys: PublishedKeys) -> Result<()>;

    /// Adds one-time prekeys to an existing device.
    async fn add_one_time_prekeys(
        &self,
        account_id: Id,
        device_id: Id,
        prekeys: Vec<(i32, Vec<u8>)>,
        at: Timestamp,
    ) -> Result<u64>;

    /// Fetches a bundle and consumes a one-time prekey.
    ///
    /// Consumption is the point: handing the same one-time prekey to two peers
    /// would silently reduce the guarantee to the signed prekey alone. If none
    /// are left the bundle comes back without one, and the caller tells the owner
    /// to publish more rather than failing the conversation.
    async fn take_key_bundle(&self, account_id: Id, device_id: Id) -> Result<Option<KeyBundle>>;

    /// Bundles for every live device of an account, for multi-device fanout.
    async fn take_key_bundles_for_account(&self, account_id: Id) -> Result<Vec<KeyBundle>>;

    /// How many unconsumed one-time prekeys remain. The client tops up when this
    /// falls below its threshold.
    async fn one_time_prekey_count(&self, account_id: Id, device_id: Id) -> Result<u32>;

    /// Marks a device's key material revoked, e.g. after a device is removed.
    async fn revoke_device_keys(&self, account_id: Id, device_id: Id, at: Timestamp) -> Result<()>;
}

/// Conversations, messages, and read state.
#[async_trait]
pub trait MessagingStore: Send + Sync {
    /// Creates a conversation with its initial members.
    async fn create_conversation(
        &self,
        conversation: Conversation,
        members: Vec<Id>,
    ) -> Result<Conversation>;

    /// Finds or creates the direct conversation between two accounts.
    ///
    /// Idempotent under concurrency: two devices tapping "message Bob" at the
    /// same moment must not produce two conversations, so the pair is a unique
    /// key and the loser of the race reads the winner's row.
    async fn direct_conversation(
        &self,
        a: Id,
        b: Id,
        conversation_id: Id,
        encryption: migo_protocol::EncryptionMode,
        at: Timestamp,
    ) -> Result<Conversation>;

    /// Reads a conversation.
    async fn conversation(&self, conversation_id: Id) -> Result<Option<Conversation>>;

    /// Members of a conversation, including those who left.
    async fn members(&self, conversation_id: Id) -> Result<Vec<ConversationMember>>;

    /// Whether an account is currently a member. The hot-path authorisation
    /// check, called on every send.
    async fn is_member(&self, conversation_id: Id, account_id: Id) -> Result<bool>;

    /// Adds a member.
    async fn add_member(&self, member: ConversationMember) -> Result<()>;

    /// Marks a member as having left. The row stays, so history access remains
    /// answerable after the fact.
    async fn remove_member(&self, conversation_id: Id, account_id: Id, at: Timestamp)
        -> Result<()>;

    /// Appends a message, assigning the next sequence number.
    ///
    /// Sequence assignment and insert are one operation, serialised per
    /// conversation. Idempotent by `message_id`: a retry returns
    /// [`Appended::Duplicate`] with the original.
    async fn append_message(&self, new: NewMessage) -> Result<Appended>;

    /// Reads one message.
    async fn message(&self, conversation_id: Id, message_id: Id) -> Result<Option<StoredMessage>>;

    /// Newest-first history, for scrolling up.
    async fn history_before(
        &self,
        conversation_id: Id,
        before_seq: Option<i64>,
        limit: u16,
    ) -> Result<Vec<StoredMessage>>;

    /// Oldest-first messages after a sequence, for catch-up sync.
    ///
    /// This is the whole sync engine on the storage side: a client that was
    /// offline sends its last known sequence and reads forward. No diffing, no
    /// timestamps, no clock agreement required.
    async fn history_after(
        &self,
        conversation_id: Id,
        after_seq: i64,
        limit: u16,
    ) -> Result<Vec<StoredMessage>>;

    /// Edits a message's payload.
    async fn edit_message(
        &self,
        conversation_id: Id,
        message_id: Id,
        envelope: Vec<u8>,
        at: Timestamp,
    ) -> Result<Option<StoredMessage>>;

    /// Tombstones a message.
    async fn delete_message(
        &self,
        conversation_id: Id,
        message_id: Id,
        by: Id,
        at: Timestamp,
    ) -> Result<Option<StoredMessage>>;

    /// Reads a member's cursor.
    async fn cursor(&self, conversation_id: Id, account_id: Id) -> Result<Cursor>;

    /// Advances a cursor. Each field moves forward only: a client whose clock or
    /// ordering is confused must not be able to reset someone's read state.
    async fn advance_cursor(
        &self,
        conversation_id: Id,
        account_id: Id,
        delivered_seq: Option<i64>,
        read_seq: Option<i64>,
        notified_seq: Option<i64>,
        at: Timestamp,
    ) -> Result<Cursor>;

    /// The conversation list, most recently active first.
    ///
    /// Paging is by keyset: `after` is the position of the last row the caller
    /// already has, and `None` asks for the first page. The order is activity
    /// descending, then creation descending, then id ascending — the whole
    /// order, so that a conversation receiving a message mid-page cannot make a
    /// row appear twice or vanish, which an offset would.
    async fn conversation_list(
        &self,
        account_id: Id,
        limit: u16,
        member_preview: u16,
        after: Option<ConversationPosition>,
    ) -> Result<Vec<ConversationSummary>>;

    /// Conversations whose `last_seq` is beyond the caller's cursor, for a
    /// reconnect that needs to know where to look before fetching anything.
    async fn conversations_with_unread(&self, account_id: Id) -> Result<Vec<(Id, i64, i64)>>;

    /// Deletes messages whose disappearing-message deadline has passed.
    async fn purge_expired_messages(&self, before: Timestamp, limit: u16) -> Result<u64>;
}

/// Rooms and room membership.
#[async_trait]
pub trait RoomStore: Send + Sync {
    /// Creates a room, its conversation, and the owner's membership.
    async fn create_room(&self, new: NewRoom) -> Result<Room>;

    /// Reads a room.
    async fn room(&self, room_id: Id) -> Result<Option<Room>>;

    /// Reads a room by slug, case-insensitively.
    async fn room_by_slug(&self, slug: &str) -> Result<Option<Room>>;

    /// Updates mutable settings.
    async fn update_room(
        &self,
        room_id: Id,
        name: Option<String>,
        topic: crate::model::Patch<String>,
        slow_mode_seconds: Option<i32>,
        join_policy: Option<i16>,
        at: Timestamp,
    ) -> Result<Room>;

    /// Archives a room. Not a delete: links and history keep resolving.
    async fn archive_room(&self, room_id: Id, at: Timestamp) -> Result<()>;

    /// Browse listing, most populated first.
    async fn browse_rooms(&self, kind: Option<RoomKindFilter>, limit: u16) -> Result<Vec<Room>>;

    /// Joins a room, or rejoins after leaving. Returns the membership.
    async fn join_room(&self, member: RoomMember) -> Result<RoomMember>;

    /// Marks a member as having left.
    async fn leave_room(&self, room_id: Id, account_id: Id, at: Timestamp) -> Result<()>;

    /// Reads one membership, including a lapsed or banned one — the caller needs
    /// to see a ban in order to enforce it.
    async fn room_member(&self, room_id: Id, account_id: Id) -> Result<Option<RoomMember>>;

    /// Roster page, ordered by role then join time.
    async fn room_members(
        &self,
        room_id: Id,
        limit: u16,
        after: Option<Id>,
    ) -> Result<Vec<RoomMember>>;

    /// Rooms an account is currently in.
    async fn rooms_for_account(&self, account_id: Id) -> Result<Vec<Room>>;

    /// Sets a member's role.
    async fn set_room_role(
        &self,
        room_id: Id,
        account_id: Id,
        role: migo_protocol::RoomRole,
        at: Timestamp,
    ) -> Result<()>;

    /// Moves ownership from one member to another, atomically.
    ///
    /// One method rather than three calls from a service, because the three writes it
    /// makes — the room's owner column, the new owner's role, the old owner's
    /// demotion — have no valid intermediate state. A crash between them leaves a
    /// room with two owners or with none, and neither is a state any later request
    /// could repair without being told what was intended.
    ///
    /// `from` must be the current owner and `to` must be an active member. The
    /// outgoing owner is demoted rather than removed: brief section 85 asks for a
    /// transfer, and a transfer that ejected the previous owner would make a mistaken
    /// one unrecoverable by the only person who could explain it.
    async fn transfer_room_ownership(
        &self,
        room_id: Id,
        from: Id,
        to: Id,
        at: Timestamp,
    ) -> Result<()>;

    /// Sets per-member permission overrides.
    async fn set_room_permissions(
        &self,
        room_id: Id,
        account_id: Id,
        grant: u64,
        deny: u64,
        at: Timestamp,
    ) -> Result<()>;

    /// Applies a mute or a ban. `None` lifts it.
    async fn set_room_sanction(
        &self,
        room_id: Id,
        account_id: Id,
        muted_until: Option<Timestamp>,
        banned_until: Option<Timestamp>,
        reason: Option<String>,
        at: Timestamp,
    ) -> Result<()>;

    /// Recomputes the cached member count from the membership rows.
    ///
    /// The count is derived, so it can be rebuilt. That is deliberate: a
    /// denormalised counter that cannot be recomputed is a permanent source of
    /// numbers nobody trusts.
    async fn recount_room(&self, room_id: Id) -> Result<i32>;
}

/// Which rooms a browse request wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomKindFilter {
    /// Open to anyone.
    Public,
    /// Listed, but joining is moderated.
    Managed,
}

/// The social graph.
#[async_trait]
pub trait SocialStore: Send + Sync {
    /// Creates or refreshes an edge.
    async fn put_relationship(&self, relationship: Relationship) -> Result<Relationship>;

    /// Reads one edge.
    async fn relationship(
        &self,
        account_id: Id,
        other_id: Id,
        kind: RelationshipKind,
    ) -> Result<Option<Relationship>>;

    /// Removes an edge. Removing something absent is not an error.
    async fn remove_relationship(
        &self,
        account_id: Id,
        other_id: Id,
        kind: RelationshipKind,
    ) -> Result<()>;

    /// Accepts a pending friend request, creating the reciprocal edge.
    async fn accept_friend(&self, account_id: Id, other_id: Id, at: Timestamp) -> Result<()>;

    /// Lists edges of one kind that the account owns.
    async fn relationships(
        &self,
        account_id: Id,
        kind: RelationshipKind,
        limit: u16,
    ) -> Result<Vec<Relationship>>;

    /// How many edges of one kind the account owns.
    ///
    /// Separate from [`SocialStore::relationships`] because a ceiling cannot be
    /// enforced from a page. `relationships` clamps to [`MAX_PAGE`], so a service that
    /// counted its result would refuse a two-hundred-and-first friend and call it a
    /// five-thousand limit. A limit the storage layer cannot answer is a limit the
    /// service should not claim to enforce.
    async fn count_relationships(&self, account_id: Id, kind: RelationshipKind) -> Result<u64>;

    /// Incoming edges of one kind, e.g. pending friend requests.
    async fn inbound_relationships(
        &self,
        account_id: Id,
        kind: RelationshipKind,
        limit: u16,
    ) -> Result<Vec<Relationship>>;

    /// Whether either side has blocked the other.
    ///
    /// Symmetric on purpose. A block has to stop contact in both directions, and
    /// asking the question one way round is how "blocked users can still reply in
    /// threads" bugs happen.
    async fn is_blocked_either_way(&self, a: Id, b: Id) -> Result<bool>;
}

/// The economy ledger.
#[async_trait]
pub trait EconomyStore: Send + Sync {
    /// Finds or creates a user's ledger account for one currency.
    async fn ledger_account(
        &self,
        owner_id: Option<Id>,
        kind: LedgerAccountKind,
        currency: crate::model::Currency,
        create_with_id: Id,
        at: Timestamp,
    ) -> Result<LedgerAccount>;

    /// Posts a transaction. Rejects legs that do not sum to zero; returns the
    /// original on a repeated idempotency key.
    async fn post_transaction(&self, new: NewTransaction) -> Result<Posted>;

    /// Balance as the sum of entries, using the latest snapshot as a base.
    async fn balance(&self, ledger_account_id: Id) -> Result<i64>;

    /// Recent entries for an account statement.
    async fn ledger_history(
        &self,
        ledger_account_id: Id,
        limit: u16,
    ) -> Result<Vec<(LedgerTransaction, i64)>>;

    /// Total of every entry in a currency. Must be zero. Run as a scheduled
    /// audit, because an invariant that nothing checks is a wish.
    async fn currency_sum(&self, currency: crate::model::Currency) -> Result<i64>;

    /// Gifts an account has been given, newest first.
    ///
    /// Reads only, because a gift is written by [`EconomyStore::post_transaction`] as the
    /// receipt half of the purchase that paid for it. There is deliberately no
    /// `create_gift`: a gift row that could be written on its own is a gift somebody can
    /// have without paying for it.
    async fn gifts_received(&self, account_id: Id, limit: u16) -> Result<Vec<GiftSent>>;

    /// Gifts shown in one conversation, newest first.
    async fn gifts_in_conversation(&self, conversation_id: Id, limit: u16)
        -> Result<Vec<GiftSent>>;

    /// How many of each gift code an account holds, for the profile shelf.
    ///
    /// A tally rather than the rows, because a profile that has been given forty thousand
    /// roses needs the number forty thousand and not forty thousand rows. Ordered by count
    /// descending, then by code, so the same shelf renders the same way twice.
    async fn gift_tally(&self, account_id: Id) -> Result<Vec<(String, u32)>>;

    /// Everything an account owns, oldest first.
    async fn entitlements(&self, account_id: Id) -> Result<Vec<Entitlement>>;

    /// Whether an account already owns one thing.
    ///
    /// Checked before a purchase is priced, so that buying the same theme twice is refused
    /// rather than charged. The composite primary key would refuse the insert anyway; this
    /// exists so the refusal happens before the money moves.
    async fn has_entitlement(&self, account_id: Id, sku: &str) -> Result<bool>;
}

/// Experience, levels, badges, and the leaderboards over them.
///
/// Separate from [`EconomyStore`] because the two answer to different rules. Currency is
/// double-entry and must sum to zero; XP is a counter that only goes up and has no
/// counterparty. Folding them together would put a mint account and a badge in one trait
/// and invite a transaction with an XP leg in it, which is a transaction that cannot
/// balance.
#[async_trait]
pub trait ProgressionStore: Send + Sync {
    /// An account's XP and level, absent until it earns something.
    async fn progression(&self, account_id: Id) -> Result<Option<Progression>>;

    /// Records one award and adds it to the running total, returning the totals on both
    /// sides of the addition.
    ///
    /// Two rows, one transaction. The event row is what section 32's weekly leaderboard
    /// and section 30's anti-farming caps read; the total is what every profile reads.
    /// Writing them separately would let a crash leave an award that counts towards a
    /// cap but not towards a rank, or the reverse.
    ///
    /// The addition happens in the database, so two awards landing together sum rather
    /// than overwrite: a read-modify-write here would silently lose whichever award lost
    /// the race, and the account holder would have no way to notice.
    ///
    /// `amount` must be positive. XP that can go down is XP that can be taken away by a
    /// bug, and section 30 describes earning, never spending.
    ///
    /// # Errors
    ///
    /// `ALREADY_EXISTS` when `idempotency_key` names an award that was already granted.
    /// The caller reads the current total if it needs one; a retry that quietly returned
    /// the earlier `XpChange` would be indistinguishable from a fresh award and would
    /// make the caller announce a level-up twice.
    async fn award_xp(&self, award: NewXpAward) -> Result<XpChange>;

    /// Rewrites the cached level.
    ///
    /// Called only when [`ProgressionStore::award_xp`] crossed a threshold, which is why it
    /// is separate: the level is a projection of `xp`, and writing it on every award would
    /// be a second write for a value that did not change.
    async fn set_level(&self, account_id: Id, level: i32, at: Timestamp) -> Result<()>;

    /// Grants a badge. Returns whether this call was the one that granted it.
    ///
    /// Idempotent by primary key, so the caller does not have to ask first. `false` is not
    /// an error: awarding "Veteran" twice is what happens when a job runs twice.
    async fn award_badge(&self, award: BadgeAward) -> Result<bool>;

    /// Badges an account holds, newest first.
    async fn badges(&self, account_id: Id) -> Result<Vec<BadgeAward>>;

    /// What one account has earned since a cutoff, optionally from one source only.
    ///
    /// This is section 30's anti-farming mechanism, and it reads the durable rows rather
    /// than a counter on purpose. A cache counter is cheaper and answers the same
    /// question until the cache restarts, at which point every account's daily cap
    /// silently resets and the abuse this is here to stop becomes free for an hour.
    ///
    /// `source` of `None` sums every source, which is the global daily cap; `Some` narrows
    /// to one, which is the per-source cap.
    async fn xp_earned_since(
        &self,
        account_id: Id,
        source: Option<i16>,
        since: Timestamp,
    ) -> Result<i64>;

    /// A leaderboard: the highest XP in a population, over all time or over a window.
    ///
    /// `since` of `None` ranks lifetime totals, which is section 32's all-time board and
    /// is one indexed read of `progression`. `Some` ranks what was earned at or after
    /// that instant, which is how weekly and monthly are built — the caller decides where
    /// a week starts, because a server that decided would be deciding in its own timezone
    /// for an audience that is not in it.
    ///
    /// `level` is always the account's current level, in both modes. A windowed board
    /// ranks by the window's earnings and shows the person's actual standing beside it;
    /// there is no such thing as the level somebody held last Tuesday.
    ///
    /// Ordered by XP descending then account id ascending. The tiebreak is part of the
    /// contract: without it, two accounts on the same total swap places between one page
    /// and the next, and a client paging through can show somebody twice or not at all.
    async fn leaderboard(
        &self,
        scope: Scope<'_>,
        since: Option<Timestamp>,
        limit: u16,
    ) -> Result<Vec<Standing>>;
}

/// Media metadata.
#[async_trait]
pub trait MediaStore: Send + Sync {
    /// Records an upload.
    async fn create_media(&self, media: MediaObject) -> Result<MediaObject>;

    /// Reads metadata.
    async fn media(&self, media_id: Id) -> Result<Option<MediaObject>>;

    /// Updates the scan verdict.
    async fn set_media_scan_status(&self, media_id: Id, status: i16, at: Timestamp) -> Result<()>;

    /// Tombstones an object. The bytes are removed by a sweeper, so a failed
    /// delete in object storage cannot leave a row pointing at nothing.
    async fn delete_media(&self, media_id: Id, at: Timestamp) -> Result<()>;
}

/// Reports and the audit log.
#[async_trait]
pub trait SafetyStore: Send + Sync {
    /// Files a report.
    async fn create_report(&self, report: Report) -> Result<Report>;

    /// Reads a report.
    async fn report(&self, report_id: Id) -> Result<Option<Report>>;

    /// The moderation queue, oldest first: a report that waits longest is the one
    /// that matters most.
    async fn open_reports(&self, limit: u16) -> Result<Vec<Report>>;

    /// An unresolved report this reporter already filed about this subject.
    ///
    /// The idempotency key brief section 153 asks for on a report: the pair of
    /// reporter and subject, while a report about it is still open. Without this the
    /// only way to notice a repeat is to scan the queue, which is bounded, so past the
    /// bound every repeat becomes a new row — and a script can turn one grievance into
    /// a hundred thousand rows in the table moderators have to read.
    ///
    /// Deliberately scoped to *open* reports. A subject reported, actioned, and then
    /// misbehaving again is a new report and not a duplicate of a closed one.
    async fn open_report_by_reporter(
        &self,
        reporter_id: Id,
        subject_kind: i16,
        subject_id: Id,
    ) -> Result<Option<Report>>;

    /// How many reports were filed about one subject at or after `since`.
    ///
    /// Served by `report_subject_idx`. This is the one abuse signal in the schema that
    /// is neither a rate counter nor message content: how many *other people*
    /// independently complained. A count and not the rows, because the caller needs a
    /// number and the rows would carry reporter identities into whatever asked.
    async fn count_reports_about(
        &self,
        subject_kind: i16,
        subject_id: Id,
        since: Timestamp,
    ) -> Result<u32>;

    /// Resolves a report.
    async fn resolve_report(
        &self,
        report_id: Id,
        status: i16,
        resolution: i16,
        by: Id,
        at: Timestamp,
    ) -> Result<()>;

    /// Appends an audit entry.
    async fn append_audit(&self, entry: AuditEntry) -> Result<()>;

    /// Audit entries about one target, newest first.
    async fn audit_for_target(
        &self,
        target_kind: i16,
        target_id: Id,
        limit: u16,
    ) -> Result<Vec<AuditEntry>>;
}

/// The notification inbox and the push registrations behind it.
///
/// Separate from [`DeviceStore`] on purpose, and this is the reason: `DeviceStore`
/// has no method that can read or write a push token, so `migo-auth` — which holds
/// `Arc<dyn DeviceStore>` to register a device at sign-in — structurally cannot
/// touch one. The registration is written by the crate that sends pushes and by
/// nothing else. It is the same instinct as the PostgreSQL backend's partial device
/// model: a credential that a query never selects cannot be logged by accident.
///
/// Nothing here takes a raw token. The caller seals it and hashes it before the call,
/// so no key for a push credential exists at this layer.
#[async_trait]
pub trait NotifyStore: Send + Sync {
    /// Appends one notification.
    ///
    /// Refuses a kind that [`crate::model::notification_kind::is_storable`] rejects, with a
    /// validation error rather than an insert. A message is not an inbox row, and
    /// the storage layer is the last place that can still say so before the mistake
    /// becomes a count that disagrees with `conversation_cursor`.
    async fn create_notification(&self, notification: Notification) -> Result<Notification>;

    /// An account's inbox, newest first.
    async fn notifications(&self, account_id: Id, limit: u16) -> Result<Vec<Notification>>;

    /// How many are unread.
    ///
    /// Served by the partial index, which is the size of what is unread rather than
    /// of everything that ever happened. This runs on every app foreground; the list
    /// above runs only when somebody taps the bell.
    async fn unread_notifications(&self, account_id: Id) -> Result<u32>;

    /// Marks everything created at or before `through` as read, returning how many
    /// rows changed.
    ///
    /// A watermark rather than a list of ids, because the client's gesture is
    /// "I have seen the inbox" and a list would race with anything that arrived
    /// while the request was in flight — marking read a row the user never saw.
    /// Already-read rows keep their original `read_at`, so a retry is a no-op and
    /// the count it returns is honest.
    async fn mark_notifications_read(
        &self,
        account_id: Id,
        through: Timestamp,
        at: Timestamp,
    ) -> Result<u32>;

    /// Deletes read notifications created before `before`, up to `limit` rows.
    ///
    /// Bounded, because a retention sweep that deletes everything old in one
    /// statement takes a lock proportional to how long the sweep was broken. The
    /// caller loops until this returns zero.
    ///
    /// Unread rows are never swept. An achievement nobody has seen is not stale, it
    /// is unseen, and deleting it would mean the badge simply never arrived.
    async fn purge_notifications(&self, before: Timestamp, limit: u16) -> Result<u64>;

    /// Records a device's push registration, replacing whatever it had.
    ///
    /// Takes the token from any other device that had registered the same hash. That
    /// is not a courtesy: restore a backup onto a new handset and the platform hands
    /// the same token to a row that did not exist yesterday, and if the old row keeps
    /// it then one notification becomes two pushes to one phone, forever.
    ///
    /// Fails with `NOT_FOUND` for an unknown or revoked device. A registration
    /// pointing at a revoked device is a wake-up sent to a session somebody
    /// deliberately ended.
    async fn set_push_registration(
        &self,
        device_id: Id,
        registration: PushRegistration,
        at: Timestamp,
    ) -> Result<()>;

    /// Forgets a device's registration.
    ///
    /// Called on sign-out and on revocation. Idempotent: a device with no
    /// registration is the state this asks for.
    async fn clear_push_registration(&self, device_id: Id) -> Result<()>;

    /// Forgets whichever registration has this hash, wherever it is.
    ///
    /// The path for a provider that answered `UNREGISTERED`: the sender knows the
    /// hash it failed on and must be able to retire it without first learning which
    /// account it belonged to. Returns whether anything was removed.
    async fn retire_push_hash(&self, hash: &str) -> Result<bool>;

    /// Where an account's pushes have to go.
    ///
    /// Live devices only, and only those with a registration. An empty answer is the
    /// normal case for an account signed in on the web with notifications declined,
    /// so it is `Ok(vec![])` and not an error.
    async fn push_targets(&self, account_id: Id) -> Result<Vec<PushTarget>>;

    /// Registrations last refreshed before `before`, for the staleness sweep.
    ///
    /// Both providers expire tokens without saying so, and a send to a dead token is
    /// charged, rate-limited, and useless. Returns the hashes, not the accounts:
    /// the sweeper's job is to retire registrations, and it has no business
    /// assembling a list of who has stopped using the product.
    async fn stale_push_hashes(&self, before: Timestamp, limit: u16) -> Result<Vec<String>>;
}

/// Games played inside a conversation.
///
/// The store's whole job here is to hold an authoritative state blob and to move it
/// from one value to the next without ever letting two moves land on the same base.
/// It never reads the blob: brief section 89 keeps the game's state on the server so
/// the client cannot compute a flattering score, and the store treating the bytes as
/// opaque is what makes that a structural fact rather than a promise. Turn rules,
/// action validity, and win detection all live in `migo-games`; what lives here is
/// the one guarantee a game engine cannot give itself — that a move computed against
/// a state is applied to that state or to nothing.
#[async_trait]
pub trait GameStore: Send + Sync {
    /// Creates a game with its opening state.
    async fn create_game(&self, new: NewGame) -> Result<GameSession>;

    /// Reads a game.
    async fn game(&self, game_id: Id) -> Result<Option<GameSession>>;

    /// Open games in a conversation, newest first.
    ///
    /// Bounded by the same clamp as every other paged read. A conversation that has
    /// somehow accumulated more open games than a screenful is a bug being contained,
    /// not a listing to render whole.
    async fn active_games(&self, conversation_id: Id, limit: u16) -> Result<Vec<GameSession>>;

    /// Applies a move as a compare-and-swap, returning the new state on success.
    ///
    /// Applied only if the row still carries [`AdvanceGame::expected_updated_at`] and
    /// is still [`crate::model::game_status::OPEN`]. On a lost race, a stale base, or
    /// a game that has already ended, nothing is written and the result is `Ok(None)`:
    /// the caller re-reads and decides again. This is the store half of section 90's
    /// anti-cheat — a replayed or superseded move cannot overwrite a newer one because
    /// the token it names is no longer there.
    async fn advance_game(&self, advance: AdvanceGame) -> Result<Option<GameSession>>;

    /// Marks an open game abandoned. A no-op returning `Ok(None)` if it was already
    /// terminal, so a member leaving twice, or leaving a finished game, is harmless.
    async fn abandon_game(&self, game_id: Id, at: Timestamp) -> Result<Option<GameSession>>;
}

/// The registry of bots and the credential behind each one.
///
/// A bot is an account that authenticates by a bearer token rather than a password
/// (brief section 36), so `migo-auth` deliberately does not mint sessions for one —
/// turning a bot token into a caller is a different set of checks and belongs to
/// `migo-bots`. What lives here is the persistence those checks read and write: the
/// bot row, its backing account, and the lookup from a presented token to the bot
/// it belongs to.
///
/// The store never sees a raw token. [`register_bot`](BotStore::register_bot) and
/// [`set_bot_token_hash`](BotStore::set_bot_token_hash) take an already-computed
/// keyed HMAC tag, and [`bot_by_token_hash`](BotStore::bot_by_token_hash) is queried
/// with the tag of the token the client presented — so a database dump holds no
/// credential and, because the tag is keyed by the deployment secret, cannot even be
/// probed offline (section 145). The scope bits are raw here; their meaning belongs
/// to `migo-bots`, the same way a game's `kind` belongs to `migo-games`.
///
/// Bots reach none of this directly: brief section 42 forbids a bot any direct
/// database access, and it has none — a bot speaks the wire protocol and the gateway
/// speaks to the store on its behalf.
#[async_trait]
pub trait BotStore: Send + Sync {
    /// Registers a bot: its backing account, that account's profile, and the bot
    /// row, atomically.
    ///
    /// One write and not three, because the intermediate states are all invalid: an
    /// account whose password is a hash of discarded bytes is unusable until the bot
    /// row exists to authenticate it, and a profile is required for it to appear
    /// anywhere. A backend that can offer a transaction uses one; the in-memory
    /// backend takes its single lock for the whole operation. A username or token
    /// collision fails the whole thing with [`fault::already_exists`], having
    /// written nothing.
    async fn register_bot(&self, new: NewBot) -> Result<Bot>;

    /// Reads a bot by its own id.
    async fn bot(&self, bot_id: Id) -> Result<Option<Bot>>;

    /// Reads the bot backed by an account, or `None` if that account is not a bot.
    ///
    /// The reverse of [`Bot::account_id`], and how a path that holds an account id —
    /// a moderation subject, a conversation member — asks whether it is looking at a
    /// bot.
    async fn bot_by_account(&self, account_id: Id) -> Result<Option<Bot>>;

    /// Reads the bot whose stored tag equals `token_hash`, or `None`.
    ///
    /// The authentication lookup. The caller passes the keyed HMAC tag of the token
    /// the client presented; a hit is the bot that token belongs to, a miss is a
    /// token that matches no bot. A disabled bot is returned like any other — whether
    /// a disabled bot may act is `migo-bots`' decision, not the store's, and hiding
    /// the row here would only turn a clear "disabled" into a confusing "unknown".
    async fn bot_by_token_hash(&self, token_hash: &[u8]) -> Result<Option<Bot>>;

    /// Every bot an account owns, newest first, bounded by the shared page clamp.
    async fn bots_for_owner(&self, owner_id: Id, limit: u16) -> Result<Vec<Bot>>;

    /// Replaces a bot's scope bitmask, returning the updated row.
    ///
    /// `Ok(None)` if the bot is gone, so a caller acting on a stale id learns it
    /// rather than silently succeeding.
    async fn set_bot_scopes(&self, bot_id: Id, scopes: i64) -> Result<Option<Bot>>;

    /// Replaces a bot's token tag — a rotation — returning the updated row.
    ///
    /// The new tag is that of a freshly minted token the owner now holds; the old
    /// token stops authenticating the instant this lands, because
    /// [`bot_by_token_hash`](BotStore::bot_by_token_hash) will never match it again.
    /// `Ok(None)` if the bot is gone. A tag that collides with another bot's fails
    /// with [`fault::already_exists`].
    async fn set_bot_token_hash(&self, bot_id: Id, token_hash: Vec<u8>) -> Result<Option<Bot>>;

    /// Sets or clears a bot's disabled timestamp, returning the updated row.
    ///
    /// `Some(at)` disables it; `None` re-enables it. Idempotent — disabling a
    /// disabled bot restamps the time and is harmless — and `Ok(None)` if the bot is
    /// gone. This is what a moderator's "disable bot" action and an owner's "pause my
    /// bot" both reach.
    async fn set_bot_disabled(
        &self,
        bot_id: Id,
        disabled_at: Option<Timestamp>,
    ) -> Result<Option<Bot>>;
}

/// The federation allow-list and the outbound event queue (brief sections 169, 170).
///
/// Born with `migo-federation`, which is the only crate that should hold it. Two
/// concerns share the trait because they share a lifetime — a peer, and the events
/// queued for it — and both must be durable. The allow-list is the mesh's security
/// boundary: a node absent from it does not federate, and losing it on a restart
/// would either open the mesh or close it, both wrong. The outbox is written in the
/// same transaction as the change it announces, so an event exists exactly when its
/// change committed.
///
/// The store keeps `node_id` as text and `status` as a raw `i16`; what a status
/// means, and the mapping between a node id and its crypto identity, belong to
/// `migo-federation` — the same division `migo-bots` keeps with `scopes`.
#[async_trait]
pub trait FederationStore: Send + Sync {
    /// Adds a peer to the allow-list, returning the stored row.
    ///
    /// A node id already present, or a public key already claimed by another peer,
    /// fails with [`fault::already_exists`] having written nothing: a peer's identity
    /// is not something a second `add_peer` may quietly replace.
    async fn add_peer(&self, new: NewPeer) -> Result<PeerRecord>;

    /// Reads one peer by node id, or `None` if it is not in the allow-list.
    ///
    /// The lookup a handshake makes before anything else. A `None` here is a node the
    /// operator never named, and the connection is refused before a payload is
    /// decoded (section 169).
    async fn peer(&self, node_id: &str) -> Result<Option<PeerRecord>>;

    /// Every peer in the allow-list, newest first, bounded by the shared page clamp.
    async fn peers(&self, limit: u16) -> Result<Vec<PeerRecord>>;

    /// Sets a peer's allow-list status, returning the updated row.
    ///
    /// `Ok(None)` if the peer is gone. How an operator pauses or blocks a peer
    /// without forgetting its key — a blocked peer's row survives so it can be
    /// re-allowed without a fresh key exchange.
    async fn set_peer_status(&self, node_id: &str, status: i16) -> Result<Option<PeerRecord>>;

    /// Stamps a peer's last-seen time after a successful handshake, returning the
    /// updated row.
    ///
    /// `Ok(None)` if the peer is gone. Purely a record for an operator; nothing in
    /// the protocol compares against it.
    async fn touch_peer(&self, node_id: &str, seen_at: Timestamp) -> Result<Option<PeerRecord>>;

    /// Enqueues an outbound event, returning the stored row.
    ///
    /// Meant to run inside the caller's transaction, alongside the state change it
    /// announces, so the event becomes durable exactly when that change does. A
    /// duplicate `event_id` fails with [`fault::already_exists`].
    async fn enqueue_event(&self, new: NewOutboxEvent) -> Result<OutboxRecord>;

    /// Reads up to `limit` events due for delivery at or before `now`.
    ///
    /// "Due" means not yet delivered and `next_attempt_at <= now`. Returned in
    /// `next_attempt_at` order, oldest first. This is a plain read, not a claim: it
    /// takes no lock and mutates nothing, so both backends hand back the same set and
    /// two senders draining at once may pick up the same event. That is deliberate —
    /// delivery is at least once and the consumer is idempotent (section 153), so a
    /// double send costs a wasted request, not a wrong one. A drainer that wants to
    /// avoid the waste narrows the window itself, by advancing `next_attempt_at`
    /// through [`mark_failed`](FederationStore::mark_failed) as soon as it takes an
    /// event, rather than relying on the store to hand each event out once.
    async fn due_events(&self, now: Timestamp, limit: u16) -> Result<Vec<OutboxRecord>>;

    /// Marks an event delivered, returning the updated row.
    ///
    /// `Ok(None)` if the event id is unknown. Idempotent: marking a delivered event
    /// delivered again is harmless, and afterwards it is never handed out by
    /// [`due_events`](FederationStore::due_events) again.
    async fn mark_delivered(
        &self,
        event_id: Id,
        delivered_at: Timestamp,
    ) -> Result<Option<OutboxRecord>>;

    /// Records a failed delivery attempt, returning the updated row.
    ///
    /// Increments `attempts`, sets `next_attempt_at` to the caller-computed backoff
    /// deadline, and stores `error` for an operator. `Ok(None)` if the event id is
    /// unknown. The event stays in the queue and becomes due again at
    /// `next_attempt_at` — delivery is at least once, so the consumer must be
    /// idempotent (section 153).
    async fn mark_failed(
        &self,
        event_id: Id,
        next_attempt_at: Timestamp,
        error: &str,
    ) -> Result<Option<OutboxRecord>>;
}

/// A row in the `captcha_challenge` table.
///
/// The service in `migo-captcha` is the only writer and reader; the store
/// trait exists so the same service can be pointed at a memory or Postgres
/// backend without changing its source. The struct mirrors the table shape:
/// a primary key, a tag, and the two timestamps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptchaRow {
    /// The challenge id, surfaced to the client as the `challenge_id` field.
    pub challenge_id: Id,
    /// The HMAC tag over (challenge_id || code). Never the code itself.
    pub tag: Vec<u8>,
    /// When the challenge stops being accepted.
    pub expires_at: Timestamp,
    /// When the row was inserted.
    pub created_at: Timestamp,
}

/// Storage for the captcha challenge table.
///
/// The captcha service in `migo-captcha` issues the challenge; the store
/// persists it. Splitting them lets the same service run against the
/// in-memory store used in tests and the Postgres store used in production
/// without re-implementing the issuance or verification logic.
#[async_trait]
pub trait CaptchaStore: Send + Sync {
    /// Inserts or replaces a challenge. The captcha is one-shot per id, so a
    /// second call with the same `challenge_id` overwrites the first.
    async fn put_captcha(&self, row: CaptchaRow) -> Result<()>;

    /// Reads a live challenge by id. Returns `None` if the row does not exist
    /// or its `expires_at` is at or before `now`.
    async fn get_captcha(&self, challenge_id: Id, now: Timestamp) -> Result<Option<CaptchaRow>>;

    /// Drops a challenge. Called on success and on a permanent failure so a
    /// tag cannot be replayed.
    async fn delete_captcha(&self, challenge_id: Id) -> Result<()>;
}

/// A row in the `password_recovery` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryRow {
    /// The token id, surfaced to the client.
    pub token_id: Id,
    /// Owning account.
    pub account_id: Id,
    /// The HMAC tag over (token_id || 'recovery').
    pub tag: Vec<u8>,
    /// When the token stops being accepted.
    pub expires_at: Timestamp,
    /// Stamped when the token is exchanged for a new password.
    pub consumed_at: Option<Timestamp>,
    /// When the row was inserted.
    pub created_at: Timestamp,
}

/// Storage for the password-recovery token table.
///
/// Distinct from the captcha store because the two are not interchangeable:
/// captcha rows are short-lived and one-shot, recovery rows are
/// twenty-minute-scoped and have a consumed state. The `recovery_consume`
/// call is the atomic "stamp and return" the confirm handler relies on; an
/// in-memory backend takes its lock, the Postgres backend uses a single
/// update with a `where consumed_at is null` predicate.
#[async_trait]
pub trait RecoveryStore: Send + Sync {
    /// Inserts a new token row.
    async fn recovery_put(&self, row: RecoveryRow) -> Result<()>;

    /// Reads a row by token id.
    async fn recovery_get(&self, token_id: Id) -> Result<Option<RecoveryRow>>;

    /// Stamps the row as consumed, returning the row as it was on a successful
    /// transition (i.e. a row whose `consumed_at` is `None` and whose
    /// `expires_at` is still in the future). Returns `Ok(None)` when the
    /// transition cannot happen — already consumed, expired, or unknown.
    async fn recovery_consume(&self, token_id: Id, at: Timestamp) -> Result<Option<RecoveryRow>>;

    /// Deletes rows whose `expires_at` is at or before `before`. Bounded by
    /// `limit` so a sweeper that has been broken for a long time cannot
    /// take a lock proportional to the cleanup debt.
    async fn recovery_delete_expired(&self, before: Timestamp, limit: u32) -> Result<u64>;
}

/// A row in the `identity_keys` table: one version of an account's ML-DSA
/// identity (brief section 182).
///
/// Not the E2EE `identity_key` table — that one is the 64-byte
/// Ed25519||X25519 pair X3DH starts from, lives at one row per device, and
/// predates this table by three migrations. The names are adjacent; the keys
/// have nothing to do with each other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityKeyRow {
    /// Primary key.
    pub key_id: Id,
    /// Owning account.
    pub account_id: Id,
    /// Algorithm name, e.g. "ML-DSA-65". A column, not a constant, so the
    /// next algorithm is a new row (agility, brief section 182).
    pub algorithm: String,
    /// 1-based version, unique per account.
    pub key_version: i32,
    /// The public half only. 1952 bytes for ML-DSA-65.
    pub public_key: Vec<u8>,
    /// Active, rotated, or revoked.
    pub status: crate::model::IdentityKeyStatus,
    /// When this version was registered.
    pub created_at: Timestamp,
    /// When this version was revoked, if it was.
    pub revoked_at: Option<Timestamp>,
}

/// Storage for the account's ML-DSA identity keys.
///
/// The server only ever holds public material here; the seed stays on the
/// device that derived it. Rotation is one store call rather than a
/// put-then-update sequence because the two writes must not be separable: a
/// crash between them is an account with either two live identities or none.
#[async_trait]
pub trait IdentityStore: Send + Sync {
    /// Inserts a key. Rejects a duplicate `(account_id, key_version)`, which
    /// is how an idempotent retry is told apart from a versioning mistake.
    async fn put_identity_key(&self, row: IdentityKeyRow) -> Result<()>;

    /// The account's keys, newest version first.
    async fn identity_keys(&self, account_id: Id) -> Result<Vec<IdentityKeyRow>>;

    /// The one active key, if the account has one.
    async fn active_identity_key(&self, account_id: Id) -> Result<Option<IdentityKeyRow>>;

    /// Appends `new` as active and marks every other key of the account
    /// rotated, in one operation. `new.key_version` must be the successor of
    /// the current active version; the store enforces that rather than
    /// trusting the caller's arithmetic.
    async fn rotate_identity_key(&self, new: IdentityKeyRow) -> Result<()>;

    /// Revokes one key by id. Revoking the active key is allowed — that is
    /// what an emergency withdrawal is — and leaves the account without an
    /// active identity until a new one is registered.
    async fn revoke_identity_key(&self, key_id: Id, account_id: Id, at: Timestamp) -> Result<()>;
}

/// A row in the `login_challenge` table: one single-use ML-DSA challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginChallengeRow {
    /// Primary key, surfaced to the client as the id to answer.
    pub challenge_id: Id,
    /// The account the challenge authenticates.
    pub account_id: Id,
    /// The device the challenge is bound to. For add-device, the pending row.
    pub device_id: Id,
    /// Login, add-device, or rotate — from the protocol's `MlDsaPurpose`.
    pub purpose: MlDsaPurpose,
    /// The exact canonical bytes the client must sign, stored as issued.
    pub payload: Vec<u8>,
    /// When the challenge stops being accepted. Five minutes by policy.
    pub expires_at: Timestamp,
    /// Stamped on first successful use.
    pub consumed_at: Option<Timestamp>,
    /// When the challenge was issued.
    pub created_at: Timestamp,
}

/// Storage for single-use ML-DSA login challenges.
///
/// The same shape as the recovery store: a short-lived row whose interesting
/// transition is "consume", which must be atomic — the second presentation of
/// a challenge sees exactly what an expired one does, so replay tells the
/// attacker nothing about which of the two happened.
#[async_trait]
pub trait ChallengeStore: Send + Sync {
    /// Inserts a new challenge.
    async fn put_login_challenge(&self, row: LoginChallengeRow) -> Result<()>;

    /// Reads a challenge by id, live ones only.
    async fn get_login_challenge(
        &self,
        challenge_id: Id,
        now: Timestamp,
    ) -> Result<Option<LoginChallengeRow>>;

    /// Stamps the row as consumed, returning the row as it was on a successful
    /// transition (unconsumed and unexpired). `Ok(None)` when the transition
    /// cannot happen — already consumed, expired, or unknown — and the caller
    /// answers all three identically.
    async fn consume_login_challenge(
        &self,
        challenge_id: Id,
        now: Timestamp,
    ) -> Result<Option<LoginChallengeRow>>;

    /// Deletes rows whose `expires_at` is at or before `before`, bounded by
    /// `limit`, for the same sweeper that owns the captcha and recovery rows.
    async fn delete_expired_login_challenges(&self, before: Timestamp, limit: u32) -> Result<u64>;
}

/// A row in the `wallet` table: one registered EVM address.
///
/// An address, not a wallet: the private key never leaves the device, and the
/// server stores exactly what a directory needs — which account shows this
/// address, and which derivation index produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletRow {
    /// Primary key.
    pub wallet_id: Id,
    /// Owning account.
    pub account_id: Id,
    /// Lowercase hex, 40 characters, no 0x prefix — the canonical form.
    pub address: String,
    /// "evm" today.
    pub chain_type: String,
    /// User-chosen display label, if any.
    pub label: Option<String>,
    /// The `i` in `m/44'/60'/0'/0/i`, so a restore re-registers in order.
    pub derivation_index: i32,
    /// Active or archived.
    pub status: crate::model::WalletStatus,
    /// When the wallet was first registered.
    pub created_at: Timestamp,
    /// When the user archived it, if they did.
    pub archived_at: Option<Timestamp>,
}

/// Storage for the EVM wallet registry.
///
/// Display and recovery metadata only: no balances, no broadcasting, no RPC —
/// the brief is explicit that none of that exists in this version, and a store
/// method for it would be the first lie.
#[async_trait]
pub trait WalletStore: Send + Sync {
    /// Registers a wallet. Idempotent per `(account_id, chain_type, address)`:
    /// re-registering the same address updates the label and derivation index
    /// rather than failing, which is what a restore from a `.migo` container
    /// does.
    async fn put_wallet(&self, row: WalletRow) -> Result<()>;

    /// The account's wallets, active ones first, newest first within that.
    async fn wallets_for_account(&self, account_id: Id) -> Result<Vec<WalletRow>>;

    /// Archives a wallet. Unknown ids and other owners' wallets are `Ok(())`,
    /// because the list the client reads from already reflects the outcome.
    async fn archive_wallet(&self, wallet_id: Id, account_id: Id, at: Timestamp) -> Result<()>;
}

/// Everything, for the composition root.
///
/// Domain crates should depend on the narrow traits instead. This exists so
/// `migod` can hold one object, and so a backend swap is one line in one place.
#[async_trait]
pub trait Store:
    AccountStore
    + DeviceStore
    + SessionStore
    + KeyStore
    + MessagingStore
    + RoomStore
    + SocialStore
    + EconomyStore
    + ProgressionStore
    + GameStore
    + BotStore
    + FederationStore
    + MediaStore
    + NotifyStore
    + SafetyStore
    + CaptchaStore
    + RecoveryStore
    + IdentityStore
    + ChallengeStore
    + WalletStore
    + Send
    + Sync
    + 'static
{
    /// Human-readable backend name, for the startup banner and metrics labels.
    fn backend_name(&self) -> &'static str;

    /// Applies pending schema migrations. A no-op for backends that have no
    /// schema.
    async fn migrate(&self) -> Result<()>;

    /// Cheap liveness probe for the readiness endpoint. Must not be a query that
    /// gets slower as the database grows: a health check that times out under
    /// load takes the service out of rotation exactly when it is needed.
    async fn health(&self) -> Result<()>;
}
