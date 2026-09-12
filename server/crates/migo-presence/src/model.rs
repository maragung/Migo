//! Inputs, cadences, and the limits presence works from.
//!
//! Three conventions, all inherited from the rest of the server.
//!
//! Every operation takes a [`Caller`], and the caller carries `now`. Nothing in
//! this crate reads a clock, which is what makes a presence deadline testable
//! rather than hopeful (ADR-0009).
//!
//! Every limit is a named constant with the reasoning attached. A number nobody
//! can explain is a number nobody dares change.
//!
//! Every interval is *derived* from the one interval the client was actually told
//! about — the `heartbeat_ms` in `Limits`, sent in `Welcome`. A time-to-live that
//! is shorter than the heartbeat the server itself advertised does not express a
//! policy, it expresses a bug: the user blinks offline between two heartbeats that
//! both arrived exactly when they were asked for.

use migo_cache::Ttl;
use migo_core::config::GatewayConfig;
use migo_core::{Id, Timestamp};
use migo_protocol::BandwidthMode;
use migo_ratelimit::TrustTier;

// The cadence table moved to `migo-protocol` (see `migo_protocol::cadence`): the
// gateway's coalescing queue now enforces the presence interval section 159
// assigns to it, the gateway may not name this crate (section 177), and a table
// two crates must agree on can only live where both may read it. Re-exported
// here so every caller that learned these names from `migo_presence` keeps
// compiling against the same values rather than a copy.
pub use migo_protocol::{cadence_for, Cadence, PresenceScope, MAX_HEARTBEAT_MS, MIN_HEARTBEAT_MS};

/// Who is calling, reduced to what presence actually needs.
///
/// Deliberately **not** `migo_auth::RequestContext`, and deliberately not the same
/// `Caller` as `migo_messaging`: authentication, messaging, and presence are all
/// layer-3 domain crates, and two of those may not depend on each other (see
/// `docs/01-architecture.md`). The gateway holds all of them and translates at the
/// edge, which is one short function in the one place allowed to know about all
/// three.
///
/// The address is absent on purpose. Rate limiting here is per account and per
/// endpoint, because the caller is authenticated and an account is a stronger
/// subject than a network; and brief section 174 forbids a full address in a log
/// line, so a field that never arrives cannot be logged by accident.
#[derive(Clone, Debug)]
pub struct Caller {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection this request arrived on.
    ///
    /// Load-bearing rather than decorative. Presence is stored per device, so this
    /// says which row to write; and it is the device excluded from fanout, because
    /// the socket that reported a state already knows it.
    pub device_id: Id,
    /// Standing, for the rate limiter.
    pub tier: TrustTier,
    /// The bandwidth mode this session negotiated in `HELLO`.
    ///
    /// Present here and absent from `migo_messaging::Caller` because it changes
    /// what this crate *stores*, not merely what the gateway forwards: a session
    /// on a longer heartbeat needs a longer presence lifetime, or its own
    /// punctual heartbeats arrive after the entry they were meant to refresh has
    /// already expired. See [`cadence_for`].
    pub mode: BandwidthMode,
    /// Server time for this request.
    pub now: Timestamp,
    /// Correlation id, for joining a trace to a log line.
    pub request_id: Option<String>,
}

impl Caller {
    /// A caller with the five facts every operation needs.
    #[must_use]
    pub fn new(
        account_id: Id,
        device_id: Id,
        tier: TrustTier,
        mode: BandwidthMode,
        now: Timestamp,
    ) -> Self {
        Self {
            account_id,
            device_id,
            tier,
            mode,
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

/// How many heartbeats a device may miss before it is presumed gone.
///
/// Three, so that two consecutive losses do not evict a device that is still
/// there. One is not enough — a mobile radio waking up, a proxy holding a frame,
/// or a garbage-collection pause routinely eats a single interval, and evicting on
/// the first miss makes presence flicker for every user on a train.
///
/// The cost of the choice is stated plainly: after an *unclean* disconnect a user
/// keeps showing as online for up to three heartbeats. A clean disconnect does not
/// pay it, because the gateway calls `disconnected` and the entry goes immediately.
pub const MISSED_HEARTBEATS: u32 = 3;

/// Accounts one snapshot will answer for.
///
/// Matches [`migo_cache::traits::MAX_PRESENCE_FANOUT`], because that is where the
/// truncation actually happens: asking the cache for more than it will look up
/// only produces a longer request, not a longer answer. Naming the same number
/// here keeps the clamp visible at the layer that chose to clamp.
pub const MAX_SNAPSHOT_SUBJECTS: usize = migo_cache::traits::MAX_PRESENCE_FANOUT;

/// Accounts one snapshot will do last-seen work for.
///
/// Last seen costs a profile read, sometimes a friendship read, and a device read
/// *per subject*, so it is bounded far below [`MAX_SNAPSHOT_SUBJECTS`]. Sixty-four
/// covers the case the field exists for — opening a conversation, where the
/// subject is one person — with room to spare for a small group, and refuses to
/// turn a room subscribe into two hundred round trips.
///
/// Subjects past the bound come back with `last_seen: None`, which is the same
/// answer they would get if the user had hidden it. A client renders both
/// identically, so the degradation is invisible rather than wrong.
pub const MAX_LAST_SEEN_LOOKUPS: usize = 64;

/// How long a presence entry from one cadence should live.
///
/// An extension trait rather than an inherent method only because [`Cadence`]
/// now lives in `migo-protocol` and a `Ttl` is a cache type this crate owns; the
/// method keeps its name and its call sites.
pub trait CadenceTtl {
    /// [`MISSED_HEARTBEATS`] times the heartbeat, saturating. The saturation is
    /// not decoration: `heartbeat_ms` is bounded by [`MAX_HEARTBEAT_MS`] and the
    /// multiplier by three, so the product cannot overflow a `u32` today — and a
    /// future mode with a longer multiplier would silently wrap instead of
    /// clamping if this said `*`.
    fn presence_ttl(&self) -> Ttl;
}

impl CadenceTtl for Cadence {
    fn presence_ttl(&self) -> Ttl {
        Ttl::from_millis(self.heartbeat_ms.saturating_mul(MISSED_HEARTBEATS))
    }
}

/// How much of a presence answer the caller is willing to pay for.
///
/// An explicit parameter rather than a guess, because the two callers want
/// genuinely different things and only they know which. Opening a direct
/// conversation asks about one person and wants their last-seen line; subscribing
/// to a room asks about hundreds and wants a dot each. Deciding here would either
/// make the room case expensive or make the conversation case incomplete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    /// State only. One cache read for any number of subjects.
    StateOnly,
    /// State, plus last seen where the subject's privacy settings allow it.
    WithLastSeen,
}

/// What presence needs to know about how this node is configured.
///
/// One number. It is a struct anyway so that adding a second one later is not a
/// signature change in every caller, and because `PresenceConfig::default()` at a
/// call site says what it is while `30_000` does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceConfig {
    /// The heartbeat advertised to a `Normal` session.
    pub heartbeat_ms: u32,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            heartbeat_ms: 30_000,
        }
    }
}

impl PresenceConfig {
    /// Takes the heartbeat from the gateway that will advertise it.
    ///
    /// Derived rather than configured separately, because these two numbers being
    /// equal is the whole invariant: the gateway tells the client how often to
    /// heartbeat, and this crate decides how long to believe it. Two independent
    /// settings would let an operator make presence flicker by editing one of
    /// them.
    ///
    /// The value is clamped, not validated. `GatewayConfig` already refuses a
    /// heartbeat below a second, so the clamp is a belt for the paths that build
    /// a config in code — tests, and a future admin surface — and the failure it
    /// prevents is presence that expires faster than it can be refreshed.
    #[must_use]
    pub fn from_gateway(gateway: &GatewayConfig) -> Self {
        let millis = u32::try_from(gateway.heartbeat_ms).unwrap_or(MAX_HEARTBEAT_MS);
        Self {
            heartbeat_ms: millis.clamp(MIN_HEARTBEAT_MS, MAX_HEARTBEAT_MS),
        }
    }
}
