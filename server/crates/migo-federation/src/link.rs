//! Per-link state: the sequence rule each packet must satisfy, and the
//! reachability a partitioned node is remembered by.
//!
//! Section 169 requires a per-link sequence that strictly increases — a packet whose number
//! does not advance is rejected — and section 152 adds that a *gap* in that sequence is a
//! suspected replay or a lost segment and must reset the link. [`LinkSequences`] is the state
//! behind both rules and nothing more: it does not open or close a connection, it reports
//! what the number means so the transport layer can.
//!
//! A link starts with no entry, which reads as "last seen 0", so the first packet of a fresh
//! session must be sequence 1. A successful handshake [`reset`](LinkSequences::reset)s the
//! link, because a new session numbers its packets from the start again.
//!
//! [`LinkHealth`] lives beside it because it is the same shape of fact — per peer, in-memory,
//! written only by the transport that actually tried the link — and the opposite kind of
//! evidence: a connect that failed marks a node down, a delivered batch or an inbound
//! handshake marks it up, and the layer above reads the answer to decide whether a room
//! whose home node sits behind that link may still be written (sections 170, 173).

use std::collections::{HashMap, HashSet};

use parking_lot::Mutex;

use migo_core::Id;

use crate::model::SequenceVerdict;

/// The last in-order sequence number accepted on each link, keyed by peer node id.
pub(crate) struct LinkSequences {
    last: Mutex<HashMap<Id, u64>>,
}

impl LinkSequences {
    /// A tracker with no links yet established.
    pub(crate) fn new() -> Self {
        Self {
            last: Mutex::new(HashMap::new()),
        }
    }

    /// Judges `link_seq` against `node`'s last accepted number and advances the link if it
    /// fits.
    ///
    /// Exactly one greater than the last is [`Accept`](SequenceVerdict::Accept), and the link
    /// moves to it. Not greater is a [`Replay`](SequenceVerdict::Replay) and the link is left
    /// untouched. More than one greater is a [`Gap`](SequenceVerdict::Gap): the link's state
    /// is cleared so the caller's reset-and-re-handshake starts the next session cleanly from
    /// sequence 1 (section 152).
    #[must_use]
    pub(crate) fn observe(&self, node: Id, link_seq: u64) -> SequenceVerdict {
        let mut last = self.last.lock();
        let previous = last.get(&node).copied().unwrap_or(0);
        if link_seq <= previous {
            return SequenceVerdict::Replay;
        }
        // Reaching here guarantees `link_seq > previous`, so `previous + 1` cannot overflow:
        // `previous < link_seq <= u64::MAX`.
        if link_seq == previous + 1 {
            last.insert(node, link_seq);
            return SequenceVerdict::Accept;
        }
        last.remove(&node);
        SequenceVerdict::Gap
    }

    /// Clears a link's sequence state, so its next packet must be sequence 1.
    ///
    /// Called after a successful handshake — a new session restarts numbering — and after a
    /// gap forces the link down.
    pub(crate) fn reset(&self, node: Id) {
        self.last.lock().remove(&node);
    }
}

/// Whether each peer node is currently believed unreachable, keyed by peer node id.
///
/// This is the state behind scenario 2 of section 173's read-only half: section 170 says a
/// room whose home node cannot be reached becomes read-only rather than silently diverging,
/// and "cannot be reached" is not a guess the mesh makes about the future but evidence it
/// remembers about the past. The only evidence that marks a link down is a delivery attempt
/// that could not connect; the evidence that marks it up again is a delivered batch or a
/// peer that completed an inbound handshake. A node with no entry reads as reachable —
/// silence about a peer is never treated as a partition, because refusing a room's writes
/// on a hunch would cost availability the spec does not ask to lose.
///
/// The state is in-memory and per-process by design: it describes *this* node's view of
/// *this* moment's links, not a fact about the peer. A restart forgets everything, which
/// reads as "all links reachable" until the next drain attempt says otherwise — the honest
/// answer for a process that has not yet tried.
pub(crate) struct LinkHealth {
    down: Mutex<HashSet<Id>>,
}

impl LinkHealth {
    /// A tracker that believes every peer reachable, because it has tried none.
    pub(crate) fn new() -> Self {
        Self {
            down: Mutex::new(HashSet::new()),
        }
    }

    /// Marks `node` unreachable: a delivery attempt could not even connect.
    ///
    /// Idempotent — a link already down stays down, and the retries that keep failing
    /// (on their exponential backoff) keep finding the same entry.
    pub(crate) fn mark_down(&self, node: Id) {
        self.down.lock().insert(node);
    }

    /// Marks `node` reachable again: a batch was delivered, or the node completed an
    /// inbound handshake.
    ///
    /// Either direction of the link proves the peer is there, so the rooms it homes may
    /// be written to again. Clearing an absent entry is a no-op.
    pub(crate) fn mark_up(&self, node: Id) {
        self.down.lock().remove(&node);
    }

    /// Whether `node` is believed reachable right now.
    ///
    /// `true` unless a failed delivery attempt is still standing uncontradicted — the
    /// unknown is deliberately the permissive answer, per the module docs above.
    #[must_use]
    pub(crate) fn is_reachable(&self, node: Id) -> bool {
        !self.down.lock().contains(&node)
    }
}
