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

use migo_core::metrics::{Counter, Histogram, Registry};
use migo_protocol::generated::CallRating;

use crate::model::EndReason;

/// Bucket bounds for `migo_call_setup_seconds`, in seconds.
///
/// The observation is the client's own `setup_ms` from `CALL_STATS` — initiation to media
/// up, as the one party that can see both ends measures it — so the bounds span what that
/// number can honestly be: a sub-second local answer at the bottom, a human taking the
/// call at a ring's length in the middle, and the long tail a satellite link earns at the
/// top. No bound is fine enough to tell one call from another; only transport scale.
const SETUP_SECONDS_BUCKETS: &[f64] = &[
    0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0, 300.0,
];

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

/// How a group-call join ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupJoinKind {
    /// Seated; the roster hears the announcement.
    Joined,
    /// The same seat again: the roster is served, nobody is told.
    Duplicate,
    /// The roster is at the ceiling.
    Full,
    /// Membership or the conversation's call policy said no.
    Blocked,
    /// The id was reused for a different conversation's call.
    Conflict,
    /// Refused on shape before anything was read.
    Invalid,
    /// Refused by the rate limiter.
    RateLimited,
}

impl GroupJoinKind {
    const ALL: [Self; 7] = [
        Self::Joined,
        Self::Duplicate,
        Self::Full,
        Self::Blocked,
        Self::Conflict,
        Self::Invalid,
        Self::RateLimited,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Duplicate => "duplicate",
            Self::Full => "full",
            Self::Blocked => "blocked",
            Self::Conflict => "conflict",
            Self::Invalid => "invalid",
            Self::RateLimited => "rate_limited",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// A user's verdict on a call it has just left, as section 180 asks for it.
///
/// The vocabulary is the requirement's own four words, plus `Unknown` so that
/// a build whose protocol enum has grown a variant this server does not know
/// still lands on a series rather than being silently dropped: a rating this
/// node cannot name is still a rating a user gave, and counting it as anything
/// else would be a lie about what the users said.
///
/// The order is discriminant order, which is the order the vector is registered
/// in, so a verdict's own counter is found at its wire value.
const RATING_KINDS: [CallRating; 5] = [
    CallRating::Unknown,
    CallRating::Excellent,
    CallRating::Good,
    CallRating::Average,
    CallRating::Poor,
];

/// The label one verdict is counted under.
///
/// Lower case and one word per verdict, because these are label values rather
/// than prose: a dashboard groups by them and an alert matches on them.
const fn rating_label(rating: CallRating) -> &'static str {
    match rating {
        CallRating::Unknown => "unknown",
        CallRating::Excellent => "excellent",
        CallRating::Good => "good",
        CallRating::Average => "average",
        CallRating::Poor => "poor",
    }
}

/// The optional details a user can attach to a rating, one bit each.
///
/// A mask rather than a list because the four are not exclusive — a call can
/// have had bad audio and a bad connection at once — and because a client that
/// knows a fifth reason must not be able to make this build's decoder fail.
/// Bits this build does not name are ignored rather than counted: an unnamed
/// bit is a statement about a future build, not a category the operator has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallIssue {
    Audio,
    Video,
    Connection,
    Dropped,
}

impl CallIssue {
    const ALL: [Self; 4] = [Self::Audio, Self::Video, Self::Connection, Self::Dropped];

    /// The bit this issue occupies in the `issues` mask.
    const fn bit(self) -> u64 {
        match self {
            Self::Audio => 1,
            Self::Video => 1 << 1,
            Self::Connection => 1 << 2,
            Self::Dropped => 1 << 3,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Connection => "connection",
            Self::Dropped => "dropped",
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
    setup_seconds: Arc<Histogram>,
    turn_fallback: Arc<Counter>,
    rating: Vec<Arc<Counter>>,
    issue: Vec<Arc<Counter>>,
    group_join: Vec<Arc<Counter>>,
    group_left: Arc<Counter>,
    group_relayed: Arc<Counter>,
    group_rekeyed: Arc<Counter>,
    history_pruned: Arc<Counter>,
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
            setup_seconds: registry.histogram(
                "migo_call_setup_seconds",
                "Call setup latency in seconds, as the client reports it over CALL_STATS: \
                 from its own initiation to media being up. The server cannot measure this \
                 itself — it never sees the media — so the number is the one party that can \
                 see both ends.",
                &[],
                SETUP_SECONDS_BUCKETS,
            ),
            turn_fallback: registry.counter(
                "migo_call_turn_fallback_total",
                "Calls whose client reported the media fell back from P2P to a TURN relay. \
                 Reported, not observed: the relay is between the two devices and this \
                 server never sees the media, so the count is the client's own used_turn \
                 claim from CALL_STATS.",
                &[],
            ),
            rating: RATING_KINDS
                .iter()
                .map(|rating| {
                    registry.counter(
                        "migo_call_rating_total",
                        "Ratings users gave calls they had just left, by verdict. The \
                         post-call quality rating section 180 asks for: a user's own \
                         summary of a call this node could not hear, so it is reported \
                         as given and never inferred.",
                        &[("rating", rating_label(*rating))],
                    )
                })
                .collect(),
            issue: CallIssue::ALL
                .iter()
                .map(|issue| {
                    registry.counter(
                        "migo_call_issue_total",
                        "Problems users attached to a call rating, by kind. One rating \
                         can carry several, which is why these are separate counters \
                         rather than one series over a mask. Only the four kinds this \
                         build names are exported; a client's other bits are dropped \
                         rather than guessed at.",
                        &[("issue", issue.label())],
                    )
                })
                .collect(),
            group_join: GroupJoinKind::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_calls_group_join_total",
                        "Group-call joins, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            group_left: registry.counter(
                "migo_calls_group_left_total",
                "Group-call seats vacated, by leave or replacement.",
                &[],
            ),
            group_relayed: registry.counter(
                "migo_calls_group_relayed_total",
                "Sealed payloads relayed between group-call devices.",
                &[],
            ),
            group_rekeyed: registry.counter(
                "migo_calls_group_rekeyed_total",
                "Group-call frame-key rotations this node distributed to a roster.",
                &[],
            ),
            history_pruned: registry.counter(
                "migo_calls_history_pruned_total",
                "Ended calls dropped from the call history by the retention prune, because \
                 they aged past the retention window or because one of their parties was \
                 already over the per-account cap. A node whose rate here tracks its call \
                 traffic is a node whose store is bounded by retention rather than by the \
                 cap, which is the shape the default expects.",
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

    /// Records one client-reported setup latency, in milliseconds, on the setup histogram.
    ///
    /// The value is the client's own measurement, accepted as-is: a number this node could
    /// not check is still the number an operator pages on, and the alternative — deriving
    /// something from the call row's timestamps — would measure a different thing (invite
    /// to connected on the server's clock) and answer a different question.
    pub(crate) fn setup_observed(&self, setup_ms: u32) {
        // A u32 of milliseconds widened to f64 loses nothing below 24 days.
        self.setup_seconds.observe(f64::from(setup_ms) / 1000.0);
    }

    /// Counts one client-reported TURN fallback: media that left P2P for a relay.
    pub(crate) fn turn_fallback(&self) {
        self.turn_fallback.inc();
    }

    /// Counts one post-call rating, on the series for the verdict given.
    ///
    /// The index is the wire discriminant and the vector is registered in that
    /// same order, so the lookup is the verdict itself rather than a second
    /// mapping that could drift from the protocol enum.
    pub(crate) fn rated(&self, rating: CallRating) {
        if let Some(counter) = self.rating.get(rating.to_wire() as usize) {
            counter.inc();
        }
    }

    /// Counts each problem a rating named, one increment per bit set.
    ///
    /// A rating that named two problems is two increments, because these series
    /// answer how often each problem was reported and not how many ratings
    /// mentioned at least one. Bits this build does not name are dropped: the
    /// mask is a client's, and a truth about it that this build cannot state is
    /// better left unsaid than folded into a category it is not.
    pub(crate) fn issues(&self, mask: u64) {
        for issue in CallIssue::ALL {
            if mask & issue.bit() != 0 {
                if let Some(counter) = self.issue.get(issue.index()) {
                    counter.inc();
                }
            }
        }
    }

    pub(crate) fn group_join(&self, outcome: GroupJoinKind) {
        if let Some(counter) = self.group_join.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn group_left(&self) {
        self.group_left.inc();
    }

    pub(crate) fn group_relayed(&self) {
        self.group_relayed.inc();
    }

    pub(crate) fn group_rekeyed(&self) {
        self.group_rekeyed.inc();
    }

    /// Counts ended calls dropped by the retention prune.
    pub(crate) fn history_pruned(&self, count: usize) {
        self.history_pruned.add(count as u64);
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
        EndReason::Busy => "busy",
    }
}
