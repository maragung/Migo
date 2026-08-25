//! Per-link sequence tracking: the rule that each packet on a link must carry a number one
//! greater than the last.
//!
//! Section 169 requires a per-link sequence that strictly increases — a packet whose number
//! does not advance is rejected — and section 152 adds that a *gap* in that sequence is a
//! suspected replay or a lost segment and must reset the link. This module is the state
//! behind both rules and nothing more: it does not open or close a connection, it reports
//! what the number means so the transport layer can.
//!
//! A link starts with no entry, which reads as "last seen 0", so the first packet of a fresh
//! session must be sequence 1. A successful handshake [`reset`](LinkSequences::reset)s the
//! link, because a new session numbers its packets from the start again.

use std::collections::HashMap;

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

    /// Judges `seq` against `node`'s last accepted number and advances the link if it fits.
    ///
    /// Exactly one greater than the last is [`Accept`](SequenceVerdict::Accept), and the link
    /// moves to it. Not greater is a [`Replay`](SequenceVerdict::Replay) and the link is left
    /// untouched. More than one greater is a [`Gap`](SequenceVerdict::Gap): the link's state
    /// is cleared so the caller's reset-and-re-handshake starts the next session cleanly from
    /// sequence 1 (section 152).
    #[must_use]
    pub(crate) fn observe(&self, node: Id, seq: u64) -> SequenceVerdict {
        let mut last = self.last.lock();
        let previous = last.get(&node).copied().unwrap_or(0);
        if seq <= previous {
            return SequenceVerdict::Replay;
        }
        // Reaching here guarantees `seq > previous`, so `previous + 1` cannot overflow:
        // `previous < seq <= u64::MAX`.
        if seq == previous + 1 {
            last.insert(node, seq);
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
