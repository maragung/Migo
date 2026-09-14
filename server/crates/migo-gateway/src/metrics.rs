//! Counters and gauges for the transport: sessions opened and closed, frames in and out,
//! frames dropped under backpressure, resume attempts, handshakes refused, reconnects served,
//! decode failures, and the byte size of inbound frames.
//!
//! # What may label a series here, and what may never
//!
//! Brief section 174 forbids a metric series labelled by account, device, conversation, or
//! session id. A gateway is the richest possible source of exactly that shape — a counter
//! keyed on a session id would let a dashboard rebuild who was connected, for how long, and
//! how much they sent, straight off the metrics endpoint. So every series here is either
//! unlabelled or labelled by a closed enum — a close reason, a drop class, a resume outcome,
//! a handshake-rejection reason — whose cardinality is fixed at compile time and whose growth
//! is a diff a reviewer sees.
//!
//! The handshake-rejection reasons are recorded even though every refused client is handed
//! the same opaque error (sections 48, 161): the client must not learn why it was turned
//! away, but an operator must, because a spike of `version_unsupported` and a spike of
//! `overloaded` are different incidents.

use std::sync::Arc;

use migo_core::metrics::{Counter, Gauge, Histogram, Registry};
use migo_core::Error as CoreError;

/// Why a session ended, for the `migo_gateway_sessions_closed_total` series.
///
/// A superset of the wire [`CloseReason`](migo_protocol::CloseReason): it also names the
/// operational endings a client never sees a code for — a heartbeat that stopped, a transport
/// that broke, a handshake that never completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Closed {
    /// The client asked to disconnect.
    ClientRequest,
    /// The node is shutting down.
    ServerShutdown,
    /// The node is draining before a planned stop.
    NodeDraining,
    /// The outbound queue stayed full past the lagging deadline (section 151).
    SessionLagging,
    /// A resume was required but could not be served.
    ResumeRequired,
    /// The access token expired mid-session.
    AuthExpired,
    /// The client was asked to move to another node.
    Rebalance,
    /// The client broke the protocol — a frame out of turn, a reserved flag, a second hello.
    ProtocolViolation,
    /// Two heartbeat intervals passed with no frame from the client (section 149).
    HeartbeatTimeout,
    /// The transport failed underneath the session.
    TransportError,
    /// The handshake never completed, so no full session ever existed.
    HandshakeFailed,
}

impl Closed {
    pub(crate) const ALL: [Self; 11] = [
        Self::ClientRequest,
        Self::ServerShutdown,
        Self::NodeDraining,
        Self::SessionLagging,
        Self::ResumeRequired,
        Self::AuthExpired,
        Self::Rebalance,
        Self::ProtocolViolation,
        Self::HeartbeatTimeout,
        Self::TransportError,
        Self::HandshakeFailed,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ClientRequest => "client_request",
            Self::ServerShutdown => "server_shutdown",
            Self::NodeDraining => "node_draining",
            Self::SessionLagging => "session_lagging",
            Self::ResumeRequired => "resume_required",
            Self::AuthExpired => "auth_expired",
            Self::Rebalance => "rebalance",
            Self::ProtocolViolation => "protocol_violation",
            Self::HeartbeatTimeout => "heartbeat_timeout",
            Self::TransportError => "transport_error",
            Self::HandshakeFailed => "handshake_failed",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Which delivery class a dropped frame belonged to. Critical is deliberately absent: a
/// Critical frame is never dropped, so a series for it would only ever read zero and would
/// invite someone to make it non-zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dropped {
    /// A newer value for the same coalescing key arrived while the queue was full, and there
    /// was no older one in the queue to replace.
    Coalescable,
    /// A droppable frame met a full queue and was dropped silently, as section 151 allows —
    /// but counted here, because a frame that vanishes without a trace is how a bug hides for
    /// months.
    Droppable,
}

impl Dropped {
    pub(crate) const ALL: [Self; 2] = [Self::Coalescable, Self::Droppable];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Coalescable => "coalescable",
            Self::Droppable => "droppable",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Why a subscription a client asked for was not granted.
///
/// The two reasons stay apart because they mean different things to whoever reads the dashboard:
/// the cap is one client subscribing to too much at once, and an unauthorised topic is one client
/// asking for something that is not theirs — the first is a client to fix, the second is a client
/// to watch. Both series are aggregates that name nobody: no account, no session, and no topic id
/// ever reaches a label here (section 174).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The session already held the per-session subscription ceiling.
    Cap,
    /// The dispatcher did not grant the topic to this caller.
    Unauthorized,
}

impl Refused {
    pub(crate) const ALL: [Self; 2] = [Self::Cap, Self::Unauthorized];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Cap => "cap",
            Self::Unauthorized => "unauthorized",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// How a resume attempt ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResumeOutcome {
    /// The buffer still covered the client's last frame, so the tunnel was bridged.
    Resumed,
    /// The buffer no longer covered it; the client fell back to a full cursor sync.
    Rejected,
    /// No retained session matched the id, or it had expired past the resume window.
    Unknown,
}

impl ResumeOutcome {
    pub(crate) const ALL: [Self; 3] = [Self::Resumed, Self::Rejected, Self::Unknown];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Resumed => "resumed",
            Self::Rejected => "rejected",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Why a handshake was refused. Every one of these hands the client the same opaque error;
/// only this series tells them apart, for the operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandshakeReject {
    /// The client asked for a protocol version this node does not speak.
    VersionUnsupported,
    /// A token supplied in the hello did not verify.
    BadToken,
    /// The node is already at its session ceiling.
    Overloaded,
    /// The opening frame was malformed or out of turn.
    ProtocolViolation,
}

impl HandshakeReject {
    pub(crate) const ALL: [Self; 4] = [
        Self::VersionUnsupported,
        Self::BadToken,
        Self::Overloaded,
        Self::ProtocolViolation,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::VersionUnsupported => "version_unsupported",
            Self::BadToken => "bad_token",
            Self::Overloaded => "overloaded",
            Self::ProtocolViolation => "protocol_violation",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Bucket bounds for `migo_frame_bytes`, in bytes, capped at
/// [`MAX_FRAME_BYTES`](migo_wire::limits::MAX_FRAME_BYTES). Deliberately coarse: the first
/// bound swallows everything a text-sized frame can be, so the series can tell a PING from a
/// media-sized burst without ever telling a one-word reply from a paragraph — the distinction
/// section 174's side-channel rule exists to keep off the metrics endpoint.
pub(crate) const FRAME_BYTES_BUCKETS: &[f64] = &[1024.0, 4096.0, 16_384.0, 65_536.0, 262_144.0];

/// Why a client had to come back, for the `migo_reconnect_total` series: the reason its
/// previous session closed, carried by the retained resume buffer and counted only when the
/// reconnect is actually served.
///
/// Exactly the reasons a close retains a resume buffer (the same membership as
/// `retains_resume` in the connection driver), and no more: a close that keeps nothing can
/// never produce a reconnect, so a series for its reasons would only ever read zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reconnect {
    /// The node shut down or drained, and told the client to come back.
    ServerShutdown,
    /// The node drained before a planned stop.
    NodeDraining,
    /// The writer could not keep up with the client's connection.
    SessionLagging,
    /// Two heartbeat intervals passed with no frame from the client.
    HeartbeatTimeout,
    /// The transport failed underneath the session.
    TransportError,
    /// The client was asked to move to another node.
    Rebalance,
}

impl Reconnect {
    pub(crate) const ALL: [Self; 6] = [
        Self::ServerShutdown,
        Self::NodeDraining,
        Self::SessionLagging,
        Self::HeartbeatTimeout,
        Self::TransportError,
        Self::Rebalance,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ServerShutdown => "server_shutdown",
            Self::NodeDraining => "node_draining",
            Self::SessionLagging => "session_lagging",
            Self::HeartbeatTimeout => "heartbeat_timeout",
            Self::TransportError => "transport_error",
            Self::Rebalance => "rebalance",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    /// The label for a close reason, or `None` for a close that retains no resume buffer —
    /// and so can never be followed by a counted reconnect.
    pub(crate) fn of(closed: Closed) -> Option<Self> {
        match closed {
            Closed::ServerShutdown => Some(Self::ServerShutdown),
            Closed::NodeDraining => Some(Self::NodeDraining),
            Closed::SessionLagging => Some(Self::SessionLagging),
            Closed::HeartbeatTimeout => Some(Self::HeartbeatTimeout),
            Closed::TransportError => Some(Self::TransportError),
            Closed::Rebalance => Some(Self::Rebalance),
            _ => None,
        }
    }
}

/// How a frame or message body failed to decode, for the `migo_decode_errors_total` series.
///
/// The label is the error symbol the wire layer maps the failure onto — the same symbol the
/// client is handed in its `ERROR` frame — restricted to the four codes a decode failure can
/// carry. Before this series, a decode failure was visible only as a session closed
/// `protocol_violation`; now the operator can tell a client sending oversized frames from one
/// speaking a mangled dialect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodeFailure {
    /// The bytes did not parse as the frame or message they claimed to be.
    DecodeFailed,
    /// The frame exceeded the byte budget the WELCOME advertised.
    FrameTooLarge,
    /// The frame's version byte named a version this node does not speak.
    UnsupportedVersion,
    /// The frame's header set a reserved flag bit.
    UnsupportedFlag,
}

impl DecodeFailure {
    pub(crate) const ALL: [Self; 4] = [
        Self::DecodeFailed,
        Self::FrameTooLarge,
        Self::UnsupportedVersion,
        Self::UnsupportedFlag,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::DecodeFailed => "decode_failed",
            Self::FrameTooLarge => "frame_too_large",
            Self::UnsupportedVersion => "unsupported_version",
            Self::UnsupportedFlag => "unsupported_flag",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    /// Maps the error the wire layer raised onto the label, by code, so the series and the
    /// `ERROR` frame the client receives can never disagree about what failed.
    pub(crate) fn of(error: &CoreError) -> Self {
        match error.code() {
            migo_protocol::codes::FRAME_TOO_LARGE => Self::FrameTooLarge,
            migo_protocol::codes::PROTOCOL_VERSION_UNSUPPORTED => Self::UnsupportedVersion,
            migo_protocol::codes::UNSUPPORTED_FLAG => Self::UnsupportedFlag,
            _ => Self::DecodeFailed,
        }
    }
}

/// Every series this crate publishes.
pub(crate) struct Meters {
    sessions_opened: Arc<Counter>,
    sessions_closed: Vec<Arc<Counter>>,
    frames_in: Arc<Counter>,
    frames_out: Arc<Counter>,
    batches_out: Arc<Counter>,
    frames_dropped: Vec<Arc<Counter>>,
    resume: Vec<Arc<Counter>>,
    handshake_rejected: Vec<Arc<Counter>>,
    rate_limited: Arc<Counter>,
    subscriptions_refused: Vec<Arc<Counter>>,
    reconnect: Vec<Arc<Counter>>,
    decode_errors: Vec<Arc<Counter>>,
    frame_bytes: Arc<Histogram>,
    sessions_live: Arc<Gauge>,
    subscriptions_live: Arc<Gauge>,
}

/// Registers one counter per variant, each tagged `key` with the variant's own label, so a
/// dashboard shows a flat line rather than a gap for a reason nobody has hit yet.
fn per_variant<T>(
    registry: &Registry,
    name: &'static str,
    help: &'static str,
    key: &'static str,
    variants: &[T],
    label: impl Fn(&T) -> &'static str,
) -> Vec<Arc<Counter>> {
    variants
        .iter()
        .map(|variant| registry.counter(name, help, &[(key, label(variant))]))
        .collect()
}

/// Registers the three section-174 series — reconnects by cause, decode failures by error
/// symbol, and the coarse frame-size histogram — and hands them back for `Meters::new` to
/// store. Split out so the constructor stays under clippy's line budget.
fn section_174_series(
    registry: &Registry,
) -> (Vec<Arc<Counter>>, Vec<Arc<Counter>>, Arc<Histogram>) {
    (
        per_variant(
            registry,
            "migo_reconnect_total",
            "Sessions resumed after a disconnect, by the reason the previous session closed.",
            "reason",
            &Reconnect::ALL,
            |reason| reason.label(),
        ),
        per_variant(
            registry,
            "migo_decode_errors_total",
            "Frames or message bodies that failed to decode, by error symbol.",
            "error",
            &DecodeFailure::ALL,
            |failure| failure.label(),
        ),
        registry.histogram(
            "migo_frame_bytes",
            "Bytes per frame received from clients. Bucket bounds are deliberately coarse: \
             the first bound swallows every text-sized frame, so the series reads transport \
             scale, never message length.",
            &[],
            FRAME_BYTES_BUCKETS,
        ),
    )
}

impl Meters {
    /// Registers every series at zero up front.
    pub(crate) fn new(registry: &Registry) -> Self {
        let (reconnect, decode_errors, frame_bytes) = section_174_series(registry);
        Self {
            sessions_opened: registry.counter(
                "migo_gateway_sessions_opened_total",
                "Sessions that completed a handshake.",
                &[],
            ),
            sessions_closed: per_variant(
                registry,
                "migo_gateway_sessions_closed_total",
                "Sessions closed, by reason.",
                "reason",
                &Closed::ALL,
                |reason| reason.label(),
            ),
            frames_in: registry.counter(
                "migo_gateway_frames_in_total",
                "Frames accepted from clients.",
                &[],
            ),
            frames_out: registry.counter(
                "migo_gateway_frames_out_total",
                "Frames written to clients.",
                &[],
            ),
            batches_out: registry.counter(
                "migo_gateway_batches_out_total",
                "BATCH envelopes written to clients that negotiated the feature. Every element \
                 is already counted in frames_out, so this series measures the sends batching \
                 saved, not traffic.",
                &[],
            ),
            frames_dropped: per_variant(
                registry,
                "migo_gateway_frames_dropped_total",
                "Frames dropped under backpressure, by delivery class.",
                "class",
                &Dropped::ALL,
                |class| class.label(),
            ),
            resume: per_variant(
                registry,
                "migo_gateway_resume_total",
                "Resume attempts, by outcome.",
                "outcome",
                &ResumeOutcome::ALL,
                |outcome| outcome.label(),
            ),
            handshake_rejected: per_variant(
                registry,
                "migo_gateway_handshake_rejected_total",
                "Handshakes refused, by reason.",
                "reason",
                &HandshakeReject::ALL,
                |reason| reason.label(),
            ),
            rate_limited: registry.counter(
                "migo_gateway_rate_limited_total",
                "Frames refused by the rate limiter.",
                &[],
            ),
            subscriptions_refused: per_variant(
                registry,
                "migo_gateway_subscriptions_refused_total",
                "Subscriptions asked for and not granted, by reason.",
                "reason",
                &Refused::ALL,
                |reason| reason.label(),
            ),
            reconnect,
            decode_errors,
            frame_bytes,
            sessions_live: registry.gauge(
                "migo_gateway_sessions_live",
                "Sessions currently connected.",
                &[],
            ),
            subscriptions_live: registry.gauge(
                "migo_gateway_subscriptions_live",
                "Topic subscriptions currently held across all sessions.",
                &[],
            ),
        }
    }

    pub(crate) fn session_opened(&self) {
        self.sessions_opened.inc();
        self.sessions_live.inc();
    }

    pub(crate) fn session_closed(&self, reason: Closed) {
        if let Some(counter) = self.sessions_closed.get(reason.index()) {
            counter.inc();
        }
        self.sessions_live.dec();
    }

    /// Frame byte counts are bounded by the wire limit (well under 2^53), so widening to f64
    /// loses nothing; the buckets' own bounds are f64 anyway.
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn frame_in(&self, bytes: usize) {
        self.frames_in.inc();
        self.frame_bytes.observe(bytes as f64);
    }

    pub(crate) fn frames_out(&self, n: u64) {
        self.frames_out.add(n);
    }

    pub(crate) fn batches_out(&self, n: u64) {
        self.batches_out.add(n);
    }

    pub(crate) fn frame_dropped(&self, class: Dropped) {
        if let Some(counter) = self.frames_dropped.get(class.index()) {
            counter.inc();
        }
    }

    /// The frames-written counter's current value, for a composition test that needs
    /// the number in a failure message rather than on a dashboard.
    pub(crate) fn frames_out_total(&self) -> u64 {
        self.frames_out.get()
    }

    /// The dropped-frame counters' current values by delivery class, for the same
    /// readers `frames_out_total` serves: a frame that vanishes between an ingest log
    /// and a silent socket is named here or nowhere, because the drop itself is silent.
    pub(crate) fn dropped_frames_total(&self) -> Vec<(&'static str, u64)> {
        Dropped::ALL
            .iter()
            .map(|class| {
                let value = self
                    .frames_dropped
                    .get(class.index())
                    .map_or(0, |counter| counter.get());
                (class.label(), value)
            })
            .collect()
    }

    pub(crate) fn resume(&self, outcome: ResumeOutcome) {
        if let Some(counter) = self.resume.get(outcome.index()) {
            counter.inc();
        }
    }

    /// Counts one served reconnect, labelled by why the client had to come back.
    pub(crate) fn reconnected(&self, reason: Reconnect) {
        if let Some(counter) = self.reconnect.get(reason.index()) {
            counter.inc();
        }
    }

    /// Counts one frame or message body the wire layer could not decode, by error symbol.
    pub(crate) fn decode_failed(&self, failure: DecodeFailure) {
        if let Some(counter) = self.decode_errors.get(failure.index()) {
            counter.inc();
        }
    }

    pub(crate) fn handshake_rejected(&self, reason: HandshakeReject) {
        if let Some(counter) = self.handshake_rejected.get(reason.index()) {
            counter.inc();
        }
    }

    pub(crate) fn rate_limited(&self) {
        self.rate_limited.inc();
    }

    pub(crate) fn subscriptions_added(&self, n: u64) {
        for _ in 0..n {
            self.subscriptions_live.inc();
        }
    }

    pub(crate) fn subscriptions_refused(&self, reason: Refused, n: u64) {
        if n == 0 {
            return;
        }
        if let Some(counter) = self.subscriptions_refused.get(reason.index()) {
            counter.add(n);
        }
    }

    pub(crate) fn subscriptions_removed(&self, n: u64) {
        for _ in 0..n {
            self.subscriptions_live.dec();
        }
    }
}
