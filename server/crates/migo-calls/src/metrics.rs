//! What the call service reports.
//!
//! # What is deliberately absent
//!
//! No series here is labelled by account, device, or conversation. A call is
//! the most sensitive fact this server holds — *these two accounts, at this
//! hour, talked for this long* — and brief section 174 forbids exporting it
//! to whatever scrapes the metrics endpoint. The labels that remain are
//! closed vocabularies: outcomes, reasons, relay kinds.
//!
//! No series measures a payload either. Sealed SDP length is a side channel
//! to the shape of a conversation (an offer with video tracks is longer than
//! one without), and a histogram of it would publish, per call, something the
//! server promised not to look at. The codec's bound is enforced; its
//! distribution is not reported.
//!
//! What is left is the shape of the traffic: how many rings landed, how they
//! ended, how much relay happened, how many invites died on the sweep. That
//! is what an operator pages on — a ring success rate that collapsed or a
//! relay volume that spiked — and it says nothing about any one person.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};

use crate::model::EndReason;

/// How an invite ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InviteOutcome {
    /// Stored as ringing; the callee gets the event.
    Ringing,
    /// The same call id again: the first answer stands, no second ring.
    Duplicate,
    /// A block in either direction stopped it before it rang.
    Blocked,
    /// The id names a call whose ring already died.
    Expired,
    /// Not a member, a stranger's id, or a self-call. One outcome, because
    /// the caller cannot tell these apart and neither should a dashboard.
    Unknown,
    /// The id was reused for a different invite, or names a finished call.
    Conflict,
    /// Refused on shape before anything was read.
    Invalid,
    /// Refused by the rate limiter.
    RateLimited,
}

impl InviteOutcome {
    const ALL: [Self; 8] = [
        Self::Ringing,
        Self::Duplicate,
        Self::Blocked,
        Self::Expired,
        Self::Unknown,
        Self::Conflict,
        Self::Invalid,
        Self::RateLimited,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Ringing => "ringing",
            Self::Duplicate => "duplicate",
            Self::Blocked => "blocked",
            Self::Expired => "expired",
            Self::Unknown => "unknown",
            Self::Conflict => "conflict",
            Self::Invalid => "invalid",
            Self::RateLimited => "rate_limited",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// How an answer ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnswerOutcome {
    /// The call is connecting and the caller is told.
    Answered,
    /// The same device answered again, or the call is already over.
    Duplicate,
    /// Another device of this account won the race.
    Conflict,
    /// Not this account's call to answer.
    Unknown,
    /// The ring had already expired; it was retired as `NoAnswer`.
    Expired,
    /// Refused on shape.
    Invalid,
    /// Refused by the rate limiter.
    RateLimited,
}

impl AnswerOutcome {
    const ALL: [Self; 7] = [
        Self::Answered,
        Self::Duplicate,
        Self::Conflict,
        Self::Unknown,
        Self::Expired,
        Self::Invalid,
        Self::RateLimited,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Duplicate => "duplicate",
            Self::Conflict => "conflict",
            Self::Unknown => "unknown",
            Self::Expired => "expired",
            Self::Invalid => "invalid",
            Self::RateLimited => "rate_limited",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// Which relay a frame rode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayKind {
    /// Sealed SDP.
    Sdp,
    /// A sealed batch of ICE candidates.
    Ice,
}

impl RelayKind {
    const ALL: [Self; 2] = [Self::Sdp, Self::Ice];

    const fn label(self) -> &'static str {
        match self {
            Self::Sdp => "sdp",
            Self::Ice => "ice",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// The series, resolved once at construction.
pub(crate) struct Meters {
    invite: Vec<Arc<Counter>>,
    answer: Vec<Arc<Counter>>,
    ended: Vec<Arc<Counter>>,
    relayed: Vec<Arc<Counter>>,
    connected: Arc<Counter>,
    expired: Arc<Counter>,
}

impl Meters {
    /// Registers every series, including the outcomes that have not happened
    /// yet.
    ///
    /// A counter that springs into existence on its first occurrence cannot
    /// be alerted on beforehand: `rate(migo_calls_invite_total{outcome="blocked"}[5m]) > 0`
    /// against a series that does not exist does not evaluate to false, it
    /// fails to evaluate — so the alert could only be written after the first
    /// incident it was supposed to catch. Everything is created at zero for
    /// that reason.
    pub(crate) fn new(registry: &Registry) -> Self {
        Self {
            invite: InviteOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_calls_invite_total",
                        "Call invites, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            answer: AnswerOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_calls_answer_total",
                        "Call answers, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            ended: EndReason::ALL
                .iter()
                .map(|reason| {
                    registry.counter(
                        "migo_calls_ended_total",
                        "Calls ended, by reason. Covers every path: end, decline, cancel, and the expiry sweep.",
                        &[("reason", reason_label(*reason))],
                    )
                })
                .collect(),
            relayed: RelayKind::ALL
                .iter()
                .map(|kind| {
                    registry.counter(
                        "migo_calls_relayed_total",
                        "Sealed payloads relayed between call devices, by kind.",
                        &[("kind", kind.label())],
                    )
                })
                .collect(),
            connected: registry.counter(
                "migo_calls_connected_total",
                "Calls that reached the connected state.",
                &[],
            ),
            expired: registry.counter(
                "migo_calls_expired_total",
                "Invites retired by the expiry sweep or on answer.",
                &[],
            ),
        }
    }

    pub(crate) fn invite(&self, outcome: InviteOutcome) {
        if let Some(counter) = self.invite.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn answer(&self, outcome: AnswerOutcome) {
        if let Some(counter) = self.answer.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn ended(&self, reason: EndReason) {
        if let Some(counter) = self.ended.get(reason.to_wire() as usize) {
            counter.inc();
        }
    }

    pub(crate) fn relayed(&self, kind: RelayKind) {
        if let Some(counter) = self.relayed.get(kind.index()) {
            counter.inc();
        }
    }

    pub(crate) fn connected(&self) {
        self.connected.inc();
    }

    /// Counts invites retired by expiry, on both series that care: the
    /// sweep's own counter, and the end-reason counter a `NoAnswer` belongs
    /// to alongside every other way a call ends.
    pub(crate) fn expired(&self, count: usize) {
        self.expired.add(count as u64);
        if let Some(counter) = self.ended.get(EndReason::NoAnswer.to_wire() as usize) {
            counter.add(count as u64);
        }
    }
}

/// The label for an end reason.
const fn reason_label(reason: EndReason) -> &'static str {
    match reason {
        EndReason::ByCaller => "by_caller",
        EndReason::ByCallee => "by_callee",
        EndReason::Declined => "declined",
        EndReason::NoAnswer => "no_answer",
        EndReason::Failed => "failed",
        EndReason::Network => "network",
    }
}
