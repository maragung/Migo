//! What the media plane reports.
//!
//! # What is deliberately absent
//!
//! No series is labelled by account, device, call, or conversation, for the
//! same reason the call service's metrics are not (section 174): which
//! people were in which call is the most sensitive fact this plane holds,
//! and a metrics endpoint is a public window. The labels that remain are
//! closed vocabularies — outcomes, reasons, directions.
//!
//! No series measures a frame's payload either. Bytes forwarded in the
//! aggregate would be defensible; bytes per anything would be a size
//! profile of someone's media, and the plane's whole promise is that it
//! does not look at the contents it moves. Counting frames is the honest
//! ceiling: how much forwarding happened, how much was dropped, and why.

use std::sync::Arc;

use migo_core::metrics::{Counter, Gauge, Registry};

/// How a join ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JoinKind {
    /// Seated.
    Joined,
    /// The same seat again.
    Duplicate,
    /// The call was full; answered with the quota error.
    Quota,
}

impl JoinKind {
    const ALL: [Self; 3] = [Self::Joined, Self::Duplicate, Self::Quota];

    const fn label(self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Duplicate => "duplicate",
            Self::Quota => "quota",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// How a publish ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublishKind {
    /// A new stream, holding a video slot if it is video.
    Published,
    /// The same stream again.
    Republished,
    /// The call's video slots were full.
    Quota,
}

impl PublishKind {
    const ALL: [Self; 3] = [Self::Published, Self::Republished, Self::Quota];

    const fn label(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::Republished => "republished",
            Self::Quota => "quota",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// How a subscribe ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubscribeKind {
    /// Held, at the requested layer.
    Granted,
    /// The same subscription, moved to a new layer.
    Relayered,
    /// The participant holds their complement of subscriptions.
    Quota,
    /// The churn window was spent.
    RateLimited,
}

impl SubscribeKind {
    const ALL: [Self; 4] = [
        Self::Granted,
        Self::Relayered,
        Self::Quota,
        Self::RateLimited,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Relayered => "relayered",
            Self::Quota => "quota",
            Self::RateLimited => "rate_limited",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// Why a frame did not reach a subscriber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropReason {
    /// The subscriber's shape forwards another layer.
    Layer,
    /// The frame's sequence lost the stride's coin toss.
    Stride,
    /// No video is being forwarded to this subscriber at all.
    VideoOff,
}

impl DropReason {
    const ALL: [Self; 3] = [Self::Layer, Self::Stride, Self::VideoOff];

    const fn label(self) -> &'static str {
        match self {
            Self::Layer => "layer",
            Self::Stride => "stride",
            Self::VideoOff => "video_off",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// Which way an adaptation moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdaptKind {
    /// Down the ladder, immediately.
    Lowered,
    /// Up the ladder, one rung.
    Raised,
    /// The report changed nothing.
    Held,
}

impl AdaptKind {
    const ALL: [Self; 3] = [Self::Lowered, Self::Raised, Self::Held];

    const fn label(self) -> &'static str {
        match self {
            Self::Lowered => "lowered",
            Self::Raised => "raised",
            Self::Held => "held",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// The series, resolved once at construction.
pub(crate) struct Meters {
    joins: Vec<Arc<Counter>>,
    leaves: Arc<Counter>,
    publishes: Vec<Arc<Counter>>,
    unpublishes: Arc<Counter>,
    subscriptions: Vec<Arc<Counter>>,
    unsubscriptions: Arc<Counter>,
    forwarded: Arc<Counter>,
    dropped: Vec<Arc<Counter>>,
    adaptations: Vec<Arc<Counter>>,
    participants: Arc<Gauge>,
    active_video: Arc<Gauge>,
}

impl Meters {
    /// Registers every series, including the outcomes that have not happened
    /// yet.
    ///
    /// A counter that springs into existence on its first occurrence cannot
    /// be alerted on beforehand, so everything is created at zero — the same
    /// reasoning as every other crate's meters.
    pub(crate) fn new(registry: &Registry) -> Self {
        Self {
            joins: JoinKind::ALL
                .iter()
                .map(|kind| {
                    registry.counter(
                        "migo_sfu_joins_total",
                        "Group-call media joins, by outcome.",
                        &[("outcome", kind.label())],
                    )
                })
                .collect(),
            leaves: registry.counter(
                "migo_sfu_leaves_total",
                "Group-call media leaves, including the idempotent no-ops.",
                &[],
            ),
            publishes: PublishKind::ALL
                .iter()
                .map(|kind| {
                    registry.counter(
                        "migo_sfu_publishes_total",
                        "Stream publishes on the media plane, by outcome.",
                        &[("outcome", kind.label())],
                    )
                })
                .collect(),
            unpublishes: registry.counter(
                "migo_sfu_unpublishes_total",
                "Stream unpublishes on the media plane.",
                &[],
            ),
            subscriptions: SubscribeKind::ALL
                .iter()
                .map(|kind| {
                    registry.counter(
                        "migo_sfu_subscriptions_total",
                        "Stream subscription requests, by outcome.",
                        &[("outcome", kind.label())],
                    )
                })
                .collect(),
            unsubscriptions: registry.counter(
                "migo_sfu_unsubscriptions_total",
                "Stream unsubscription requests, including the idempotent no-ops.",
                &[],
            ),
            forwarded: registry.counter(
                "migo_sfu_frames_forwarded_total",
                "Sealed frames delivered to a subscriber. Never labelled by who.",
                &[],
            ),
            dropped: DropReason::ALL
                .iter()
                .map(|reason| {
                    registry.counter(
                        "migo_sfu_frames_dropped_total",
                        "Sealed frames not delivered to a would-be subscriber, by reason.",
                        &[("reason", reason.label())],
                    )
                })
                .collect(),
            adaptations: AdaptKind::ALL
                .iter()
                .map(|direction| {
                    registry.counter(
                        "migo_sfu_adaptations_total",
                        "Adaptive-quality decisions, by direction.",
                        &[("direction", direction.label())],
                    )
                })
                .collect(),
            participants: registry.gauge(
                "migo_sfu_participants",
                "Participants seated on the media plane, across all calls.",
                &[],
            ),
            active_video: registry.gauge(
                "migo_sfu_active_video_streams",
                "Video streams active on the media plane, across all calls.",
                &[],
            ),
        }
    }

    /// One join, by outcome.
    pub(crate) fn join(&self, kind: JoinKind) {
        self.joins[kind.index()].inc();
    }

    /// One leave.
    pub(crate) fn leave(&self) {
        self.leaves.inc();
    }

    /// One publish, by outcome.
    pub(crate) fn publish(&self, kind: PublishKind) {
        self.publishes[kind.index()].inc();
    }

    /// One unpublish.
    pub(crate) fn unpublish(&self) {
        self.unpublishes.inc();
    }

    /// One subscribe request, by outcome.
    pub(crate) fn subscribe(&self, kind: SubscribeKind) {
        self.subscriptions[kind.index()].inc();
    }

    /// One unsubscribe request.
    pub(crate) fn unsubscribe(&self) {
        self.unsubscriptions.inc();
    }

    /// Frames delivered.
    pub(crate) fn forwarded(&self, count: u64) {
        self.forwarded.add(count);
    }

    /// One frame not delivered, for a reason.
    pub(crate) fn dropped(&self, reason: DropReason) {
        self.dropped[reason.index()].inc();
    }

    /// One adaptation decision, by direction.
    pub(crate) fn adapt(&self, kind: AdaptKind) {
        self.adaptations[kind.index()].inc();
    }

    /// The plane's current load, recomputed after every mutation that can
    /// change it.
    pub(crate) fn set_load(&self, participants: i64, active_video: i64) {
        self.participants.set(participants);
        self.active_video.set(active_video);
    }
}
