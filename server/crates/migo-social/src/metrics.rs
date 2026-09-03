//! Counters for the social graph.
//!
//! # No account labels, and no pair labels
//!
//! Brief section 174 forbids a metric series labelled by account, device, or
//! conversation, and the social graph is where that rule earns its keep twice over. A
//! series labelled by account would be an attendance register. A series labelled by
//! *pair* would be the social graph itself, exported in plain text to whatever
//! scrapes the endpoint — which is the one dataset this crate exists to protect.
//!
//! So every series here is labelled by outcome or by edge kind, both of which are
//! closed enums with a handful of values. The cardinality of this whole module is
//! fixed at compile time.
//!
//! # Why refusals are not one series
//!
//! `Blocked` and `Restricted` are separate outcomes on the interaction counter even
//! though the caller is told the same thing. Brief section 180 requires the *caller*
//! not to be able to distinguish them; it says nothing about the operator, who needs
//! to know whether a spike in refused calls is a blocklist working or a privacy
//! default being too tight. Aggregated across every account, neither value says
//! anything about a person.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};

/// What a friend request did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestOutcome {
    /// A new request is waiting.
    Sent,
    /// A request was already waiting; nothing was written.
    Duplicate,
    /// The other side had asked first, so the two became friends.
    Reciprocated,
    /// They were already friends.
    Redundant,
    /// One of the two had blocked the other.
    Blocked,
    /// The subject's `who_can_add` excluded the caller.
    Restricted,
    /// One of the two is at the friendship ceiling.
    Full,
    /// Malformed, self-addressed, or aimed at an account that does not exist.
    Invalid,
    /// The limiter refused it.
    RateLimited,
}

impl RequestOutcome {
    pub(crate) const ALL: [Self; 9] = [
        Self::Sent,
        Self::Duplicate,
        Self::Reciprocated,
        Self::Redundant,
        Self::Blocked,
        Self::Restricted,
        Self::Full,
        Self::Invalid,
        Self::RateLimited,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Duplicate => "duplicate",
            Self::Reciprocated => "reciprocated",
            Self::Redundant => "redundant",
            Self::Blocked => "blocked",
            Self::Restricted => "restricted",
            Self::Full => "full",
            Self::Invalid => "invalid",
            Self::RateLimited => "rate_limited",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// What answering a request did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponseOutcome {
    /// Accepted; both friend edges exist.
    Accepted,
    /// Declined; the pending edges are gone.
    Declined,
    /// There was nothing to answer.
    Missing,
}

impl ResponseOutcome {
    pub(crate) const ALL: [Self; 3] = [Self::Accepted, Self::Declined, Self::Missing];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Missing => "missing",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Which edge moved.
///
/// A five-value projection of `RelationshipKind` rather than the enum itself: the two
/// pending kinds are bookkeeping for a friendship in progress, not edges a user asked
/// for, and `Unknown` is a value from a newer build that this crate never writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EdgeKind {
    /// An accepted friendship.
    Friend,
    /// A one-directional follow.
    Follow,
    /// A block.
    Block,
    /// A personal mute.
    Mute,
    /// A favourite.
    Favorite,
}

impl EdgeKind {
    pub(crate) const ALL: [Self; 5] = [
        Self::Friend,
        Self::Follow,
        Self::Block,
        Self::Mute,
        Self::Favorite,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Friend => "friend",
            Self::Follow => "follow",
            Self::Block => "block",
            Self::Mute => "mute",
            Self::Favorite => "favorite",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// What a permission question answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GateOutcome {
    /// Allowed.
    Allowed,
    /// One of the two had blocked the other.
    Blocked,
    /// A visibility setting excluded the caller.
    Restricted,
    /// The subject has no profile, or no account.
    Unknown,
}

impl GateOutcome {
    pub(crate) const ALL: [Self; 4] = [
        Self::Allowed,
        Self::Blocked,
        Self::Restricted,
        Self::Unknown,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Blocked => "blocked",
            Self::Restricted => "restricted",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Every series this crate publishes.
pub(crate) struct Meters {
    requests: Vec<Arc<Counter>>,
    responses: Vec<Arc<Counter>>,
    added: Vec<Arc<Counter>>,
    removed: Vec<Arc<Counter>>,
    gates: Vec<Arc<Counter>>,
    suggested: Arc<Counter>,
    suggestion_scans: Arc<Counter>,
    searches: Arc<Counter>,
    search_hits: Arc<Counter>,
    profiles_asked: Arc<Counter>,
    profiles_served: Arc<Counter>,
}

impl Meters {
    /// Registers every series at zero.
    ///
    /// All of them, before anything happens, so a dashboard shows a flat line rather
    /// than a gap for an outcome nobody has hit yet. A panel that renders "no data"
    /// for "friend requests refused because of a block" is indistinguishable from a
    /// panel whose query is wrong, and the difference matters at three in the morning.
    pub(crate) fn new(registry: &Registry) -> Self {
        let requests = RequestOutcome::ALL
            .iter()
            .map(|outcome| {
                registry.counter(
                    "migo_social_friend_requests_total",
                    "Friend requests by outcome.",
                    &[("outcome", outcome.label())],
                )
            })
            .collect();
        let responses = ResponseOutcome::ALL
            .iter()
            .map(|outcome| {
                registry.counter(
                    "migo_social_friend_responses_total",
                    "Answers to friend requests by outcome.",
                    &[("outcome", outcome.label())],
                )
            })
            .collect();
        let added = EdgeKind::ALL
            .iter()
            .map(|kind| {
                registry.counter(
                    "migo_social_edges_added_total",
                    "Social graph edges created, by kind.",
                    &[("kind", kind.label())],
                )
            })
            .collect();
        let removed = EdgeKind::ALL
            .iter()
            .map(|kind| {
                registry.counter(
                    "migo_social_edges_removed_total",
                    "Social graph edges removed, by kind.",
                    &[("kind", kind.label())],
                )
            })
            .collect();
        let gates = GateOutcome::ALL
            .iter()
            .map(|outcome| {
                registry.counter(
                    "migo_social_interaction_checks_total",
                    "Privacy gate decisions by outcome.",
                    &[("outcome", outcome.label())],
                )
            })
            .collect();
        Self {
            requests,
            responses,
            added,
            removed,
            gates,
            suggested: registry.counter(
                "migo_social_suggestions_returned_total",
                "Accounts returned by the suggestion endpoint.",
                &[],
            ),
            suggestion_scans: registry.counter(
                "migo_social_suggestion_edges_scanned_total",
                "Friend edges read while building suggestions.",
                &[],
            ),
            searches: registry.counter(
                "migo_social_searches_total",
                "Account searches performed.",
                &[],
            ),
            search_hits: registry.counter(
                "migo_social_search_hits_total",
                "Accounts returned by search, after the caller's blocklist is applied.",
                &[],
            ),
            profiles_asked: registry.counter(
                "migo_social_profiles_requested_total",
                "Account ids asked for across all profile fetches, after deduplication.",
                &[],
            ),
            profiles_served: registry.counter(
                "migo_social_profiles_served_total",
                "Profiles returned, after blocks and missing accounts are omitted.",
                &[],
            ),
        }
    }

    pub(crate) fn request(&self, outcome: RequestOutcome) {
        if let Some(counter) = self.requests.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn response(&self, outcome: ResponseOutcome) {
        if let Some(counter) = self.responses.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn added(&self, kind: EdgeKind) {
        if let Some(counter) = self.added.get(kind.index()) {
            counter.inc();
        }
    }

    pub(crate) fn removed(&self, kind: EdgeKind) {
        if let Some(counter) = self.removed.get(kind.index()) {
            counter.inc();
        }
    }

    pub(crate) fn gate(&self, outcome: GateOutcome) {
        if let Some(counter) = self.gates.get(outcome.index()) {
            counter.inc();
        }
    }

    /// Records one suggestion round.
    ///
    /// Both numbers, because the ratio is the health signal: a deployment returning
    /// two suggestions per thousand edges scanned has a suggestion feature that costs
    /// more than it delivers, and neither number alone shows that.
    pub(crate) fn suggestions(&self, returned: usize, scanned: usize) {
        self.suggested.add(returned as u64);
        self.suggestion_scans.add(scanned as u64);
    }

    /// Records one search.
    pub(crate) fn search(&self, hits: usize) {
        self.searches.inc();
        self.search_hits.add(hits as u64);
    }

    /// Records one profile fetch.
    ///
    /// Both numbers, and the gap between them is the reason. A profile fetch omits what
    /// the caller may not see rather than refusing it, so the *only* signal that
    /// somebody is walking the id space asking for faces they will never get is the
    /// ratio of served to asked. Neither number alone shows it, and section 174 forbids
    /// labelling either by account, so an aggregate pair is exactly as much as this can
    /// honestly report.
    pub(crate) fn profiles(&self, asked: usize, served: usize) {
        self.profiles_asked.add(asked as u64);
        self.profiles_served.add(served as u64);
    }
}
