//! Turning stored rows into the summary the wire carries.
//!
//! One place, because a room appears in five responses — the join, the listing, the
//! "my rooms" screen, a single fetch, and a deep link — and five projections of the
//! same row would disagree about something eventually. The one that matters is
//! `my_role`: a summary built on the browse path must not carry the *caller's* role
//! from a row that was read for somebody else.

use migo_core::{Id, PublicId};
use migo_protocol::{EncryptionMode, RoomKind, RoomRole, RoomStateEvent, RoomSummary};
use migo_store::model::Room;

/// What `online_count` is set to by this crate.
///
/// Zero, and not because nobody is online. The real count is how many of a room's
/// members hold a live session right now, which means intersecting the roster with
/// presence on every listing — the query brief section 14 exists to prevent, and the
/// one `migo_presence` refuses for the same reason. So this crate does not compute it
/// on the read path at all.
///
/// The dispatcher overwrites it on the way out, from a tally it keeps in memory as
/// sessions come and go rather than by querying anything per request. A named constant
/// rather than a bare `0` so that a reader who finds a zero in a response knows which of
/// the two it is.
pub const ONLINE_COUNT_UNSET: u32 = 0;

/// A room as one caller sees it.
///
/// `my_role` is a parameter rather than something this function looks up, so the
/// caller has to have read the membership to claim one. A signature that took the
/// account id and fetched the row itself would turn a listing of fifty rooms into
/// fifty-one queries.
#[must_use]
pub fn summary(room: &Room, my_role: Option<RoomRole>) -> RoomSummary {
    RoomSummary {
        room_id: room.room_id,
        // Brief section 81: the shareable, immutable alias. Derived from the id
        // rather than stored, so it cannot drift from the room it names.
        public_id: room.room_id.public_id(PublicId::Room),
        kind: room.kind,
        name: room.name.clone(),
        member_count: count(room.member_count),
        online_count: ONLINE_COUNT_UNSET,
        topic: room.topic.clone(),
        // The four fields below have no column in `rooms`, so `None` here is the
        // honest answer rather than a placeholder. Returning an empty string instead
        // would make "this room has no description" and "this build has no
        // descriptions" the same value on the client, and the client would render a
        // blank box for both. `docs/04-data-model.md` is where they arrive.
        description: None,
        avatar_url: None,
        category: None,
        language: None,
        country: None,
        // Brief section 84 gives a room a verification badge. `None` rather than
        // `Some(false)`: an unverified room and a build that does not know how to
        // verify are different claims, and only one of them should draw a UI element
        // that says "not verified".
        verified: None,
        my_role,
        slow_mode_ms: slow_mode_ms(room),
        // The ceiling the join path refuses at, so a client can draw "2/33" instead of
        // learning the room is full only by being refused.
        max_members: Some(count(room.max_members)),
    }
}

/// Slow mode as the wire wants it, absent when off.
///
/// The store keeps seconds because that is what an operator types; the wire carries
/// milliseconds because every other interval in the protocol does. Zero becomes
/// `None` rather than `Some(0)` so a client cannot render "slow mode: 0s".
#[must_use]
pub fn slow_mode_ms(room: &Room) -> Option<u32> {
    match room.slow_mode_seconds {
        seconds if seconds > 0 => Some((seconds as u32).saturating_mul(1000)),
        _ => None,
    }
}

/// A member count as the wire wants it.
///
/// Saturating rather than `as`: the column is signed and a recount bug that produced
/// a negative would otherwise arrive on the client as four billion members.
#[must_use]
pub fn count(members: i32) -> u32 {
    members.max(0) as u32
}

/// A state event carrying only the fields that moved.
///
/// Brief section 156 asks for deltas, and the `RoomStateEvent` doc line says so too:
/// "Deltas only". Every field is `None` until something sets it.
///
/// # The one thing this shape cannot say
///
/// A cleared topic. `topic: None` already means "unchanged", so removing a topic is
/// published as `Some("")` — an empty string, which every client renders as no topic
/// — rather than as an absence that would be indistinguishable from silence. The
/// alternative was a second field saying whether the first one means anything, and a
/// nullable-nullable on the wire is a shape that gets decoded wrongly once per
/// client.
#[must_use]
pub fn delta(room_id: Id) -> RoomStateEvent {
    RoomStateEvent {
        room_id,
        online_count: None,
        member_count: None,
        topic: None,
        slow_mode_ms: None,
        max_members: None,
    }
}

/// Whether a state event would tell a subscriber anything.
///
/// Called before a fanout is built rather than after, so that a settings request
/// which changed nothing produces no frame at all instead of an empty one.
#[must_use]
pub fn is_empty(event: &RoomStateEvent) -> bool {
    event.online_count.is_none()
        && event.member_count.is_none()
        && event.topic.is_none()
        && event.slow_mode_ms.is_none()
        && event.max_members.is_none()
}

/// The encryption a room of this kind runs under.
///
/// Public and Managed rooms are `Transport`: the server can read them, and it has to
/// be able to, because brief section 49's moderation cannot act on what it cannot see
/// and section 59 requires the client to show which of the two a conversation is.
/// Deriving it from the kind in one function is what keeps a room from being created
/// as `EndToEnd` and then moderated anyway.
///
/// An unrecognised kind gets `Unknown` rather than a guess. The service refuses to
/// create such a room, so the only way to reach this arm is a row written by a newer
/// build, and answering `Transport` for it would be this build claiming it may read
/// something it knows nothing about.
#[must_use]
pub const fn encryption_for(kind: RoomKind) -> EncryptionMode {
    match kind {
        RoomKind::Public | RoomKind::Managed => EncryptionMode::Transport,
        RoomKind::Unknown => EncryptionMode::Unknown,
    }
}
