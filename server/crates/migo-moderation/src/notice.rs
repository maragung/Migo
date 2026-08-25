//! What to tell the person an action was taken against.
//!
//! # There is no moderation frame, and no notification kind either
//!
//! Brief section 145 reserves opcode 194 for `MODERATION_EVENT` and leaves the block at
//! `STATUS: SPEC`, so there is no generated frame to send. That much this crate shares
//! with social and media.
//!
//! What makes this one different is that the fallback the social crate used is not
//! available: `NotificationKind` has twelve variants and not one of them is a moderation
//! outcome. There is no `Warning`, no `AccountSuspended`, no `ContentRemoved`. So a
//! moderation notice cannot travel as a `NotificationEvent` either, and inventing a kind
//! would be a change to the protocol's enums and therefore to its golden vectors — which
//! a domain crate does not get to make.
//!
//! [`Notice`] is therefore a crate type in full. The API layer decides how it reaches a
//! device: today as a typed REST response on the account's own moderation history, and
//! as opcode 194 the day the frame is generated.
//!
//! # Why the operator's words are not in here
//!
//! The notice carries a [`Reason`] code and never the free text an operator typed into
//! the audit trail.
//!
//! That is not squeamishness about telling somebody why they were warned — the code says
//! why, and the client renders it in the recipient's own language, which a server-written
//! sentence could not do. It is that operator free text is written for the next operator.
//! It names other accounts, quotes what was reported, and sometimes records what is still
//! being investigated. `migo_rooms` reached the same conclusion for the same reason and
//! keeps a moderator-written ban reason out of every error it constructs; a notification
//! travels further than an error does, through a push service and onto a lock screen, so
//! the rule holds at least as strongly here.
//!
//! # Why a takedown does not notify
//!
//! [`Notice::of`] returns `None` for a message, media, or room removal.
//!
//! Not because the person should not be told, but because this crate does not know who
//! to tell. Removing a message yields a tombstone; the sender is in a row the moderation
//! path never reads, and for an end-to-end conversation the content that would identify
//! the audience is ciphertext. Guessing would mean notifying the wrong person that their
//! content was removed, which is worse than notifying nobody. The messaging layer already
//! publishes the tombstone to the conversation, which is how the people who can see the
//! message find out it is gone.

use migo_core::{Id, Timestamp};

use crate::model::{Action, Reason};

/// What happened, in a form a client can render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The account was warned.
    Warned,
    /// The account was suspended.
    Suspended {
        /// When it lifts, if it lifts.
        until: Option<Timestamp>,
    },
    /// The account was returned to normal.
    Reinstated,
}

impl Outcome {
    /// A short, stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Warned => "warned",
            Self::Suspended { .. } => "suspended",
            Self::Reinstated => "reinstated",
        }
    }
}

/// One moderation notice, addressed to one account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The account to deliver to, across every device it has.
    ///
    /// An account and not a device. A suspension has to reach whichever screen its owner
    /// next opens, and there is no device here that already knows: the actor is somebody
    /// else entirely.
    pub audience: Id,
    /// What was done.
    pub outcome: Outcome,
    /// Why, as a code the client renders.
    ///
    /// `None` when the action was not tied to a report — a reinstatement, or an operator
    /// acting on something they saw themselves.
    pub reason: Option<Reason>,
    /// When it was done.
    pub at: Timestamp,
}

impl Notice {
    /// The notice an action produces, if it produces one.
    ///
    /// `None` for every content takedown. See the module docs for why that is a fact
    /// about what this crate knows rather than a decision about what a user deserves.
    #[must_use]
    pub fn of(action: &Action, reason: Option<Reason>, at: Timestamp) -> Option<Self> {
        let (audience, outcome) = match *action {
            Action::Warn { account_id } => (account_id, Outcome::Warned),
            Action::Suspend { account_id, until } => (account_id, Outcome::Suspended { until }),
            Action::Reinstate { account_id } => (account_id, Outcome::Reinstated),
            Action::RemoveMessage { .. }
            | Action::RemoveMedia { .. }
            | Action::ArchiveRoom { .. }
            | Action::DisableBot { .. } => return None,
        };
        Some(Self {
            audience,
            outcome,
            // A reinstatement carries no reason. "You are no longer suspended, because
            // spam" is a sentence that reads as an accusation being repeated.
            reason: match outcome {
                Outcome::Reinstated => None,
                _ => reason,
            },
            at,
        })
    }
}
