//! The gateway's runtime knobs, resolved once from [`GatewayConfig`] into the units the hot
//! path wants.
//!
//! [`GatewayConfig`] stores milliseconds and counts, because that is what a config file and an
//! operator speak. The driver wants [`Duration`]s to hand to timers and signed millisecond
//! deltas to compare against [`Timestamp`](migo_core::Timestamp) arithmetic. Converting once
//! at startup keeps the per-frame path free of unit juggling and keeps the saturating casts in
//! one place.

use std::time::Duration;

use migo_core::config::GatewayConfig;

/// The ceiling on how many topics one session may hold, from brief section 149. A session that
/// asks for more is answered `TOO_MANY_SUBSCRIPTIONS` rather than allowed to pin unbounded
/// server memory.
pub(crate) const MAX_SUBSCRIPTIONS: usize = 512;

/// Resolved settings for one running gateway.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    /// The most sessions this node accepts before refusing new handshakes as overloaded.
    pub(crate) max_sessions: usize,
    /// The bounded depth of one session's outbound queue.
    pub(crate) queue_capacity: usize,
    /// How long a session may go without a frame before it is closed; a missed heartbeat is
    /// two of these (section 149).
    pub(crate) heartbeat: Duration,
    /// The interval at which the driver re-checks liveness and lagging.
    pub(crate) tick: Duration,
    /// How long a disconnected session's resume buffer is retained (section 150), in
    /// milliseconds, for comparison against timestamps.
    pub(crate) resume_window_ms: i64,
    /// The most Critical frames retained per session for resume (section 150).
    pub(crate) resume_buffer_frames: usize,
    /// How long the outbound queue may stay full before the session is closed as lagging
    /// (section 151), in milliseconds.
    pub(crate) lagging_deadline_ms: i64,
    /// Whether outbound frames may be compressed per section 155.
    pub(crate) compression: bool,
    /// How long a connection has to complete its handshake before it is dropped.
    pub(crate) handshake_timeout: Duration,
}

impl Settings {
    /// Resolves a [`GatewayConfig`] into runtime settings.
    ///
    /// Milliseconds become [`Duration`]s and signed deltas; the tick that drives liveness and
    /// lagging checks is a quarter of the heartbeat, clamped to at least a quarter second, so
    /// a five-second lagging deadline is noticed within a tick of expiring without spinning a
    /// timer needlessly fast.
    pub(crate) fn from_config(config: &GatewayConfig) -> Self {
        let heartbeat = Duration::from_millis(config.heartbeat_ms);
        let tick = (heartbeat / 4).max(Duration::from_millis(250));
        Self {
            max_sessions: config.max_sessions,
            queue_capacity: config.session_queue_capacity.max(1),
            heartbeat,
            tick,
            resume_window_ms: i64::try_from(config.resume_window_ms).unwrap_or(i64::MAX),
            resume_buffer_frames: config.resume_buffer_frames,
            lagging_deadline_ms: i64::try_from(config.lagging_deadline_ms).unwrap_or(i64::MAX),
            compression: config.compression_enabled,
            handshake_timeout: Duration::from_millis(config.handshake_timeout_ms),
        }
    }
}
