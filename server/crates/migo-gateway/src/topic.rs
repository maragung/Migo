//! The key a subscription is filed under.
//!
//! A [`Topic`] on the wire is a kind plus an id — a conversation, a room, a user's presence,
//! a game. The hub files subscribers under the same pair, reduced to something small, `Copy`,
//! and hashable so a fan-out is a map lookup and a set iteration, never a scan.

use migo_core::Id;
use migo_protocol::{Topic, TopicKind};

/// The hashable identity of a topic: its kind (as the wire discriminant) and its id.
///
/// The kind is kept as its raw discriminant rather than the [`TopicKind`] enum so this key never
/// depends on that enum deriving `Hash`, and so a kind this build does not recognise still hashes
/// to a distinct bucket rather than collapsing into `Unknown`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct TopicKey {
    kind: u32,
    id: Id,
}

impl TopicKey {
    /// Reduces a wire [`Topic`] to its key.
    pub(crate) fn of(topic: &Topic) -> Self {
        Self {
            kind: topic.kind as u32,
            id: topic.id,
        }
    }

    /// Rebuilds the wire [`Topic`] this key stands for.
    ///
    /// The inverse of [`TopicKey::of`], for the one place that must hand topics back to a caller
    /// that speaks the wire's vocabulary: the resume retention, which carries a dropped session's
    /// topics to the reconnect that will re-ask about them. A kind this build does not recognise
    /// round-trips as `Unknown` rather than being lost — the same tolerance `from_wire` gives every
    /// other enum on the wire.
    pub(crate) fn to_topic(self) -> Topic {
        Topic {
            kind: TopicKind::from_wire(self.kind),
            id: self.id,
        }
    }
}
