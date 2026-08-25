//! Turning a set of device states into the one state the world is told.
//!
//! An account is online on devices, but presence is rendered per person: a contact
//! row shows one dot. Something has to reduce "phone says Busy, laptop says Away,
//! tablet's entry expired ten seconds ago" to a single answer, and doing it here —
//! in pure functions over a slice, with no clock of their own and no I/O — is what
//! makes the rule testable instead of emergent.
//!
//! # The two rules
//!
//! **Invisible is projected, not hidden.** Brief section 14 puts invisibility on
//! the server and says a client must not be trusted to hide its own presence. The
//! literal reading — "send nothing about this user" — is a trap: a user who is seen
//! as Online and then goes Invisible would stay Online on every watching screen
//! until their entry expired, which is the opposite of what they asked for. So
//! Invisible projects to Offline, and the frame that says so is indistinguishable
//! from the frame a genuinely offline user produces. The server sends nothing
//! *about their being online*, which is the part that matters.
//!
//! **A stronger state wins.** With several devices live, the account takes the
//! highest-ranked state among them. The order is Busy, Online, Away, Offline, and
//! Busy above Online is the one that needs defending: Busy is only ever set on
//! purpose, so letting another device's automatic Online override it would let an
//! idle laptop cancel a user's do-not-disturb.

use migo_cache::PresenceEntry;
use migo_core::{Id, Timestamp};
use migo_protocol::PresenceState;

/// What the world is allowed to see of one declared state.
///
/// `Unknown` collapses to Offline as well. It should never be stored — `set`
/// refuses it — but a value that arrived from a newer peer's enum decodes to
/// `Unknown`, and the safe reading of a state this version does not understand is
/// the one that discloses least.
#[must_use]
pub const fn public(state: PresenceState) -> PresenceState {
    match state {
        PresenceState::Invisible | PresenceState::Unknown | PresenceState::Offline => {
            PresenceState::Offline
        }
        PresenceState::Online => PresenceState::Online,
        PresenceState::Away => PresenceState::Away,
        PresenceState::Busy => PresenceState::Busy,
    }
}

/// Precedence of a state when several devices disagree. Higher wins.
///
/// Invisible sits above Offline even though [`public`] collapses the two, because
/// this ordering is also used to answer a viewer about their own devices, and a
/// user with one hidden device and one that has gone Offline is hidden, not
/// offline. It makes no difference to [`visible_state`], which ranks only states
/// that have already been through `public`.
const fn rank(state: PresenceState) -> u8 {
    match state {
        PresenceState::Busy => 5,
        PresenceState::Online => 4,
        PresenceState::Away => 3,
        PresenceState::Invisible => 2,
        PresenceState::Offline => 1,
        PresenceState::Unknown => 0,
    }
}

/// The single state to publish for an account.
///
/// Expired entries are skipped rather than trusted. The cache filters them too, so
/// this is redundant on the happy path — and it is kept because the alternative
/// failure is a user shown online by a backend whose expiry ran late, and one
/// comparison is a cheap price for not depending on somebody else's punctuality.
///
/// An account with no live entries is Offline. That is the same answer an account
/// that has never connected gets, which is deliberate: presence is not a record of
/// having existed.
#[must_use]
pub fn visible_state(entries: &[PresenceEntry], now: Timestamp) -> PresenceState {
    entries
        .iter()
        .filter(|entry| !entry.is_expired(now))
        .map(|entry| public(entry.state))
        .max_by_key(|state| rank(*state))
        .unwrap_or(PresenceState::Offline)
}

/// The live entry belonging to one device, if it has one.
#[must_use]
pub fn entry_of(
    entries: &[PresenceEntry],
    device_id: Id,
    now: Timestamp,
) -> Option<&PresenceEntry> {
    entries
        .iter()
        .find(|entry| entry.device_id == device_id && !entry.is_expired(now))
}

/// Whether any *other* device of this account has declared itself Invisible.
///
/// Used when a device connects without saying what it wants to be. Invisibility is
/// account-level intent in practice — nobody sets it per device on purpose — and
/// a client that reconnects has no way to declare it before the socket is up, so a
/// user hiding on their phone would flash Online for one round trip every time the
/// network wobbled.
///
/// Only Invisible is inherited, and only because inheriting it can fail in one
/// direction: a user who wanted to be seen sends `PRESENCE_SET` and is seen.
/// Inheriting Busy or Away the same way would fail the other way, by telling
/// everyone something about a device that never said it.
#[must_use]
pub fn any_invisible(entries: &[PresenceEntry], except: Id, now: Timestamp) -> bool {
    entries.iter().any(|entry| {
        entry.device_id != except
            && entry.state == PresenceState::Invisible
            && !entry.is_expired(now)
    })
}

/// The state an account would show if `device_id` reported `state`.
///
/// The whole point of computing it locally instead of writing and reading back:
/// brief section 156 forbids a frame when nothing changed, and answering "did
/// anything change" needs the before and the after in the same breath. One cache
/// read plus arithmetic beats a read, a write, and a second read — and the second
/// read would be answering a question about a value this call already knows.
#[must_use]
pub fn state_with(
    entries: &[PresenceEntry],
    device_id: Id,
    state: PresenceState,
    now: Timestamp,
) -> PresenceState {
    let others = entries
        .iter()
        .filter(|entry| entry.device_id != device_id && !entry.is_expired(now))
        .map(|entry| public(entry.state));
    others
        .chain(std::iter::once(public(state)))
        .max_by_key(|state| rank(*state))
        .unwrap_or(PresenceState::Offline)
}

/// The state an account would show if `device_id` went away.
#[must_use]
pub fn state_without(entries: &[PresenceEntry], device_id: Id, now: Timestamp) -> PresenceState {
    entries
        .iter()
        .filter(|entry| entry.device_id != device_id && !entry.is_expired(now))
        .map(|entry| public(entry.state))
        .max_by_key(|state| rank(*state))
        .unwrap_or(PresenceState::Offline)
}

/// The strongest state an account has actually declared, Invisible included.
///
/// Only ever used to answer a viewer about themselves. Everybody else gets
/// [`visible_state`], and the difference between the two functions is the whole of
/// invisibility: a user can see that they are hidden, and nobody else can.
#[must_use]
pub fn declared_state(entries: &[PresenceEntry], now: Timestamp) -> PresenceState {
    entries
        .iter()
        .filter(|entry| !entry.is_expired(now))
        .map(|entry| entry.state)
        .max_by_key(|state| rank(*state))
        .unwrap_or(PresenceState::Offline)
}
