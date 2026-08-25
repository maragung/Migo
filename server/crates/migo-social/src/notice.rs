//! What to tell the other person, and why it is not a fanout.
//!
//! # There is no social frame to broadcast
//!
//! Every other domain crate in this workspace returns a `Fanout`: a topic, an event,
//! and the device that already knows. This one cannot. Brief section 145 reserves
//! opcode 115 for `FRIEND_EVENT` and leaves the whole social block at `STATUS: SPEC`,
//! so the packet registry has no frame that says "somebody wants to be your friend".
//!
//! What it does have is `NotificationEvent`, opcode 144, and
//! `NotificationKind::FriendRequest`. A friend request is a notification in every
//! product sense — it arrives when the recipient is not looking, it survives being
//! offline, and it is acted on later — so it travels as one. That is a better fit than
//! a broadcast would have been anyway: a friend request has an audience of exactly one
//! account, and a fanout describes an audience of many.
//!
//! # Why the text is empty
//!
//! `title` and `body` are always `None`, and only `actor_id` is filled.
//!
//! The client renders the sentence. It knows the display name, it knows the user's
//! language, and it knows whether the app is showing a list or a push banner. A server
//! that wrote "Budi wants to be your friend" would be choosing a language from a
//! column that describes the *sender's* locale, copying a display name that goes stale
//! the moment its owner changes it, and putting a person's name into a push payload
//! that leaves this system's control — brief section 174's logging rules exist because
//! that kind of copy is hard to take back.
//!
//! # The one thing this shape cannot say
//!
//! An acceptance. `NotificationKind` has `FriendRequest` and no `FriendAccepted`, so
//! "your request was accepted" is delivered under the same kind, with `actor_id` set to
//! the account that accepted. The client can tell the two apart without a new kind,
//! because in one case the standing is now `friends` and in the other it is
//! `awaiting_response` — and it has to ask for the standing regardless in order to draw
//! the right button.
//!
//! Adding the variant would be the honest fix, and it is a change to the protocol's
//! enums and therefore to its golden vectors. A domain crate does not get to make it.

use migo_core::{Id, Timestamp};
use migo_protocol::{NotificationEvent, NotificationKind, Opcode};

/// One notification, addressed to one account.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    /// The account to deliver to, across every device it has.
    ///
    /// An account and not a device: a friend request should be waiting on the phone
    /// whichever screen it was accepted on, and there is no device here that already
    /// knows — the actor is somebody else.
    pub audience: Id,
    /// What to send.
    pub event: NotificationEvent,
}

impl Notice {
    /// Tells `audience` that `actor` wants to be their friend.
    #[must_use]
    pub fn friend_request(audience: Id, actor: Id, at: Timestamp) -> Self {
        Self {
            audience,
            event: event(actor, at),
        }
    }

    /// Tells `audience` that `actor` accepted their request.
    ///
    /// The same kind as a request; see the module docs for why the registry leaves no
    /// other choice.
    #[must_use]
    pub fn friend_accepted(audience: Id, actor: Id, at: Timestamp) -> Self {
        Self {
            audience,
            event: event(actor, at),
        }
    }

    /// The opcode this notice is carried by.
    ///
    /// A method rather than knowledge the gateway keeps, so a second notice kind
    /// cannot be sent under the wrong opcode by an unchanged `match` somewhere else.
    #[must_use]
    pub fn opcode(&self) -> Opcode {
        Opcode::NotificationEvent
    }
}

/// A social notification with every optional field left empty but the actor.
fn event(actor: Id, at: Timestamp) -> NotificationEvent {
    NotificationEvent {
        kind: NotificationKind::FriendRequest,
        at,
        // See the module docs: the client writes the sentence.
        title: None,
        body: None,
        // Neither applies. A friend request is not about a conversation or a room, and
        // filling either field with something plausible would have clients rendering a
        // tap target that opens the wrong screen.
        conversation_id: None,
        room_id: None,
        actor_id: Some(actor),
    }
}
