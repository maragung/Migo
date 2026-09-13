//! The bookkeeping behind section 163's trigger: what changed, and who is owed.
//!
//! # Why a book at all
//!
//! A group's membership moves, and every member's client must rotate its sender
//! keys — that rule is the client's to obey, not the server's, because the
//! server holds no key and cannot rotate, seal, or install anything. What the
//! server *can* do is answer the two questions a rotating client has no other
//! way to answer:
//!
//! * **Which membership generation is this?** The number travels on the member
//!   event itself (`group_key_epoch`), so a client that reconnects after
//!   missing a redistribution can compare the wire's generation against the one
//!   its keys carry and know to ask for the new distribution rather than wait
//!   for a message it will never be able to open.
//! * **Who is the change owed to?** The redistribution audience — every member
//!   after the change — is recorded here, so the relay path
//!   ([`Messaging::distribute_key`]) can refuse a frame aimed at somebody the
//!   change has already removed, and a test can pin that the audience of a kick
//!   excludes the kicked member by name.
//!
//! # Why in memory, and why that is enough
//!
//! The generation this book counts is bookkeeping about a *trigger*, not the
//! epoch the crypto enforces. The real epoch lives inside the sealed
//! distribution, and `migo-crypto`'s `ReceiverKeyState::adopt` (spelled
//! without a link, because this crate must not depend on that one) refuses a
//! distribution that does not advance it — so a restart that resets this
//! counter to zero costs a client a cosmetic number on the next member event
//! and nothing else: no distribution is accepted or refused on the strength
//! of this count alone. A durable counter would be a second sequencer for a
//! sequence the crypto already owns, and the same argument that keeps
//! conversation sequencing in one place (brief section 67) keeps this out of
//! the store.
//!
//! [`Messaging::distribute_key`]: crate::traits::Messaging::distribute_key

use std::collections::HashMap;

use migo_core::Id;
use parking_lot::Mutex;

/// One recorded membership change: the conversation, the generation it
/// produced, and the members the redistribution that follows is owed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redistribution {
    /// The group whose membership moved.
    pub conversation_id: Id,
    /// The membership generation this change produced. Starts at 1 on the
    /// create and rises by one on every later change — informational ordering
    /// for the client, not the crypto epoch, which travels sealed inside the
    /// distribution and is enforced by the receiver.
    pub generation: u32,
    /// Every active member after the change, sorted and deduplicated. The
    /// distributor is in this list; the member a removal just removed is not,
    /// which is the whole point: a redistribution is owed to the group that
    /// remains, and the departed hold nothing further.
    pub audience: Vec<Id>,
}

/// The trigger book: one row per group, the latest change it recorded.
///
/// In memory, for the reasons the module docs give. One row is kept rather
/// than a history because only the latest generation is ever actionable — a
/// client that missed generation 4 does not want generation 3, it wants the
/// current one.
#[derive(Debug, Default)]
pub struct RedistributionBook {
    entries: Mutex<HashMap<Id, Redistribution>>,
}

impl RedistributionBook {
    /// An empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one membership change and returns the redistribution it owes.
    ///
    /// The audience is normalised here — sorted, deduplicated — so the
    /// recorded row is canonical no matter what order the caller's member
    /// list happened to be in, and two recordings of the same roster compare
    /// equal. The generation rises by one per recording; the first recording
    /// for a conversation is generation 1, so a client can treat "no number
    /// yet" and "number 0" as the same never-happened state.
    pub fn record(&self, conversation_id: Id, audience: Vec<Id>) -> Redistribution {
        let mut members = audience;
        members.sort_unstable();
        members.dedup();
        let entry = Redistribution {
            conversation_id,
            generation: self
                .entries
                .lock()
                .get(&conversation_id)
                .map_or(1, |latest| latest.generation.saturating_add(1)),
            audience: members,
        };
        self.entries.lock().insert(conversation_id, entry.clone());
        entry
    }

    /// The latest recorded change for a conversation, if it has had one.
    #[must_use]
    pub fn latest(&self, conversation_id: Id) -> Option<Redistribution> {
        self.entries.lock().get(&conversation_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONVERSATION: u128 = 0x1631;

    fn ids(values: &[u128]) -> Vec<Id> {
        values.iter().map(|v| Id::from(*v)).collect()
    }

    #[test]
    fn a_create_records_every_seated_member_at_generation_one() {
        let book = RedistributionBook::new();
        let recorded = book.record(Id::from(CONVERSATION), ids(&[0x11, 0x12, 0x13]));
        assert_eq!(recorded.generation, 1, "the create is the first change");
        assert_eq!(
            recorded.audience,
            ids(&[0x11, 0x12, 0x13]),
            "the redistribution is owed to every member the create seated"
        );
        assert_eq!(
            book.latest(Id::from(CONVERSATION)),
            Some(recorded),
            "the latest row is the one just recorded"
        );
    }

    #[test]
    fn a_join_raises_the_generation_and_grows_the_audience() {
        let book = RedistributionBook::new();
        book.record(Id::from(CONVERSATION), ids(&[0x11, 0x12]));
        let recorded = book.record(Id::from(CONVERSATION), ids(&[0x11, 0x12, 0x13]));
        assert_eq!(recorded.generation, 2);
        assert_eq!(
            recorded.audience,
            ids(&[0x11, 0x12, 0x13]),
            "the newcomer is owed the redistribution as much as the founders"
        );
    }

    #[test]
    fn a_removal_records_the_remaining_members_and_not_the_removed_one() {
        let book = RedistributionBook::new();
        book.record(Id::from(CONVERSATION), ids(&[0x11, 0x12, 0x13]));
        let recorded = book.record(Id::from(CONVERSATION), ids(&[0x11, 0x13]));
        assert_eq!(recorded.generation, 2);
        assert_eq!(
            recorded.audience,
            ids(&[0x11, 0x13]),
            "the removed member is not owed the redistribution that follows their removal"
        );
        assert!(
            !recorded.audience.contains(&Id::from(0x12u128)),
            "a kick's audience excludes the kicked member by name"
        );
    }

    #[test]
    fn the_audience_is_normalised_so_order_does_not_change_the_row() {
        let book = RedistributionBook::new();
        let recorded = book.record(Id::from(CONVERSATION), ids(&[0x13, 0x11, 0x12, 0x11]));
        assert_eq!(
            recorded.audience,
            ids(&[0x11, 0x12, 0x13]),
            "sorted and deduplicated, whatever order the member rows arrived in"
        );
    }

    #[test]
    fn the_generation_saturates_rather_than_wrapping() {
        let book = RedistributionBook::new();
        let conversation = Id::from(CONVERSATION);
        book.entries.lock().insert(
            conversation,
            Redistribution {
                conversation_id: conversation,
                generation: u32::MAX,
                audience: ids(&[0x11]),
            },
        );
        let recorded = book.record(conversation, ids(&[0x11, 0x12]));
        assert_eq!(
            recorded.generation, u32::MAX,
            "the counter pins at its ceiling instead of wrapping to zero, which a client would read as \"nothing has ever changed\""
        );
    }

    #[test]
    fn an_unknown_conversation_has_no_row() {
        let book = RedistributionBook::new();
        assert_eq!(book.latest(Id::from(CONVERSATION)), None);
    }
}
