//! The key a subscription is filed under.
//!
//! A [`Topic`] on the wire is a kind plus an id — a conversation, a room, a user's presence,
//! a game. The hub files subscribers under the same pair, reduced to something small, `Copy`,
//! and hashable so a fan-out is a map lookup and a set iteration, never a scan.

use migo_core::Id;
use migo_protocol::Topic;

/// The hashable identity of a topic: its kind (as the wire discriminant) and its id.
///
/// The kind is kept as its raw discriminant rather than the [`TopicKind`](migo_protocol::TopicKind)
/// enum so this key never depends on that enum deriving `Hash`, and so a kind this build does
/// not recognise still hashes to a distinct bucket rather than collapsing into `Unknown`.
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
}
