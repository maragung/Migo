//! An in-memory backend.
//!
//! This is not a mock. It implements every trait in [`crate::traits`] with the
//! same visible behaviour as Postgres — the same uniqueness rules, the same
//! monotonic sequence numbers, the same idempotence, the same clamped pages — so
//! that a test which passes here is testing the domain logic rather than testing
//! a stub that agrees with it.
//!
//! It exists for three reasons:
//!
//! 1. **Deterministic simulation** (ADR-0009). A simulated run needs storage that
//!    cannot introduce timing of its own. There is no I/O here, so a replay with
//!    the same injected clock and randomness produces the same result.
//! 2. **Tests that stay fast.** A unit test suite that needs a database is a
//!    suite people stop running.
//! 3. **`migod` without Docker.** A contributor should be able to clone, build,
//!    and see the product work before installing anything.
//!
//! What it deliberately does *not* do is persist. Restarting loses everything,
//! and `migod` refuses to use this backend when `MIGO_ENV=production` — a store
//! that forgets is a data-loss incident waiting for its first deploy.
//!
//! # Locking
//!
//! One `RwLock` over one `State`, not a lock per table. Fine-grained locking
//! would buy throughput that a development backend does not need, and would allow
//! interleavings that Postgres serialises — which is the opposite of useful,
//! because then a test could pass here and fail in production. Nothing awaits
//! while the guard is held.

use std::collections::{HashMap, VecDeque};

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::{fault, ConversationKind, EncryptionMode, RelationshipKind, RoomRole};
use parking_lot::RwLock;

use crate::model::{
    advanced_token, game_status, notification_kind, report_status, Account, AccountStatus,
    AdvanceGame, Appended, AuditEntry, BadgeAward, Bot, Conversation, ConversationMember,
    ConversationPosition, ConversationSummary, Currency, Cursor, Device, Entitlement, GameSession,
    GiftSent, KeyBundle, LedgerAccount, LedgerAccountKind, LedgerTransaction, MediaObject,
    NewAccount, NewBot, NewDevice, NewGame, NewMessage, NewOutboxEvent, NewPeer, NewRoom,
    NewSession, NewTransaction, NewXpAward, Notification, OutboxRecord, Patch, PeerRecord, Posted,
    Profile, ProfilePatch, Progression, PublishedKeys, PushRegistration, PushTarget, Receipt,
    Relationship, Report, RevokeReason, Room, RoomMember, Scope, Session, Standing, StoredMessage,
    Visibility, XpChange,
};
use crate::traits::{
    canonical_country, clamp_limit, AccountStore, BotStore, CaptchaRow, CaptchaStore, DeviceStore,
    EconomyStore, FederationStore, GameStore, KeyStore, MediaStore, MessagingStore, NotifyStore,
    ProgressionStore, RecoveryRow, RecoveryStore, RoomKindFilter, RoomStore, SafetyStore,
    SessionStore, SocialStore, Store, MAX_LEDGER_LEGS,
};

/// Case-insensitive index key for a name, email, or slug.
///
/// One function so that the write path and the read path can never disagree
/// about what "the same username" means. Postgres does the same thing with a
/// `lower(...)` unique index.
fn fold(value: &str) -> String {
    value.trim().to_lowercase()
}

/// Orders a pair of ids so that the direct-conversation key is the same whoever
/// initiates. Postgres enforces the same thing with `check (low < high)`.
fn pair(a: Id, b: Id) -> (Id, Id) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// A device's published key material.
#[derive(Clone, Debug, Default)]
struct DeviceKeys {
    identity_key: Vec<u8>,
    signed_prekey_id: i32,
    signed_prekey: Vec<u8>,
    signed_prekey_signature: Vec<u8>,
    signed_prekey_expires_at: Timestamp,
    /// A queue, so one-time prekeys are consumed in publication order and the
    /// oldest is used first.
    one_time: VecDeque<(i32, Vec<u8>)>,
    revoked_at: Option<Timestamp>,
}

/// Everything the backend holds.
#[derive(Default)]
struct State {
    accounts: HashMap<Id, Account>,
    by_username: HashMap<String, Id>,
    by_email: HashMap<String, Id>,
    by_phone: HashMap<String, Id>,
    profiles: HashMap<Id, Profile>,

    devices: HashMap<Id, Device>,

    sessions: HashMap<Id, Session>,
    by_refresh: HashMap<Vec<u8>, Id>,

    keys: HashMap<(Id, Id), DeviceKeys>,

    conversations: HashMap<Id, Conversation>,
    conversation_members: HashMap<Id, Vec<ConversationMember>>,
    direct_index: HashMap<(Id, Id), Id>,
    /// Messages per conversation, always ascending by `seq`.
    messages: HashMap<Id, Vec<StoredMessage>>,
    /// `(conversation_id, message_id) -> seq`, for idempotent appends.
    message_seq: HashMap<(Id, Id), i64>,
    cursors: HashMap<(Id, Id), Cursor>,

    rooms: HashMap<Id, Room>,
    room_slugs: HashMap<String, Id>,
    room_members: HashMap<Id, Vec<RoomMember>>,

    relationships: HashMap<(Id, Id, RelationshipKind), Relationship>,

    ledger_accounts: HashMap<Id, LedgerAccount>,
    ledger_index: HashMap<(Option<Id>, LedgerAccountKind, Currency), Id>,
    transactions: HashMap<Id, LedgerTransaction>,
    /// Insertion order, because a ledger is append-only and a statement reads
    /// backwards through it.
    tx_order: Vec<Id>,
    tx_idempotency: HashMap<String, Id>,
    entries: HashMap<Id, Vec<(Id, i64)>>,
    gifts: HashMap<Id, GiftSent>,
    /// Insertion order, for the same reason as `tx_order`.
    gift_order: Vec<Id>,
    /// `tx_id -> gift_id`, which is the unique index the PostgreSQL backend gets
    /// from the schema. One transaction delivers one gift, and a retry that reuses
    /// the idempotency key must not deliver a second.
    gift_by_tx: HashMap<Id, Id>,
    entitlements: HashMap<(Id, String), Entitlement>,

    progression: HashMap<Id, Progression>,
    /// Every award, because a running total cannot be windowed. Section 32 ranks by
    /// week and section 30 caps by day, and both are sums over a range of time.
    xp_awards: HashMap<Id, NewXpAward>,
    /// Insertion order, for the same reason as `tx_order`.
    xp_order: Vec<Id>,
    /// The partial unique index on `xp_award.idempotency_key`, kept by hand.
    xp_keys: HashMap<String, Id>,
    badges: HashMap<(Id, String), BadgeAward>,

    media: HashMap<Id, MediaObject>,

    notifications: HashMap<Id, Notification>,
    /// Insertion order, so the inbox reads backwards through it. An inbox is
    /// append-only in the same way a ledger is.
    notification_order: Vec<Id>,
    /// `device_id -> registration`, kept beside `devices` rather than inside
    /// [`Device`] so that the struct `DeviceStore` hands out cannot carry a
    /// credential. The PostgreSQL backend achieves the same thing with a partial
    /// model over a wider table.
    push: HashMap<Id, (PushRegistration, Timestamp)>,

    reports: HashMap<Id, Report>,
    report_order: Vec<Id>,
    audit: Vec<AuditEntry>,

    games: HashMap<Id, GameSession>,
    /// Insertion order, so a conversation's open games list newest-first the same
    /// way Postgres does with `order by created_at desc`.
    game_order: Vec<Id>,

    bots: HashMap<Id, Bot>,
    /// Insertion order, so an owner's bots list newest-first the same way Postgres
    /// does with `order by created_at desc`.
    bot_order: Vec<Id>,
    /// `account_id -> bot_id`, the unique backing-account index the schema keeps as
    /// `bot.account_id unique`.
    bot_by_account: HashMap<Id, Id>,
    /// `token_hash -> bot_id`, the unique token index and the authentication lookup,
    /// the schema's `bot_token_hash_key`.
    bot_by_token: HashMap<Vec<u8>, Id>,

    /// The federation allow-list, keyed by node id, the `node_peer` table.
    peers: HashMap<String, PeerRecord>,
    /// Insertion order, so `peers` lists newest-first the same way Postgres does with
    /// `order by added_at desc`.
    peer_order: Vec<String>,
    /// `public_key -> node_id`, the unique key index the schema keeps as
    /// `node_peer_key_key`: two peers cannot share an identity.
    peer_by_key: HashMap<Vec<u8>, String>,

    /// The outbound federation queue, keyed by event id, the `federation_outbox`
    /// table.
    outbox: HashMap<Id, OutboxRecord>,
    /// Insertion order, the tiebreak for two events with the same `next_attempt_at`
    /// so the drain order is deterministic across a replay.
    outbox_order: Vec<Id>,

    /// Captcha challenges, keyed by `challenge_id`. Mirrors the
    /// `captcha_challenge` table; expired rows are dropped lazily on read.
    captcha: HashMap<Id, CaptchaRow>,

    /// Password-recovery tokens, keyed by `token_id`. Mirrors the
    /// `password_recovery` table; consumed or expired rows are dropped by
    /// the background sweeper.
    recovery: HashMap<Id, RecoveryRow>,
}

impl State {
    /// Whether an account is currently an active member of a conversation.
    fn member_of(&self, conversation_id: Id, account_id: Id) -> bool {
        self.conversation_members
            .get(&conversation_id)
            .is_some_and(|members| {
                members
                    .iter()
                    .any(|m| m.account_id == account_id && m.left_at.is_none())
            })
    }

    /// Live active members of a room.
    fn active_room_members(&self, room_id: Id) -> usize {
        self.room_members.get(&room_id).map_or(0, |members| {
            members.iter().filter(|m| m.is_active()).count()
        })
    }
}

/// The in-memory [`Store`].
#[derive(Default)]
pub struct MemoryStore {
    state: RwLock<State>,
}

impl MemoryStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Row counts, for tests and for the `/debug/store` endpoint in development.
    ///
    /// Returning counts rather than the data itself keeps this usable from a
    /// handler without turning it into a way to dump the database.
    #[must_use]
    pub fn counts(&self) -> Vec<(&'static str, usize)> {
        let s = self.state.read();
        vec![
            ("accounts", s.accounts.len()),
            ("devices", s.devices.len()),
            ("sessions", s.sessions.len()),
            ("conversations", s.conversations.len()),
            ("messages", s.messages.values().map(Vec::len).sum()),
            ("rooms", s.rooms.len()),
            ("relationships", s.relationships.len()),
            ("transactions", s.transactions.len()),
            ("reports", s.reports.len()),
            ("audit", s.audit.len()),
        ]
    }
}

#[async_trait]
impl AccountStore for MemoryStore {
    async fn create_account(&self, new: NewAccount) -> Result<Account> {
        let mut s = self.state.write();
        let username_key = fold(&new.username);
        if s.by_username.contains_key(&username_key) {
            return Err(fault::already_exists("username"));
        }
        let email_key = new.email.as_deref().map(fold);
        if let Some(key) = &email_key {
            if s.by_email.contains_key(key) {
                return Err(fault::already_exists("email"));
            }
        }
        let phone_key = new.phone.as_deref().map(fold);
        if let Some(key) = &phone_key {
            if s.by_phone.contains_key(key) {
                return Err(fault::already_exists("phone"));
            }
        }
        if s.accounts.contains_key(&new.account_id) {
            return Err(fault::already_exists("account id"));
        }

        let account = Account {
            account_id: new.account_id,
            username: new.username,
            email: new.email,
            phone: new.phone,
            password_hash: new.password_hash,
            status: AccountStatus::Active,
            country: canonical_country(new.country.as_deref())?,
            locale: new.locale,
            created_at: new.created_at,
            updated_at: new.created_at,
            last_login_at: None,
            suspended_until: None,
            deleted_at: None,
        };
        s.by_username.insert(username_key, account.account_id);
        if let Some(key) = email_key {
            s.by_email.insert(key, account.account_id);
        }
        if let Some(key) = phone_key {
            s.by_phone.insert(key, account.account_id);
        }
        s.accounts.insert(account.account_id, account.clone());
        Ok(account)
    }

    async fn account_by_id(&self, account_id: Id) -> Result<Option<Account>> {
        Ok(self.state.read().accounts.get(&account_id).cloned())
    }

    async fn account_by_username(&self, username: &str) -> Result<Option<Account>> {
        let s = self.state.read();
        Ok(s.by_username
            .get(&fold(username))
            .and_then(|id| s.accounts.get(id))
            .cloned())
    }

    async fn account_by_email(&self, email: &str) -> Result<Option<Account>> {
        let s = self.state.read();
        Ok(s.by_email
            .get(&fold(email))
            .and_then(|id| s.accounts.get(id))
            .cloned())
    }

    async fn account_by_phone(&self, phone: &str) -> Result<Option<Account>> {
        let s = self.state.read();
        Ok(s.accounts
            .values()
            .find(|a| a.phone.as_deref() == Some(phone))
            .cloned())
    }

    async fn set_password_hash(&self, account_id: Id, hash: &str, at: Timestamp) -> Result<()> {
        let mut s = self.state.write();
        let account = s
            .accounts
            .get_mut(&account_id)
            .ok_or_else(|| fault::not_found("account"))?;
        account.password_hash = migo_core::Secret::new(hash);
        account.updated_at = at;
        Ok(())
    }

    async fn set_contact(&self, account_id: Id, contact: &str, at: Timestamp) -> Result<()> {
        let trimmed = contact.trim();
        if trimmed.is_empty() {
            return Err(fault::validation("contact", "must not be empty"));
        }
        // Branch on the visible shape before locking the state: the kind
        // of contact decides what to validate, and the validation has to
        // happen before we touch the account row, so a malformed value
        // never has to be unwound.
        let new_email_lower: Option<String>;
        let new_email: Option<String>;
        let new_phone: Option<String>;
        if trimmed.contains('@') {
            if !trimmed.contains('.') || trimmed.split('@').count() != 2 {
                return Err(fault::validation("email", "domain needs a dot"));
            }
            new_email = Some(trimmed.to_string());
            new_email_lower = Some(fold(trimmed));
            new_phone = None;
        } else if trimmed.starts_with('+') {
            let normalised: String = trimmed
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '+')
                .collect();
            if normalised.len() < 9 {
                return Err(fault::validation("phone", "must contain at least 8 digits"));
            }
            new_email = None;
            new_email_lower = None;
            new_phone = Some(normalised);
        } else {
            return Err(fault::validation(
                "contact",
                "must be an email (containing @) or a phone (starting with +)",
            ));
        }
        let mut s = self.state.write();
        // Take the existing account out of the map by cloning its current
        // contact fields. Removing it lets us reinsert under the new
        // values without holding two mutable references to `s` at once.
        let account = s
            .accounts
            .get(&account_id)
            .cloned()
            .ok_or_else(|| fault::not_found("account"))?;
        // The two `by_*` indexes have to be kept consistent: removing
        // the old contact so it does not still resolve to this account
        // is part of the same write that adds the new one.
        if let Some(old) = account.email.as_deref() {
            s.by_email.remove(&fold(old));
        }
        if let Some(old) = account.phone.as_deref() {
            s.by_phone.remove(old);
        }
        // Collision check on the new key. `Some(other) != Some(account_id)`
        // is "this email or phone is already in use by another account".
        if let Some(key) = new_email_lower.as_deref() {
            if let Some(other) = s.by_email.get(key) {
                if *other != account_id {
                    return Err(fault::already_exists("email"));
                }
            }
        }
        if let Some(key) = new_phone.as_deref() {
            if let Some(other) = s.by_phone.get(key) {
                if *other != account_id {
                    return Err(fault::already_exists("phone"));
                }
            }
        }
        let mut updated = account;
        updated.email = new_email.clone();
        updated.phone = new_phone.clone();
        updated.updated_at = at;
        s.accounts.insert(account_id, updated);
        if let Some(key) = new_email_lower {
            s.by_email.insert(key, account_id);
        }
        if let Some(key) = new_phone {
            s.by_phone.insert(key, account_id);
        }
        Ok(())
    }

    async fn record_login(&self, account_id: Id, at: Timestamp) -> Result<()> {
        let mut s = self.state.write();
        let account = s
            .accounts
            .get_mut(&account_id)
            .ok_or_else(|| fault::not_found("account"))?;
        account.last_login_at = Some(at);
        Ok(())
    }

    async fn set_status(
        &self,
        account_id: Id,
        status: AccountStatus,
        until: Option<Timestamp>,
        at: Timestamp,
    ) -> Result<()> {
        let mut s = self.state.write();
        let account = s
            .accounts
            .get_mut(&account_id)
            .ok_or_else(|| fault::not_found("account"))?;
        account.status = status;
        account.suspended_until = until;
        account.updated_at = at;
        if status == AccountStatus::Deleted && account.deleted_at.is_none() {
            account.deleted_at = Some(at);
        }
        Ok(())
    }

    async fn profile(&self, account_id: Id) -> Result<Option<Profile>> {
        Ok(self.state.read().profiles.get(&account_id).cloned())
    }

    async fn create_profile(&self, profile: Profile) -> Result<Profile> {
        let mut s = self.state.write();
        if !s.accounts.contains_key(&profile.account_id) {
            return Err(fault::not_found("account"));
        }
        if s.profiles.contains_key(&profile.account_id) {
            return Err(fault::already_exists("profile"));
        }
        s.profiles.insert(profile.account_id, profile.clone());
        Ok(profile)
    }

    async fn update_profile(
        &self,
        account_id: Id,
        patch: ProfilePatch,
        at: Timestamp,
    ) -> Result<Profile> {
        let mut s = self.state.write();
        let profile = s
            .profiles
            .get_mut(&account_id)
            .ok_or_else(|| fault::not_found("profile"))?;
        if let Some(name) = patch.display_name {
            profile.display_name = name;
        }
        patch.bio.apply(&mut profile.bio);
        patch.avatar_media_id.apply(&mut profile.avatar_media_id);
        patch.birth_year.apply(&mut profile.birth_year);
        if let Some(v) = patch.show_last_seen {
            profile.show_last_seen = v;
        }
        if let Some(v) = patch.who_can_message {
            profile.who_can_message = v;
        }
        if let Some(v) = patch.who_can_add {
            profile.who_can_add = v;
        }
        if let Some(v) = patch.searchable {
            profile.searchable = v;
        }
        profile.updated_at = at;
        Ok(profile.clone())
    }

    async fn search_accounts(&self, query: &str, limit: u16) -> Result<Vec<(Account, Profile)>> {
        let needle = fold(query);
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let s = self.state.read();
        let mut hits: Vec<(Account, Profile)> = s
            .profiles
            .values()
            .filter(|p| p.searchable)
            .filter_map(|p| {
                let account = s.accounts.get(&p.account_id)?;
                if account.status != AccountStatus::Active {
                    return None;
                }
                let matches = fold(&account.username).starts_with(&needle)
                    || fold(&p.display_name).contains(&needle);
                matches.then(|| (account.clone(), p.clone()))
            })
            .collect();
        // Shorter usernames first, then alphabetically: a stable order matters
        // more than a clever one, because an unstable order makes paging repeat
        // and skip rows.
        hits.sort_by(|a, b| {
            a.0.username
                .len()
                .cmp(&b.0.username.len())
                .then_with(|| a.0.username.cmp(&b.0.username))
        });
        hits.truncate(clamp_limit(limit));
        Ok(hits)
    }
}

#[async_trait]
impl DeviceStore for MemoryStore {
    async fn register_device(&self, new: NewDevice) -> Result<Device> {
        let mut s = self.state.write();
        if !s.accounts.contains_key(&new.account_id) {
            return Err(fault::not_found("account"));
        }
        if s.devices.contains_key(&new.device_id) {
            return Err(fault::already_exists("device"));
        }
        let device = Device {
            device_id: new.device_id,
            account_id: new.account_id,
            platform: new.platform,
            display_name: new.display_name,
            app_version: new.app_version,
            os_version: new.os_version,
            device_model: new.device_model,
            created_at: new.created_at,
            last_seen_at: new.created_at,
            revoked_at: None,
        };
        s.devices.insert(device.device_id, device.clone());
        Ok(device)
    }

    async fn device_by_id(&self, device_id: Id) -> Result<Option<Device>> {
        Ok(self.state.read().devices.get(&device_id).cloned())
    }

    async fn devices_for_account(&self, account_id: Id) -> Result<Vec<Device>> {
        let s = self.state.read();
        let mut devices: Vec<Device> = s
            .devices
            .values()
            .filter(|d| d.account_id == account_id && d.revoked_at.is_none())
            .cloned()
            .collect();
        devices.sort_by_key(|d| (d.created_at, d.device_id));
        Ok(devices)
    }

    async fn touch_device(&self, device_id: Id, at: Timestamp) -> Result<()> {
        let mut s = self.state.write();
        if let Some(device) = s.devices.get_mut(&device_id) {
            // Monotonic: a device whose clock ran backwards must not make its own
            // last-seen go backwards either.
            if at > device.last_seen_at {
                device.last_seen_at = at;
            }
        }
        Ok(())
    }

    async fn revoke_device(&self, device_id: Id, at: Timestamp) -> Result<()> {
        let mut s = self.state.write();
        if let Some(device) = s.devices.get_mut(&device_id) {
            if device.revoked_at.is_none() {
                device.revoked_at = Some(at);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn create_session(&self, new: NewSession) -> Result<Session> {
        let mut s = self.state.write();
        if s.by_refresh.contains_key(&new.refresh_hash) {
            return Err(fault::already_exists("refresh token"));
        }
        let session = Session {
            session_id: new.session_id,
            account_id: new.account_id,
            device_id: new.device_id,
            family_id: new.family_id,
            refresh_hash: new.refresh_hash,
            generation: new.generation,
            created_at: new.created_at,
            authenticated_at: new.authenticated_at,
            rotated_at: None,
            access_expires_at: new.access_expires_at,
            refresh_expires_at: new.refresh_expires_at,
            revoked_at: None,
            revoked_reason: None,
            ip_class: new.ip_class,
            user_agent: new.user_agent,
        };
        s.by_refresh
            .insert(session.refresh_hash.clone(), session.session_id);
        s.sessions.insert(session.session_id, session.clone());
        Ok(session)
    }

    async fn session_by_id(&self, session_id: Id) -> Result<Option<Session>> {
        Ok(self.state.read().sessions.get(&session_id).cloned())
    }

    async fn session_by_refresh_hash(&self, hash: &[u8]) -> Result<Option<Session>> {
        let s = self.state.read();
        Ok(s.by_refresh
            .get(hash)
            .and_then(|id| s.sessions.get(id))
            .cloned())
    }

    async fn rotate_session(&self, previous: Id, next: NewSession) -> Result<Session> {
        let mut s = self.state.write();
        let old = s
            .sessions
            .get(&previous)
            .ok_or_else(|| fault::not_found("session"))?;
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
        if s.by_refresh.contains_key(&next.refresh_hash) {
            return Err(fault::already_exists("refresh token"));
        }
        let rotated_at = next.created_at;

        let session = Session {
            session_id: next.session_id,
            account_id: next.account_id,
            device_id: next.device_id,
            family_id: next.family_id,
            refresh_hash: next.refresh_hash,
            generation: next.generation,
            created_at: next.created_at,
            authenticated_at: next.authenticated_at,
            rotated_at: None,
            access_expires_at: next.access_expires_at,
            refresh_expires_at: next.refresh_expires_at,
            revoked_at: None,
            revoked_reason: None,
            ip_class: next.ip_class,
            user_agent: next.user_agent,
        };
        s.by_refresh
            .insert(session.refresh_hash.clone(), session.session_id);
        s.sessions.insert(session.session_id, session.clone());
        if let Some(old) = s.sessions.get_mut(&previous) {
            old.rotated_at = Some(rotated_at);
        }
        Ok(session)
    }

    async fn revoke_session(
        &self,
        session_id: Id,
        reason: RevokeReason,
        at: Timestamp,
    ) -> Result<()> {
        let mut s = self.state.write();
        if let Some(session) = s.sessions.get_mut(&session_id) {
            if session.revoked_at.is_none() {
                session.revoked_at = Some(at);
                session.revoked_reason = Some(reason);
            }
        }
        Ok(())
    }

    async fn revoke_family(
        &self,
        family_id: Id,
        reason: RevokeReason,
        at: Timestamp,
    ) -> Result<u64> {
        let mut s = self.state.write();
        let mut revoked = 0;
        for session in s.sessions.values_mut() {
            if session.family_id == family_id && session.revoked_at.is_none() {
                session.revoked_at = Some(at);
                session.revoked_reason = Some(reason);
                revoked += 1;
            }
        }
        Ok(revoked)
    }

    async fn revoke_account_sessions(
        &self,
        account_id: Id,
        except: Option<Id>,
        reason: RevokeReason,
        at: Timestamp,
    ) -> Result<u64> {
        let mut s = self.state.write();
        let mut revoked = 0;
        for session in s.sessions.values_mut() {
            if session.account_id != account_id || session.revoked_at.is_some() {
                continue;
            }
            if Some(session.session_id) == except {
                continue;
            }
            session.revoked_at = Some(at);
            session.revoked_reason = Some(reason);
            revoked += 1;
        }
        Ok(revoked)
    }

    async fn sessions_for_account(&self, account_id: Id, now: Timestamp) -> Result<Vec<Session>> {
        let s = self.state.read();
        let mut live: Vec<Session> = s
            .sessions
            .values()
            .filter(|session| session.account_id == account_id && session.is_live(now))
            .cloned()
            .collect();
        live.sort_by_key(|session| (session.created_at, session.session_id));
        Ok(live)
    }

    async fn purge_expired_sessions(&self, before: Timestamp) -> Result<u64> {
        let mut s = self.state.write();
        let doomed: Vec<Id> = s
            .sessions
            .values()
            .filter(|session| session.refresh_expires_at < before)
            .map(|session| session.session_id)
            .collect();
        for id in &doomed {
            if let Some(session) = s.sessions.remove(id) {
                s.by_refresh.remove(&session.refresh_hash);
            }
        }
        Ok(doomed.len() as u64)
    }
}

#[async_trait]
impl KeyStore for MemoryStore {
    async fn publish_keys(&self, keys: PublishedKeys) -> Result<()> {
        let mut s = self.state.write();
        if !s.devices.contains_key(&keys.device_id) {
            return Err(fault::not_found("device"));
        }
        if keys.signed_prekey_expires_at <= keys.created_at {
            // A prekey that has already expired on arrival can only produce
            // sessions that fail later, for a reason nobody will connect back to
            // this moment.
            return Err(fault::validation(
                "signed_prekey_expires_at",
                "must be in the future",
            ));
        }
        s.keys.insert(
            (keys.account_id, keys.device_id),
            DeviceKeys {
                identity_key: keys.identity_key,
                signed_prekey_id: keys.signed_prekey_id,
                signed_prekey: keys.signed_prekey,
                signed_prekey_signature: keys.signed_prekey_signature,
                signed_prekey_expires_at: keys.signed_prekey_expires_at,
                one_time: keys.one_time_prekeys.into_iter().collect(),
                revoked_at: None,
            },
        );
        Ok(())
    }

    async fn add_one_time_prekeys(
        &self,
        account_id: Id,
        device_id: Id,
        prekeys: Vec<(i32, Vec<u8>)>,
        _at: Timestamp,
    ) -> Result<u64> {
        let mut s = self.state.write();
        let entry = s
            .keys
            .get_mut(&(account_id, device_id))
            .ok_or_else(|| fault::not_found("published keys"))?;
        let mut added = 0;
        for (id, key) in prekeys {
            // Republishing an id must not replace the key behind it: two peers
            // holding different bytes for the same prekey id would each derive a
            // session the other cannot read.
            if entry.one_time.iter().any(|(existing, _)| *existing == id) {
                continue;
            }
            entry.one_time.push_back((id, key));
            added += 1;
        }
        Ok(added)
    }

    async fn take_key_bundle(&self, account_id: Id, device_id: Id) -> Result<Option<KeyBundle>> {
        let mut s = self.state.write();
        let Some(entry) = s.keys.get_mut(&(account_id, device_id)) else {
            return Ok(None);
        };
        if entry.revoked_at.is_some() {
            return Ok(None);
        }
        let one_time = entry.one_time.pop_front();
        Ok(Some(KeyBundle {
            account_id,
            device_id,
            identity_key: entry.identity_key.clone(),
            signed_prekey_id: entry.signed_prekey_id,
            signed_prekey: entry.signed_prekey.clone(),
            signed_prekey_signature: entry.signed_prekey_signature.clone(),
            signed_prekey_expires_at: entry.signed_prekey_expires_at,
            one_time_prekey: one_time,
        }))
    }

    async fn take_key_bundles_for_account(&self, account_id: Id) -> Result<Vec<KeyBundle>> {
        let device_ids: Vec<Id> = {
            let s = self.state.read();
            let mut ids: Vec<Id> = s
                .devices
                .values()
                .filter(|d| d.account_id == account_id && d.revoked_at.is_none())
                .map(|d| d.device_id)
                .collect();
            ids.sort_unstable();
            ids
        };
        let mut bundles = Vec::with_capacity(device_ids.len());
        for device_id in device_ids {
            if let Some(bundle) = self.take_key_bundle(account_id, device_id).await? {
                bundles.push(bundle);
            }
        }
        Ok(bundles)
    }

    async fn one_time_prekey_count(&self, account_id: Id, device_id: Id) -> Result<u32> {
        let s = self.state.read();
        Ok(s.keys
            .get(&(account_id, device_id))
            .map_or(0, |entry| entry.one_time.len() as u32))
    }

    async fn revoke_device_keys(&self, account_id: Id, device_id: Id, at: Timestamp) -> Result<()> {
        let mut s = self.state.write();
        if let Some(entry) = s.keys.get_mut(&(account_id, device_id)) {
            entry.revoked_at = Some(at);
            // Unconsumed prekeys go immediately: handing out key material for a
            // device that no longer exists can only produce sessions nobody can
            // read.
            entry.one_time.clear();
        }
        Ok(())
    }
}

#[async_trait]
impl MessagingStore for MemoryStore {
    async fn create_conversation(
        &self,
        conversation: Conversation,
        members: Vec<Id>,
    ) -> Result<Conversation> {
        let mut s = self.state.write();
        if s.conversations.contains_key(&conversation.conversation_id) {
            return Err(fault::already_exists("conversation"));
        }
        let rows: Vec<ConversationMember> = members
            .into_iter()
            .map(|account_id| ConversationMember {
                conversation_id: conversation.conversation_id,
                account_id,
                role: 0,
                joined_at: conversation.created_at,
                left_at: None,
                muted_until: None,
                pinned: false,
            })
            .collect();
        s.conversation_members
            .insert(conversation.conversation_id, rows);
        s.conversations
            .insert(conversation.conversation_id, conversation.clone());
        Ok(conversation)
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
        let mut s = self.state.write();
        let key = pair(a, b);
        if let Some(existing) = s.direct_index.get(&key) {
            // The loser of the race reads the winner's row. Two devices tapping
            // "message Bob" at the same instant must not produce two threads.
            let id = *existing;
            return s
                .conversations
                .get(&id)
                .cloned()
                .ok_or_else(|| fault::internal("direct index points at a missing conversation"));
        }
        let conversation = Conversation {
            conversation_id,
            kind: ConversationKind::Direct,
            encryption,
            room_id: None,
            last_seq: 0,
            created_by: a,
            created_at: at,
            last_message_at: None,
            archived_at: None,
        };
        let rows = [a, b]
            .into_iter()
            .map(|account_id| ConversationMember {
                conversation_id,
                account_id,
                role: 0,
                joined_at: at,
                left_at: None,
                muted_until: None,
                pinned: false,
            })
            .collect();
        s.direct_index.insert(key, conversation_id);
        s.conversation_members.insert(conversation_id, rows);
        s.conversations
            .insert(conversation_id, conversation.clone());
        Ok(conversation)
    }

    async fn conversation(&self, conversation_id: Id) -> Result<Option<Conversation>> {
        Ok(self
            .state
            .read()
            .conversations
            .get(&conversation_id)
            .cloned())
    }

    async fn members(&self, conversation_id: Id) -> Result<Vec<ConversationMember>> {
        Ok(self
            .state
            .read()
            .conversation_members
            .get(&conversation_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn is_member(&self, conversation_id: Id, account_id: Id) -> Result<bool> {
        Ok(self.state.read().member_of(conversation_id, account_id))
    }

    async fn add_member(&self, member: ConversationMember) -> Result<()> {
        let mut s = self.state.write();
        if !s.conversations.contains_key(&member.conversation_id) {
            return Err(fault::not_found("conversation"));
        }
        let rows = s
            .conversation_members
            .entry(member.conversation_id)
            .or_default();
        if let Some(existing) = rows.iter_mut().find(|m| m.account_id == member.account_id) {
            // Rejoining clears the departure but keeps the original join time, so
            // "member since" does not reset every time somebody leaves and comes
            // back.
            existing.left_at = None;
            existing.role = member.role;
            return Ok(());
        }
        rows.push(member);
        Ok(())
    }

    async fn remove_member(
        &self,
        conversation_id: Id,
        account_id: Id,
        at: Timestamp,
    ) -> Result<()> {
        let mut s = self.state.write();
        if let Some(rows) = s.conversation_members.get_mut(&conversation_id) {
            if let Some(member) = rows.iter_mut().find(|m| m.account_id == account_id) {
                if member.left_at.is_none() {
                    member.left_at = Some(at);
                }
            }
        }
        Ok(())
    }

    async fn append_message(&self, new: NewMessage) -> Result<Appended> {
        let mut s = self.state.write();
        if !s.conversations.contains_key(&new.conversation_id) {
            return Err(fault::not_found("conversation"));
        }
        if let Some(seq) = s.message_seq.get(&(new.conversation_id, new.message_id)) {
            let seq = *seq;
            let existing = s
                .messages
                .get(&new.conversation_id)
                .and_then(|list| list.iter().find(|m| m.seq == seq))
                .cloned()
                .ok_or_else(|| fault::internal("message index points at a missing row"))?;
            return Ok(Appended::Duplicate(existing));
        }

        let seq = {
            let conversation = s
                .conversations
                .get_mut(&new.conversation_id)
                .ok_or_else(|| fault::not_found("conversation"))?;
            conversation.last_seq += 1;
            conversation.last_message_at = Some(new.created_at);
            conversation.last_seq
        };

        let message = StoredMessage {
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
        };
        s.message_seq
            .insert((new.conversation_id, new.message_id), seq);
        s.messages
            .entry(new.conversation_id)
            .or_default()
            .push(message.clone());
        Ok(Appended::Created(message))
    }

    async fn message(&self, conversation_id: Id, message_id: Id) -> Result<Option<StoredMessage>> {
        let s = self.state.read();
        let Some(seq) = s.message_seq.get(&(conversation_id, message_id)) else {
            return Ok(None);
        };
        Ok(s.messages
            .get(&conversation_id)
            .and_then(|list| list.iter().find(|m| m.seq == *seq))
            .cloned())
    }

    async fn history_before(
        &self,
        conversation_id: Id,
        before_seq: Option<i64>,
        limit: u16,
    ) -> Result<Vec<StoredMessage>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        let Some(list) = s.messages.get(&conversation_id) else {
            return Ok(Vec::new());
        };
        // The list is ascending by seq, so the exclusive upper bound is a
        // partition point rather than a scan.
        let end = match before_seq {
            Some(before) => list.partition_point(|m| m.seq < before),
            None => list.len(),
        };
        let start = end.saturating_sub(limit);
        let mut page: Vec<StoredMessage> = list[start..end].to_vec();
        page.reverse();
        Ok(page)
    }

    async fn history_after(
        &self,
        conversation_id: Id,
        after_seq: i64,
        limit: u16,
    ) -> Result<Vec<StoredMessage>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        let Some(list) = s.messages.get(&conversation_id) else {
            return Ok(Vec::new());
        };
        let start = list.partition_point(|m| m.seq <= after_seq);
        let end = (start + limit).min(list.len());
        Ok(list[start..end].to_vec())
    }

    async fn edit_message(
        &self,
        conversation_id: Id,
        message_id: Id,
        envelope: Vec<u8>,
        at: Timestamp,
    ) -> Result<Option<StoredMessage>> {
        let mut s = self.state.write();
        let Some(seq) = s.message_seq.get(&(conversation_id, message_id)).copied() else {
            return Ok(None);
        };
        let Some(list) = s.messages.get_mut(&conversation_id) else {
            return Ok(None);
        };
        let Some(message) = list.iter_mut().find(|m| m.seq == seq) else {
            return Ok(None);
        };
        if message.deleted_at.is_some() {
            // Editing a tombstone would resurrect content the sender asked to be
            // gone. That is a caller bug, so it gets an error rather than a
            // silently ignored write.
            return Err(fault::conflict("message is deleted"));
        }
        message.envelope = envelope;
        message.edited_at = Some(at);
        Ok(Some(message.clone()))
    }

    async fn delete_message(
        &self,
        conversation_id: Id,
        message_id: Id,
        by: Id,
        at: Timestamp,
    ) -> Result<Option<StoredMessage>> {
        let mut s = self.state.write();
        let Some(seq) = s.message_seq.get(&(conversation_id, message_id)).copied() else {
            return Ok(None);
        };
        let Some(list) = s.messages.get_mut(&conversation_id) else {
            return Ok(None);
        };
        let Some(message) = list.iter_mut().find(|m| m.seq == seq) else {
            return Ok(None);
        };
        if message.deleted_at.is_none() {
            message.deleted_at = Some(at);
            message.deleted_by = Some(by);
        }
        // The row stays so every client converges on the deletion, but the
        // payload goes now. Keeping the ciphertext of a deleted message would
        // mean "delete" only removed it from one screen.
        message.envelope.clear();
        Ok(Some(message.clone()))
    }

    async fn cursor(&self, conversation_id: Id, account_id: Id) -> Result<Cursor> {
        Ok(self
            .state
            .read()
            .cursors
            .get(&(conversation_id, account_id))
            .copied()
            .unwrap_or_default())
    }

    async fn advance_cursor(
        &self,
        conversation_id: Id,
        account_id: Id,
        delivered_seq: Option<i64>,
        read_seq: Option<i64>,
        notified_seq: Option<i64>,
        _at: Timestamp,
    ) -> Result<Cursor> {
        let mut s = self.state.write();
        let last_seq = s
            .conversations
            .get(&conversation_id)
            .map(|c| c.last_seq)
            .ok_or_else(|| fault::not_found("conversation"))?;
        let cursor = s.cursors.entry((conversation_id, account_id)).or_default();
        // Forward only, and never past the end. A client that reports having read
        // message 9000 in a conversation with 12 messages is either confused or
        // probing; either way the stored value stays sane.
        if let Some(seq) = delivered_seq {
            cursor.delivered_seq = cursor.delivered_seq.max(seq.min(last_seq));
        }
        if let Some(seq) = read_seq {
            cursor.read_seq = cursor.read_seq.max(seq.min(last_seq));
        }
        if let Some(seq) = notified_seq {
            cursor.notified_seq = cursor.notified_seq.max(seq.min(last_seq));
        }
        // Reading implies delivery: a client that reports a read without the
        // delivery that preceded it should not leave the two inconsistent.
        cursor.delivered_seq = cursor.delivered_seq.max(cursor.read_seq);
        Ok(*cursor)
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
        let s = self.state.read();
        let mut summaries: Vec<ConversationSummary> = s
            .conversations
            .values()
            .filter(|c| s.member_of(c.conversation_id, account_id))
            // The keyset is applied before the summaries are built rather than
            // after: the rows it excludes would otherwise each cost a cursor
            // lookup, a last-message clone, and a member scan to be thrown away.
            .filter(|c| after.is_none_or(|position| position.precedes(c)))
            .map(|conversation| {
                let cursor = s
                    .cursors
                    .get(&(conversation.conversation_id, account_id))
                    .copied()
                    .unwrap_or_default();
                let last_message = s
                    .messages
                    .get(&conversation.conversation_id)
                    .and_then(|list| list.last())
                    .cloned();
                let rows = s.conversation_members.get(&conversation.conversation_id);
                let mut members: Vec<Id> = rows
                    .map(|rows| {
                        rows.iter()
                            .filter(|m| m.left_at.is_none())
                            .map(|m| m.account_id)
                            .collect()
                    })
                    .unwrap_or_default();
                members.sort_unstable();
                members.truncate(preview);
                // The membership filter above already established that this row
                // exists; the fallback keeps the expression infallible rather
                // than making a list read able to fail on a torn state.
                let member = rows
                    .and_then(|rows| rows.iter().find(|m| m.account_id == account_id))
                    .cloned()
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
                    conversation: conversation.clone(),
                    last_message,
                    unread: (conversation.last_seq - cursor.read_seq).max(0),
                    cursor,
                    member,
                    members,
                }
            })
            .collect();
        // Most recent activity first, with the id as a tiebreaker so the order is
        // total. An order that is merely "mostly sorted" makes paging drop rows.
        // This must stay in step with `ConversationPosition::precedes`, which is
        // the same order expressed as a predicate.
        summaries.sort_by(|a, b| {
            b.conversation
                .last_message_at
                .cmp(&a.conversation.last_message_at)
                .then_with(|| b.conversation.created_at.cmp(&a.conversation.created_at))
                .then_with(|| {
                    a.conversation
                        .conversation_id
                        .cmp(&b.conversation.conversation_id)
                })
        });
        summaries.truncate(limit);
        Ok(summaries)
    }

    async fn conversations_with_unread(&self, account_id: Id) -> Result<Vec<(Id, i64, i64)>> {
        let s = self.state.read();
        let mut out: Vec<(Id, i64, i64)> = s
            .conversations
            .values()
            .filter(|c| s.member_of(c.conversation_id, account_id))
            .filter_map(|c| {
                let read = s
                    .cursors
                    .get(&(c.conversation_id, account_id))
                    .map_or(0, |cursor| cursor.read_seq);
                (c.last_seq > read).then_some((c.conversation_id, c.last_seq, read))
            })
            .collect();
        out.sort_unstable_by_key(|(id, _, _)| *id);
        Ok(out)
    }

    async fn purge_expired_messages(&self, before: Timestamp, limit: u16) -> Result<u64> {
        let budget = clamp_limit(limit);
        let mut s = self.state.write();
        let mut removed = 0usize;
        // A deterministic order over conversations, so a run that hits the budget
        // makes the same progress every time. Iterating a HashMap directly would
        // purge a different subset on every call.
        let mut conversation_ids: Vec<Id> = s.messages.keys().copied().collect();
        conversation_ids.sort_unstable();
        for conversation_id in conversation_ids {
            if removed >= budget {
                break;
            }
            let Some(list) = s.messages.get_mut(&conversation_id) else {
                continue;
            };
            let mut doomed = Vec::new();
            for message in list.iter() {
                if removed + doomed.len() >= budget {
                    break;
                }
                if message.expires_at.is_some_and(|expiry| expiry <= before) {
                    doomed.push((message.seq, message.message_id));
                }
            }
            if doomed.is_empty() {
                continue;
            }
            list.retain(|m| !doomed.iter().any(|(seq, _)| *seq == m.seq));
            for (_, message_id) in &doomed {
                s.message_seq.remove(&(conversation_id, *message_id));
            }
            removed += doomed.len();
        }
        Ok(removed as u64)
    }
}

#[async_trait]
impl RoomStore for MemoryStore {
    async fn create_room(&self, new: NewRoom) -> Result<Room> {
        let mut s = self.state.write();
        let slug_key = fold(&new.slug);
        if s.room_slugs.contains_key(&slug_key) {
            return Err(fault::already_exists("room slug"));
        }
        if s.rooms.contains_key(&new.room_id) {
            return Err(fault::already_exists("room"));
        }
        if s.conversations.contains_key(&new.conversation_id) {
            return Err(fault::already_exists("conversation"));
        }
        if new.max_members <= 0 {
            return Err(fault::validation("max_members", "must be positive"));
        }

        let room = Room {
            room_id: new.room_id,
            conversation_id: new.conversation_id,
            slug: new.slug,
            name: new.name,
            topic: new.topic,
            kind: new.kind,
            owner_id: new.owner_id,
            home_region: new.home_region,
            member_count: 1,
            max_members: new.max_members,
            slow_mode_seconds: 0,
            join_policy: crate::model::join_policy::OPEN,
            encryption: new.encryption,
            created_at: new.created_at,
            updated_at: new.created_at,
            archived_at: None,
        };
        // The room, its conversation, and the owner's membership are one unit. A
        // room without a conversation is unusable and a room without an owner is
        // unmoderatable, so none of the three may exist alone.
        let conversation = Conversation {
            conversation_id: room.conversation_id,
            kind: ConversationKind::Room,
            encryption: room.encryption,
            room_id: Some(room.room_id),
            last_seq: 0,
            created_by: room.owner_id,
            created_at: room.created_at,
            last_message_at: None,
            archived_at: None,
        };
        s.conversation_members.insert(
            conversation.conversation_id,
            vec![ConversationMember {
                conversation_id: conversation.conversation_id,
                account_id: room.owner_id,
                role: RoomRole::Owner.to_wire() as i16,
                joined_at: room.created_at,
                left_at: None,
                muted_until: None,
                pinned: false,
            }],
        );
        s.conversations
            .insert(conversation.conversation_id, conversation);
        s.room_members.insert(
            room.room_id,
            vec![RoomMember {
                room_id: room.room_id,
                account_id: room.owner_id,
                role: RoomRole::Owner,
                permissions_grant: 0,
                permissions_deny: 0,
                joined_at: room.created_at,
                left_at: None,
                muted_until: None,
                banned_until: None,
                ban_reason: None,
                invited_by: None,
            }],
        );
        s.room_slugs.insert(slug_key, room.room_id);
        s.rooms.insert(room.room_id, room.clone());
        Ok(room)
    }

    async fn room(&self, room_id: Id) -> Result<Option<Room>> {
        Ok(self.state.read().rooms.get(&room_id).cloned())
    }

    async fn room_by_slug(&self, slug: &str) -> Result<Option<Room>> {
        let s = self.state.read();
        Ok(s.room_slugs
            .get(&fold(slug))
            .and_then(|id| s.rooms.get(id))
            .cloned())
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
        let mut s = self.state.write();
        let room = s
            .rooms
            .get_mut(&room_id)
            .ok_or_else(|| fault::not_found("room"))?;
        if let Some(name) = name {
            room.name = name;
        }
        topic.apply(&mut room.topic);
        if let Some(seconds) = slow_mode_seconds {
            if seconds < 0 {
                return Err(fault::validation(
                    "slow_mode_seconds",
                    "must not be negative",
                ));
            }
            room.slow_mode_seconds = seconds;
        }
        if let Some(policy) = join_policy {
            room.join_policy = policy;
        }
        room.updated_at = at;
        Ok(room.clone())
    }

    async fn archive_room(&self, room_id: Id, at: Timestamp) -> Result<()> {
        let mut s = self.state.write();
        let conversation_id = match s.rooms.get_mut(&room_id) {
            Some(room) if room.archived_at.is_none() => {
                room.archived_at = Some(at);
                room.updated_at = at;
                Some(room.conversation_id)
            }
            _ => None,
        };
        if let Some(conversation_id) = conversation_id {
            if let Some(conversation) = s.conversations.get_mut(&conversation_id) {
                conversation.archived_at = Some(at);
            }
        }
        Ok(())
    }

    async fn browse_rooms(&self, kind: Option<RoomKindFilter>, limit: u16) -> Result<Vec<Room>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        let mut rooms: Vec<Room> = s
            .rooms
            .values()
            .filter(|room| room.archived_at.is_none())
            .filter(|room| match kind {
                None => true,
                Some(RoomKindFilter::Public) => room.kind == migo_protocol::RoomKind::Public,
                Some(RoomKindFilter::Managed) => room.kind == migo_protocol::RoomKind::Managed,
            })
            .cloned()
            .collect();
        rooms.sort_by(|a, b| {
            b.member_count
                .cmp(&a.member_count)
                .then_with(|| a.created_at.cmp(&b.created_at))
                .then_with(|| a.room_id.cmp(&b.room_id))
        });
        rooms.truncate(limit);
        Ok(rooms)
    }

    async fn join_room(&self, member: RoomMember) -> Result<RoomMember> {
        let mut s = self.state.write();
        let room = s
            .rooms
            .get(&member.room_id)
            .cloned()
            .ok_or_else(|| fault::not_found("room"))?;
        if room.archived_at.is_some() {
            return Err(fault::conflict("room is archived"));
        }
        let conversation_id = room.conversation_id;
        let already_active = s
            .room_members
            .get(&member.room_id)
            .and_then(|rows| rows.iter().find(|m| m.account_id == member.account_id))
            .is_some_and(RoomMember::is_active);
        // Capacity is checked here, inside the same critical section as the
        // insert, because a check in the caller is a race: two joins that both
        // read "one seat left" would both take it.
        if !already_active
            && s.active_room_members(member.room_id) >= room.max_members.max(0) as usize
        {
            return Err(fault::conflict("room is full"));
        }

        let account_id = member.account_id;
        let rows = s.room_members.entry(member.room_id).or_default();
        let stored = if let Some(existing) = rows.iter_mut().find(|m| m.account_id == account_id) {
            // Rejoining clears the departure but keeps the sanctions. A ban that
            // could be shed by leaving and coming back would not be a ban; the
            // caller checks `is_banned` before ever getting here.
            existing.left_at = None;
            existing.invited_by = member.invited_by.or(existing.invited_by);
            existing.clone()
        } else {
            rows.push(member.clone());
            member
        };
        if !already_active {
            let count = s.active_room_members(stored.room_id) as i32;
            if let Some(room) = s.rooms.get_mut(&stored.room_id) {
                room.member_count = count;
            }
            let members = s.conversation_members.entry(conversation_id).or_default();
            if let Some(existing) = members.iter_mut().find(|m| m.account_id == account_id) {
                existing.left_at = None;
            } else {
                members.push(ConversationMember {
                    conversation_id,
                    account_id,
                    role: stored.role.to_wire() as i16,
                    joined_at: stored.joined_at,
                    left_at: None,
                    muted_until: None,
                    pinned: false,
                });
            }
        }
        Ok(stored)
    }

    async fn leave_room(&self, room_id: Id, account_id: Id, at: Timestamp) -> Result<()> {
        let mut s = self.state.write();
        let Some(conversation_id) = s.rooms.get(&room_id).map(|room| room.conversation_id) else {
            return Ok(());
        };
        let left = s
            .room_members
            .get_mut(&room_id)
            .and_then(|rows| rows.iter_mut().find(|m| m.account_id == account_id))
            .filter(|m| m.left_at.is_none())
            .map(|m| {
                m.left_at = Some(at);
            })
            .is_some();
        if left {
            let count = s.active_room_members(room_id) as i32;
            if let Some(room) = s.rooms.get_mut(&room_id) {
                room.member_count = count;
            }
            if let Some(members) = s.conversation_members.get_mut(&conversation_id) {
                if let Some(member) = members.iter_mut().find(|m| m.account_id == account_id) {
                    member.left_at = Some(at);
                }
            }
        }
        Ok(())
    }

    async fn room_member(&self, room_id: Id, account_id: Id) -> Result<Option<RoomMember>> {
        let s = self.state.read();
        Ok(s.room_members
            .get(&room_id)
            .and_then(|rows| rows.iter().find(|m| m.account_id == account_id))
            .cloned())
    }

    async fn room_members(
        &self,
        room_id: Id,
        limit: u16,
        after: Option<Id>,
    ) -> Result<Vec<RoomMember>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        let Some(rows) = s.room_members.get(&room_id) else {
            return Ok(Vec::new());
        };
        let mut roster: Vec<RoomMember> = rows.iter().filter(|m| m.is_active()).cloned().collect();
        // Highest role first, then longest-standing, then by id so the order is
        // total and the keyset cursor below is unambiguous.
        roster.sort_by(|a, b| {
            b.role
                .to_wire()
                .cmp(&a.role.to_wire())
                .then_with(|| a.joined_at.cmp(&b.joined_at))
                .then_with(|| a.account_id.cmp(&b.account_id))
        });
        let start = match after {
            Some(cursor) => roster
                .iter()
                .position(|m| m.account_id == cursor)
                .map_or(0, |index| index + 1),
            None => 0,
        };
        let end = (start + limit).min(roster.len());
        Ok(roster[start..end].to_vec())
    }

    async fn rooms_for_account(&self, account_id: Id) -> Result<Vec<Room>> {
        let s = self.state.read();
        let mut rooms: Vec<Room> = s
            .room_members
            .iter()
            .filter(|(_, rows)| {
                rows.iter()
                    .any(|m| m.account_id == account_id && m.is_active())
            })
            .filter_map(|(room_id, _)| s.rooms.get(room_id).cloned())
            .collect();
        rooms.sort_by_key(|room| (room.created_at, room.room_id));
        Ok(rooms)
    }

    async fn set_room_role(
        &self,
        room_id: Id,
        account_id: Id,
        role: RoomRole,
        _at: Timestamp,
    ) -> Result<()> {
        let mut s = self.state.write();
        let member = s
            .room_members
            .get_mut(&room_id)
            .and_then(|rows| rows.iter_mut().find(|m| m.account_id == account_id))
            .ok_or_else(|| fault::not_found("room member"))?;
        member.role = role;
        Ok(())
    }

    async fn transfer_room_ownership(
        &self,
        room_id: Id,
        from: Id,
        to: Id,
        at: Timestamp,
    ) -> Result<()> {
        let mut s = self.state.write();
        let room = s
            .rooms
            .get(&room_id)
            .cloned()
            .ok_or_else(|| fault::not_found("room"))?;
        if room.owner_id != from {
            return Err(fault::conflict("not the owner of the room"));
        }
        if from == to {
            return Ok(());
        }
        let rows = s
            .room_members
            .get_mut(&room_id)
            .ok_or_else(|| fault::not_found("room member"))?;
        // Checked before either write, because the whole point of doing this in the
        // store is that a half-applied transfer must not be reachable.
        if !rows
            .iter()
            .any(|m| m.account_id == to && m.left_at.is_none())
        {
            return Err(fault::not_found("room member"));
        }
        for member in rows.iter_mut() {
            if member.account_id == to {
                member.role = RoomRole::Owner;
            } else if member.account_id == from {
                // Demoted, not removed. The outgoing owner keeps the highest role
                // below owner so they can still undo a transfer they regret.
                member.role = RoomRole::Manager;
            }
        }
        if let Some(room) = s.rooms.get_mut(&room_id) {
            room.owner_id = to;
            room.updated_at = at;
        }
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
        let mut s = self.state.write();
        let member = s
            .room_members
            .get_mut(&room_id)
            .and_then(|rows| rows.iter_mut().find(|m| m.account_id == account_id))
            .ok_or_else(|| fault::not_found("room member"))?;
        member.permissions_grant = grant;
        member.permissions_deny = deny;
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
        let mut s = self.state.write();
        let Some(conversation_id) = s.rooms.get(&room_id).map(|room| room.conversation_id) else {
            return Err(fault::not_found("room"));
        };
        let member = s
            .room_members
            .get_mut(&room_id)
            .and_then(|rows| rows.iter_mut().find(|m| m.account_id == account_id))
            .ok_or_else(|| fault::not_found("room member"))?;
        member.muted_until = muted_until;
        member.banned_until = banned_until;
        member.ban_reason = reason;
        let banned = banned_until.is_some();
        if banned && member.left_at.is_none() {
            member.left_at = Some(at);
        }
        if banned {
            // A ban that leaves the conversation membership in place would leave
            // the banned account still receiving the room's messages.
            let count = s.active_room_members(room_id) as i32;
            if let Some(room) = s.rooms.get_mut(&room_id) {
                room.member_count = count;
            }
            if let Some(members) = s.conversation_members.get_mut(&conversation_id) {
                if let Some(member) = members.iter_mut().find(|m| m.account_id == account_id) {
                    member.left_at = Some(at);
                }
            }
        }
        Ok(())
    }

    async fn recount_room(&self, room_id: Id) -> Result<i32> {
        let mut s = self.state.write();
        if !s.rooms.contains_key(&room_id) {
            return Err(fault::not_found("room"));
        }
        let count = s.active_room_members(room_id) as i32;
        if let Some(room) = s.rooms.get_mut(&room_id) {
            room.member_count = count;
        }
        Ok(count)
    }
}

#[async_trait]
impl SocialStore for MemoryStore {
    async fn put_relationship(&self, relationship: Relationship) -> Result<Relationship> {
        if relationship.account_id == relationship.other_id {
            return Err(fault::validation(
                "other_id",
                "an account cannot relate to itself",
            ));
        }
        let mut s = self.state.write();
        let key = (
            relationship.account_id,
            relationship.other_id,
            relationship.kind,
        );
        // Upsert, keeping the original creation time: re-sending a friend request
        // should not make an old one look new.
        let created_at = s
            .relationships
            .get(&key)
            .map_or(relationship.created_at, |existing| existing.created_at);
        let stored = Relationship {
            created_at,
            ..relationship
        };
        s.relationships.insert(key, stored.clone());
        Ok(stored)
    }

    async fn relationship(
        &self,
        account_id: Id,
        other_id: Id,
        kind: RelationshipKind,
    ) -> Result<Option<Relationship>> {
        Ok(self
            .state
            .read()
            .relationships
            .get(&(account_id, other_id, kind))
            .cloned())
    }

    async fn remove_relationship(
        &self,
        account_id: Id,
        other_id: Id,
        kind: RelationshipKind,
    ) -> Result<()> {
        self.state
            .write()
            .relationships
            .remove(&(account_id, other_id, kind));
        Ok(())
    }

    async fn accept_friend(&self, account_id: Id, other_id: Id, at: Timestamp) -> Result<()> {
        let mut s = self.state.write();
        let incoming = (account_id, other_id, RelationshipKind::PendingIncoming);
        let outgoing = (other_id, account_id, RelationshipKind::PendingOutgoing);
        let requested_at = s
            .relationships
            .get(&incoming)
            .or_else(|| s.relationships.get(&outgoing))
            .map(|r| r.created_at)
            .ok_or_else(|| fault::not_found("friend request"))?;

        s.relationships.remove(&incoming);
        s.relationships.remove(&outgoing);
        // Both directions, in one operation. A friendship stored on one side only
        // is how "we are friends but you are not in my list" bugs happen.
        for (owner, peer) in [(account_id, other_id), (other_id, account_id)] {
            s.relationships.insert(
                (owner, peer, RelationshipKind::Friend),
                Relationship {
                    account_id: owner,
                    other_id: peer,
                    kind: RelationshipKind::Friend,
                    created_at: requested_at,
                    accepted_at: Some(at),
                },
            );
        }
        Ok(())
    }

    async fn relationships(
        &self,
        account_id: Id,
        kind: RelationshipKind,
        limit: u16,
    ) -> Result<Vec<Relationship>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        let mut edges: Vec<Relationship> = s
            .relationships
            .values()
            .filter(|r| r.account_id == account_id && r.kind == kind)
            .cloned()
            .collect();
        edges.sort_by_key(|r| (std::cmp::Reverse(r.created_at), r.other_id));
        edges.truncate(limit);
        Ok(edges)
    }

    async fn count_relationships(&self, account_id: Id, kind: RelationshipKind) -> Result<u64> {
        let s = self.state.read();
        Ok(s.relationships
            .values()
            .filter(|r| r.account_id == account_id && r.kind == kind)
            .count() as u64)
    }

    async fn inbound_relationships(
        &self,
        account_id: Id,
        kind: RelationshipKind,
        limit: u16,
    ) -> Result<Vec<Relationship>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        let mut edges: Vec<Relationship> = s
            .relationships
            .values()
            .filter(|r| r.other_id == account_id && r.kind == kind)
            .cloned()
            .collect();
        edges.sort_by_key(|r| (std::cmp::Reverse(r.created_at), r.account_id));
        edges.truncate(limit);
        Ok(edges)
    }

    async fn is_blocked_either_way(&self, a: Id, b: Id) -> Result<bool> {
        let s = self.state.read();
        Ok(s.relationships
            .contains_key(&(a, b, RelationshipKind::Block))
            || s.relationships
                .contains_key(&(b, a, RelationshipKind::Block)))
    }
}

#[async_trait]
impl EconomyStore for MemoryStore {
    async fn ledger_account(
        &self,
        owner_id: Option<Id>,
        kind: LedgerAccountKind,
        currency: Currency,
        create_with_id: Id,
        at: Timestamp,
    ) -> Result<LedgerAccount> {
        let mut s = self.state.write();
        let key = (owner_id, kind, currency);
        if let Some(existing) = s.ledger_index.get(&key) {
            let id = *existing;
            return s
                .ledger_accounts
                .get(&id)
                .cloned()
                .ok_or_else(|| fault::internal("ledger index points at a missing account"));
        }
        let account = LedgerAccount {
            ledger_account_id: create_with_id,
            owner_id,
            kind,
            currency,
            created_at: at,
        };
        s.ledger_index.insert(key, account.ledger_account_id);
        s.ledger_accounts
            .insert(account.ledger_account_id, account.clone());
        Ok(account)
    }

    async fn post_transaction(&self, new: NewTransaction) -> Result<Posted> {
        let mut s = self.state.write();
        if let Some(existing) = s.tx_idempotency.get(&new.idempotency_key) {
            let id = *existing;
            return s
                .transactions
                .get(&id)
                .cloned()
                .map(Posted::Duplicate)
                .ok_or_else(|| {
                    fault::internal("idempotency index points at a missing transaction")
                });
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
        // Double entry, enforced rather than hoped for. If the legs do not sum to
        // zero then value was created or destroyed, and a currency whose total
        // drifts is a currency nobody can audit.
        let mut total: i64 = 0;
        for leg in &new.legs {
            if leg.amount == 0 {
                return Err(fault::validation(
                    "legs",
                    "a zero-amount leg carries no meaning",
                ));
            }
            let account = s
                .ledger_accounts
                .get(&leg.ledger_account_id)
                .ok_or_else(|| fault::not_found("ledger account"))?;
            if account.currency != new.currency {
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
        if s.transactions.contains_key(&new.tx_id) {
            return Err(fault::already_exists("transaction"));
        }
        // The receipt is validated before anything is written, so a refusal leaves
        // no half-posted transaction behind. The PostgreSQL backend gets the same
        // guarantee from a transaction; here it comes from checking first.
        match &new.receipt {
            Some(Receipt::Gift(gift)) => {
                if s.gifts.contains_key(&gift.gift_id) || s.gift_by_tx.contains_key(&new.tx_id) {
                    return Err(fault::already_exists("gift"));
                }
                if gift.gift_code.trim().is_empty() {
                    return Err(fault::validation("gift_code", "must not be empty"));
                }
                if !s.accounts.contains_key(&gift.recipient_id) {
                    return Err(fault::not_found("account"));
                }
            }
            Some(Receipt::Entitlement { sku }) => {
                if sku.trim().is_empty() {
                    return Err(fault::validation("sku", "must not be empty"));
                }
                let owner = new.created_by.ok_or_else(|| {
                    fault::validation("created_by", "an entitlement needs an owner")
                })?;
                if s.entitlements.contains_key(&(owner, sku.clone())) {
                    return Err(fault::already_exists("entitlement"));
                }
            }
            None => {}
        }
        // A user account may not be driven below zero. A user cannot spend money
        // they do not have, and a negative user balance would be the ledger
        // asserting the platform had extended them credit it never agreed to.
        // System accounts are exempt by design: Mint is negative by construction
        // (its balance is the total ever issued), and Fee and Escrow only ever
        // accumulate what users have already paid in. This is the floor that lets
        // a caller post a debit and trust the store to refuse an overdraft,
        // rather than reading the balance first and racing its own retry.
        let mut deltas: HashMap<Id, i64> = HashMap::new();
        for leg in &new.legs {
            *deltas.entry(leg.ledger_account_id).or_default() += leg.amount;
        }
        for (ledger_account_id, delta) in &deltas {
            if *delta >= 0 {
                continue;
            }
            // Proved present in the leg loop above.
            let account = &s.ledger_accounts[ledger_account_id];
            if account.kind != LedgerAccountKind::User {
                continue;
            }
            let current: i64 = s
                .entries
                .get(ledger_account_id)
                .map_or(0, |entries| entries.iter().map(|(_, amount)| amount).sum());
            let projected = current
                .checked_add(*delta)
                .ok_or_else(|| fault::validation("legs", "balance overflow"))?;
            if projected < 0 {
                return Err(fault::insufficient_balance("account"));
            }
        }

        let receipt = new.receipt;
        let transaction = LedgerTransaction {
            tx_id: new.tx_id,
            reason: new.reason,
            ref_id: new.ref_id,
            idempotency_key: new.idempotency_key,
            created_by: new.created_by,
            created_at: new.created_at,
            legs: new.legs,
        };
        for leg in &transaction.legs {
            s.entries
                .entry(leg.ledger_account_id)
                .or_default()
                .push((transaction.tx_id, leg.amount));
        }
        match receipt {
            Some(Receipt::Gift(gift)) => {
                let sent = GiftSent {
                    gift_id: gift.gift_id,
                    tx_id: transaction.tx_id,
                    sender_id: gift.sender_id,
                    recipient_id: gift.recipient_id,
                    gift_code: gift.gift_code,
                    conversation_id: gift.conversation_id,
                    created_at: transaction.created_at,
                };
                s.gift_by_tx.insert(transaction.tx_id, sent.gift_id);
                s.gift_order.push(sent.gift_id);
                s.gifts.insert(sent.gift_id, sent);
            }
            Some(Receipt::Entitlement { sku }) => {
                // `created_by` was proved present above.
                let owner = transaction.created_by.unwrap_or_default();
                s.entitlements.insert(
                    (owner, sku.clone()),
                    Entitlement {
                        account_id: owner,
                        sku,
                        acquired_at: transaction.created_at,
                        tx_id: Some(transaction.tx_id),
                    },
                );
            }
            None => {}
        }
        s.tx_idempotency
            .insert(transaction.idempotency_key.clone(), transaction.tx_id);
        s.tx_order.push(transaction.tx_id);
        s.transactions
            .insert(transaction.tx_id, transaction.clone());
        Ok(Posted::Created(transaction))
    }

    async fn balance(&self, ledger_account_id: Id) -> Result<i64> {
        let s = self.state.read();
        if !s.ledger_accounts.contains_key(&ledger_account_id) {
            return Err(fault::not_found("ledger account"));
        }
        Ok(s.entries
            .get(&ledger_account_id)
            .map_or(0, |entries| entries.iter().map(|(_, amount)| amount).sum()))
    }

    async fn ledger_history(
        &self,
        ledger_account_id: Id,
        limit: u16,
    ) -> Result<Vec<(LedgerTransaction, i64)>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        let Some(entries) = s.entries.get(&ledger_account_id) else {
            return Ok(Vec::new());
        };
        Ok(entries
            .iter()
            .rev()
            .take(limit)
            .filter_map(|(tx_id, amount)| s.transactions.get(tx_id).map(|tx| (tx.clone(), *amount)))
            .collect())
    }

    async fn currency_sum(&self, currency: Currency) -> Result<i64> {
        let s = self.state.read();
        let mut total: i64 = 0;
        for (ledger_account_id, entries) in &s.entries {
            let Some(account) = s.ledger_accounts.get(ledger_account_id) else {
                continue;
            };
            if account.currency != currency {
                continue;
            }
            for (_, amount) in entries {
                total = total.saturating_add(*amount);
            }
        }
        Ok(total)
    }

    async fn gifts_received(&self, account_id: Id, limit: u16) -> Result<Vec<GiftSent>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        Ok(s.gift_order
            .iter()
            .rev()
            .filter_map(|id| s.gifts.get(id))
            .filter(|gift| gift.recipient_id == account_id)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn gifts_in_conversation(
        &self,
        conversation_id: Id,
        limit: u16,
    ) -> Result<Vec<GiftSent>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        Ok(s.gift_order
            .iter()
            .rev()
            .filter_map(|id| s.gifts.get(id))
            .filter(|gift| gift.conversation_id == Some(conversation_id))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn gift_tally(&self, account_id: Id) -> Result<Vec<(String, u32)>> {
        let s = self.state.read();
        let mut counts: HashMap<&str, u32> = HashMap::new();
        for gift in s.gifts.values().filter(|g| g.recipient_id == account_id) {
            *counts.entry(gift.gift_code.as_str()).or_default() += 1;
        }
        let mut tally: Vec<(String, u32)> = counts
            .into_iter()
            .map(|(code, count)| (code.to_owned(), count))
            .collect();
        // Count descending then code ascending, matching the SQL backend's
        // `order by`. Without the second key a shelf with two equal counts renders
        // in whichever order the map felt like, and a screenshot taken twice
        // disagrees with itself.
        tally.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        Ok(tally)
    }

    async fn entitlements(&self, account_id: Id) -> Result<Vec<Entitlement>> {
        let s = self.state.read();
        let mut owned: Vec<Entitlement> = s
            .entitlements
            .values()
            .filter(|held| held.account_id == account_id)
            .cloned()
            .collect();
        owned.sort_by(|left, right| {
            left.acquired_at
                .cmp(&right.acquired_at)
                .then_with(|| left.sku.cmp(&right.sku))
        });
        Ok(owned)
    }

    async fn has_entitlement(&self, account_id: Id, sku: &str) -> Result<bool> {
        let s = self.state.read();
        Ok(s.entitlements.contains_key(&(account_id, sku.to_owned())))
    }
}

#[async_trait]
impl ProgressionStore for MemoryStore {
    async fn progression(&self, account_id: Id) -> Result<Option<Progression>> {
        let s = self.state.read();
        Ok(s.progression.get(&account_id).copied())
    }

    async fn award_xp(&self, award: NewXpAward) -> Result<XpChange> {
        if award.amount <= 0 {
            return Err(fault::validation("amount", "must be positive"));
        }
        let mut s = self.state.write();
        if !s.accounts.contains_key(&award.account_id) {
            return Err(fault::not_found("account"));
        }
        // Checked before either write, so a refused retry leaves the total alone. The
        // PostgreSQL backend gets the same guarantee from a unique index inside a
        // transaction.
        if let Some(key) = award.idempotency_key.as_deref() {
            if s.xp_keys.contains_key(key) {
                return Err(fault::already_exists("xp award"));
            }
        }
        if s.xp_awards.contains_key(&award.award_id) {
            return Err(fault::already_exists("xp award"));
        }

        let row = s
            .progression
            .entry(award.account_id)
            .or_insert(Progression {
                account_id: award.account_id,
                xp: 0,
                level: 1,
                updated_at: award.at,
            });
        let before = row.xp;
        let after = before
            .checked_add(award.amount)
            .ok_or_else(|| fault::validation("amount", "overflows the total"))?;
        row.xp = after;
        row.updated_at = award.at;

        if let Some(key) = award.idempotency_key.clone() {
            s.xp_keys.insert(key, award.award_id);
        }
        s.xp_order.push(award.award_id);
        s.xp_awards.insert(award.award_id, award);
        Ok(XpChange { before, after })
    }

    async fn set_level(&self, account_id: Id, level: i32, at: Timestamp) -> Result<()> {
        if level < 1 {
            return Err(fault::validation("level", "must be at least one"));
        }
        let mut s = self.state.write();
        let row = s
            .progression
            .get_mut(&account_id)
            .ok_or_else(|| fault::not_found("progression"))?;
        row.level = level;
        row.updated_at = at;
        Ok(())
    }

    async fn award_badge(&self, award: BadgeAward) -> Result<bool> {
        if award.badge_code.trim().is_empty() {
            return Err(fault::validation("badge_code", "must not be empty"));
        }
        let mut s = self.state.write();
        if !s.accounts.contains_key(&award.account_id) {
            return Err(fault::not_found("account"));
        }
        let key = (award.account_id, award.badge_code.clone());
        if s.badges.contains_key(&key) {
            return Ok(false);
        }
        s.badges.insert(key, award);
        Ok(true)
    }

    async fn badges(&self, account_id: Id) -> Result<Vec<BadgeAward>> {
        let s = self.state.read();
        let mut held: Vec<BadgeAward> = s
            .badges
            .values()
            .filter(|award| award.account_id == account_id)
            .cloned()
            .collect();
        held.sort_by(|left, right| {
            right
                .awarded_at
                .cmp(&left.awarded_at)
                .then_with(|| left.badge_code.cmp(&right.badge_code))
        });
        Ok(held)
    }

    async fn xp_earned_since(
        &self,
        account_id: Id,
        source: Option<i16>,
        since: Timestamp,
    ) -> Result<i64> {
        let s = self.state.read();
        Ok(s.xp_awards
            .values()
            .filter(|award| award.account_id == account_id)
            .filter(|award| award.at >= since)
            .filter(|award| source.is_none_or(|wanted| award.source == wanted))
            // Saturating, because the answer is compared against a cap: a total that
            // wrapped would come back small and hand an abuser the very allowance this
            // read exists to deny them.
            .fold(0_i64, |total, award| total.saturating_add(award.amount)))
    }

    async fn leaderboard(
        &self,
        scope: Scope<'_>,
        since: Option<Timestamp>,
        limit: u16,
    ) -> Result<Vec<Standing>> {
        // Validated before the lock, so a malformed country code costs nothing.
        let country = match scope {
            Scope::Country(code) => Some(
                canonical_country(Some(code))?
                    .ok_or_else(|| fault::validation("country", "must be two ASCII letters"))?,
            ),
            _ => None,
        };
        let s = self.state.read();

        let eligible = |account_id: Id| -> bool {
            match scope {
                Scope::Global => true,
                Scope::Country(_) => s
                    .accounts
                    .get(&account_id)
                    .and_then(|account| account.country.as_deref())
                    // Exact, not case-insensitive. `canonical_country` ran on the way
                    // in, so a stored value that differs in case cannot exist -- and a
                    // lenient comparison here would find rows the SQL backend's indexed
                    // comparison cannot, which is a divergence rather than a kindness.
                    .is_some_and(|code| Some(code) == country.as_deref()),
                Scope::Room(room_id) => s.room_members.get(&room_id).is_some_and(|members| {
                    members
                        .iter()
                        .any(|member| member.account_id == account_id && member.left_at.is_none())
                }),
            }
        };

        let level_of =
            |account_id: Id| -> i32 { s.progression.get(&account_id).map_or(1, |row| row.level) };

        let Some(since) = since else {
            return Ok(rank(
                s.progression
                    .values()
                    .copied()
                    .filter(|row| eligible(row.account_id))
                    .map(|row| Standing {
                        account_id: row.account_id,
                        xp: row.xp,
                        level: row.level,
                    }),
                limit,
            ));
        };

        // The windowed board sums the events. `level` still comes from `progression`,
        // because the number beside a weekly rank is the person's standing now — there
        // is no level somebody held last Tuesday.
        let mut earned: HashMap<Id, i64> = HashMap::new();
        for award in s.xp_awards.values() {
            if award.at < since || !eligible(award.account_id) {
                continue;
            }
            let total = earned.entry(award.account_id).or_default();
            *total = total.saturating_add(award.amount);
        }
        Ok(rank(
            earned.into_iter().map(|(account_id, xp)| Standing {
                account_id,
                xp,
                level: level_of(account_id),
            }),
            limit,
        ))
    }
}

/// Sorts progression rows into a leaderboard page.
///
/// XP descending, then account id ascending. The second key is what makes the page
/// stable: a hundred accounts sitting on the same round number is the normal shape of a
/// leaderboard's tail, and without a tiebreak the same request returns them in a
/// different order every time, which reads as a leaderboard that will not sit still.
fn rank(rows: impl Iterator<Item = Standing>, limit: u16) -> Vec<Standing> {
    let mut standings: Vec<Standing> = rows.collect();
    standings.sort_by(|left, right| {
        right
            .xp
            .cmp(&left.xp)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
    standings.truncate(clamp_limit(limit));
    standings
}

#[async_trait]
impl GameStore for MemoryStore {
    async fn create_game(&self, new: NewGame) -> Result<GameSession> {
        let mut s = self.state.write();
        if s.games.contains_key(&new.game_id) {
            return Err(fault::already_exists("game"));
        }
        // The conversation has to exist. Postgres refuses a dangling game with its
        // foreign key; the memory backend refuses it here, so a test cannot create a
        // game in a conversation that production would reject.
        if !s.conversations.contains_key(&new.conversation_id) {
            return Err(fault::not_found("conversation"));
        }
        let game = GameSession {
            game_id: new.game_id,
            kind: new.kind,
            conversation_id: new.conversation_id,
            state: new.state,
            turn_of: new.turn_of,
            status: game_status::OPEN,
            stake_currency: new.stake_currency,
            stake_amount: new.stake_amount,
            created_at: new.at,
            updated_at: new.at,
            finished_at: None,
        };
        s.games.insert(game.game_id, game.clone());
        s.game_order.push(game.game_id);
        Ok(game)
    }

    async fn game(&self, game_id: Id) -> Result<Option<GameSession>> {
        Ok(self.state.read().games.get(&game_id).cloned())
    }

    async fn active_games(&self, conversation_id: Id, limit: u16) -> Result<Vec<GameSession>> {
        let s = self.state.read();
        let cap = clamp_limit(limit);
        let mut out = Vec::new();
        // Newest first: walk insertion order backwards, the same order Postgres gets
        // from `order by created_at desc`.
        for id in s.game_order.iter().rev() {
            if out.len() >= cap {
                break;
            }
            if let Some(game) = s.games.get(id) {
                if game.conversation_id == conversation_id && game.status == game_status::OPEN {
                    out.push(game.clone());
                }
            }
        }
        Ok(out)
    }

    async fn advance_game(&self, advance: AdvanceGame) -> Result<Option<GameSession>> {
        let mut s = self.state.write();
        let Some(game) = s.games.get_mut(&advance.game_id) else {
            return Ok(None);
        };
        // The compare-and-swap. Only an open game still carrying the expected token
        // may move; a stale or replayed move finds the token changed and is refused
        // with `None`, exactly as the Postgres `update ... where updated_at = $expected
        // and status = 0` would affect zero rows.
        if game.status != game_status::OPEN || game.updated_at != advance.expected_updated_at {
            return Ok(None);
        }
        game.state = advance.state;
        game.turn_of = advance.turn_of;
        game.status = advance.status;
        // The token has to move on every write, and `at` alone does not guarantee that: two
        // moves arriving inside the same millisecond would leave it unchanged, and the second
        // writer would find its own now-stale expectation satisfied and overwrite the first
        // move without ever seeing it. The comparison above pins the current value, so a
        // strictly greater one is computable here without a second read. Only the token is
        // nudged; `finished_at` stays the real time the game ended.
        game.updated_at = advanced_token(advance.expected_updated_at, advance.at);
        game.finished_at = if advance.status == game_status::OPEN {
            None
        } else {
            Some(advance.at)
        };
        Ok(Some(game.clone()))
    }

    async fn abandon_game(&self, game_id: Id, at: Timestamp) -> Result<Option<GameSession>> {
        let mut s = self.state.write();
        let Some(game) = s.games.get_mut(&game_id) else {
            return Ok(None);
        };
        if game.status != game_status::OPEN {
            return Ok(None);
        }
        game.status = game_status::ABANDONED;
        game.turn_of = None;
        game.updated_at = at;
        game.finished_at = Some(at);
        Ok(Some(game.clone()))
    }
}

#[async_trait]
impl BotStore for MemoryStore {
    async fn register_bot(&self, new: NewBot) -> Result<Bot> {
        let mut s = self.state.write();
        // The backing account's uniqueness, checked the same way `create_account`
        // checks it, because this is `create_account` folded into the same lock. A
        // username collision is the realistic one; the account-id and token
        // collisions are checked too so no half-written triple can survive a bug.
        let username_key = fold(&new.username);
        if s.by_username.contains_key(&username_key) {
            return Err(fault::already_exists("username"));
        }
        if s.accounts.contains_key(&new.account_id) {
            return Err(fault::already_exists("account id"));
        }
        if s.bots.contains_key(&new.bot_id) {
            return Err(fault::already_exists("bot"));
        }
        if s.bot_by_account.contains_key(&new.account_id) {
            return Err(fault::already_exists("bot account"));
        }
        if s.bot_by_token.contains_key(&new.token_hash) {
            return Err(fault::already_exists("bot token"));
        }

        // The account the bot posts under. Its password is the caller's locked hash;
        // it has no email or phone, and it starts active like any other.
        let account = Account {
            account_id: new.account_id,
            username: new.username,
            email: None,
            phone: None,
            password_hash: new.password_hash,
            status: AccountStatus::Active,
            country: None,
            locale: new.locale,
            created_at: new.created_at,
            updated_at: new.created_at,
            last_login_at: None,
            suspended_until: None,
            deleted_at: None,
        };
        // The profile, private by default, exactly as a human registration builds it.
        let profile = Profile {
            account_id: new.account_id,
            display_name: new.display_name.clone(),
            bio: None,
            avatar_media_id: None,
            birth_year: None,
            show_last_seen: Visibility::Friends,
            who_can_message: Visibility::Friends,
            who_can_add: Visibility::Everyone,
            searchable: true,
            updated_at: new.created_at,
        };
        let bot = Bot {
            bot_id: new.bot_id,
            owner_id: new.owner_id,
            account_id: new.account_id,
            name: new.display_name,
            token_hash: new.token_hash,
            scopes: new.scopes,
            webhook_url: new.webhook_url,
            created_at: new.created_at,
            disabled_at: None,
        };

        s.by_username.insert(username_key, account.account_id);
        s.accounts.insert(account.account_id, account);
        s.profiles.insert(profile.account_id, profile);
        s.bot_by_account.insert(bot.account_id, bot.bot_id);
        s.bot_by_token.insert(bot.token_hash.clone(), bot.bot_id);
        s.bots.insert(bot.bot_id, bot.clone());
        s.bot_order.push(bot.bot_id);
        Ok(bot)
    }

    async fn bot(&self, bot_id: Id) -> Result<Option<Bot>> {
        Ok(self.state.read().bots.get(&bot_id).cloned())
    }

    async fn bot_by_account(&self, account_id: Id) -> Result<Option<Bot>> {
        let s = self.state.read();
        Ok(s.bot_by_account
            .get(&account_id)
            .and_then(|id| s.bots.get(id))
            .cloned())
    }

    async fn bot_by_token_hash(&self, token_hash: &[u8]) -> Result<Option<Bot>> {
        let s = self.state.read();
        Ok(s.bot_by_token
            .get(token_hash)
            .and_then(|id| s.bots.get(id))
            .cloned())
    }

    async fn bots_for_owner(&self, owner_id: Id, limit: u16) -> Result<Vec<Bot>> {
        let s = self.state.read();
        let cap = clamp_limit(limit);
        let mut out = Vec::new();
        // Newest first: walk insertion order backwards, matching Postgres' `order by
        // created_at desc`.
        for id in s.bot_order.iter().rev() {
            if out.len() >= cap {
                break;
            }
            if let Some(bot) = s.bots.get(id) {
                if bot.owner_id == owner_id {
                    out.push(bot.clone());
                }
            }
        }
        Ok(out)
    }

    async fn set_bot_scopes(&self, bot_id: Id, scopes: i64) -> Result<Option<Bot>> {
        let mut s = self.state.write();
        let Some(bot) = s.bots.get_mut(&bot_id) else {
            return Ok(None);
        };
        bot.scopes = scopes;
        Ok(Some(bot.clone()))
    }

    async fn set_bot_token_hash(&self, bot_id: Id, token_hash: Vec<u8>) -> Result<Option<Bot>> {
        let mut s = self.state.write();
        // Guard the unique token index before touching the row: a collision with a
        // different bot must fail the whole rotation, the way `bot_token_hash_key`
        // would reject it in Postgres.
        if s.bot_by_token
            .get(&token_hash)
            .is_some_and(|owner| *owner != bot_id)
        {
            return Err(fault::already_exists("bot token"));
        }
        let Some(old) = s.bots.get(&bot_id).map(|bot| bot.token_hash.clone()) else {
            return Ok(None);
        };
        s.bot_by_token.remove(&old);
        s.bot_by_token.insert(token_hash.clone(), bot_id);
        let bot = s
            .bots
            .get_mut(&bot_id)
            .expect("bot present: its token index was just read");
        bot.token_hash = token_hash;
        Ok(Some(bot.clone()))
    }

    async fn set_bot_disabled(
        &self,
        bot_id: Id,
        disabled_at: Option<Timestamp>,
    ) -> Result<Option<Bot>> {
        let mut s = self.state.write();
        let Some(bot) = s.bots.get_mut(&bot_id) else {
            return Ok(None);
        };
        bot.disabled_at = disabled_at;
        Ok(Some(bot.clone()))
    }
}

#[async_trait]
impl FederationStore for MemoryStore {
    async fn add_peer(&self, new: NewPeer) -> Result<PeerRecord> {
        let mut s = self.state.write();
        // Both unique constraints of `node_peer`, checked before anything is written:
        // the id is the primary key, and the public key has its own unique index
        // because the key — not the id — is the identity a handshake is checked
        // against.
        if s.peers.contains_key(&new.node_id) {
            return Err(fault::already_exists("node peer"));
        }
        if s.peer_by_key.contains_key(&new.public_key) {
            return Err(fault::already_exists("node key"));
        }
        let peer = PeerRecord {
            node_id: new.node_id,
            public_key: new.public_key,
            base_url: new.base_url,
            region: new.region,
            status: new.status,
            added_at: new.added_at,
            last_seen_at: None,
        };
        s.peer_by_key
            .insert(peer.public_key.clone(), peer.node_id.clone());
        s.peer_order.push(peer.node_id.clone());
        s.peers.insert(peer.node_id.clone(), peer.clone());
        Ok(peer)
    }

    async fn peer(&self, node_id: &str) -> Result<Option<PeerRecord>> {
        Ok(self.state.read().peers.get(node_id).cloned())
    }

    async fn peers(&self, limit: u16) -> Result<Vec<PeerRecord>> {
        let s = self.state.read();
        let cap = clamp_limit(limit);
        let mut out = Vec::new();
        // Newest first: walk insertion order backwards, matching Postgres' `order by
        // added_at desc`.
        for id in s.peer_order.iter().rev() {
            if out.len() >= cap {
                break;
            }
            if let Some(peer) = s.peers.get(id) {
                out.push(peer.clone());
            }
        }
        Ok(out)
    }

    async fn set_peer_status(&self, node_id: &str, status: i16) -> Result<Option<PeerRecord>> {
        let mut s = self.state.write();
        let Some(peer) = s.peers.get_mut(node_id) else {
            return Ok(None);
        };
        peer.status = status;
        Ok(Some(peer.clone()))
    }

    async fn touch_peer(&self, node_id: &str, seen_at: Timestamp) -> Result<Option<PeerRecord>> {
        let mut s = self.state.write();
        let Some(peer) = s.peers.get_mut(node_id) else {
            return Ok(None);
        };
        peer.last_seen_at = Some(seen_at);
        Ok(Some(peer.clone()))
    }

    async fn enqueue_event(&self, new: NewOutboxEvent) -> Result<OutboxRecord> {
        let mut s = self.state.write();
        if s.outbox.contains_key(&new.event_id) {
            return Err(fault::already_exists("federation event"));
        }
        let event = OutboxRecord {
            event_id: new.event_id,
            target_node: new.target_node,
            opcode: new.opcode,
            payload: new.payload,
            attempts: 0,
            created_at: new.created_at,
            next_attempt_at: new.next_attempt_at,
            delivered_at: None,
            last_error: None,
        };
        s.outbox_order.push(event.event_id);
        s.outbox.insert(event.event_id, event.clone());
        Ok(event)
    }

    async fn due_events(&self, now: Timestamp, limit: u16) -> Result<Vec<OutboxRecord>> {
        let s = self.state.read();
        let cap = clamp_limit(limit);
        // Insertion order first, so the stable sort below breaks `next_attempt_at`
        // ties by age — the tiebreak Postgres gets for free from the index scan.
        let mut due: Vec<OutboxRecord> = s
            .outbox_order
            .iter()
            .filter_map(|id| s.outbox.get(id))
            .filter(|event| {
                event.delivered_at.is_none() && now.is_at_or_after(event.next_attempt_at)
            })
            .cloned()
            .collect();
        due.sort_by_key(|event| event.next_attempt_at.as_millis());
        due.truncate(cap);
        Ok(due)
    }

    async fn mark_delivered(
        &self,
        event_id: Id,
        delivered_at: Timestamp,
    ) -> Result<Option<OutboxRecord>> {
        let mut s = self.state.write();
        let Some(event) = s.outbox.get_mut(&event_id) else {
            return Ok(None);
        };
        // Idempotent: the first delivery wins the timestamp, a retry that races it is
        // harmless. Either way the event stops being due.
        if event.delivered_at.is_none() {
            event.delivered_at = Some(delivered_at);
        }
        Ok(Some(event.clone()))
    }

    async fn mark_failed(
        &self,
        event_id: Id,
        next_attempt_at: Timestamp,
        error: &str,
    ) -> Result<Option<OutboxRecord>> {
        let mut s = self.state.write();
        let Some(event) = s.outbox.get_mut(&event_id) else {
            return Ok(None);
        };
        event.attempts = event.attempts.saturating_add(1);
        event.next_attempt_at = next_attempt_at;
        event.last_error = Some(error.to_string());
        Ok(Some(event.clone()))
    }
}

#[async_trait]
impl MediaStore for MemoryStore {
    async fn create_media(&self, media: MediaObject) -> Result<MediaObject> {
        // Shape before identity, so both backends agree: PostgreSQL learns about a
        // duplicate id from its primary key, which it can only do after the row has
        // passed validation.
        if media.byte_size <= 0 {
            return Err(fault::validation("byte_size", "must be positive"));
        }
        let mut s = self.state.write();
        if s.media.contains_key(&media.media_id) {
            return Err(fault::already_exists("media object"));
        }
        s.media.insert(media.media_id, media.clone());
        Ok(media)
    }

    async fn media(&self, media_id: Id) -> Result<Option<MediaObject>> {
        Ok(self.state.read().media.get(&media_id).cloned())
    }

    async fn set_media_scan_status(&self, media_id: Id, status: i16, _at: Timestamp) -> Result<()> {
        let mut s = self.state.write();
        let media = s
            .media
            .get_mut(&media_id)
            .ok_or_else(|| fault::not_found("media object"))?;
        media.scan_status = status;
        Ok(())
    }

    async fn delete_media(&self, media_id: Id, at: Timestamp) -> Result<()> {
        let mut s = self.state.write();
        if let Some(media) = s.media.get_mut(&media_id) {
            if media.deleted_at.is_none() {
                media.deleted_at = Some(at);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl NotifyStore for MemoryStore {
    async fn create_notification(&self, notification: Notification) -> Result<Notification> {
        // Kind before identity, so both backends agree on which complaint a caller
        // hears first: PostgreSQL learns about a duplicate id from its primary key,
        // which it can only do after the row has passed the check constraint.
        if !notification_kind::is_storable(notification.kind) {
            return Err(fault::validation("kind", "is not a storable notification"));
        }
        let mut s = self.state.write();
        if !s.accounts.contains_key(&notification.account_id) {
            return Err(fault::not_found("account"));
        }
        if s.notifications.contains_key(&notification.notification_id) {
            return Err(fault::already_exists("notification"));
        }
        s.notification_order.push(notification.notification_id);
        s.notifications
            .insert(notification.notification_id, notification.clone());
        Ok(notification)
    }

    async fn notifications(&self, account_id: Id, limit: u16) -> Result<Vec<Notification>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        // Newest first, which is the opposite of the moderation queue and for the
        // opposite reason: nobody scrolls to the bottom of their own notifications to
        // find out what just happened.
        Ok(s.notification_order
            .iter()
            .rev()
            .filter_map(|id| s.notifications.get(id))
            .filter(|n| n.account_id == account_id)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn unread_notifications(&self, account_id: Id) -> Result<u32> {
        let s = self.state.read();
        let count = s
            .notifications
            .values()
            .filter(|n| n.account_id == account_id && n.read_at.is_none())
            .count();
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    async fn mark_notifications_read(
        &self,
        account_id: Id,
        through: Timestamp,
        at: Timestamp,
    ) -> Result<u32> {
        let mut s = self.state.write();
        let mut changed = 0u32;
        for notification in s.notifications.values_mut() {
            if notification.account_id == account_id
                && notification.read_at.is_none()
                && notification.created_at <= through
            {
                notification.read_at = Some(at);
                changed = changed.saturating_add(1);
            }
        }
        Ok(changed)
    }

    async fn purge_notifications(&self, before: Timestamp, limit: u16) -> Result<u64> {
        let limit = clamp_limit(limit);
        let mut s = self.state.write();
        // Oldest first, so a caller looping until this returns zero makes progress
        // from the far end rather than nibbling at whatever the map iterated over.
        let doomed: Vec<Id> = s
            .notification_order
            .iter()
            .filter(|id| {
                s.notifications
                    .get(*id)
                    .is_some_and(|n| n.read_at.is_some() && n.created_at < before)
            })
            .take(limit)
            .copied()
            .collect();
        for id in &doomed {
            s.notifications.remove(id);
        }
        let survivors: Vec<Id> = s
            .notification_order
            .iter()
            .filter(|id| s.notifications.contains_key(id))
            .copied()
            .collect();
        s.notification_order = survivors;
        Ok(doomed.len() as u64)
    }

    async fn set_push_registration(
        &self,
        device_id: Id,
        registration: PushRegistration,
        at: Timestamp,
    ) -> Result<()> {
        let mut s = self.state.write();
        if s.devices
            .get(&device_id)
            .is_none_or(|device| device.revoked_at.is_some())
        {
            return Err(fault::not_found("device"));
        }
        // The unique index in PostgreSQL is enforced here by hand: whoever registers
        // the hash last owns it, and every earlier holder loses it. Same phone.
        s.push
            .retain(|held, (existing, _)| *held == device_id || existing.hash != registration.hash);
        s.push.insert(device_id, (registration, at));
        Ok(())
    }

    async fn clear_push_registration(&self, device_id: Id) -> Result<()> {
        self.state.write().push.remove(&device_id);
        Ok(())
    }

    async fn retire_push_hash(&self, hash: &str) -> Result<bool> {
        let mut s = self.state.write();
        let before = s.push.len();
        s.push
            .retain(|_, (registration, _)| registration.hash != hash);
        Ok(s.push.len() != before)
    }

    async fn push_targets(&self, account_id: Id) -> Result<Vec<PushTarget>> {
        let s = self.state.read();
        let mut targets: Vec<PushTarget> = s
            .devices
            .values()
            .filter(|device| device.account_id == account_id && device.revoked_at.is_none())
            .filter_map(|device| {
                let (registration, updated_at) = s.push.get(&device.device_id)?;
                Some(PushTarget {
                    device_id: device.device_id,
                    platform: device.platform,
                    registration: registration.clone(),
                    updated_at: *updated_at,
                })
            })
            .collect();
        // Sorted, because a `HashMap` iterates in whatever order it feels like and a
        // deterministic simulation cannot afford "whatever order it feels like".
        targets.sort_by_key(|target| target.device_id);
        Ok(targets)
    }

    async fn stale_push_hashes(&self, before: Timestamp, limit: u16) -> Result<Vec<String>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        let mut stale: Vec<(Timestamp, String)> = s
            .push
            .values()
            .filter(|(_, updated_at)| *updated_at < before)
            .map(|(registration, updated_at)| (*updated_at, registration.hash.clone()))
            .collect();
        stale.sort();
        Ok(stale
            .into_iter()
            .take(limit)
            .map(|(_, hash)| hash)
            .collect())
    }
}

#[async_trait]
impl SafetyStore for MemoryStore {
    async fn create_report(&self, report: Report) -> Result<Report> {
        let mut s = self.state.write();
        if s.reports.contains_key(&report.report_id) {
            return Err(fault::already_exists("report"));
        }
        s.report_order.push(report.report_id);
        s.reports.insert(report.report_id, report.clone());
        Ok(report)
    }

    async fn report(&self, report_id: Id) -> Result<Option<Report>> {
        Ok(self.state.read().reports.get(&report_id).cloned())
    }

    async fn open_reports(&self, limit: u16) -> Result<Vec<Report>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        // Oldest first. A queue ordered newest-first starves the reports that have
        // been waiting longest, which are the ones most likely to matter.
        //
        // By `created_at` and then by id, matching the ORDER BY the SQL store issues, and
        // not by the order rows were inserted. The two agree for reports that arrive as
        // they happen, and disagree the moment anything writes a report with an older
        // timestamp than the one before it -- an import, a backfill, a replayed queue. A
        // double whose order came from insertion would let a test prove an ordering the
        // real store does not have.
        let mut open: Vec<&Report> = s
            .report_order
            .iter()
            .filter_map(|id| s.reports.get(id))
            .filter(|report| report.status == report_status::OPEN)
            .collect();
        open.sort_by_key(|report| (report.created_at, report.report_id));
        Ok(open.into_iter().take(limit).cloned().collect())
    }

    async fn open_report_by_reporter(
        &self,
        reporter_id: Id,
        subject_kind: i16,
        subject_id: Id,
    ) -> Result<Option<Report>> {
        let s = self.state.read();
        // Oldest first, matching the queue order, so a caller that shows the duplicate
        // back to the reporter shows the one they actually filed rather than whichever
        // one the map happened to yield.
        let mut held: Vec<&Report> = s
            .report_order
            .iter()
            .filter_map(|id| s.reports.get(id))
            .filter(|report| {
                report.status == report_status::OPEN
                    && report.reporter_id == reporter_id
                    && report.subject_kind == subject_kind
                    && report.subject_id == subject_id
            })
            .collect();
        held.sort_by_key(|report| (report.created_at, report.report_id));
        Ok(held.into_iter().next().cloned())
    }

    async fn count_reports_about(
        &self,
        subject_kind: i16,
        subject_id: Id,
        since: Timestamp,
    ) -> Result<u32> {
        let s = self.state.read();
        let count = s
            .reports
            .values()
            .filter(|report| {
                report.subject_kind == subject_kind
                    && report.subject_id == subject_id
                    && report.created_at.is_at_or_after(since)
            })
            .count();
        // Saturating rather than `as`: the count is a `usize` and the contract is a
        // `u32`, and a wrapped count would read as a quiet account on a busy one.
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
        let mut s = self.state.write();
        let report = s
            .reports
            .get_mut(&report_id)
            .ok_or_else(|| fault::not_found("report"))?;
        if report.status != report_status::OPEN {
            return Err(fault::conflict("report already resolved"));
        }
        report.status = status;
        report.resolution = Some(resolution);
        report.resolved_by = Some(by);
        report.resolved_at = Some(at);
        Ok(())
    }

    async fn append_audit(&self, entry: AuditEntry) -> Result<()> {
        self.state.write().audit.push(entry);
        Ok(())
    }

    async fn audit_for_target(
        &self,
        target_kind: i16,
        target_id: Id,
        limit: u16,
    ) -> Result<Vec<AuditEntry>> {
        let limit = clamp_limit(limit);
        let s = self.state.read();
        Ok(s.audit
            .iter()
            .rev()
            .filter(|entry| entry.target_kind == target_kind && entry.target_id == Some(target_id))
            .take(limit)
            .cloned()
            .collect())
    }
}

#[async_trait]
impl CaptchaStore for MemoryStore {
    async fn put_captcha(&self, row: CaptchaRow) -> Result<()> {
        self.state.write().captcha.insert(row.challenge_id, row);
        Ok(())
    }

    async fn get_captcha(&self, challenge_id: Id, now: Timestamp) -> Result<Option<CaptchaRow>> {
        let mut s = self.state.write();
        let Some(row) = s.captcha.get(&challenge_id).cloned() else {
            return Ok(None);
        };
        if row.expires_at <= now {
            // Expired: the row is gone the moment somebody asks. The captcha is
            // one-shot per id, so a subsequent put with the same id will mint a
            // fresh challenge with a fresh code anyway.
            s.captcha.remove(&challenge_id);
            return Ok(None);
        }
        Ok(Some(row))
    }

    async fn delete_captcha(&self, challenge_id: Id) -> Result<()> {
        self.state.write().captcha.remove(&challenge_id);
        Ok(())
    }
}

#[async_trait]
impl RecoveryStore for MemoryStore {
    async fn recovery_put(&self, row: RecoveryRow) -> Result<()> {
        self.state.write().recovery.insert(row.token_id, row);
        Ok(())
    }

    async fn recovery_get(&self, token_id: Id) -> Result<Option<RecoveryRow>> {
        Ok(self.state.read().recovery.get(&token_id).cloned())
    }

    async fn recovery_consume(&self, token_id: Id, at: Timestamp) -> Result<Option<RecoveryRow>> {
        // The whole point of `consume` is that it is atomic: a row that was
        // already consumed, that does not exist, or that has already expired
        // returns `Ok(None)`, and nothing is written. The caller can then
        // answer 404 without having to guess which case it was — the brief's
        // 404-vs-200 split is "we never reveal whether the token was real,
        // only whether it was acceptable".
        let mut s = self.state.write();
        let Some(row) = s.recovery.get_mut(&token_id) else {
            return Ok(None);
        };
        if row.consumed_at.is_some() {
            return Ok(None);
        }
        if row.expires_at <= at {
            return Ok(None);
        }
        row.consumed_at = Some(at);
        Ok(Some(row.clone()))
    }

    async fn recovery_delete_expired(&self, before: Timestamp, limit: u32) -> Result<u64> {
        let mut s = self.state.write();
        let mut removed = 0u64;
        // A deterministic order so two passes over the same data evict the same
        // rows. Iterating a HashMap directly would mean a sweeper that hits the
        // budget evicts a different subset on every call.
        let mut ids: Vec<Id> = s
            .recovery
            .iter()
            .filter_map(|(id, row)| (row.expires_at <= before).then_some(*id))
            .collect();
        ids.sort_unstable();
        for id in ids {
            if removed >= u64::from(limit) {
                break;
            }
            s.recovery.remove(&id);
            removed += 1;
        }
        Ok(removed)
    }
}

#[async_trait]
impl Store for MemoryStore {
    fn backend_name(&self) -> &'static str {
        "memory"
    }

    async fn migrate(&self) -> Result<()> {
        // Nothing to migrate: the schema is the Rust types.
        Ok(())
    }

    async fn health(&self) -> Result<()> {
        // Taking the lock proves the state is not poisoned and that no writer is
        // wedged, which is the only failure this backend has.
        let _guard = self.state.read();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
