//! Counters and gauges for the transport: sessions opened and closed, frames in and out,
//! frames dropped under backpressure, resume attempts, and handshakes refused.
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

use migo_core::metrics::{Counter, Gauge, Registry};

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

/// Every series this crate publishes.
pub(crate) struct Meters {
    sessions_opened: Arc<Counter>,
    sessions_closed: Vec<Arc<Counter>>,
    frames_in: Arc<Counter>,
    frames_out: Arc<Counter>,
    frames_dropped: Vec<Arc<Counter>>,
    resume: Vec<Arc<Counter>>,
    handshake_rejected: Vec<Arc<Counter>>,
    rate_limited: Arc<Counter>,
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

impl Meters {
    /// Registers every series at zero up front.
    pub(crate) fn new(registry: &Registry) -> Self {
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

    pub(crate) fn frame_in(&self) {
        self.frames_in.inc();
    }

    pub(crate) fn frames_out(&self, n: u64) {
        self.frames_out.add(n);
    }

    pub(crate) fn frame_dropped(&self, class: Dropped) {
        if let Some(counter) = self.frames_dropped.get(class.index()) {
            counter.inc();
        }
    }

    pub(crate) fn resume(&self, outcome: ResumeOutcome) {
        if let Some(counter) = self.resume.get(outcome.index()) {
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

    pub(crate) fn subscriptions_removed(&self, n: u64) {
        for _ in 0..n {
            self.subscriptions_live.dec();
        }
    }
}
