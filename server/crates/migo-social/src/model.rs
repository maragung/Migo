//! Types the social service takes and returns.
//!
//! # Why so many of these are not protocol structs
//!
//! Brief section 145 reserves opcodes 113 to 117 for the social frames —
//! `FRIEND_REQUEST`, `FRIEND_RESPOND`, `FRIEND_EVENT`, `BLOCK_SET`,
//! `RELATIONSHIP_LIST` — and marks the block `STATUS: SPEC`. None of them is in the
//! generated packet registry, so there is no `FriendRequest` wire struct to accept
//! and no `FriendEvent` to publish.
//!
//! These types exist instead, and the API layer maps them. That is deliberately not
//! a workaround: adding five frames to the IDL from a domain crate would change the
//! protocol's golden vectors, and a wire format is not something one feature's author
//! gets to extend on the way past. When the frames land, these structs are what they
//! will be generated to match.

use migo_core::{Id, Timestamp};
use migo_protocol::RelationshipKind;
use migo_ratelimit::TrustTier;
use migo_store::model::{Relationship, Visibility};

/// Largest page any listing here will return.
///
/// The store's own ceiling, restated so a caller can size a buffer without importing
/// the storage layer.
pub const MAX_PAGE: u16 = 200;

/// Page size for a caller that named none.
pub const DEFAULT_PAGE: u16 = 50;

/// Longest search term accepted.
///
/// Forty-eight characters, matching the room search bound. A username is shorter than
/// this and a display name that needs more than this is not being searched for, it is
/// being pasted.
pub const MAX_QUERY_LEN: usize = 48;

/// Accepted friendships one account may hold.
///
/// Five thousand. Large enough that no real person meets it, small enough that the
/// friend list of a compromised account is a bounded object: every gate in this crate
/// reads that list, and an unbounded one turns a privacy check into a table scan.
pub const MAX_FRIENDS: usize = 5_000;

/// Accounts one account may follow.
///
/// Ten thousand — higher than the friend ceiling because following needs no consent
/// from the other side, so it is the cheaper edge to create and the one that wants a
/// limit more.
pub const MAX_FOLLOWING: usize = 10_000;

/// Accounts one account may block.
///
/// A thousand. A blocklist is a list of people somebody met and did not want to meet
/// again; a number far above this is a script, and a script filling a blocklist is
/// filling a table on the server's disk.
pub const MAX_BLOCKS: usize = 1_000;

/// Accounts one account may mark as a favourite.
pub const MAX_FAVORITES: usize = 200;

/// Profiles one `PROFILE_FETCH` may ask for.
///
/// Sixty-four. The batch exists so that a member list or a conversation header renders
/// in one round trip instead of one request per face, and sixty-four is more faces than
/// any screen shows at once. It needs a hard ceiling because the price is flat: brief
/// section 145 charges `PROFILE_FETCH` 3 whether it carries one id or a thousand, so the
/// ceiling is the only thing standing between that price and an unbounded read. Each id
/// costs three keyed reads — the symmetric block check, the profile, the account — so a
/// full batch is a hundred and ninety-two, the same order as one listing at
/// [`MAX_PAGE`].
pub const MAX_PROFILE_BATCH: usize = 64;

/// How far a mutual-friend answer will look.
///
/// Two hundred each side, so a mutual check costs two bounded reads rather than one
/// per friend. Past that the answer is *incomplete*, and this crate treats an
/// incomplete answer as "no mutual friend found" — a refusal — rather than as a
/// permission. A privacy gate that fails open when the data gets large is a privacy
/// gate that stops working exactly for the accounts that have the most to lose.
pub const MAX_MUTUAL_SCAN: u16 = 200;

/// Who is asking.
///
/// No `reauthenticated` flag, unlike `migo_rooms::Caller`: nothing here is a
/// step-up-protected action. Blocking somebody is reversible, and unblocking somebody
/// is not a privilege escalation — it restores the state that existed before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection the request arrived on.
    pub device_id: Id,
    /// Standing, for the rate limiter.
    pub tier: TrustTier,
    /// Server time for this request.
    pub now: Timestamp,
    /// Correlation id, for joining a trace to a log line.
    pub request_id: Option<String>,
}

impl Caller {
    /// A caller at `now`.
    #[must_use]
    pub fn new(account_id: Id, device_id: Id, tier: TrustTier, now: Timestamp) -> Self {
        Self {
            account_id,
            device_id,
            tier,
            now,
            request_id: None,
        }
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
pub struct SocialConfig {
    /// Accepted friendships one account may hold.
    pub max_friends: usize,
    /// Accounts one account may follow.
    pub max_following: usize,
    /// Accounts one account may block.
    pub max_blocks: usize,
    /// The strictest default this deployment applies to calls.
    ///
    /// Brief section 180 says a call defaults to `Friends`, and there is no
    /// `who_can_call` column for a user to widen it with — `docs/04-data-model.md`
    /// gives a profile three visibility columns and this is not one of them. So the
    /// call gate takes the stricter of this value and the account's message policy,
    /// which honours the default without inventing a column and lets somebody who set
    /// messages to `Nobody` refuse calls too.
    pub call_default: Visibility,
}

impl Default for SocialConfig {
    fn default() -> Self {
        Self {
            max_friends: MAX_FRIENDS,
            max_following: MAX_FOLLOWING,
            max_blocks: MAX_BLOCKS,
            call_default: Visibility::Friends,
        }
    }
}

/// One edge, as a caller sees it.
///
/// A projection of `migo_store::model::Relationship` rather than the row itself, so
/// that `accepted` is a boolean a client can render instead of an `Option<Timestamp>`
/// whose `None` means two different things depending on the kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    /// The other end of the edge.
    pub other_id: Id,
    /// What kind of edge it is.
    pub kind: RelationshipKind,
    /// When it was created.
    pub since: Timestamp,
    /// Whether a friendship is mutual and settled.
    ///
    /// Always true for a follow, a block, and a favourite: those need no consent, so
    /// there is no pending state for them to be in. For a friendship it is the
    /// difference between a relationship and a request, and the two must never be
    /// conflated — a pending request that read as a friendship would let anybody see a
    /// `Friends`-only field by asking to be a friend.
    pub accepted: bool,
}

impl Edge {
    /// Projects a stored row.
    #[must_use]
    pub fn of(row: &Relationship) -> Self {
        Self {
            other_id: row.other_id,
            kind: row.kind,
            since: row.created_at,
            accepted: match row.kind {
                RelationshipKind::Friend => row.accepted_at.is_some(),
                _ => true,
            },
        }
    }
}

/// Friend requests waiting on somebody.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// Requests this account received and has not answered.
    pub incoming: Vec<Edge>,
    /// Requests this account sent and nobody has answered.
    pub outgoing: Vec<Edge>,
}

/// What one account is to another, from the asking account's side.
///
/// # What is deliberately missing
///
/// There is no `blocked_by` field. Brief section 180 requires that a caller cannot
/// tell "this person blocked me" from "this person's privacy settings exclude you",
/// and a boolean on a profile response would answer the question that the error codes
/// were carefully arranged not to answer. The caller's *own* block is reported,
/// because telling somebody what they themselves did leaks nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Standing {
    /// A settled, mutual friendship.
    pub friends: bool,
    /// This account asked and is waiting.
    pub requested: bool,
    /// The other account asked and is waiting for an answer.
    pub awaiting_response: bool,
    /// This account follows the other.
    pub following: bool,
    /// The other account follows this one.
    pub followed_by: bool,
    /// This account marked the other as a favourite.
    pub favorite: bool,
    /// This account blocked the other.
    pub blocked: bool,
}

/// What a friend request did.
///
/// Brief section 153 keys friend-request idempotency on the pair of accounts, so a
/// repeat is an outcome and not an error: the client that retried never saw the first
/// answer, and an error would make it report a failure for a request that was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FriendOutcome {
    /// A new request is now waiting.
    Requested,
    /// A request from this account was already waiting. Nothing was written.
    AlreadyRequested,
    /// The other account had already asked, so the two are now friends.
    ///
    /// The case that makes a friend request feel like it works. Two people who asked
    /// each other before either answered should not both be left staring at an
    /// unanswered request, so the second request accepts the first.
    Accepted,
    /// They were already friends. Nothing was written.
    AlreadyFriends,
}

/// What answering a friend request did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RespondOutcome {
    /// The request was accepted and both edges now exist.
    Accepted,
    /// The request was declined and the pending edges are gone.
    Declined,
}

/// A thing one account might try to do to another.
///
/// Four, and not the seven brief section 124 lists. `docs/04-data-model.md` gives a
/// profile three visibility columns — `who_can_message`, `who_can_add`,
/// `show_last_seen` — so gifts, room invitations, and profile visibility have nothing
/// to read. A fifth variant here would be a gate that always answered `Everyone`,
/// which is worse than no gate at all: it would look like a privacy control in the
/// API and behave like a no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interaction {
    /// Start or continue a conversation. Reads `who_can_message`.
    Message,
    /// Place a voice or video call. See [`SocialConfig::call_default`].
    Call,
    /// Send a friend request. Reads `who_can_add`.
    FriendRequest,
    /// Read the subject's last-seen time. Reads `show_last_seen`.
    LastSeen,
}

/// An account a listing suggests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// Who is being suggested.
    pub account_id: Id,
    /// How many of the caller's friends are also friends with them.
    ///
    /// The only reason offered, because it is the only one the schema can support: the
    /// other five discovery axes in brief section 24 — same interests, same country,
    /// same rooms, online now — need either a column that does not exist or a query
    /// this crate refuses to run on every profile view.
    pub mutual_friends: u32,
}

/// An account a search found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The account.
    pub account_id: Id,
    /// The username, as its owner typed it.
    pub username: String,
    /// The display name.
    pub display_name: String,
    /// The avatar, if there is one.
    pub avatar_media_id: Option<Id>,
}

/// One account's public face.
///
/// # Why this is not `migo_protocol::UserProfile`
///
/// The wire struct has thirteen fields and this crate can honestly fill seven of them.
/// `level` belongs to progression, `presence` to presence, `badges` and `verified` to
/// moderation, and `custom_status` to a column the data model does not have. Returning
/// the wire struct from here would mean returning it with six fields defaulted, and a
/// defaulted `verified: false` on a verified account is not a missing field, it is a
/// wrong answer that looks like an answer. The composition root joins the other domains
/// in and leaves absent what is absent.
///
/// # What is deliberately missing
///
/// No visibility settings, no relationship flags, no last-seen time. A profile card is
/// what a stranger may see; who may message this account is the account's own business,
/// what the caller is to them is [`Standing`], and whether the caller may see a
/// last-seen time is [`Interaction::LastSeen`]. Three separate answers, because they are
/// governed by three separate rules and a struct that carried all of them would be
/// filled by whichever caller happened to be convenient.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileCard {
    /// The account.
    pub account_id: Id,
    /// The username, as its owner typed it.
    pub username: String,
    /// The display name.
    pub display_name: String,
    /// Free text the owner wrote, if any.
    pub bio: Option<String>,
    /// The avatar object, if there is one.
    ///
    /// An id and not a URL. Brief section 168 forbids the server from proxying media
    /// bytes, so the URL is a signed one the media service mints on request, and minting
    /// it here would put an expiring credential in a response that a client may cache.
    pub avatar_media_id: Option<Id>,
    /// ISO-3166 alpha-2, if the account has one.
    pub country: Option<String>,
    /// BCP-47 language tag.
    pub locale: String,
}

/// The stricter of two visibility settings.
///
/// Used where a policy has a floor as well as a user preference. Strictness is the
/// numeric order of the enum — `Nobody` 0, `Friends` 1, `Everyone` 2 — so this is a
/// minimum, and it is written as one rather than as a match so a fourth visibility
/// added later cannot fall through a missing arm into `Everyone`.
#[must_use]
pub fn strictest(left: Visibility, right: Visibility) -> Visibility {
    Visibility::from_i16(left.to_i16().min(right.to_i16()))
}

/// Whether a term is worth sending to the store.
#[must_use]
pub fn query_is_usable(query: &str) -> bool {
    let trimmed = query.trim();
    !trimmed.is_empty() && trimmed.chars().count() <= MAX_QUERY_LEN
}
