//! What has to be delivered, described rather than delivered.
//!
//! # Why this crate does not send anything
//!
//! A room is the widest audience in the product. One join in an eight-hundred-member
//! room is eight hundred deliveries, and brief section 55 answers rooms an order of
//! magnitude larger than that with shards and regional relays. Delivering from here
//! would mean this crate owned the subscription registry, the per-connection queues,
//! the coalescing window, and the encoder — which is the gateway, rebuilt inside the
//! domain layer and reachable only through a store.
//!
//! So every mutating operation returns a [`Fanout`]: one description of one change,
//! which the gateway encodes **once** into a refcounted `bytes::Bytes` and hands to
//! every subscriber on the room's topic (`docs/01-architecture.md` section 4).
//!
//! # Why the audience is a room and not a list of accounts
//!
//! Because the roster is the wrong list. Members who are not connected are not in the
//! audience, and computing which ones are would mean intersecting an eight-hundred-row
//! roster with the presence cache on every join — the query brief section 14 exists to
//! prevent. The gateway already holds the set of sockets subscribed to this room, and
//! that set *is* the audience; naming the room is one field.
//!
//! # Why an `Option`
//!
//! Brief section 156 forbids a frame when nothing changed, and the `Option` is that
//! rule made visible in the type. Leaving a room you are not in, setting the topic to
//! the topic it already has, and lifting a ban nobody was under all produce `None`, so
//! a caller with nothing to send cannot forget to check.

use migo_core::Id;
use migo_protocol::{Opcode, RoomMemberEvent, RoomStateEvent, RoomVoteEvent};

/// A room change, and whose topic it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct Fanout {
    /// The room whose subscribers should hear this. Also the subscription topic.
    pub room_id: Id,
    /// The connection that caused the change, which is skipped.
    ///
    /// Skipped rather than included-and-ignored: the socket that asked for a join
    /// gets the join *response*, which carries more than the event would, and sending
    /// both would have a client render its own arrival twice.
    ///
    /// `None` for a change nobody's socket caused — a sanction applied by a
    /// background job, or a member count corrected by a recount.
    pub exclude_device: Option<Id>,
    /// What to send.
    pub event: Broadcast,
}

/// The three frames rooms publish.
///
/// An enum rather than three fanout types because the gateway's dispatch is one match
/// either way, and because a member event, a state event, and a vote tally about the
/// same room have to keep their order: a client that learned the new member count
/// before it learned who joined would render a count nobody accounts for, and one
/// that learned a vote closed before the removal it caused would show a kicked
/// member as merely away.
#[derive(Clone, Debug, PartialEq)]
pub enum Broadcast {
    /// Somebody joined, left, or had their role changed.
    Member(RoomMemberEvent),
    /// A counter or a setting moved.
    State(RoomStateEvent),
    /// A kick vote's running tally, or its closing.
    Vote(RoomVoteEvent),
}

impl Fanout {
    /// A membership change caused by `device_id`.
    #[must_use]
    pub fn member(room_id: Id, device_id: Id, event: RoomMemberEvent) -> Self {
        Self {
            room_id,
            exclude_device: Some(device_id),
            event: Broadcast::Member(event),
        }
    }

    /// A settings or counter change caused by `device_id`.
    #[must_use]
    pub fn state(room_id: Id, device_id: Id, event: RoomStateEvent) -> Self {
        Self {
            room_id,
            exclude_device: Some(device_id),
            event: Broadcast::State(event),
        }
    }

    /// A kick-vote tally caused by `device_id`.
    ///
    /// Excluding the voter's socket for the same reason the member fanout excludes
    /// the joiner's: the reply to `ROOM_VOTE_KICK` already carries this tally, and
    /// delivering both would have a client count its own voice twice.
    #[must_use]
    pub fn vote(room_id: Id, device_id: Id, event: RoomVoteEvent) -> Self {
        Self {
            room_id,
            exclude_device: Some(device_id),
            event: Broadcast::Vote(event),
        }
    }

    /// A change nobody's socket caused, so nobody is excluded.
    #[must_use]
    pub fn unattributed(room_id: Id, event: Broadcast) -> Self {
        Self {
            room_id,
            exclude_device: None,
            event,
        }
    }

    /// The opcode this fanout is carried by.
    ///
    /// A method rather than knowledge the gateway keeps, so that adding a third
    /// broadcast cannot leave a `match` in the gateway silently sending it under the
    /// wrong opcode. Both are `Coalescable` in the packet registry (brief section
    /// 145), which is what lets a backed-up queue collapse three counter updates
    /// about one room into the last one.
    #[must_use]
    pub fn opcode(&self) -> Opcode {
        match self.event {
            Broadcast::Member(_) => Opcode::RoomMemberEvent,
            Broadcast::State(_) => Opcode::RoomStateEvent,
            Broadcast::Vote(_) => Opcode::RoomVoteEvent,
        }
    }
}
