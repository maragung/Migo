//! What presence reports.
//!
//! # What is deliberately absent
//!
//! No series is labelled by account or device. A counter labelled by account is an
//! attendance register — who was online, when, for how long — exported to whatever
//! scrapes the metrics endpoint. On a product whose premise is that the server
//! cannot read the messages, publishing a per-user activity timeline instead is not
//! a smaller disclosure.
//!
//! Labelling by *state* is a different thing and it is here: the distribution of
//! Online against Away against Busy is a property of the population, not of a
//! person, and it is what tells an operator that a release broke the idle
//! detector.
//!
//! No series counts subjects per account either, for the same reason a fan-out
//! histogram keyed by user would be a social-graph degree distribution.

use std::sync::Arc;

use migo_core::metrics::{Counter, Histogram, Registry};
use migo_protocol::PresenceState;

/// Buckets for "how many accounts did one snapshot answer for".
///
/// The interesting questions are at the ends. Near one it separates opening a
/// conversation from opening a group; at the top it separates a room subscribe from
/// a client that is asking about everyone it has ever heard of and should be paged
/// on.
const SUBJECT_BUCKETS: &[f64] = &[1.0, 2.0, 8.0, 32.0, 64.0, 128.0, 256.0, 512.0];

/// How a presence update ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdateOutcome {
    /// Stored, and the account's visible state changed.
    Accepted,
    /// Stored, and nobody needed to hear about it (brief section 156).
    Unchanged,
    /// Refused on shape: no state named, or one this version does not know.
    Invalid,
    /// Refused because the request used a field this server does not implement.
    Unsupported,
    /// Refused by the rate limiter.
    RateLimited,
}

impl UpdateOutcome {
    const ALL: [Self; 5] = [
        Self::Accepted,
        Self::Unchanged,
        Self::Invalid,
        Self::Unsupported,
        Self::RateLimited,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Unchanged => "unchanged",
            Self::Invalid => "invalid",
            Self::Unsupported => "unsupported",
            Self::RateLimited => "rate_limited",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Accepted => 0,
            Self::Unchanged => 1,
            Self::Invalid => 2,
            Self::Unsupported => 3,
            Self::RateLimited => 4,
        }
    }
}

/// A device arriving or leaving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionEvent {
    /// A socket came up and its presence entry was written.
    Connected,
    /// A socket went down cleanly and its entry was removed.
    Disconnected,
}

impl SessionEvent {
    const ALL: [Self; 2] = [Self::Connected, Self::Disconnected];

    const fn label(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Connected => 0,
            Self::Disconnected => 1,
        }
    }
}

/// What happened to a last-seen field that was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LastSeenOutcome {
    /// The subject's settings allowed it and a timestamp was returned.
    Disclosed,
    /// The subject's settings said no.
    Withheld,
    /// The bound on lookups was reached before this subject.
    Skipped,
}

impl LastSeenOutcome {
    const ALL: [Self; 3] = [Self::Disclosed, Self::Withheld, Self::Skipped];

    const fn label(self) -> &'static str {
        match self {
            Self::Disclosed => "disclosed",
            Self::Withheld => "withheld",
            Self::Skipped => "skipped",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Disclosed => 0,
            Self::Withheld => 1,
            Self::Skipped => 2,
        }
    }
}

/// The four states a broadcast can carry, in registration order.
///
/// Invisible and Unknown are absent because they are never broadcast — the
/// projection in `crate::state` turns both into Offline before a frame exists. A
/// series for them would read zero forever and invite the conclusion that nobody
/// uses invisibility.
const BROADCAST_STATES: [PresenceState; 4] = [
    PresenceState::Offline,
    PresenceState::Online,
    PresenceState::Away,
    PresenceState::Busy,
];

const fn state_index(state: PresenceState) -> usize {
    match state {
        PresenceState::Offline | PresenceState::Invisible | PresenceState::Unknown => 0,
        PresenceState::Online => 1,
        PresenceState::Away => 2,
        PresenceState::Busy => 3,
    }
}

/// The series, resolved once at construction.
pub(crate) struct Meters {
    updates: Vec<Arc<Counter>>,
    broadcasts: Vec<Arc<Counter>>,
    sessions: Vec<Arc<Counter>>,
    last_seen: Vec<Arc<Counter>>,
    heartbeats: Arc<Counter>,
    revivals: Arc<Counter>,
    snapshots: Arc<Counter>,
    snapshot_subjects: Arc<Histogram>,
}

impl Meters {
    /// Registers every series, including the outcomes that have not happened yet.
    ///
    /// A counter that springs into existence on its first occurrence cannot be
    /// alerted on beforehand: `rate(migo_presence_updates_total{outcome="rate_limited"}[5m]) > 0`
    /// against a series that does not exist does not evaluate to false, it fails to
    /// evaluate — so the alert could only be written after the first incident it was
    /// supposed to catch. Everything is created at zero for that reason.
    pub(crate) fn new(registry: &Registry) -> Self {
        Self {
            updates: UpdateOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_presence_updates_total",
                        "Presence updates requested by a client, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            broadcasts: BROADCAST_STATES
                .iter()
                .map(|state| {
                    registry.counter(
                        "migo_presence_broadcasts_total",
                        "Presence changes published to a subject's topic, by visible state.",
                        &[("state", state.as_str())],
                    )
                })
                .collect(),
            sessions: SessionEvent::ALL
                .iter()
                .map(|event| {
                    registry.counter(
                        "migo_presence_sessions_total",
                        "Device sessions registered and released, by event.",
                        &[("event", event.label())],
                    )
                })
                .collect(),
            last_seen: LastSeenOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_presence_last_seen_total",
                        "Last-seen fields resolved for a viewer, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            heartbeats: registry.counter(
                "migo_presence_heartbeats_total",
                "Presence entries refreshed by a heartbeat.",
                &[],
            ),
            revivals: registry.counter(
                "migo_presence_revivals_total",
                "Heartbeats that found no live entry and had to recreate one.",
                &[],
            ),
            snapshots: registry.counter(
                "migo_presence_snapshots_total",
                "Presence snapshots served.",
                &[],
            ),
            snapshot_subjects: registry.histogram(
                "migo_presence_snapshot_subjects",
                "Accounts answered for by one presence snapshot.",
                &[],
                SUBJECT_BUCKETS,
            ),
        }
    }

    pub(crate) fn update(&self, outcome: UpdateOutcome) {
        if let Some(counter) = self.updates.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn broadcast(&self, state: PresenceState) {
        if let Some(counter) = self.broadcasts.get(state_index(state)) {
            counter.inc();
        }
    }

    pub(crate) fn session(&self, event: SessionEvent) {
        if let Some(counter) = self.sessions.get(event.index()) {
            counter.inc();
        }
    }

    pub(crate) fn last_seen(&self, outcome: LastSeenOutcome) {
        if let Some(counter) = self.last_seen.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn heartbeat(&self, revived: bool) {
        self.heartbeats.inc();
        if revived {
            self.revivals.inc();
        }
    }

    pub(crate) fn snapshot(&self, subjects: usize) {
        self.snapshots.inc();
        // `as f64` on a count the snapshot bound holds under 513.
        self.snapshot_subjects.observe(subjects as f64);
    }
}
