//! What has to be delivered, described rather than delivered.
//!
//! # Why this crate does not send anything
//!
//! One user's presence change is interesting to everyone who shares a conversation
//! or a room with them, which on a social product is a set with no useful upper
//! bound. Delivering it here would mean this crate owned the subscription
//! registry, the per-connection queues, the coalescing window, and the encoder —
//! which is the gateway, reimplemented inside the domain layer and reachable only
//! through a cache.
//!
//! So the service returns a [`Fanout`]: one description of one change, which the
//! gateway encodes **once** into a refcounted `bytes::Bytes` and hands to every
//! subscriber (`docs/01-architecture.md` section 4).
//!
//! # Why the audience is an account and not a list
//!
//! Presence is subscribed to, not addressed. The gateway already holds a topic per
//! account — built when a client subscribed to a conversation or a room, which is
//! where brief section 14's "presence berbasis scope" is actually enforced — so the
//! audience of a presence change is exactly that topic, and naming it is one field.
//!
//! Computing the audience here instead would mean reading every conversation and
//! every room the subject belongs to on every state change, per heartbeat, per
//! device. That is the query section 14 exists to prevent.

use migo_core::Id;
use migo_protocol::{Opcode, PresenceEvent};

/// A presence change, and whose topic it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct Fanout {
    /// The account whose presence changed. Also the subscription topic.
    pub subject_id: Id,
    /// The connection that caused the change, which is skipped.
    ///
    /// Skipped rather than included-and-ignored: the socket that reported a state
    /// already knows it, and the acknowledgement it gets back is the confirmation.
    ///
    /// Every *other* device on the account is in the audience, and that is not an
    /// accident either. A user who sets themselves Busy on their phone expects the
    /// laptop to show Busy too, and it does so by the same path as everybody
    /// else's client rather than by a second mechanism that can disagree.
    pub exclude_device: Option<Id>,
    /// What to send.
    ///
    /// Always carries the *visible* state, never the stored one: a subject who set
    /// themselves Invisible is broadcast as Offline, because brief section 14 puts
    /// the enforcement of invisibility on the server and the only enforcement a
    /// client cannot undo is not being told.
    ///
    /// `last_seen` is always absent here. It is a per-viewer field — the subject's
    /// `show_last_seen` setting decides it, and the answer differs between two
    /// subscribers on the same topic — so it cannot appear in a frame that is
    /// encoded once and sent to all of them. It is filled in on the read path
    /// instead, where the viewer is known; see `Detail::WithLastSeen`.
    pub event: PresenceEvent,
}

impl Fanout {
    /// A presence change about `subject_id`, skipping the device that caused it.
    #[must_use]
    pub fn about(subject_id: Id, device_id: Id, event: PresenceEvent) -> Self {
        Self {
            subject_id,
            exclude_device: Some(device_id),
            event,
        }
    }

    /// The opcode this fanout is carried by.
    ///
    /// Constant, because presence has exactly one server-to-client frame. It is a
    /// method rather than a bare constant so the gateway can ask a fanout what it
    /// is instead of knowing, and so the delivery class comes with it: presence is
    /// `Coalescable`, keyed by user id (brief section 154), which is what lets a
    /// backed-up queue drop a stale Online in favour of a fresh Away.
    #[must_use]
    pub fn opcode(&self) -> Opcode {
        Opcode::PresenceEvent
    }
}
