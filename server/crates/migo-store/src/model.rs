//! Records that cross the storage boundary.
//!
//! These are storage shapes, not wire shapes and not domain shapes. Keeping them
//! separate costs a few conversions and buys the ability to change a column
//! without changing a protocol version — the alternative, passing generated
//! protocol structs straight into SQL, welds the database schema to the wire
//! format and makes every migration a protocol negotiation.
//!
//! Enumerations that a client also sees are reused from `migo-protocol` rather
//! than redefined here, because two definitions of the same numbering is one too
//! many. Enumerations that never leave the server are defined here.

use std::cmp::Ordering;

use migo_core::{Id, Secret, Timestamp};
use migo_protocol::{
    ConversationKind, EncryptionMode, MessageKind, Platform, RelationshipKind, RoomKind, RoomRole,
};

/// A field update that distinguishes "leave alone" from "set to null".
///
/// The obvious alternative, `Option<Option<T>>`, compiles and then quietly
/// wrecks a profile update six months later when someone reads the outer
/// `Option` as the value. Naming the three cases makes the intent readable at
/// the call site and in review.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Patch<T> {
    /// Leave the current value in place.
    #[default]
    Keep,
    /// Replace the value.
    Set(T),
    /// Set the column to null.
    Clear,
}

impl<T> Patch<T> {
    /// True when this patch changes nothing.
    #[must_use]
    pub fn is_keep(&self) -> bool {
        matches!(self, Patch::Keep)
    }

    /// Applies the patch to a nullable field.
    pub fn apply(self, target: &mut Option<T>) {
        match self {
            Patch::Keep => {}
            Patch::Set(value) => *target = Some(value),
            Patch::Clear => *target = None,
        }
    }

    /// The value to store, given the current one.
    #[must_use]
    pub fn resolve(self, current: Option<T>) -> Option<T> {
        match self {
            Patch::Keep => current,
            Patch::Set(value) => Some(value),
            Patch::Clear => None,
        }
    }
}

/// Lifecycle state of an account.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccountStatus {
    /// Normal.
    #[default]
    Active = 0,
    /// Suspended by moderation, possibly until a date.
    Suspended = 1,
    /// Deactivated by the user. Reversible; data intact.
    Deactivated = 2,
    /// Deletion requested and personal data purged. The row survives so that
    /// ledger and moderation history stay referentially intact while unlinked
    /// from identity (docs/04-data-model.md §6).
    Deleted = 3,
}

impl AccountStatus {
    /// Numeric form, as stored.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Parses the stored form. An unknown value reads as [`AccountStatus::Suspended`]:
    /// if the database says something this build does not understand, the safe
    /// reading is "do not let them in", not "assume they are fine".
    #[must_use]
    pub const fn from_i16(value: i16) -> Self {
        match value {
            0 => Self::Active,
            2 => Self::Deactivated,
            3 => Self::Deleted,
            _ => Self::Suspended,
        }
    }

    /// Whether the account may authenticate.
    #[must_use]
    pub const fn can_sign_in(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Who may see a thing, or do a thing to you.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Visibility {
    /// Nobody.
    Nobody = 0,
    /// Accepted friends only.
    Friends = 1,
    /// Anyone.
    #[default]
    Everyone = 2,
}

impl Visibility {
    /// Numeric form, as stored.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Parses the stored form. An unknown value reads as the most private
    /// option, for the same reason unknown account status reads as suspended.
    #[must_use]
    pub const fn from_i16(value: i16) -> Self {
        match value {
            1 => Self::Friends,
            2 => Self::Everyone,
            _ => Self::Nobody,
        }
    }
}

/// Why a session was revoked. Recorded so support can answer "why was I logged
/// out" without guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevokeReason {
    /// The user signed out.
    Logout = 0,
    /// Superseded by a rotation. Normal and frequent.
    Rotated = 1,
    /// A retired refresh token was presented. Treated as theft: the whole
    /// family dies, not just this session.
    ReuseDetected = 2,
    /// Password changed.
    PasswordChanged = 3,
    /// Revoked by an operator or by moderation.
    AdminAction = 4,
    /// The device was removed.
    DeviceRemoved = 5,
}

impl RevokeReason {
    /// Numeric form, as stored.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Parses the stored form.
    #[must_use]
    pub const fn from_i16(value: i16) -> Option<Self> {
        Some(match value {
            0 => Self::Logout,
            1 => Self::Rotated,
            2 => Self::ReuseDetected,
            3 => Self::PasswordChanged,
            4 => Self::AdminAction,
            5 => Self::DeviceRemoved,
            _ => return None,
        })
    }
}

/// What kind of thing an audit entry is about.
///
/// `AuditEntry::target_kind` is an `i16` in the row and this is the numbering. It
/// lives here rather than in whichever crate writes the entry because the numbers
/// are shared: `migo-auth` writing 1 for a device and `migo-moderation` writing 1
/// for a room would make the audit log unreadable, and nothing in the schema would
/// have complained.
///
/// Numbers are append-only. A retired kind keeps its number, because old rows keep
/// their value forever and renumbering rewrites history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditTargetKind {
    /// The whole account: registration, suspension, deletion.
    Account = 0,
    /// One device of an account.
    Device = 1,
    /// One login session.
    Session = 2,
    /// A conversation.
    Conversation = 3,
    /// One message.
    Message = 4,
    /// A room.
    Room = 5,
    /// One member of a room.
    RoomMember = 6,
    /// A media object.
    Media = 7,
    /// An abuse report.
    Report = 8,
    /// A ledger account.
    LedgerAccount = 9,
    /// A ledger transaction.
    Transaction = 10,
    /// A bot.
    Bot = 11,
    /// A server node.
    Node = 12,
}

impl AuditTargetKind {
    /// Numeric form, as stored.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Parses the stored form. `None` for a number this build does not know, which
    /// is a row written by a newer version: the reader shows it as unknown rather
    /// than guessing a kind and mislabelling somebody's history.
    #[must_use]
    pub const fn from_i16(value: i16) -> Option<Self> {
        Some(match value {
            0 => Self::Account,
            1 => Self::Device,
            2 => Self::Session,
            3 => Self::Conversation,
            4 => Self::Message,
            5 => Self::Room,
            6 => Self::RoomMember,
            7 => Self::Media,
            8 => Self::Report,
            9 => Self::LedgerAccount,
            10 => Self::Transaction,
            11 => Self::Bot,
            12 => Self::Node,
            _ => return None,
        })
    }
}

/// Who performed an audited action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditActorKind {
    /// The account named in `actor_id` did it themselves.
    User = 0,
    /// The server did it with nobody asking: a scheduled purge, an expiry.
    System = 1,
    /// A bot acting on behalf of its owner.
    Bot = 2,
    /// A human operator using an administrative path.
    Operator = 3,
}

impl AuditActorKind {
    /// Numeric form, as stored.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Parses the stored form.
    #[must_use]
    pub const fn from_i16(value: i16) -> Option<Self> {
        Some(match value {
            0 => Self::User,
            1 => Self::System,
            2 => Self::Bot,
            3 => Self::Operator,
            _ => return None,
        })
    }
}

/// An account as stored.
#[derive(Clone, Debug)]
pub struct Account {
    /// Primary key.
    pub account_id: Id,
    /// Display form of the username, as the user typed it.
    pub username: String,
    /// Email, if verified or pending.
    pub email: Option<String>,
    /// Phone, if verified or pending.
    pub phone: Option<String>,
    /// Argon2id PHC string. Never a plaintext password, never a reversible form.
    pub password_hash: Secret,
    /// Lifecycle state.
    pub status: AccountStatus,
    /// ISO-3166 alpha-2, if known.
    pub country: Option<String>,
    /// BCP-47 language tag.
    pub locale: String,
    /// Creation time.
    pub created_at: Timestamp,
    /// Last modification of any account column.
    pub updated_at: Timestamp,
    /// Last successful authentication.
    pub last_login_at: Option<Timestamp>,
    /// End of a temporary suspension.
    pub suspended_until: Option<Timestamp>,
    /// When deletion was processed.
    pub deleted_at: Option<Timestamp>,
}

/// Everything needed to create an account.
#[derive(Clone, Debug)]
pub struct NewAccount {
    /// Caller-generated id, so the caller can reference it before the insert.
    pub account_id: Id,
    /// Display form of the username.
    pub username: String,
    /// Email, optional: Migo allows username-only registration.
    pub email: Option<String>,
    /// Phone, optional.
    pub phone: Option<String>,
    /// Argon2id PHC string.
    pub password_hash: Secret,
    /// BCP-47 language tag.
    pub locale: String,
    /// ISO-3166 alpha-2.
    pub country: Option<String>,
    /// Creation time, injected rather than read from the host clock.
    pub created_at: Timestamp,
}

/// The mutable, user-facing part of an account.
#[derive(Clone, Debug)]
pub struct Profile {
    /// Owner.
    pub account_id: Id,
    /// Name shown to others. Mutable, never a key.
    pub display_name: String,
    /// Free text.
    pub bio: Option<String>,
    /// Avatar media object.
    pub avatar_media_id: Option<Id>,
    /// Year only: a full birth date is more personal data than a chat app needs.
    pub birth_year: Option<i16>,
    /// Who may see last-seen time.
    pub show_last_seen: Visibility,
    /// Who may start a conversation.
    pub who_can_message: Visibility,
    /// Who may send a friend request.
    pub who_can_add: Visibility,
    /// Whether the account appears in search.
    pub searchable: bool,
    /// Last modification.
    pub updated_at: Timestamp,
}

/// A profile update. Absent fields are left alone.
#[derive(Clone, Debug, Default)]
pub struct ProfilePatch {
    /// New display name.
    pub display_name: Option<String>,
    /// New bio, or cleared.
    pub bio: Patch<String>,
    /// New avatar, or cleared.
    pub avatar_media_id: Patch<Id>,
    /// New birth year, or cleared.
    pub birth_year: Patch<i16>,
    /// New last-seen policy.
    pub show_last_seen: Option<Visibility>,
    /// New messaging policy.
    pub who_can_message: Option<Visibility>,
    /// New friend-request policy.
    pub who_can_add: Option<Visibility>,
    /// New search visibility.
    pub searchable: Option<bool>,
}

/// A registered device.
#[derive(Clone, Debug)]
pub struct Device {
    /// Primary key.
    pub device_id: Id,
    /// Owner.
    pub account_id: Id,
    /// Platform, from the protocol's enumeration.
    pub platform: Platform,
    /// Name shown in the session list.
    pub display_name: String,
    /// Client version, for compatibility decisions and for support.
    pub app_version: String,
    /// OS version, if the client disclosed it.
    pub os_version: Option<String>,
    /// Model, if the client disclosed it.
    pub device_model: Option<String>,
    /// Registration time.
    pub created_at: Timestamp,
    /// Last time the device connected.
    pub last_seen_at: Timestamp,
    /// When the device was revoked, if it was.
    pub revoked_at: Option<Timestamp>,
}

/// Everything needed to register a device.
#[derive(Clone, Debug)]
pub struct NewDevice {
    /// Caller-generated id.
    pub device_id: Id,
    /// Owner.
    pub account_id: Id,
    /// Platform.
    pub platform: Platform,
    /// Name shown in the session list.
    pub display_name: String,
    /// Client version.
    pub app_version: String,
    /// OS version, if disclosed.
    pub os_version: Option<String>,
    /// Model, if disclosed.
    pub device_model: Option<String>,
    /// Registration time.
    pub created_at: Timestamp,
}

/// A login session: one refresh-token family generation.
#[derive(Clone, Debug)]
pub struct Session {
    /// Primary key.
    pub session_id: Id,
    /// Owner.
    pub account_id: Id,
    /// Device this session belongs to.
    pub device_id: Id,
    /// All rotations of one login share this. Reuse detection kills the family.
    pub family_id: Id,
    /// Keyed hash of the refresh token, 32 bytes. The token itself is never
    /// stored: a database dump must not yield working credentials.
    pub refresh_hash: Vec<u8>,
    /// How many times this family has rotated. Useful for anomaly detection.
    pub generation: i32,
    /// Creation time.
    pub created_at: Timestamp,
    /// When the human last proved presence by typing a password or passkey.
    ///
    /// Carried forward across rotations rather than reset, because a refresh is not
    /// evidence a person is there — only evidence that whoever holds the token still
    /// holds it. Resetting it on refresh would let a stolen token keep itself
    /// permanently "freshly authenticated", which is precisely backwards.
    ///
    /// Per-session rather than per-account: an account-wide stamp would mean signing in
    /// on a phone grants freshness to a stolen session on a laptop.
    pub authenticated_at: Timestamp,
    /// When this generation was superseded.
    pub rotated_at: Option<Timestamp>,
    /// Access token expiry.
    pub access_expires_at: Timestamp,
    /// Refresh token expiry.
    pub refresh_expires_at: Timestamp,
    /// Revocation time.
    pub revoked_at: Option<Timestamp>,
    /// Why it was revoked.
    pub revoked_reason: Option<RevokeReason>,
    /// Truncated network class, never a full address.
    pub ip_class: Option<String>,
    /// Client user agent, for the session list.
    pub user_agent: Option<String>,
}

impl Session {
    /// Whether this session may still be used to refresh.
    #[must_use]
    pub fn is_live(&self, now: Timestamp) -> bool {
        self.revoked_at.is_none() && self.refresh_expires_at.as_millis() > now.as_millis()
    }
}

/// Everything needed to open a session.
#[derive(Clone, Debug)]
pub struct NewSession {
    /// Caller-generated id.
    pub session_id: Id,
    /// Owner.
    pub account_id: Id,
    /// Device.
    pub device_id: Id,
    /// Family this generation belongs to.
    pub family_id: Id,
    /// Keyed hash of the refresh token, 32 bytes.
    pub refresh_hash: Vec<u8>,
    /// Generation number within the family.
    pub generation: i32,
    /// Creation time.
    pub created_at: Timestamp,
    /// When the human last proved presence. See [`Session::authenticated_at`].
    pub authenticated_at: Timestamp,
    /// Access token expiry.
    pub access_expires_at: Timestamp,
    /// Refresh token expiry.
    pub refresh_expires_at: Timestamp,
    /// Truncated network class.
    pub ip_class: Option<String>,
    /// Client user agent.
    pub user_agent: Option<String>,
}

/// Public key material published by one device.
#[derive(Clone, Debug)]
pub struct PublishedKeys {
    /// Owner.
    pub account_id: Id,
    /// Device that owns the private halves. They never leave it.
    pub device_id: Id,
    /// 64 bytes: Ed25519 signing key followed by X25519 exchange key.
    pub identity_key: Vec<u8>,
    /// Signed prekey id.
    pub signed_prekey_id: i32,
    /// Signed prekey public bytes.
    pub signed_prekey: Vec<u8>,
    /// Signature over the prekey, made by the identity signing key.
    pub signed_prekey_signature: Vec<u8>,
    /// Expiry for the signed prekey.
    pub signed_prekey_expires_at: Timestamp,
    /// One-time prekeys, `(key_id, public_key)`.
    pub one_time_prekeys: Vec<(i32, Vec<u8>)>,
    /// Publication time.
    pub created_at: Timestamp,
}

/// What a peer receives in order to start a session.
#[derive(Clone, Debug)]
pub struct KeyBundle {
    /// Owner.
    pub account_id: Id,
    /// Device.
    pub device_id: Id,
    /// Identity key, 64 bytes.
    pub identity_key: Vec<u8>,
    /// Signed prekey id.
    pub signed_prekey_id: i32,
    /// Signed prekey public bytes.
    pub signed_prekey: Vec<u8>,
    /// Signature over the prekey.
    pub signed_prekey_signature: Vec<u8>,
    /// When the signed prekey stops being acceptable.
    ///
    /// Handed to the caller so a nearly-expired prekey becomes a "publish fresh
    /// keys" nudge to the owning device. A bundle whose signed prekey expires
    /// tomorrow still works today; nobody finding out until it stops working is
    /// the failure worth avoiding.
    pub signed_prekey_expires_at: Timestamp,
    /// A one-time prekey, consumed by this fetch. Absent when the device has run
    /// out, which weakens forward secrecy for the first message and is therefore
    /// reported to the owner rather than hidden.
    pub one_time_prekey: Option<(i32, Vec<u8>)>,
}

/// A conversation: direct, group, or the chat side of a room.
#[derive(Clone, Debug)]
pub struct Conversation {
    /// Primary key.
    pub conversation_id: Id,
    /// Direct, group, or room.
    pub kind: ConversationKind,
    /// Whether the server can read the payloads. Public and Managed rooms are
    /// server-readable by design; everything else is not.
    pub encryption: EncryptionMode,
    /// The room this conversation belongs to, when it is a room.
    pub room_id: Option<Id>,
    /// Highest assigned sequence number.
    pub last_seq: i64,
    /// Who created it.
    pub created_by: Id,
    /// Creation time.
    pub created_at: Timestamp,
    /// Time of the most recent message, for sorting a conversation list.
    pub last_message_at: Option<Timestamp>,
    /// Archival time.
    pub archived_at: Option<Timestamp>,
}

/// Membership of a conversation.
#[derive(Clone, Debug)]
pub struct ConversationMember {
    /// Conversation.
    pub conversation_id: Id,
    /// Member.
    pub account_id: Id,
    /// Role within the conversation, distinct from room roles.
    pub role: i16,
    /// When they joined.
    pub joined_at: Timestamp,
    /// When they left, if they did. Membership is tombstoned so that history
    /// access can be reasoned about after the fact.
    pub left_at: Option<Timestamp>,
    /// Notification mute expiry.
    pub muted_until: Option<Timestamp>,
    /// Whether the user pinned this conversation.
    pub pinned: bool,
}

/// A stored message.
#[derive(Clone, Debug)]
pub struct StoredMessage {
    /// Client-generated id, so a message created offline keeps its identity.
    pub message_id: Id,
    /// Conversation.
    pub conversation_id: Id,
    /// Position in the conversation. Monotonic, gapless, server-assigned.
    pub seq: i64,
    /// Sender.
    pub sender_id: Id,
    /// Sending device, for multi-device fanout decisions.
    pub sender_device: Option<Id>,
    /// What kind of message this is.
    pub kind: MessageKind,
    /// Opaque payload: ciphertext, or an MSE body for server-readable rooms.
    pub envelope: Vec<u8>,
    /// Message being replied to.
    pub reply_to: Option<Id>,
    /// Disappearing-message expiry.
    pub expires_at: Option<Timestamp>,
    /// Server receipt time.
    pub created_at: Timestamp,
    /// Last edit.
    pub edited_at: Option<Timestamp>,
    /// Tombstone time. Set rather than deleting the row, so offline clients can
    /// converge instead of keeping a ghost forever.
    pub deleted_at: Option<Timestamp>,
    /// Who deleted it: the sender, or a moderator.
    pub deleted_by: Option<Id>,
}

/// A message to append.
#[derive(Clone, Debug)]
pub struct NewMessage {
    /// Client-generated id. Reused on retry, which is what makes append
    /// idempotent.
    pub message_id: Id,
    /// Conversation.
    pub conversation_id: Id,
    /// Sender.
    pub sender_id: Id,
    /// Sending device.
    pub sender_device: Option<Id>,
    /// Kind.
    pub kind: MessageKind,
    /// Payload.
    pub envelope: Vec<u8>,
    /// Message being replied to.
    pub reply_to: Option<Id>,
    /// Disappearing-message expiry.
    pub expires_at: Option<Timestamp>,
    /// Server receipt time.
    pub created_at: Timestamp,
}

/// Outcome of an append.
///
/// A retry is not an error. The client that sent the same message id twice
/// because its connection dropped gets the original back and can carry on,
/// which is the whole reason ids are client-generated.
#[derive(Clone, Debug)]
pub enum Appended {
    /// Newly stored.
    Created(StoredMessage),
    /// Already present with this id; nothing was written.
    Duplicate(StoredMessage),
}

impl Appended {
    /// The message, however it got there.
    #[must_use]
    pub fn message(&self) -> &StoredMessage {
        match self {
            Appended::Created(message) | Appended::Duplicate(message) => message,
        }
    }

    /// Whether this call was the one that wrote the row.
    #[must_use]
    pub fn is_new(&self) -> bool {
        matches!(self, Appended::Created(_))
    }
}

/// Per-member position in a conversation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cursor {
    /// Highest sequence handed to any of the member's devices.
    pub delivered_seq: i64,
    /// Highest sequence the member has read.
    pub read_seq: i64,
    /// Highest sequence a push notification was sent for. Kept so a second
    /// device coming online does not re-notify.
    pub notified_seq: i64,
}

/// One row of a conversation list.
#[derive(Clone, Debug)]
pub struct ConversationSummary {
    /// The conversation.
    pub conversation: Conversation,
    /// Its most recent message, if any.
    pub last_message: Option<StoredMessage>,
    /// Unread count, computed as `last_seq - read_seq`, never stored.
    pub unread: i64,
    /// The caller's cursor.
    pub cursor: Cursor,
    /// The caller's own membership row.
    ///
    /// Here rather than fetched per row by the caller, because mute state and
    /// pin state are on every rendered conversation list and looking them up
    /// afterwards is the definition of an N+1. Both backends read it in one
    /// bounded query alongside the cursors.
    pub member: ConversationMember,
    /// Other members, capped by the caller's page request.
    pub members: Vec<Id>,
}

impl ConversationSummary {
    /// The keyset position of this row, for asking the next page to continue
    /// after it.
    pub fn position(&self) -> ConversationPosition {
        ConversationPosition {
            last_message_at: self.conversation.last_message_at,
            created_at: self.conversation.created_at,
            conversation_id: self.conversation.conversation_id,
        }
    }
}

/// A position in the conversation list, used to continue paging after it.
///
/// This is a keyset, not an offset. An offset silently drops or repeats rows
/// when a conversation receives a message between two page requests — which in
/// a chat list is not a corner case but the normal state of affairs. The three
/// fields are exactly the three the list is ordered by, and the third is a
/// primary key, so the order is total and the position is unambiguous.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConversationPosition {
    /// Activity of the conversation at the position, `None` when it has never
    /// carried a message. Such conversations sort last.
    pub last_message_at: Option<Timestamp>,
    /// Creation time of the conversation at the position.
    pub created_at: Timestamp,
    /// Primary key of the conversation at the position, which breaks the tie
    /// when two conversations share both timestamps.
    pub conversation_id: Id,
}

impl ConversationPosition {
    /// True when `candidate` sorts strictly after this position in the
    /// conversation list's order: activity descending with never-used
    /// conversations last, then creation descending, then id ascending.
    ///
    /// The store implementations both derive their paging from this one
    /// definition rather than restating it. A filter that disagreed with the
    /// sort by even one tie-break would drop rows from the list, and a dropped
    /// conversation looks to the user like a deleted one.
    pub fn precedes(&self, candidate: &Conversation) -> bool {
        match (self.last_message_at, candidate.last_message_at) {
            // Both active: later activity sorts first, so the next page starts
            // where activity is older, or equal and the tie-breaks say later.
            (Some(here), Some(there)) if there != here => there < here,
            // A conversation that has never carried a message sorts after every
            // one that has, and never before.
            (Some(_), None) => true,
            (None, Some(_)) => false,
            // Equal activity, or both never used: fall through to the
            // tie-breaks below.
            _ => match candidate.created_at.cmp(&self.created_at) {
                Ordering::Less => true,
                Ordering::Greater => false,
                Ordering::Equal => candidate.conversation_id > self.conversation_id,
            },
        }
    }
}

/// A room.
#[derive(Clone, Debug)]
pub struct Room {
    /// Primary key.
    pub room_id: Id,
    /// The conversation carrying its messages.
    pub conversation_id: Id,
    /// URL-safe name. Mutable; the id is what links persist against.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Topic line.
    pub topic: Option<String>,
    /// Public, Managed, or Private.
    pub kind: RoomKind,
    /// Owner.
    pub owner_id: Id,
    /// The region that sequences this room's messages. Exactly one, always:
    /// a second sequencer during a partition would fork history (ADR-0005).
    pub home_region: String,
    /// Cached member count, for browse ordering. Derived, and rebuildable.
    pub member_count: i32,
    /// Capacity.
    pub max_members: i32,
    /// Slow mode interval, zero when off.
    pub slow_mode_seconds: i32,
    /// 0 open, 1 request, 2 invite only.
    pub join_policy: i16,
    /// Whether the server can read messages. Public and Managed: yes, and the
    /// client must say so in the UI.
    pub encryption: EncryptionMode,
    /// Creation time.
    pub created_at: Timestamp,
    /// Last settings change.
    pub updated_at: Timestamp,
    /// Archival time.
    pub archived_at: Option<Timestamp>,
}

/// Everything needed to create a room.
#[derive(Clone, Debug)]
pub struct NewRoom {
    /// Caller-generated room id.
    pub room_id: Id,
    /// Caller-generated conversation id.
    pub conversation_id: Id,
    /// URL-safe name.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Topic.
    pub topic: Option<String>,
    /// Kind.
    pub kind: RoomKind,
    /// Owner, who becomes the first member with the owner role.
    pub owner_id: Id,
    /// Sequencing region.
    pub home_region: String,
    /// Capacity.
    pub max_members: i32,
    /// Encryption mode, which the kind constrains.
    pub encryption: EncryptionMode,
    /// Creation time.
    pub created_at: Timestamp,
}

/// Room membership, including the moderation state that attaches to it.
#[derive(Clone, Debug)]
pub struct RoomMember {
    /// Room.
    pub room_id: Id,
    /// Member.
    pub account_id: Id,
    /// Role.
    pub role: RoomRole,
    /// Permissions added on top of the role.
    pub permissions_grant: u64,
    /// Permissions removed. Deny wins over grant, always.
    pub permissions_deny: u64,
    /// Join time.
    pub joined_at: Timestamp,
    /// Leave time.
    pub left_at: Option<Timestamp>,
    /// Mute expiry.
    pub muted_until: Option<Timestamp>,
    /// Ban expiry. A permanent ban is a far-future timestamp, not a null: "no
    /// expiry" and "not banned" must never be the same value.
    pub banned_until: Option<Timestamp>,
    /// Reason shown to the banned user.
    pub ban_reason: Option<String>,
    /// Who invited them.
    pub invited_by: Option<Id>,
}

impl RoomMember {
    /// Whether the member is currently in the room.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.left_at.is_none()
    }

    /// Whether a ban is in force.
    #[must_use]
    pub fn is_banned(&self, now: Timestamp) -> bool {
        self.banned_until
            .is_some_and(|until| until.as_millis() > now.as_millis())
    }

    /// Whether a mute is in force.
    #[must_use]
    pub fn is_muted(&self, now: Timestamp) -> bool {
        self.muted_until
            .is_some_and(|until| until.as_millis() > now.as_millis())
    }
}

/// A directed edge in the social graph.
#[derive(Clone, Debug)]
pub struct Relationship {
    /// Who owns the edge.
    pub account_id: Id,
    /// The other end.
    pub other_id: Id,
    /// Friend, follow, block, or favourite.
    pub kind: RelationshipKind,
    /// When it was created.
    pub created_at: Timestamp,
    /// When a friend request was accepted. `None` on a friend edge means the
    /// request is still pending — there is no separate request table.
    pub accepted_at: Option<Timestamp>,
}

/// Currency in the ledger. Distinct units never mix in one transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Currency {
    /// Purchasable soft currency.
    Coins = 0,
    /// Premium currency.
    Gems = 1,
    /// Non-transferable reputation points.
    Points = 2,
}

impl Currency {
    /// Numeric form, as stored.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Parses the stored form.
    #[must_use]
    pub const fn from_i16(value: i16) -> Option<Self> {
        Some(match value {
            0 => Self::Coins,
            1 => Self::Gems,
            2 => Self::Points,
            _ => return None,
        })
    }
}

/// What a ledger account is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LedgerAccountKind {
    /// Belongs to a user.
    User = 0,
    /// Where new currency is created. Its balance goes negative by design, and
    /// that negative is the total ever issued — a number worth being able to
    /// read off directly.
    Mint = 1,
    /// Platform fees.
    Fee = 2,
    /// Holds value mid-transaction, e.g. a game stake.
    Escrow = 3,
}

impl LedgerAccountKind {
    /// Numeric form, as stored.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Parses the stored form.
    #[must_use]
    pub const fn from_i16(value: i16) -> Option<Self> {
        Some(match value {
            0 => Self::User,
            1 => Self::Mint,
            2 => Self::Fee,
            3 => Self::Escrow,
            _ => return None,
        })
    }
}

/// A ledger account. Users have several: one per currency.
#[derive(Clone, Debug)]
pub struct LedgerAccount {
    /// Primary key.
    pub ledger_account_id: Id,
    /// Owner, absent for system accounts.
    pub owner_id: Option<Id>,
    /// Purpose.
    pub kind: LedgerAccountKind,
    /// Unit.
    pub currency: Currency,
    /// Creation time.
    pub created_at: Timestamp,
}

/// One leg of a transaction.
#[derive(Clone, Copy, Debug)]
pub struct LedgerLeg {
    /// Ledger account, not user account.
    pub ledger_account_id: Id,
    /// Signed minor units. Never zero.
    pub amount: i64,
}

/// A transaction to post. Legs must sum to zero.
#[derive(Clone, Debug)]
pub struct NewTransaction {
    /// Caller-generated id.
    pub tx_id: Id,
    /// Why this happened, from the economy's reason table.
    pub reason: i16,
    /// What it refers to: a gift, a purchase, a game.
    pub ref_id: Option<Id>,
    /// Retry key. A repeated key returns the original transaction rather than
    /// charging twice.
    pub idempotency_key: String,
    /// Who initiated it, absent for system transactions.
    pub created_by: Option<Id>,
    /// Unit. One currency per transaction.
    pub currency: Currency,
    /// The legs.
    pub legs: Vec<LedgerLeg>,
    /// What this transaction delivers, written with it or not at all.
    pub receipt: Option<Receipt>,
    /// Posting time.
    pub created_at: Timestamp,
}

/// A posted transaction.
#[derive(Clone, Debug)]
pub struct LedgerTransaction {
    /// Primary key.
    pub tx_id: Id,
    /// Reason.
    pub reason: i16,
    /// Reference.
    pub ref_id: Option<Id>,
    /// Retry key.
    pub idempotency_key: String,
    /// Initiator.
    pub created_by: Option<Id>,
    /// Posting time.
    pub created_at: Timestamp,
    /// The legs, as stored.
    pub legs: Vec<LedgerLeg>,
}

/// Outcome of posting a transaction.
#[derive(Clone, Debug)]
pub enum Posted {
    /// Newly written.
    Created(LedgerTransaction),
    /// The idempotency key already existed; nothing was written.
    Duplicate(LedgerTransaction),
}

impl Posted {
    /// The transaction, however it got there.
    #[must_use]
    pub fn transaction(&self) -> &LedgerTransaction {
        match self {
            Posted::Created(tx) | Posted::Duplicate(tx) => tx,
        }
    }

    /// Whether this call was the one that wrote the rows.
    #[must_use]
    pub fn is_new(&self) -> bool {
        matches!(self, Posted::Created(_))
    }
}

/// What a transaction delivered, written in the same statement sequence as its legs.
///
/// A gift that charged the sender and delivered nothing, or a purchase that took the
/// coins and granted no theme, is a support ticket that arrives a week later with no
/// evidence left. Attaching the delivery to the posting is what makes those two states
/// unreachable rather than merely rare: the ledger's idempotency key already collapses a
/// retry into one transaction, and one transaction carries exactly one receipt.
#[derive(Clone, Debug)]
pub enum Receipt {
    /// A gift, which somebody else receives.
    Gift(GiftReceipt),
    /// Something the buyer keeps.
    Entitlement {
        /// Catalogue code, e.g. `theme.midnight`.
        sku: String,
    },
}

/// The delivery half of a gift purchase.
#[derive(Clone, Debug)]
pub struct GiftReceipt {
    /// Caller-generated id for the receipt row.
    pub gift_id: Id,
    /// Who paid.
    pub sender_id: Id,
    /// Who received.
    pub recipient_id: Id,
    /// Catalogue code, e.g. `gift.dragon`.
    pub gift_code: String,
    /// Where it was shown, if anywhere.
    pub conversation_id: Option<Id>,
}

/// A gift as stored.
#[derive(Clone, Debug)]
pub struct GiftSent {
    /// Primary key.
    pub gift_id: Id,
    /// The transaction that paid for it.
    pub tx_id: Id,
    /// Who paid.
    pub sender_id: Id,
    /// Who received.
    pub recipient_id: Id,
    /// Catalogue code.
    pub gift_code: String,
    /// Where it was shown.
    pub conversation_id: Option<Id>,
    /// When.
    pub created_at: Timestamp,
}

/// Something an account owns.
#[derive(Clone, Debug)]
pub struct Entitlement {
    /// Owner.
    pub account_id: Id,
    /// Catalogue code.
    pub sku: String,
    /// When it was acquired.
    pub acquired_at: Timestamp,
    /// The purchase, absent when the system granted it.
    pub tx_id: Option<Id>,
}

/// An account's XP and level.
///
/// `xp` is authoritative and `level` is a projection of it, kept as a column so that a
/// leaderboard and a profile do not have to recompute a curve for every row. A reader
/// that finds the two disagreeing should trust `xp`: the level is rewritten immediately
/// after the XP that crossed a threshold, and the window between the two writes is the
/// only way they can differ.
#[derive(Clone, Copy, Debug)]
pub struct Progression {
    /// Whose.
    pub account_id: Id,
    /// Total experience.
    pub xp: i64,
    /// Cached level.
    pub level: i32,
    /// Last change.
    pub updated_at: Timestamp,
}

/// The result of adding XP.
///
/// Both totals, because the only question the caller has afterwards is whether a
/// threshold was crossed, and answering it needs the number on each side. Returning only
/// the new total would force a read before the write, and two callers awarding at once
/// would each compute the same "before".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XpChange {
    /// Total before this award.
    pub before: i64,
    /// Total after it.
    pub after: i64,
}

/// One XP award, on its way in.
///
/// `source` is a raw `i16` for the same reason `LedgerTransaction::reason` is: the list of
/// things that earn XP is section 30's, it belongs to `migo-economy`, and a store that
/// held the enum would have to be edited every time a domain crate added a way to earn.
#[derive(Clone, Debug)]
pub struct NewXpAward {
    /// Caller-generated id.
    pub award_id: Id,
    /// Who earned it.
    pub account_id: Id,
    /// Which of section 30's activities, numbered by `migo-economy`.
    pub source: i16,
    /// How much. Must be positive; the schema refuses anything else.
    pub amount: i64,
    /// The game, event, or room that produced it, where one did.
    pub ref_id: Option<Id>,
    /// Retry key, when the caller has something stable to key on.
    ///
    /// `None` for an award that cannot be replayed. A daily bonus keys on the day, so a
    /// job that runs twice grants it once; a game win keys on the session, so a client
    /// that resends the result does not get paid twice for it.
    pub idempotency_key: Option<String>,
    /// Server time.
    pub at: Timestamp,
}

/// Which population a leaderboard ranks.
///
/// Section 32 lists a global, a country, and a room leaderboard. One parameter rather
/// than three methods, because the three differ only in which accounts are eligible —
/// the ordering, the tiebreak, the windowing, and the clamp are identical, and three
/// methods would be three places to get them consistent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope<'a> {
    /// Everybody.
    Global,
    /// Accounts registered to one ISO 3166-1 alpha-2 country.
    Country(&'a str),
    /// The current members of one room.
    ///
    /// Ranked by what they have earned anywhere, not by what they earned in the room:
    /// there is no per-room total, and five of section 30's seven earning sources have
    /// nothing to do with a room.
    Room(Id),
}

/// A badge somebody holds.
#[derive(Clone, Debug)]
pub struct BadgeAward {
    /// Holder.
    pub account_id: Id,
    /// Catalogue code, e.g. `badge.veteran`.
    pub badge_code: String,
    /// When it was granted.
    pub awarded_at: Timestamp,
    /// What earned it, if anything nameable did.
    pub ref_id: Option<Id>,
}

/// One row of a leaderboard.
///
/// Carries no display name and no country. A leaderboard row is a rank and a number; the
/// name beside it is a profile read the caller already knows how to do, and putting it
/// here would make every leaderboard query a join whose result is then cached under a key
/// that goes stale when somebody renames themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Standing {
    /// Whose.
    pub account_id: Id,
    /// Total experience.
    pub xp: i64,
    /// Level.
    pub level: i32,
}

/// Values of the `game_session.status` column.
///
/// The column is a `smallint`, so the meaning of each number lives in Rust rather
/// than only in a SQL comment. What *kind* of game a row is — the `kind` column —
/// stays a raw `i16` here and is given meaning by `migo-games`, for the same
/// reason [`NewXpAward::source`] is raw: the roster of games is a domain crate's to
/// grow, and a store that held the enum would need editing every time one was added.
pub mod game_status {
    /// In progress. The only status a move may be applied to.
    pub const OPEN: i16 = 0;
    /// Played to a conclusion — a win, a draw, or every round finished.
    pub const FINISHED: i16 = 1;
    /// Ended without a conclusion — a player left, or it timed out.
    pub const ABANDONED: i16 = 2;
}

/// One game in progress, or one that has ended.
///
/// The row is the whole authority. `state` is an opaque blob the store never reads
/// into — brief section 89 puts the game's state on the server so that a client
/// which computes its own score cannot report a flattering one, and the store
/// keeping the bytes uninterpreted is what makes that literally true: there is no
/// code here that could be fooled by them. `migo-games` owns the codec; the store
/// owns only that the bytes are written and read back unchanged.
#[derive(Clone, Debug)]
pub struct GameSession {
    /// Primary key.
    pub game_id: Id,
    /// Which game this is, numbered by `migo-games`. Raw here on purpose.
    pub kind: i16,
    /// The conversation the game is played in. Membership of it is the
    /// authorisation check for every move, answered by [`MessagingStore::is_member`].
    pub conversation_id: Id,
    /// Authoritative state, opaque to the store.
    pub state: Vec<u8>,
    /// Whose move it is, for a turn-based game. `None` for a game that is
    /// simultaneous or finished.
    pub turn_of: Option<Id>,
    /// [`game_status`].
    pub status: i16,
    /// The currency a stake is denominated in, if this deployment runs staked
    /// games. Unused by the default engines: brief sections 37 and 87 forbid
    /// anything resembling real-money gambling, so no game shipped here moves a
    /// stake. The columns exist for a future, separately reviewed extension.
    pub stake_currency: Option<i16>,
    /// The stake amount, paired with [`GameSession::stake_currency`].
    pub stake_amount: Option<i64>,
    /// When the game was created.
    pub created_at: Timestamp,
    /// When the state last changed. Doubles as the optimistic-lock token: a move
    /// is applied against the `updated_at` it was computed from, and a move
    /// computed against a stale state finds the token moved and is refused rather
    /// than written on top of a newer one. See [`AdvanceGame`].
    pub updated_at: Timestamp,
    /// When it reached a terminal status, if it has.
    pub finished_at: Option<Timestamp>,
}

/// A game to create.
#[derive(Clone, Debug)]
pub struct NewGame {
    /// Primary key, minted by the caller.
    pub game_id: Id,
    /// Which game, numbered by `migo-games`.
    pub kind: i16,
    /// The conversation it belongs to.
    pub conversation_id: Id,
    /// The opening authoritative state.
    pub state: Vec<u8>,
    /// Who moves first, for a turn-based game.
    pub turn_of: Option<Id>,
    /// A stake currency, for a staked deployment. `None` for every default game.
    pub stake_currency: Option<i16>,
    /// A stake amount, paired with [`NewGame::stake_currency`].
    pub stake_amount: Option<i64>,
    /// Creation instant; becomes both `created_at` and the first `updated_at`.
    pub at: Timestamp,
}

/// A move to apply, as a compare-and-swap on one game's state.
///
/// The store applies it only if the row still carries [`AdvanceGame::expected_updated_at`]
/// and is still open. Two moves that raced from the same state both name the same
/// expected token; the first to commit changes it, and the second finds its
/// expectation false and is refused — which is how brief section 90's "server is
/// authoritative, replays are rejected" is enforced at the storage layer rather
/// than hoped for above it. A refusal returns `None`; the caller re-reads and
/// decides afresh.
#[derive(Clone, Debug)]
pub struct AdvanceGame {
    /// Which game.
    pub game_id: Id,
    /// The `updated_at` the new state was computed from. The lock token.
    pub expected_updated_at: Timestamp,
    /// The new authoritative state.
    pub state: Vec<u8>,
    /// Whose move it becomes. `None` when the game is finishing or simultaneous.
    pub turn_of: Option<Id>,
    /// The status after this move, from [`game_status`]. `finished_at` is set
    /// whenever this is not [`game_status::OPEN`].
    pub status: i16,
    /// The new `updated_at`, and `finished_at` for a terminal status.
    pub at: Timestamp,
}

/// A bot as stored: one row of `bot`, joined to nothing.
///
/// A bot is an ordinary account that a human owns and that authenticates by a
/// bearer token instead of a password (brief section 36). The backing account —
/// [`Bot::account_id`], unique across all bots — is what carries the username,
/// avatar, and profile, and what lets a bot be a conversation member, send
/// messages, and be a moderation subject; this row is only the bot-specific part
/// beside it. The token itself is never here: [`Bot::token_hash`] is a keyed HMAC
/// tag of it, so a database dump yields no working credential (section 145). The
/// meaning of the [`Bot::scopes`] bits belongs to `migo-bots`, not to the store, so
/// this layer keeps them as the raw integer exactly as it keeps a game's `kind`.
#[derive(Clone, Debug)]
pub struct Bot {
    /// Primary key.
    pub bot_id: Id,
    /// The human account that owns and may manage this bot.
    pub owner_id: Id,
    /// The account the bot posts under. Unique: one account backs at most one bot.
    pub account_id: Id,
    /// The bot's name, kept in step with the backing account's display name by the
    /// service that owns it.
    pub name: String,
    /// The keyed HMAC tag of the current token, never the token. Unique.
    pub token_hash: Vec<u8>,
    /// The permission bitmask, as stored. Interpreted by `migo-bots`.
    pub scopes: i64,
    /// Where the deployment POSTs updates for this bot, if its owner set one.
    pub webhook_url: Option<String>,
    /// Creation time.
    pub created_at: Timestamp,
    /// When the bot was disabled, if it is. A disabled bot's token no longer
    /// authenticates; the row and its backing account survive so history stays
    /// intact and the owner can re-enable it.
    pub disabled_at: Option<Timestamp>,
}

/// Everything needed to register a bot, written as one unit.
///
/// The backing account, that account's profile, and the bot row are created
/// together in [`crate::traits::BotStore::register_bot`], because none of the three
/// is usable alone: a bot account with no bot row is an account whose password is a
/// hash of bytes nobody kept — it can never be signed into and nothing else knows
/// how to read it. There is no valid intermediate state, so there is one write, the
/// same reasoning that makes `create_room` build a room, its conversation, and its
/// owner's membership at once.
#[derive(Clone, Debug)]
pub struct NewBot {
    /// The bot row's id.
    pub bot_id: Id,
    /// The human who owns it.
    pub owner_id: Id,
    /// The backing account's id. Caller-generated, so the caller can name it before
    /// the insert, and unique across all bots.
    pub account_id: Id,
    /// The backing account's username, in display form. `migo-bots` has already
    /// validated it the same way a human registration would.
    pub username: String,
    /// The bot's name, stored on both the bot row and the profile's display name.
    pub display_name: String,
    /// The backing account's password hash: a valid Argon2id hash of random bytes
    /// that were discarded, so the account can never be signed into by password.
    /// Never a plaintext, never a known sentinel.
    pub password_hash: Secret,
    /// The keyed HMAC tag of the bot's freshly minted token. The token is returned
    /// to the owner once and never stored.
    pub token_hash: Vec<u8>,
    /// The initial permission bitmask, as stored. `migo-bots` defaults it to the
    /// minimum (section 41).
    pub scopes: i64,
    /// The owner's webhook endpoint, if one was given.
    pub webhook_url: Option<String>,
    /// BCP-47 locale for the backing account.
    pub locale: String,
    /// Creation time for all three rows.
    pub created_at: Timestamp,
}

/// A media object's metadata. The bytes live in object storage; the API process
/// never touches them.
#[derive(Clone, Debug)]
pub struct MediaObject {
    /// Primary key.
    pub media_id: Id,
    /// Uploader.
    pub owner_id: Id,
    /// Image, video, audio, file.
    pub kind: i16,
    /// Declared MIME type, validated against the sniffed one before use.
    pub mime: String,
    /// Size in bytes.
    pub byte_size: i64,
    /// Pixel width, for images and video.
    pub width: Option<i32>,
    /// Pixel height.
    pub height: Option<i32>,
    /// Duration, for audio and video.
    pub duration_ms: Option<i32>,
    /// Key in the bucket.
    pub storage_key: String,
    /// Where the upload was destined, recorded at upload time.
    ///
    /// The whole of media authorisation rests on this column. Brief section 168
    /// requires a download to be authorised by asking whether the requester belongs
    /// to the conversation or room that carries the media, and nothing else in the
    /// schema can answer that: for end-to-end media the reference lives inside the
    /// ciphertext, so the server will never see it in a message. Recording the
    /// destination when the upload begins is the only moment the server is told.
    ///
    /// `None` is profile media — an avatar — which every authenticated account may
    /// render, and which therefore needs no conversation to be checked against.
    pub conversation_id: Option<Id>,
    /// Content hash, for deduplication and integrity.
    pub checksum: Option<Vec<u8>>,
    /// 0 pending, 1 clean, 2 rejected.
    pub scan_status: i16,
    /// Creation time.
    pub created_at: Timestamp,
    /// Tombstone.
    pub deleted_at: Option<Timestamp>,
}

/// A user report.
#[derive(Clone, Debug)]
pub struct Report {
    /// Primary key.
    pub report_id: Id,
    /// Who reported.
    pub reporter_id: Id,
    /// Numbered by [`report_subject`].
    pub subject_kind: i16,
    /// What was reported.
    pub subject_id: Id,
    /// Room context, if any.
    pub room_id: Option<Id>,
    /// Reason code.
    pub reason: i16,
    /// Reporter's note.
    pub note: Option<String>,
    /// A reference to evidence, never a copy of message content.
    pub evidence_ref: Option<Id>,
    /// 0 open, 1 actioned, 2 dismissed.
    pub status: i16,
    /// Filing time.
    pub created_at: Timestamp,
    /// Resolution time.
    pub resolved_at: Option<Timestamp>,
    /// Who resolved it.
    pub resolved_by: Option<Id>,
    /// What was done.
    pub resolution: Option<i16>,
}

/// An audit record. Append-only, written in the same transaction as the action.
#[derive(Clone, Debug)]
pub struct AuditEntry {
    /// Primary key.
    pub audit_id: Id,
    /// Who acted, absent for system actions.
    pub actor_id: Option<Id>,
    /// Numbered by [`AuditActorKind`].
    pub actor_kind: i16,
    /// Stable action name, e.g. `room.member.ban`.
    pub action: String,
    /// What kind of thing was acted on, numbered by [`AuditTargetKind`].
    pub target_kind: i16,
    /// Which one.
    pub target_id: Option<Id>,
    /// What changed. Never message content.
    pub summary: String,
    /// Operator-supplied reason.
    pub reason: Option<String>,
    /// Request id, to join this against logs and traces.
    pub request_id: Option<String>,
    /// Truncated network class.
    pub ip_class: Option<String>,
    /// When it happened.
    pub created_at: Timestamp,
}

/// Values of the `report.subject_kind` column.
///
/// Numbered here rather than in the moderation crate because the numbers are what the
/// `report_subject_idx` index is keyed on: a second definition that disagreed would
/// silently split one subject's reports across two values of the same column.
pub mod report_subject {
    /// A whole account.
    pub const USER: i16 = 0;
    /// One message. `subject_id` is the message id; the conversation is not stored,
    /// because a report is a pointer and the action names its own target.
    pub const MESSAGE: i16 = 1;
    /// A room.
    pub const ROOM: i16 = 2;
    /// A media object.
    pub const MEDIA: i16 = 3;
    /// A bot, keyed by `bot.bot_id` and not by the account the bot signs in as.
    ///
    /// Brief section 49 lists a bot report beside the user, message, and room ones. A
    /// bot has an account row, so this could have been folded into [`USER`] — and then
    /// a moderator reading the queue could not tell "this person is abusive" from "this
    /// integration is broken", which are different problems with different remedies.
    pub const BOT: i16 = 4;
}

/// Values of the `report.status` column.
///
/// The column is a `smallint` rather than a Postgres enum, so the meaning of
/// each number has to live somewhere in Rust or it lives only in a SQL comment.
/// It lives here.
pub mod report_status {
    /// Filed, not yet triaged.
    pub const OPEN: i16 = 0;
    /// Triaged, and something was done about it.
    pub const ACTIONED: i16 = 1;
    /// Triaged, and nothing needed doing.
    pub const DISMISSED: i16 = 2;
}

/// Values of the `media_object.scan_status` column.
pub mod media_scan {
    /// Uploaded, not yet scanned. Servable only to the uploader.
    pub const PENDING: i16 = 0;
    /// Scanned and allowed.
    pub const CLEAN: i16 = 1;
    /// Scanned and refused. The bytes are deleted; the row stays so a repeat
    /// upload of the same checksum can be refused without rescanning.
    pub const REJECTED: i16 = 2;
}

/// Values of the `room.join_policy` column.
pub mod join_policy {
    /// Anyone may join.
    pub const OPEN: i16 = 0;
    /// A moderator must approve.
    pub const APPROVAL: i16 = 1;
    /// Invitation only.
    pub const INVITE: i16 = 2;
}

/// One row of the notification inbox.
///
/// The kinds that live here are the ones that answering does not clear by itself:
/// a gift, a level up, a badge, a room invitation, a room announcement, an event, a
/// game challenge, a missed call. The message-shaped kinds are counted from
/// [`Cursor`] instead, and a pending friend request is counted from
/// [`Relationship`], because both of those already record their own state and a
/// second copy of a count is a count that will disagree with the first.
///
/// [`notification_kind::is_storable`] is where that rule is enforced rather than
/// described.
#[derive(Clone, Debug)]
pub struct Notification {
    /// Primary key.
    pub notification_id: Id,
    /// Whose inbox.
    pub account_id: Id,
    /// Numbered by `NotificationKind`.
    pub kind: i16,
    /// Room context, where there is one.
    pub room_id: Option<Id>,
    /// Who caused it. `None` for a level up, which nobody caused.
    pub actor_id: Option<Id>,
    /// What it points at: a transaction, a badge award, a call, a game session.
    /// Which table depends on the kind, so there is no foreign key.
    pub subject_id: Option<Id>,
    /// When it happened.
    pub created_at: Timestamp,
    /// When it was read, if it was.
    pub read_at: Option<Timestamp>,
}

/// Which `NotificationKind` values may be stored, and which may only be delivered.
///
/// The numbering is the protocol's; it is repeated here because the
/// `notification_unread_idx` predicate and this list have to agree, and a second
/// definition that disagreed would silently divide one inbox into two.
pub mod notification_kind {
    /// A gift arrived. `subject_id` is the ledger transaction.
    pub const GIFT: i16 = 5;
    /// The account crossed a level. `subject_id` is unset; nobody caused it.
    pub const LEVEL_UP: i16 = 6;
    /// A badge was awarded. `subject_id` is the badge award.
    pub const ACHIEVEMENT: i16 = 7;
    /// Somebody was invited to a room. `room_id` and `actor_id` are both set.
    pub const ROOM_INVITE: i16 = 8;
    /// A room made an announcement.
    pub const ROOM_ANNOUNCEMENT: i16 = 9;
    /// A scheduled event is about to start.
    pub const EVENT: i16 = 10;
    /// Somebody challenged the account to a game. `subject_id` is the session.
    pub const GAME_CHALLENGE: i16 = 11;
    /// A call rang out unanswered. `subject_id` is the call.
    pub const MISSED_CALL: i16 = 13;

    /// Every kind this table accepts.
    pub const STORABLE: [i16; 8] = [
        GIFT,
        LEVEL_UP,
        ACHIEVEMENT,
        ROOM_INVITE,
        ROOM_ANNOUNCEMENT,
        EVENT,
        GAME_CHALLENGE,
        MISSED_CALL,
    ];

    /// Whether a kind belongs in the inbox rather than only on the wire.
    ///
    /// Both backends call this before an insert, so "a message is not a
    /// notification row" is a rule the storage layer enforces and not a convention
    /// the next caller has to have read about.
    #[must_use]
    pub fn is_storable(kind: i16) -> bool {
        STORABLE.contains(&kind)
    }
}

/// Which push service a registration belongs to.
///
/// Not in the protocol schema, and deliberately: which service carries a wake-up is
/// a deployment fact, and a client that could name its own provider could name one
/// the deployment does not run. The client reports its platform; the gateway decides
/// the provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PushProvider {
    /// Firebase Cloud Messaging, for Android and for the web where it is available.
    Fcm = 0,
    /// Apple Push Notification service.
    Apns = 1,
    /// The W3C Web Push protocol, for browsers.
    WebPush = 2,
}

impl PushProvider {
    /// Numeric form, as stored.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Parses the stored form.
    #[must_use]
    pub const fn from_i16(value: i16) -> Option<Self> {
        Some(match value {
            0 => Self::Fcm,
            1 => Self::Apns,
            2 => Self::WebPush,
            _ => return None,
        })
    }
}

/// A device's push registration, as the storage layer sees it.
///
/// Two fields for one credential, because a push token has to survive two
/// contradictory requirements: section 77 says it is stored hashed and never
/// logged, and the delivery path has to be able to send to it. A one-way hash of a
/// push token is a token nobody can ever push to.
///
/// So `sealed` is the token encrypted by the caller before it arrived here, and
/// `hash` is the handle everything else uses — lookups, deduplication, and any log
/// line that has to say *which* registration failed. The storage layer holds no key
/// for `sealed` and cannot tell you what is inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRegistration {
    /// The token sealed by the caller: `nonce || ciphertext || tag`, base64.
    pub sealed: String,
    /// Lookup handle over the raw token. Hex, lower case.
    pub hash: String,
    /// Which push service, numbered by `PushProvider`.
    pub provider: i16,
}

/// Where one push has to go.
///
/// Returned by [`crate::traits::NotifyStore::push_targets`] and carrying exactly
/// what a sender needs: which device, which service, and the sealed token to open.
/// Not the account id — the caller asked by account and already knows it, and a
/// struct that repeats the question in every answer is a struct that ends up in a
/// log line with the answer attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushTarget {
    /// Which device, so a failure can be attributed and revoked.
    pub device_id: Id,
    /// The device's platform, which decides payload shape.
    pub platform: Platform,
    /// The registration to open and send to.
    pub registration: PushRegistration,
    /// When the registration was last refreshed, for the staleness sweep.
    pub updated_at: Timestamp,
}

/// A federation peer, as the storage layer holds it: one row of the `node_peer`
/// allow-list.
///
/// The allow-list is the mesh's security boundary (brief section 170): a node that
/// is not in it does not federate, and a packet from an unknown node is refused
/// before its payload is decoded (section 169). The public key *is* the identity —
/// `node_id` is the handle an operator reads and a handshake announces, but the key
/// is what a signature is checked against — so the key column is unique in its own
/// right, not merely the id.
///
/// `status` is a raw `i16`, opaque here exactly as a bot's `scopes` are: what 0, 1,
/// and 2 mean — allowed, paused, blocked — is `migo-federation`'s to say, not the
/// store's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRecord {
    /// The peer's node id, its handle in the allow-list. The domain layer keeps an
    /// [`Id`] here in text form; the store holds it verbatim.
    pub node_id: String,
    /// The peer's Ed25519 public key, 32 bytes. Unique across the table: two peers
    /// cannot share an identity.
    pub public_key: Vec<u8>,
    /// Where to reach the peer's mesh listener.
    pub base_url: String,
    /// The peer's region.
    pub region: String,
    /// Allow-list state, raw: 0 allowed, 1 paused, 2 blocked. Interpreted by
    /// `migo-federation`.
    pub status: i16,
    /// When the operator added the peer.
    pub added_at: Timestamp,
    /// When a handshake from the peer last succeeded, if ever.
    pub last_seen_at: Option<Timestamp>,
}

/// A new federation peer to add to the allow-list.
///
/// There is no update-in-place for the identity fields: a peer's key, base URL, or
/// region changing is a remove-and-re-add the operator performs deliberately, never
/// a silent overwrite. Only `status` and `last_seen_at` move after a peer is
/// admitted.
#[derive(Clone, Debug)]
pub struct NewPeer {
    /// The peer's node id.
    pub node_id: String,
    /// The peer's Ed25519 public key, 32 bytes.
    pub public_key: Vec<u8>,
    /// Where to reach the peer's mesh listener.
    pub base_url: String,
    /// The peer's region.
    pub region: String,
    /// Initial allow-list state, raw. Ordinarily 0, allowed.
    pub status: i16,
    /// When the operator added it.
    pub added_at: Timestamp,
}

/// One outbound federation event, as the storage layer holds it: a row of
/// `federation_outbox`.
///
/// The outbox is a durable queue rather than a Redis one, for the two reasons the
/// schema records: it must survive a restart, and it is written in the *same
/// transaction* as the state change it announces, so an event exists if and only if
/// the change it describes committed. Delivery is at least once — a crash between
/// the wire acknowledgement and [`crate::traits::FederationStore::mark_delivered`]
/// resends — so every federation consumer must be idempotent (section 153).
///
/// The `payload` is the already-encoded MWP frame body: opaque bytes here, and a
/// private message inside one is a sealed envelope this layer cannot open (section
/// 169).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxRecord {
    /// The event's id, time-ordered so the queue drains roughly in creation order.
    pub event_id: Id,
    /// The node id this event is addressed to.
    pub target_node: String,
    /// The federation opcode, 208 to 223, the payload is framed as.
    pub opcode: i32,
    /// The encoded frame body. Opaque bytes; a sealed envelope stays sealed.
    pub payload: Vec<u8>,
    /// How many delivery attempts have been made and failed.
    pub attempts: i32,
    /// When the event was enqueued.
    pub created_at: Timestamp,
    /// The earliest time the next attempt may be made — `created_at` for a fresh
    /// event, pushed out by backoff after each failure.
    pub next_attempt_at: Timestamp,
    /// When delivery was confirmed, if it has been. A delivered event is never read
    /// by [`due_events`](crate::traits::FederationStore::due_events) again.
    pub delivered_at: Option<Timestamp>,
    /// The last delivery error, for an operator. Never sent to a peer.
    pub last_error: Option<String>,
}

/// A new outbound federation event to enqueue.
///
/// The caller supplies `event_id` — so it can reference the event before the insert
/// lands — and `next_attempt_at`, ordinarily equal to `created_at`, which makes the
/// event due at once. `attempts` starts at zero and `delivered_at` at `None`; those
/// are the store's to set.
#[derive(Clone, Debug)]
pub struct NewOutboxEvent {
    /// The event's id, caller-generated and time-ordered.
    pub event_id: Id,
    /// The node id to deliver to.
    pub target_node: String,
    /// The federation opcode the payload is framed as.
    pub opcode: i32,
    /// The encoded frame body.
    pub payload: Vec<u8>,
    /// When the event was enqueued.
    pub created_at: Timestamp,
    /// The earliest time it may be attempted — usually `created_at`.
    pub next_attempt_at: Timestamp,
}
