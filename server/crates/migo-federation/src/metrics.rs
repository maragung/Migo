//! Counters for the mesh: peers admitted, handshakes accepted and refused, packets rejected
//! by the replay defences, links reset, and the outbox's enqueue/deliver/fail flow.
//!
//! # What may label a series here, and what may never
//!
//! Brief section 174 forbids a metric series labelled by account, and this crate adds that
//! none is labelled by node or by peer either. A counter keyed on a node id would let a
//! dashboard rebuild the mesh's topology and traffic straight off the metrics endpoint —
//! which peers a node talks to, how often, when a link went quiet — and that is exactly the
//! shape section 174 keeps out of it, doubly so for federation where the topology is itself
//! sensitive. So every series here is either unlabelled or labelled by a closed enum — a
//! handshake-rejection reason, a replay reason — whose cardinality is fixed at compile time
//! and whose growth is a diff a reviewer sees.
//!
//! The rejection reasons are recorded here even though every one of them hands the peer the
//! same opaque error (sections 48, 161): the peer must not learn why it was turned away, but
//! an operator must, because a spike of `blocked` replays and a spike of `proof_invalid`
//! attempts are different incidents.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};

/// Why a handshake was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandshakeReject {
    /// The claimed node id is not in the allow-list. A node the operator never named
    /// (section 170), refused before its payload is decoded (section 169).
    UnknownPeer,
    /// The peer is in the allow-list but blocked by an operator.
    Blocked,
    /// The peer is in the allow-list but paused by an operator.
    Paused,
    /// The peer is allowed, but its proof did not verify — a bad signature, a skewed clock,
    /// a wrong protocol version, or a reflected hello. All one reason here, because
    /// [`verify_proof`](migo_crypto::node::verify_proof) is the single gate they fail at.
    ProofInvalid,
}

impl HandshakeReject {
    pub(crate) const ALL: [Self; 4] = [
        Self::UnknownPeer,
        Self::Blocked,
        Self::Paused,
        Self::ProofInvalid,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::UnknownPeer => "unknown_peer",
            Self::Blocked => "blocked",
            Self::Paused => "paused",
            Self::ProofInvalid => "proof_invalid",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Why a packet was rejected by the replay defences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplayReason {
    /// A handshake nonce was seen again inside the tolerance window — a replayed hello.
    NonceReused,
    /// A packet's sequence number did not advance past the last on its link — a duplicate
    /// or a straggler.
    SequenceReplay,
    /// A packet's sequence number skipped ahead of the expected one — a gap that section 152
    /// treats as a suspected replay or a lost segment, and that resets the link.
    SequenceGap,
}

impl ReplayReason {
    pub(crate) const ALL: [Self; 3] =
        [Self::NonceReused, Self::SequenceReplay, Self::SequenceGap];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NonceReused => "nonce_reused",
            Self::SequenceReplay => "sequence_replay",
            Self::SequenceGap => "sequence_gap",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Every series this crate publishes.
pub(crate) struct Meters {
    peers_added: Arc<Counter>,
    handshakes: Arc<Counter>,
    handshake_rejected: Vec<Arc<Counter>>,
    replay_rejected: Vec<Arc<Counter>>,
    links_reset: Arc<Counter>,
    outbox_enqueued: Arc<Counter>,
    outbox_delivered: Arc<Counter>,
    outbox_failed: Arc<Counter>,
}

/// Registers one counter per variant, each tagged `key` with the variant's own label.
///
/// Registering the whole set up front is what gives a dashboard a flat line rather than a gap
/// for a reason nobody has hit yet.
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
            peers_added: registry.counter(
                "migo_federation_peers_added_total",
                "Peers admitted to the allow-list.",
                &[],
            ),
            handshakes: registry.counter(
                "migo_federation_handshakes_total",
                "Mesh handshakes accepted.",
                &[],
            ),
            handshake_rejected: per_variant(
                registry,
                "migo_federation_handshake_rejected_total",
                "Mesh handshakes refused, by reason.",
                "reason",
                &HandshakeReject::ALL,
                |reason| reason.label(),
            ),
            replay_rejected: per_variant(
                registry,
                "migo_federation_replay_rejected_total",
                "Packets refused by the replay defences, by reason.",
                "reason",
                &ReplayReason::ALL,
                |reason| reason.label(),
            ),
            links_reset: registry.counter(
                "migo_federation_links_reset_total",
                "Links reset after a sequence gap (section 152).",
                &[],
            ),
            outbox_enqueued: registry.counter(
                "migo_federation_outbox_enqueued_total",
                "Events written to the federation outbox.",
                &[],
            ),
            outbox_delivered: registry.counter(
                "migo_federation_outbox_delivered_total",
                "Outbox events confirmed delivered.",
                &[],
            ),
            outbox_failed: registry.counter(
                "migo_federation_outbox_failed_total",
                "Outbox delivery attempts that failed and were rescheduled.",
                &[],
            ),
        }
    }

    pub(crate) fn peer_added(&self) {
        self.peers_added.inc();
    }

    pub(crate) fn handshake_accepted(&self) {
        self.handshakes.inc();
    }

    pub(crate) fn handshake_rejected(&self, reason: HandshakeReject) {
        if let Some(counter) = self.handshake_rejected.get(reason.index()) {
            counter.inc();
        }
    }

    pub(crate) fn replay_rejected(&self, reason: ReplayReason) {
        if let Some(counter) = self.replay_rejected.get(reason.index()) {
            counter.inc();
        }
    }

    pub(crate) fn link_reset(&self) {
        self.links_reset.inc();
    }

    pub(crate) fn outbox_enqueued(&self) {
        self.outbox_enqueued.inc();
    }

    pub(crate) fn outbox_delivered(&self) {
        self.outbox_delivered.inc();
    }

    pub(crate) fn outbox_failed(&self) {
        self.outbox_failed.inc();
    }
}
