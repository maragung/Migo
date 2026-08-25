//! Inputs, bounds, and configuration for the rooms service.

use migo_core::config::NodeConfig;
use migo_core::{Id, Timestamp};
use migo_protocol::{RoomKind, RoomRole};
use migo_ratelimit::TrustTier;

/// Shortest a slug may be.
///
/// Three, not one. A one-character slug is a land grab on a namespace that has to
/// last, and `migo://room/a` is not a name anybody could tell somebody else over a
/// phone call.
pub const MIN_SLUG_LEN: usize = 3;

/// Longest a slug may be.
pub const MAX_SLUG_LEN: usize = 32;

/// Longest a display name may be.
pub const MAX_NAME_LEN: usize = 64;

/// Longest a topic line may be.
pub const MAX_TOPIC_LEN: usize = 256;

/// Longest ban reason shown to the person who was banned.
///
/// Bounded because it is stored, returned to the banned account, and written by
/// somebody who is annoyed. The limit is generous enough for a sentence and small
/// enough that it cannot be used as free storage.
pub const MAX_REASON_LEN: usize = 256;

/// Longest search term a listing will accept.
pub const MAX_QUERY_LEN: usize = 48;

/// Rooms returned by one listing when the caller does not say.
pub const DEFAULT_LIST_LIMIT: u32 = 20;

/// Most rooms one listing can return.
///
/// A browse screen is a screen. Asking for a thousand rooms is either a scraper or a
/// client that means to page, and both are better served by a clamp than by a
/// refusal: a listing that renders truncated is usable, one that fails is not.
pub const MAX_LIST_LIMIT: u32 = 50;

/// Members returned by one roster page.
pub const MAX_ROSTER_PAGE: u16 = 100;

/// Longest slow mode an operator can set, in seconds.
///
/// One hour. Past that the setting stops being slow mode and becomes a read-only
/// room, which is a different thing that should be expressed as a permission rather
/// than as a very large number of seconds.
pub const MAX_SLOW_MODE_SECONDS: i32 = 3600;

/// Longest a mute may last, in milliseconds.
///
/// Thirty days. A mute is a cool-down; a permanent silence is a ban, and the two are
/// kept distinct so that "muted forever" cannot be used to avoid the audit trail a
/// ban leaves.
pub const MAX_MUTE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// The timestamp a permanent ban is stored as.
///
/// A far-future instant rather than a null, because `banned_until = NULL` already
/// means *not banned* — `migo_store::model::RoomMember` says so — and one column
/// cannot carry both "no expiry" and "no ban" without the reading code guessing
/// which was meant. Year 9999 in Unix milliseconds.
pub const PERMANENT_BAN_MS: i64 = 253_402_300_799_000;

/// Smallest capacity a room may be created with.
///
/// Two, because the owner occupies one seat the moment the room exists and a room
/// nobody else can enter is a note to self. Rejected at creation rather than clamped,
/// so an operator who typed `1` finds out instead of wondering why the number moved.
pub const MIN_ROOM_CAPACITY: i32 = 2;

/// Members a room holds unless an operator says otherwise.
pub const DEFAULT_MAX_MEMBERS: i32 = 5_000;

/// The largest capacity this build will accept for a room.
///
/// Brief section 55 is about rooms with millions of members and it answers them with
/// shards, regional relays, and a fanout service — none of which exist yet. Until
/// they do, the honest ceiling is the one a single sequencer and a single roster query
/// can serve, and accepting `max_members = 5_000_000` today would be promising a
/// capacity the delivery path cannot reach.
pub const MAX_MEMBERS_CEILING: i32 = 100_000;

/// Who is asking.
///
/// No `BandwidthMode`, unlike `migo_presence::Caller`: nothing here is stored per
/// session or sized by the client's connection, so the mode would be a field this
/// crate carried and never read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection the request arrived on.
    ///
    /// Used for one thing: excluding the caller's own socket from a fanout it
    /// already knows the outcome of. Membership is per account, not per device.
    pub device_id: Id,
    /// Standing, for the rate limiter.
    pub tier: TrustTier,
    /// Whether this session proved a factor recently.
    ///
    /// Brief section 85 requires re-authentication before an ownership transfer, and
    /// this is that fact. It is a field on the caller rather than a lookup because
    /// only the gateway knows how long ago the factor was proved — the freshness
    /// window is a session property, and a service that guessed at it would either
    /// re-prompt constantly or accept a proof from an hour ago.
    pub reauthenticated: bool,
    /// Server time for this request.
    pub now: Timestamp,
    /// Correlation id, for joining a trace to a log line.
    pub request_id: Option<String>,
}

impl Caller {
    /// A caller who has not proved a second factor recently.
    ///
    /// The default, because that is the common case and because the failure of
    /// forgetting to set the flag should be a refused ownership transfer rather than
    /// an accepted one.
    #[must_use]
    pub fn new(account_id: Id, device_id: Id, tier: TrustTier, now: Timestamp) -> Self {
        Self {
            account_id,
            device_id,
            tier,
            reauthenticated: false,
            now,
            request_id: None,
        }
    }

    /// Marks the session as having proved a factor within the gateway's window.
    #[must_use]
    pub fn reauthenticated(mut self) -> Self {
        self.reauthenticated = true;
        self
    }

    /// Sets the correlation id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}

/// What the service needs that only deployment knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomsConfig {
    /// The region that will sequence rooms created here.
    ///
    /// Brief section 54: a room has exactly one sequencer, in its home region. The
    /// value is copied onto the room at creation and never derived again, because a
    /// room whose home region is recomputed from whichever node happens to answer
    /// would acquire a second sequencer during a partition — which section 54
    /// forbids outright, since two orders cannot be merged back into one.
    pub home_region: String,
    /// Capacity for a room whose creator did not name one.
    pub default_max_members: i32,
    /// Largest capacity this deployment will accept.
    pub max_members_ceiling: i32,
}

impl Default for RoomsConfig {
    fn default() -> Self {
        Self {
            home_region: "local".to_string(),
            default_max_members: DEFAULT_MAX_MEMBERS,
            max_members_ceiling: MAX_MEMBERS_CEILING,
        }
    }
}

impl RoomsConfig {
    /// Takes the home region from the node that will do the sequencing.
    ///
    /// Here rather than in `Config` so there is one source for the region: the node
    /// identity. A `[rooms] home_region` key would be a second one, and the first
    /// time the two disagreed a room would be created claiming a region that no
    /// process in it sequences.
    #[must_use]
    pub fn from_node(node: &NodeConfig) -> Self {
        Self {
            home_region: node.region.clone(),
            ..Self::default()
        }
    }
}

/// A room somebody wants created.
///
/// Ids are absent on purpose: the room id and its conversation id are minted by the
/// service, because they are what every link and every message row will point at
/// forever and a client-chosen id is a client-chosen collision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRoomRequest {
    /// URL-safe name, unique case-insensitively.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Opening topic.
    pub topic: Option<String>,
    /// Public or Managed. Brief section 21 is the difference.
    pub kind: RoomKind,
    /// Capacity, or the deployment default.
    pub max_members: Option<i32>,
}

/// A settings change, as a patch rather than a replacement.
///
/// `Option` for "leave it alone" on the scalars, and [`TopicChange`] for the topic
/// because a topic can be *removed*, and `Option<Option<String>>` at a public API
/// boundary is a field nobody reads correctly twice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// New display name.
    pub name: Option<String>,
    /// What to do with the topic.
    pub topic: TopicChange,
    /// New slow mode interval in seconds; zero turns it off.
    pub slow_mode_seconds: Option<i32>,
    /// New join policy. See `migo_store::model::join_policy`.
    pub join_policy: Option<i16>,
}

/// What a settings change does to the topic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TopicChange {
    /// Leave the current topic in place.
    #[default]
    Keep,
    /// Replace it.
    Set(String),
    /// Remove it.
    Clear,
}

/// A moderation action against one member.
///
/// One enum rather than four methods because the four share every check — the actor
/// must hold a permission, must outrank the target, and must not be acting on the
/// owner — and a shape that let one of them skip a check would eventually let one of
/// them skip a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sanction {
    /// Silence for a while. Bounded by [`MAX_MUTE_MS`].
    Mute {
        /// How long, in milliseconds.
        duration_ms: i64,
        /// Shown to the muted member.
        reason: Option<String>,
    },
    /// Lift a mute.
    Unmute,
    /// Remove, with the door left open.
    ///
    /// A kick is not a sanction row at all — the membership is simply marked as
    /// having left — and it is in this enum because it is the *action* a moderator
    /// takes and it shares every one of the checks above.
    ///
    /// No reason field, unlike the other two. `room_member.reason` is one column
    /// shared by mute and ban, and a kick drops the membership row's live state
    /// rather than writing a sanction, so there is nowhere for a kick reason to be
    /// stored. Accepting one and discarding it would be an API that documents a
    /// feature the database cannot hold.
    Kick,
    /// Remove and keep out.
    Ban {
        /// How long, in milliseconds; `None` is permanent.
        duration_ms: Option<i64>,
        /// Shown to the banned member.
        reason: Option<String>,
    },
    /// Lift a ban. Does not put the member back in the room.
    Unban,
}

impl Sanction {
    /// The permission this action requires.
    #[must_use]
    pub const fn permission(&self) -> u64 {
        match self {
            Self::Mute { .. } | Self::Unmute => crate::permission::USER_MUTE,
            Self::Kick => crate::permission::USER_KICK,
            Self::Ban { .. } | Self::Unban => crate::permission::USER_BAN,
        }
    }

    /// Whether the action removes the member from the room.
    #[must_use]
    pub const fn removes_member(&self) -> bool {
        matches!(self, Self::Kick | Self::Ban { .. })
    }
}

/// The answer to "may this account do this here", for another domain to act on.
///
/// Returned by [`crate::traits::Roomkeeper::authorize`] and deliberately more than a
/// boolean. A caller that has been told "yes" almost always needs the conversation
/// id next — that is where the message goes — and a second round trip to fetch it
/// would be a second read of a row this one already had in hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authorized {
    /// The room.
    pub room_id: Id,
    /// The conversation carrying its messages.
    pub conversation_id: Id,
    /// The room's kind, which decides whether the server can read the messages.
    pub kind: RoomKind,
    /// The caller's role.
    pub role: RoomRole,
    /// Effective permissions, role default plus grant minus deny.
    pub permissions: u64,
    /// Slow mode interval in seconds, zero when off.
    ///
    /// Carried because the crate that enforces it is the one that owns the last
    /// message time per author, which is messaging and not this crate. Enforcing it
    /// here would mean reading a conversation's tail on every permission check.
    pub slow_mode_seconds: i32,
}

/// Whether a slug is a name and not an injection.
///
/// Lowercase ASCII letters, digits, and single interior hyphens. Restrictive on
/// purpose: a slug appears in `migo://room/<slug>` and in an HTTPS fallback URL
/// (brief section 82), so anything that could need escaping in either place is
/// refused at the door rather than escaped correctly in nine renderers.
///
/// Case is rejected rather than folded. `Migo` and `migo` resolving to the same room
/// while only one of them is what the owner typed is the kind of near-miss that gets
/// used for impersonation, and the store folds case for uniqueness anyway.
///
/// # Why a slug that reads as a room id is refused
///
/// `Id::parse` accepts twenty-six lowercase Crockford characters, and a slug of that
/// length made of the same alphabet would parse as an id. A deep link carries one
/// string for both — brief section 82 — so one of the two forms has to win, and
/// whichever loses becomes a hijack: either a link written against a slug quietly
/// opens some other room, or a slug registered to match another room's id text
/// steals every link to it.
///
/// Refusing the overlap at creation removes the choice. The resolver can try the id
/// form first, because no slug can ever shadow one.
#[must_use]
pub fn slug_is_valid(slug: &str) -> bool {
    if slug.len() < MIN_SLUG_LEN || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    if Id::parse(slug).is_ok() {
        return false;
    }
    slug.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
