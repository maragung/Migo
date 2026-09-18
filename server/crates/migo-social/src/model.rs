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

/// Accounts one account may mute.
///
/// The same number as blocks for the same reason: a mute list is people somebody
/// has actually encountered, and a list far above this is a script. Mutes are
/// cheaper to hold than blocks — they are one row and no cascades — so the bound
/// exists for the table's sake, not the caller's.
pub const MAX_MUTES: usize = 1_000;

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
    /// Accounts one account may mute.
    pub max_mutes: usize,
}

impl Default for SocialConfig {
    fn default() -> Self {
        Self {
            max_friends: MAX_FRIENDS,
            max_following: MAX_FOLLOWING,
            max_blocks: MAX_BLOCKS,
            max_mutes: MAX_MUTES,
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
    /// Whether this edge is settled, meaning both ends agreed to it.
    ///
    /// False for a friendship with no acceptance date and false for a request, which is
    /// the same statement twice: those are the two rows that stand for a friendship
    /// nobody has agreed to yet, and neither may ever read as settled. A pending request
    /// that read as a friendship would have a client showing a stranger as a friend, and
    /// every gate in this crate rests on the same distinction.
    ///
    /// Always true for a follow, a block, and a favourite: those need no consent, so
    /// there is no pending state for them to be in.
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
                // A request is not settled, whichever end of it this row is. It reaches
                // this projection through `Graph::pending`, where the whole point is
                // that nobody has answered yet.
                RelationshipKind::PendingIncoming | RelationshipKind::PendingOutgoing => false,
                // No consent to record, so nothing to be pending on.
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

/// What a block changed that somebody's clients should hear about.
///
/// A block is the one social write that is also a teardown, so the answer it owes the
/// dispatcher is not "done" but "whose graph moved". The [`FRIEND_EVENT`](migo_protocol::Opcode::FriendEvent)
/// hint is how a client learns to re-read without a manual refresh, and the two flags
/// below are the service's honest account of who needs one.
///
/// Neither flag describes a notification. No bell rings and no inbox row is written for
/// a block: the [`Notice`](crate::notice::Notice) path is for requests and acceptances,
/// things worth waking somebody for. A block that removed a friendship tells the blocked
/// account only that the graph moved, with the same `state` an un-friend would carry, so
/// the two stay indistinguishable from each other — which is the arrangement section
/// 180 demands for every other way an account disappears from view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockOutcome {
    /// An edge the blocked account could observe was removed — a friendship, a pending
    /// request in either direction, or their follow of the blocker — so their clients
    /// should hear that the graph moved.
    ///
    /// False when nothing they could see changed, which is the common case: most blocks
    /// are of strangers. Publishing a "the graph moved" hint for a block that removed
    /// nothing would tell a stranger exactly who blocked them, which is a fact the wire
    /// is otherwise careful never to hand over.
    pub severed: bool,
    /// The blocker's own graph changed — the block or its carried mute is new, or an
    /// edge of theirs was removed — so the blocker's *other* devices should re-read.
    ///
    /// The device that asked already knows; section 156 excludes it from the fan-out.
    /// False only for the pure no-op: blocking an account that was already blocked and
    /// already muted, with no other edge between the two, changes nothing, and state
    /// that did not change produces no frame.
    pub moved: bool,
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
    /// Ring somebody. The kind decides which column answers: see [`CallKind`].
    Call(CallKind),
    /// Send a friend request. Reads `who_can_add`.
    FriendRequest,
    /// Read the subject's last-seen time. Reads `show_last_seen`.
    LastSeen,
}

/// Which kind of call a policy question is about.
///
/// Brief section 180 asks for the two to be decided separately, and the profile carries
/// a column for each, so the gate is asked with the kind rather than looking one up from
/// the other: refusing to be seen is not refusing to be spoken to, and the account that
/// wants video calls off while its voice line stays open is the account this split
/// exists for.
///
/// There is no `Group` variant. A group call is joined rather than rung —
/// `migo-calls` seats a participant through conversation membership alone — so a
/// group-call policy would be a column nothing reads. See `docs` and section 180's
/// status in migo.md: the missing variant is the honest record of a gate that has no
/// ring to refuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    /// An audio call. Reads `who_can_call_voice`.
    Voice,
    /// A video call. Reads `who_can_call_video`.
    Video,
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
    /// The bot this account speaks as, when it is one.
    ///
    /// A search is the one listing a stranger reaches a bot through, so it is the one
    /// listing where naming the bot matters most: a bot found by name and taken for a
    /// person is a report filed under the wrong reason, if it is filed at all, and a
    /// client that could see the bot but not name it still could not file the one report
    /// this field exists for.
    pub bot_id: Option<Id>,
}

/// One account's public face.
///
/// # Why this is not `migo_protocol::UserProfile`
///
/// The wire struct has sixteen fields and this crate can honestly fill eleven of them.
/// `level` belongs to progression, `presence` to presence, `badges` and `verified` to
/// moderation, and `avatar_url` to the media service that mints the signed link.
/// Returning the wire struct from here would mean returning it with those
/// fields defaulted, and a defaulted `verified: false` on a verified account is not a
/// missing field, it is a wrong answer that looks like an answer. The composition root
/// joins the other domains in and leaves absent what is absent.
///
/// `bot_id` is the one field this crate fills that belongs to another domain, and it is
/// filled on purpose. Whether an account is a bot is a fact about the account, one the
/// store already answers by account id, and the alternative — leaving the composition
/// root to look it up per profile — would put a second answer in the layer that has no
/// business holding one; a profile card that says nothing about it, meanwhile, is a card
/// that draws a bot as a person, which is the state section 49 opened in.
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
    /// The custom status the owner set, if any — the RICH_PRESENCE bit's own field.
    ///
    /// Read here rather than from presence: a status somebody typed is a durable fact
    /// about their profile, and a presence entry evaporates with the connection cache.
    /// Setting it is gated on the negotiated bit at the dispatcher, not here — the graph
    /// serves whatever the profile row holds.
    pub custom_status: Option<String>,
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
    /// Year of birth, if its owner disclosed it. Year only — a full birth date is
    /// more personal data than a chat profile needs, and the wire's optional field
    /// keeps "withheld" a distinct statement from any year.
    pub birth_year: Option<i16>,
    /// The bot this account speaks as, when it is one.
    ///
    /// An `Option` and not a flag, here as on the wire, for the reason section 49 gives:
    /// a client has to be able to *name* the bot it is looking at, because a report about
    /// a bot carries `bot.bot_id` and not the account the bot signs in as. A flag would
    /// have shown a client a bot it could not report, which is the complaint the section
    /// opens with.
    pub bot_id: Option<Id>,
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
