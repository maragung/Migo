//! What has to be delivered, described rather than delivered.
//!
//! # Why this crate does not send anything
//!
//! A message accepted into a group of forty has to reach up to forty accounts on
//! however many devices each. Doing that here would mean this crate owned the
//! subscription registry, the per-connection queues, the backpressure policy, and
//! the encoder — which is the gateway, reimplemented inside the domain layer and
//! reachable only through a database.
//!
//! So the service returns a [`Fanout`]: one description of one change, which the
//! gateway encodes **once** into a refcounted [`bytes::Bytes`] and hands to every
//! subscriber (`docs/01-architecture.md` section 4). Encoding per subscriber
//! instead would make a forty-member group cost forty encodes of identical bytes,
//! and that is the single easiest way to lose a fanout benchmark.
//!
//! It also keeps the storage write and the network write from sharing a fate. The
//! message is durable before the first byte is queued, so a subscriber whose
//! connection is wedged delays a delivery and does not roll back an append.
//!
//! [`bytes::Bytes`]: https://docs.rs/bytes/latest/bytes/struct.Bytes.html

use migo_core::Id;
use migo_protocol::{
    ConversationMemberEvent, ConversationStateEvent, ConversationVoteEvent, MessageEvent,
    MessageReceipt, Opcode, TypingEvent,
};

/// One thing to broadcast to a conversation.
///
/// An enum rather than a method per payload, because the caller's job — encode
/// once, publish to a topic, honour the opcode's delivery class — is identical
/// for all of them, and the only thing that varies is the payload.
#[derive(Clone, Debug, PartialEq)]
pub enum Broadcast {
    /// A new message, or a tombstone for one that was deleted.
    Message(MessageEvent),
    /// A delivery or read watermark moving forward.
    Receipt(MessageReceipt),
    /// Somebody started or stopped typing.
    Typing(TypingEvent),
    /// A group's membership moved. Clients rotate sender keys on every one of
    /// these, so it is never coalesced away into a count: *who* joined or left
    /// is the fact, not just that the roster changed size.
    Member(ConversationMemberEvent),
    /// A running group kick vote's tally.
    Vote(ConversationVoteEvent),
    /// Group metadata moved: a rename. Deltas only, coalesced per conversation.
    State(ConversationStateEvent),
}

impl Broadcast {
    /// The opcode this broadcast is carried by.
    ///
    /// The gateway needs it to frame the payload, and it also carries the
    /// delivery class the frame must be treated with — `Critical` for a message,
    /// `Coalescable` for typing, keyed by conversation and user (brief section
    /// 154). Returning the opcode rather than the class hands over the whole
    /// decision instead of a summary of it.
    #[must_use]
    pub fn opcode(&self) -> Opcode {
        match self {
            Self::Message(_) => Opcode::MessageEvent,
            Self::Receipt(_) => Opcode::MessageReceipt,
            Self::Typing(_) => Opcode::Typing,
            Self::Member(_) => Opcode::ConversationMemberEvent,
            Self::Vote(_) => Opcode::ConversationVoteEvent,
            Self::State(_) => Opcode::ConversationStateEvent,
        }
    }
}

/// A change, and who should hear about it.
///
/// The audience is always the conversation: everyone currently in it, on every
/// device they have connected. It is not a field because there is no second
/// answer — a message that reached a subset of a conversation would be a
/// consistency bug wearing a feature's clothes.
#[derive(Clone, Debug, PartialEq)]
pub struct Fanout {
    /// The conversation to publish to. Also the subscription topic.
    pub conversation_id: Id,
    /// The connection that caused the change, which is skipped.
    ///
    /// Skipped rather than included-and-ignored: the device that sent a message
    /// already has it, and already received the acknowledgement that told it the
    /// assigned sequence. Delivering a copy back would make the client choose
    /// between rendering the message twice and writing dedup logic for a frame
    /// the server should not have sent.
    ///
    /// Every *other* device on the sender's account is in the audience. That is
    /// the whole point of multi-device: a message typed on a phone appears on the
    /// laptop, and it appears there by the same path as everybody else's.
    pub exclude_device: Option<Id>,
    /// What to send.
    pub event: Broadcast,
}

impl Fanout {
    /// A broadcast to everyone in `conversation_id` except `device_id`.
    #[must_use]
    pub fn to_conversation(conversation_id: Id, device_id: Id, event: Broadcast) -> Self {
        Self {
            conversation_id,
            exclude_device: Some(device_id),
            event,
        }
    }

    /// A broadcast nobody's socket caused, so nobody is excluded: a vote that
    /// expired unanswered, or a grace timer that fired. Every subscriber hears
    /// it, including the device whose action opened the vote in the first place.
    #[must_use]
    pub fn unattributed(conversation_id: Id, event: Broadcast) -> Self {
        Self {
            conversation_id,
            exclude_device: None,
            event,
        }
    }
}
