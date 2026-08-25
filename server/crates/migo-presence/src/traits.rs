//! The presence contract.
//!
//! One trait, because the operations share one invariant. Every one of them has to
//! answer the same question — "does the account's *visible* state differ now from
//! what it was a moment ago" — and that question can only be answered by something
//! that owns both the projection and the write. Handing out a narrow `Reporter`
//! that could set a device's state but not decide whether anyone should hear about
//! it would be handing out the half of the operation without the rule.
//!
//! # Why every method returns a plan instead of performing one
//!
//! The four mutating operations return a [`Fanout`] rather than delivering it. See
//! the [`fanout`](crate::fanout) module: the short version is that the gateway owns
//! connections, encodes once, and sends N times, and a domain crate that delivered
//! its own frames would be that gateway with a cache in the middle.
//!
//! An `Option<Fanout>` is not an accident of style. Brief section 156 forbids
//! sending a frame when nothing changed, and the `Option` is that rule made visible
//! in the type: a heartbeat from a device that is still Online, a second
//! `PRESENCE_SET` for the state already held, and the disconnect of one of three
//! live devices all produce `None`, and a caller that has nothing to send cannot
//! forget to check.
//!
//! # What is deliberately not here
//!
//! No method returns a room's online count. A room shows `online_count` in its
//! summary, and the cheap way to know it is the size of the subscriber set on that
//! room's topic — which the gateway already holds. Computing it here would mean
//! reading an eight-hundred-row roster and intersecting it with the presence cache
//! on every subscribe, which is the exact query brief section 14 exists to prevent.
//!
//! No method sends a presence *digest* to another region either. Brief section 169
//! carries that on `FED_PRESENCE_DIGEST` as a periodic aggregate, and aggregating
//! across regions belongs to the crate that owns the mesh.

use async_trait::async_trait;
use migo_cache::PresenceEntry;
use migo_core::{Id, Result};
use migo_protocol::{BandwidthMode, PresenceEvent, PresenceUpdate};

use crate::fanout::Fanout;
use crate::model::{Cadence, Caller, Detail};

/// Everything presence does.
#[async_trait]
pub trait Presence: Send + Sync {
    /// Registers a device that has just connected.
    ///
    /// A connecting device is Online by definition — it is holding a socket — with
    /// one exception: if another of the account's devices is currently Invisible,
    /// this one inherits Invisible. See `crate::state::any_invisible` for why that
    /// inheritance is safe in exactly one direction.
    ///
    /// Not rate limited. The connection that carried it was already charged for at
    /// the handshake, and refusing to record a presence entry for a socket that is
    /// nevertheless connected would leave the two disagreeing for a whole
    /// heartbeat.
    async fn connected(&self, caller: &Caller) -> Result<Option<Fanout>>;

    /// Refreshes a device's entry, keeping whatever state it already declared.
    ///
    /// The deadline moves; `since` does not, so "online since" survives a refresh
    /// instead of resetting every heartbeat. Returns `Some` only when the entry had
    /// expired and its recreation changed what the account looks like — the normal
    /// case is a `None` that costs one read and one write.
    async fn heartbeat(&self, caller: &Caller) -> Result<Option<Fanout>>;

    /// Applies a state a client asked for.
    ///
    /// The one operation here that a client can call directly, and so the one that
    /// is rate limited and validated. `PresenceState::Unknown` is refused rather
    /// than coerced: a client that did not name a state has a bug, and silently
    /// choosing one for it hides the bug behind plausible behaviour.
    async fn set(&self, caller: &Caller, request: PresenceUpdate) -> Result<Option<Fanout>>;

    /// Forgets a device's entry, for a socket that closed cleanly.
    ///
    /// Immediately, rather than letting the entry expire, because a clean close is
    /// information: the alternative is showing a user online for up to three
    /// heartbeats after they closed the app. Returns `Some` only when this was the
    /// device that was holding the account's visible state up.
    async fn disconnected(&self, caller: &Caller) -> Result<Option<Fanout>>;

    /// Presence for a set of accounts, as this viewer is allowed to see it.
    ///
    /// The read path. Called by the gateway when a client subscribes, with the
    /// member ids the subscription just authorised — which is where brief section
    /// 14's scope rule is enforced, because the gateway is what knows the
    /// subscription. What this method still enforces is the pair of things a
    /// subscription cannot: invisibility, and blocks.
    ///
    /// Subjects past [`MAX_SNAPSHOT_SUBJECTS`](crate::model::MAX_SNAPSHOT_SUBJECTS)
    /// are dropped rather than refused. A truncated contact list renders; a failed
    /// one does not.
    async fn snapshot(
        &self,
        caller: &Caller,
        of: &[Id],
        detail: Detail,
    ) -> Result<Vec<PresenceEvent>>;

    /// The caller's own devices, as stored.
    ///
    /// Unprojected on purpose: this is the only read where the viewer is the
    /// subject, so Invisible is returned as Invisible. A user has to be able to see
    /// that they are hidden, or the feature is indistinguishable from being broken.
    async fn devices(&self, caller: &Caller) -> Result<Vec<PresenceEntry>>;

    /// The intervals a session on this bandwidth mode runs at.
    ///
    /// Here rather than in a free function because the base heartbeat is
    /// configuration, and configuration lives in the service. The gateway asks this
    /// for the `heartbeat_ms` it puts in `Welcome`, and asks it again for the
    /// coalescing floor it applies to this session's queue.
    fn cadence(&self, mode: BandwidthMode) -> Cadence;
}
