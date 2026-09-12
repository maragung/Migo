//! The cadence table: what one session's bandwidth mode changes about the wire.
//!
//! The second hand-written module in this crate, after `fault`. The crate's
//! charter is shapes and constants, and this table is both of those wearing one
//! coat: it is a pure function from two numbers (the mode the client declared
//! in `HELLO`, the heartbeat the node was configured with) to the intervals the
//! *wire contract* is paced by — the `heartbeat_ms` a `WELCOME` advertises, the
//! shortest gap between two presence frames about the same user, whether typing
//! frames are delivered at all. No clock is read, no state is kept, nothing is
//! async. It lives here and not in `migo-presence`, where it was born, for one
//! structural reason: brief section 159 assigns the *enforcement* of the
//! presence interval to the gateway's coalescing queue, brief section 177
//! forbids the gateway from naming a single domain crate, and a table the two
//! of them must agree on can therefore only live in the one crate both may
//! depend on. `migo-presence` re-exports it, so its public face is unchanged.
//!
//! The heartbeat the table returns is the one that belongs in `Limits`: a
//! `LowData` session is told to beat half as often and an `UltraLowData` one a
//! quarter as often, and everything derived from the advertised number — the
//! presence TTL in `migo-presence`, the liveness deadline in the gateway —
//! reads it from here so the client is never told one interval and judged by
//! another.

use crate::BandwidthMode;

/// Shortest heartbeat the server will advertise, in milliseconds.
///
/// Mirrors `GatewayConfig` validation, which already refuses anything below this.
/// A clamp rather than a second rejection: a service that refuses to start
/// because somebody typed a small number has turned a configuration typo into
/// an outage.
pub const MIN_HEARTBEAT_MS: u32 = 1_000;

/// Longest heartbeat the server will advertise, in milliseconds.
///
/// Five minutes. Beyond this the `UltraLowData` multiplier would push a presence
/// lifetime past an hour, at which point "online" stops describing anything: the
/// entry outlives the session, the battery, and usually the train journey.
pub const MAX_HEARTBEAT_MS: u32 = 300_000;

/// Which presence a session wants delivered to it.
///
/// Brief section 159 asks the *server* to stop sending presence a low-bandwidth
/// client will not render, on the grounds that filtering at the client saves
/// rendering while filtering at the server saves bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceScope {
    /// Everything the session is subscribed to.
    Everything,
    /// Only conversations and rooms the user currently has open.
    ///
    /// Section 159 makes this a recommendation on `LowData` and a requirement on
    /// `UltraLowData`. Both are answered the same way, because a recommendation
    /// the server declines to follow costs a mobile user real bytes for a
    /// presence dot they cannot see, and the client re-reads presence when it
    /// opens a conversation anyway. The gate is applied where "open" is
    /// expressible — at `SUBSCRIBE` time, in the composition root, which knows
    /// both the mode and what a user topic is.
    OpenOnly,
}

/// The intervals one session runs at.
///
/// Not stored anywhere. Computed from the session's bandwidth mode every time it
/// is needed, because it is a pure function of two numbers and a cached copy is
/// one more thing that can disagree with the `WELCOME` the client was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cadence {
    /// What to advertise in `Limits.heartbeat_ms` for this session.
    pub heartbeat_ms: u32,
    /// Shortest gap between two presence frames about the same user.
    ///
    /// Applied by the gateway's outbound queue as a hold with a trailing edge:
    /// a frame that arrives inside the window is kept, not dropped, and a newer
    /// value for the same subject replaces it in the hold, so the state the
    /// window releases is always the latest one. The queue is the only place
    /// this can be enforced without losing the final state — the reasoning is
    /// written in `migo-presence`'s service docs and in section 159.
    pub min_interval_ms: u32,
    /// Whether typing indicators are sent to this session at all.
    ///
    /// Enforced one layer down from the table, in the delivery metadata of the
    /// `TYPING` opcode (`suppress_on` in the schema), because "never deliver
    /// this frame" is a fact about the frame, not about the publisher.
    pub typing: bool,
    /// How wide the presence subscription is.
    pub scope: PresenceScope,
}

/// The cadence for one bandwidth mode, derived from the base heartbeat.
///
/// Brief section 159 fixes the shape of the table and this fixes the arithmetic:
///
/// - `Normal` runs at the configured heartbeat, with a floor of a sixth of it.
///   The floor exists even at full frequency because a user cannot meaningfully
///   change state faster than they can report it, and a client that sends
///   `PRESENCE_SET` in a loop should cost the network one frame per floor rather
///   than one frame per call.
/// - `LowData` doubles the heartbeat and multiplies the floor by four, which is
///   the "throttled four times slower" the section asks for stated as a number.
/// - `UltraLowData` quadruples the heartbeat — the section's "maximum interval" —
///   turns typing off entirely, and raises the floor to a whole heartbeat, so a
///   session on a metered connection receives at most one presence frame per
///   subject per heartbeat.
///
/// `Auto` and `Unknown` both resolve to `Normal`. `Auto` means the client asked
/// the server to decide and gave it nothing to decide with; `Unknown` means a peer
/// on either side of this version does not know the enum. Answering both with full
/// frequency is the choice that renders correctly on a client that has not
/// understood the negotiation — degrading a peer we failed to understand would
/// make a version mismatch look like a broken presence feature.
#[must_use]
pub fn cadence_for(mode: BandwidthMode, heartbeat_ms: u32) -> Cadence {
    let base = heartbeat_ms.clamp(MIN_HEARTBEAT_MS, MAX_HEARTBEAT_MS);
    // A sixth of the heartbeat, at least a second: the floor has to stay a floor
    // even when an operator configures the shortest heartbeat allowed.
    let unit = (base / 6).max(MIN_HEARTBEAT_MS);
    match mode {
        BandwidthMode::LowData => Cadence {
            heartbeat_ms: base.saturating_mul(2).min(MAX_HEARTBEAT_MS),
            min_interval_ms: unit.saturating_mul(4),
            typing: true,
            scope: PresenceScope::OpenOnly,
        },
        BandwidthMode::UltraLowData => Cadence {
            heartbeat_ms: base.saturating_mul(4).min(MAX_HEARTBEAT_MS),
            min_interval_ms: base,
            typing: false,
            scope: PresenceScope::OpenOnly,
        },
        BandwidthMode::Normal | BandwidthMode::Auto | BandwidthMode::Unknown => Cadence {
            heartbeat_ms: base,
            min_interval_ms: unit,
            typing: true,
            scope: PresenceScope::Everything,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_the_one_section_159_describes() {
        let normal = cadence_for(BandwidthMode::Normal, 30_000);
        assert_eq!(normal.heartbeat_ms, 30_000);
        assert_eq!(normal.min_interval_ms, 5_000);
        assert!(normal.typing);
        assert_eq!(normal.scope, PresenceScope::Everything);

        let low = cadence_for(BandwidthMode::LowData, 30_000);
        assert_eq!(low.heartbeat_ms, 60_000);
        assert_eq!(low.min_interval_ms, 20_000);
        assert!(low.typing);
        assert_eq!(low.scope, PresenceScope::OpenOnly);

        let ultra = cadence_for(BandwidthMode::UltraLowData, 30_000);
        assert_eq!(ultra.heartbeat_ms, 120_000);
        assert_eq!(ultra.min_interval_ms, 30_000);
        assert!(!ultra.typing);
        assert_eq!(ultra.scope, PresenceScope::OpenOnly);
    }

    #[test]
    fn the_floor_stays_a_floor_at_the_shortest_heartbeat() {
        let tight = cadence_for(BandwidthMode::Normal, 10);
        assert_eq!(tight.heartbeat_ms, 1_000);
        assert_eq!(tight.min_interval_ms, 1_000);
    }

    #[test]
    fn the_multipliers_saturate_at_the_longest_heartbeat() {
        let loose = cadence_for(BandwidthMode::UltraLowData, 300_000);
        assert_eq!(loose.heartbeat_ms, 300_000);
        assert_eq!(loose.min_interval_ms, 300_000);
    }

    #[test]
    fn unannounced_modes_run_at_full_frequency() {
        for mode in [BandwidthMode::Auto, BandwidthMode::Unknown] {
            let cadence = cadence_for(mode, 30_000);
            assert_eq!(cadence.heartbeat_ms, 30_000);
            assert_eq!(cadence.scope, PresenceScope::Everything);
            assert!(cadence.typing);
        }
    }
}
