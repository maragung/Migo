//! What rooms report.
//!
//! # What is deliberately absent
//!
//! **No series labelled by room.** It is the label an operator asks for first and it
//! is the one that cannot be given: rooms are user-created and unbounded, so a
//! per-room counter is a time series per room forever, and the first thousand-room
//! deployment turns the metrics endpoint into a bill. It is also a disclosure — the
//! set of series names would be the list of every room that exists, including the ones
//! a browse listing would never show.
//!
//! **No series labelled by account.** A counter of joins per account is an attendance
//! register: which rooms somebody entered, when, how often. On a product whose premise
//! is that the server cannot read the messages, exporting the social graph to whatever
//! scrapes `/metrics` instead is not a smaller disclosure.
//!
//! **No series labelled by ban reason.** The reason is written by one person about
//! another, and a label is a string that ends up in a dashboard title.
//!
//! Labelling by *outcome* is a different thing and it is all here: the split between
//! joins refused as full, refused as banned, and accepted is a property of the
//! population, and it is what tells an operator that a raid is in progress or that a
//! capacity setting is wrong.

use std::sync::Arc;

use migo_core::metrics::{Counter, Histogram, Registry};

/// Buckets for "how many rooms did one listing return".
///
/// The ends are what matter. Zero separates a query that found nothing from one that
/// was never asked; the top bucket is [`MAX_LIST_LIMIT`](crate::model::MAX_LIST_LIMIT),
/// and traffic piled against it is a client that means to page and cannot.
const LISTING_BUCKETS: &[f64] = &[0.0, 1.0, 5.0, 10.0, 20.0, 50.0];

/// Buckets for "how many rooms were scanned to answer one listing".
///
/// Separate from the return-size histogram on purpose: the gap between the two is the
/// cost of filtering in the service instead of in the database, and it is the number
/// that says when a search index has stopped being optional.
const SCAN_BUCKETS: &[f64] = &[0.0, 10.0, 50.0, 100.0, 200.0, 500.0];

/// How a room creation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CreateOutcome {
    /// Created, with its conversation and the owner's membership.
    Accepted,
    /// Refused on shape: a slug, name, kind, or capacity that does not pass.
    Invalid,
    /// The slug is taken.
    Taken,
    /// Refused by the rate limiter.
    RateLimited,
}

impl CreateOutcome {
    const ALL: [Self; 4] = [
        Self::Accepted,
        Self::Invalid,
        Self::Taken,
        Self::RateLimited,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Invalid => "invalid",
            Self::Taken => "taken",
            Self::RateLimited => "rate_limited",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Accepted => 0,
            Self::Invalid => 1,
            Self::Taken => 2,
            Self::RateLimited => 3,
        }
    }
}

/// How a join ended.
///
/// Five ways to be refused, each of which means something different to whoever is
/// looking at the graph: `full` is a capacity setting, `banned` is moderation working,
/// `archived` is a client holding a stale listing, `not_admitted` is a join policy this
/// build cannot satisfy, and `rate_limited` is a raid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JoinOutcome {
    /// A new membership.
    Accepted,
    /// A membership that already existed and was still active.
    Already,
    /// A membership that had left and came back.
    Rejoined,
    /// The room does not exist.
    NotFound,
    /// The room is archived.
    Archived,
    /// The room is at capacity.
    Full,
    /// The account is banned from it.
    Banned,
    /// The join policy does not admit this account.
    NotAdmitted,
    /// Refused by the rate limiter.
    RateLimited,
}

impl JoinOutcome {
    const ALL: [Self; 9] = [
        Self::Accepted,
        Self::Already,
        Self::Rejoined,
        Self::NotFound,
        Self::Archived,
        Self::Full,
        Self::Banned,
        Self::NotAdmitted,
        Self::RateLimited,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Already => "already",
            Self::Rejoined => "rejoined",
            Self::NotFound => "not_found",
            Self::Archived => "archived",
            Self::Full => "full",
            Self::Banned => "banned",
            Self::NotAdmitted => "not_admitted",
            Self::RateLimited => "rate_limited",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Accepted => 0,
            Self::Already => 1,
            Self::Rejoined => 2,
            Self::NotFound => 3,
            Self::Archived => 4,
            Self::Full => 5,
            Self::Banned => 6,
            Self::NotAdmitted => 7,
            Self::RateLimited => 8,
        }
    }
}

/// How a permission check ended.
///
/// The busiest series here by a wide margin: every message sent into a room passes
/// through one of these. `not_a_member` against a room somebody is watching means a
/// client kept a subscription it lost the right to, which is worth an alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthorizeOutcome {
    /// Allowed.
    Granted,
    /// The caller is not in the room, or the room is gone.
    NotAMember,
    /// The caller is banned.
    Banned,
    /// The caller is muted and asked to do something a mute forbids.
    Muted,
    /// The caller is a member and lacks the permission.
    Denied,
}

impl AuthorizeOutcome {
    const ALL: [Self; 5] = [
        Self::Granted,
        Self::NotAMember,
        Self::Banned,
        Self::Muted,
        Self::Denied,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::NotAMember => "not_a_member",
            Self::Banned => "banned",
            Self::Muted => "muted",
            Self::Denied => "denied",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Granted => 0,
            Self::NotAMember => 1,
            Self::Banned => 2,
            Self::Muted => 3,
            Self::Denied => 4,
        }
    }
}

/// Which moderation action was applied.
///
/// Recorded per action rather than as one "sanctions" counter because the ratio is
/// the signal: a room where bans outnumber mutes ten to one is either under attack or
/// moderated by somebody who should be given a smaller button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SanctionKind {
    /// Silenced for a while.
    Mute,
    /// Mute lifted.
    Unmute,
    /// Removed, may return.
    Kick,
    /// Removed and kept out.
    Ban,
    /// Ban lifted.
    Unban,
}

impl SanctionKind {
    const ALL: [Self; 5] = [
        Self::Mute,
        Self::Unmute,
        Self::Kick,
        Self::Ban,
        Self::Unban,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Mute => "mute",
            Self::Unmute => "unmute",
            Self::Kick => "kick",
            Self::Ban => "ban",
            Self::Unban => "unban",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Mute => 0,
            Self::Unmute => 1,
            Self::Kick => 2,
            Self::Ban => 3,
            Self::Unban => 4,
        }
    }
}

/// How a settings or role change ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChangeOutcome {
    /// Applied, and somebody needed to hear about it.
    Applied,
    /// Applied, and it changed nothing anyone could observe (brief section 156).
    Unchanged,
    /// Refused on shape.
    Invalid,
    /// Refused because the caller lacks the permission or the rank.
    Denied,
}

impl ChangeOutcome {
    const ALL: [Self; 4] = [
        Self::Applied,
        Self::Unchanged,
        Self::Invalid,
        Self::Denied,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Unchanged => "unchanged",
            Self::Invalid => "invalid",
            Self::Denied => "denied",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Applied => 0,
            Self::Unchanged => 1,
            Self::Invalid => 2,
            Self::Denied => 3,
        }
    }
}

/// The series, resolved once at construction.
pub(crate) struct Meters {
    creations: Vec<Arc<Counter>>,
    joins: Vec<Arc<Counter>>,
    leaves: Vec<Arc<Counter>>,
    authorizations: Vec<Arc<Counter>>,
    sanctions: Vec<Arc<Counter>>,
    settings: Vec<Arc<Counter>>,
    roles: Vec<Arc<Counter>>,
    overrides: Vec<Arc<Counter>>,
    archives: Arc<Counter>,
    transfers: Arc<Counter>,
    listings: Arc<Counter>,
    listing_rooms: Arc<Histogram>,
    listing_scanned: Arc<Histogram>,
}

impl Meters {
    /// Registers every series, including the outcomes that have not happened yet.
    ///
    /// A counter that springs into existence on its first occurrence cannot be alerted
    /// on beforehand: `rate(migo_rooms_joins_total{outcome="banned"}[5m]) > 10` against
    /// a series that does not exist does not evaluate to false, it fails to evaluate —
    /// so the alert could only be written after the raid it was supposed to catch.
    /// Everything is created at zero for that reason.
    pub(crate) fn new(registry: &Registry) -> Self {
        Self {
            creations: CreateOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_rooms_creations_total",
                        "Rooms requested by a client, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            joins: JoinOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_rooms_joins_total",
                        "Room joins requested, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            leaves: ChangeOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_rooms_leaves_total",
                        "Room departures requested, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            authorizations: AuthorizeOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_rooms_authorizations_total",
                        "Room permission checks, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            sanctions: SanctionKind::ALL
                .iter()
                .map(|kind| {
                    registry.counter(
                        "migo_rooms_sanctions_total",
                        "Moderation actions applied to a member, by action.",
                        &[("action", kind.label())],
                    )
                })
                .collect(),
            settings: ChangeOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_rooms_settings_total",
                        "Room settings changes requested, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            roles: ChangeOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_rooms_role_changes_total",
                        "Member role changes requested, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            overrides: ChangeOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_rooms_permission_overrides_total",
                        "Per-member permission overrides requested, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            archives: registry.counter(
                "migo_rooms_archives_total",
                "Rooms archived.",
                &[],
            ),
            transfers: registry.counter(
                "migo_rooms_ownership_transfers_total",
                "Ownership transfers completed.",
                &[],
            ),
            listings: registry.counter(
                "migo_rooms_listings_total",
                "Room listings served.",
                &[],
            ),
            listing_rooms: registry.histogram(
                "migo_rooms_listing_rooms",
                "Rooms returned by one listing.",
                &[],
                LISTING_BUCKETS,
            ),
            listing_scanned: registry.histogram(
                "migo_rooms_listing_scanned",
                "Rooms read from the store to answer one listing.",
                &[],
                SCAN_BUCKETS,
            ),
        }
    }

    pub(crate) fn create(&self, outcome: CreateOutcome) {
        if let Some(counter) = self.creations.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn join(&self, outcome: JoinOutcome) {
        if let Some(counter) = self.joins.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn leave(&self, outcome: ChangeOutcome) {
        if let Some(counter) = self.leaves.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn authorize(&self, outcome: AuthorizeOutcome) {
        if let Some(counter) = self.authorizations.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn sanction(&self, kind: SanctionKind) {
        if let Some(counter) = self.sanctions.get(kind.index()) {
            counter.inc();
        }
    }

    pub(crate) fn settings(&self, outcome: ChangeOutcome) {
        if let Some(counter) = self.settings.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn role(&self, outcome: ChangeOutcome) {
        if let Some(counter) = self.roles.get(outcome.index()) {
            counter.inc();
        }
    }

    /// A per-member permission override.
    ///
    /// A separate series from [`Self::role`] rather than a shared one, because the two
    /// answer different questions. Role changes are ordinary community management and
    /// their volume is uninteresting; an override is a hand-written mask, and a
    /// deployment where they suddenly outnumber role changes is a deployment where
    /// somebody is working around the role table.
    pub(crate) fn overrides(&self, outcome: ChangeOutcome) {
        if let Some(counter) = self.overrides.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn archive(&self) {
        self.archives.inc();
    }

    pub(crate) fn transfer(&self) {
        self.transfers.inc();
    }

    /// One listing, with both halves of its cost.
    ///
    /// `returned` and `scanned` are observed together so the two histograms cannot
    /// disagree about how many listings happened.
    pub(crate) fn listing(&self, returned: usize, scanned: usize) {
        self.listings.inc();
        // `as f64` on counts the list bound holds under 51 and the scan bound under
        // 501.
        self.listing_rooms.observe(returned as f64);
        self.listing_scanned.observe(scanned as f64);
    }
}
