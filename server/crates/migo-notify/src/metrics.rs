//! Counters for notifications and wake-ups.
//!
//! # No account labels, and no device labels
//!
//! Brief section 174 forbids a metric series labelled by account, device, or
//! conversation. In this crate the temptation is the device one, because "which
//! handset is failing to receive pushes" is a real support question — and a series
//! answering it would be a per-device attendance register, updated every time anybody
//! sent anybody anything. It would also be unbounded: one series per device in the
//! deployment, forever, including the devices that were thrown away.
//!
//! There is no series labelled by push token or by its hash either. A hash is a stable
//! per-device identifier, so a series keyed on one is the same register with an extra
//! step, and section 77's rule that the token never reaches a log is not satisfied by
//! sending a fingerprint of it to a metrics endpoint instead.
//!
//! So every series is labelled by kind, by outcome, or by nothing. Both are closed
//! enumerations, and the cardinality of this module is fixed at compile time.
//!
//! # Why withheld and failed are separate counters
//!
//! A wake-up that was not sent because the person is already looking at the app is the
//! system working. A wake-up that was not sent because the provider rejected the token
//! is the system broken. Folding them into one "not delivered" series produces a
//! dashboard where a healthy deployment and a deployment with expired FCM credentials
//! look identical, which is worse than having no dashboard.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};
use migo_protocol::NotificationKind;

use crate::model::{Failure, Withheld};

/// Every notification kind, in wire order.
///
/// Written out rather than derived, because a generated enum has no iterator and an
/// `ALL` here is what makes "every series registered at zero" possible. A variant added
/// to the protocol and forgotten here loses its series, which
/// [`kind_index`] turns into a silently dropped count — so the
/// exhaustive match in that function is what fails the build instead.
const KINDS: [NotificationKind; 15] = [
    NotificationKind::Unknown,
    NotificationKind::Message,
    NotificationKind::Mention,
    NotificationKind::Reply,
    NotificationKind::FriendRequest,
    NotificationKind::Gift,
    NotificationKind::LevelUp,
    NotificationKind::Achievement,
    NotificationKind::RoomInvite,
    NotificationKind::RoomAnnouncement,
    NotificationKind::Event,
    NotificationKind::GameChallenge,
    NotificationKind::VoiceNote,
    NotificationKind::MissedCall,
    NotificationKind::IncomingCall,
];

/// A kind's position in [`KINDS`].
///
/// An exhaustive match rather than a search, so that adding a variant to the protocol
/// and not to [`KINDS`] is a compile error here rather than a missing metric in
/// production.
const fn kind_index(kind: NotificationKind) -> usize {
    match kind {
        NotificationKind::Unknown => 0,
        NotificationKind::Message => 1,
        NotificationKind::Mention => 2,
        NotificationKind::Reply => 3,
        NotificationKind::FriendRequest => 4,
        NotificationKind::Gift => 5,
        NotificationKind::LevelUp => 6,
        NotificationKind::Achievement => 7,
        NotificationKind::RoomInvite => 8,
        NotificationKind::RoomAnnouncement => 9,
        NotificationKind::Event => 10,
        NotificationKind::GameChallenge => 11,
        NotificationKind::VoiceNote => 12,
        NotificationKind::MissedCall => 13,
        NotificationKind::IncomingCall => 14,
    }
}

/// A kind's label for a metric series.
const fn kind_label(kind: NotificationKind) -> &'static str {
    match kind {
        NotificationKind::Unknown => "unknown",
        NotificationKind::Message => "message",
        NotificationKind::Mention => "mention",
        NotificationKind::Reply => "reply",
        NotificationKind::FriendRequest => "friend_request",
        NotificationKind::Gift => "gift",
        NotificationKind::LevelUp => "level_up",
        NotificationKind::Achievement => "achievement",
        NotificationKind::RoomInvite => "room_invite",
        NotificationKind::RoomAnnouncement => "room_announcement",
        NotificationKind::Event => "event",
        NotificationKind::GameChallenge => "game_challenge",
        NotificationKind::VoiceNote => "voice_note",
        NotificationKind::MissedCall => "missed_call",
        NotificationKind::IncomingCall => "incoming_call",
    }
}

/// What happened to a registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegistrationOutcome {
    /// Recorded.
    Registered,
    /// Removed at the device's request.
    Unregistered,
    /// Removed because a provider said the token was dead.
    Retired,
    /// Refused: empty, over length, or from a device that is gone.
    Rejected,
}

impl RegistrationOutcome {
    pub(crate) const ALL: [Self; 4] = [
        Self::Registered,
        Self::Unregistered,
        Self::Retired,
        Self::Rejected,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Unregistered => "unregistered",
            Self::Retired => "retired",
            Self::Rejected => "rejected",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Every series this crate publishes.
pub(crate) struct Meters {
    events: Vec<Arc<Counter>>,
    stored: Vec<Arc<Counter>>,
    dropped: Arc<Counter>,
    woken: Vec<Arc<Counter>>,
    withheld: Vec<Arc<Counter>>,
    failed: Vec<Arc<Counter>>,
    registrations: Vec<Arc<Counter>>,
    inbox_reads: Arc<Counter>,
    badge_reads: Arc<Counter>,
    acknowledged: Arc<Counter>,
    swept: Arc<Counter>,
}

impl Meters {
    /// Registers every series at zero.
    ///
    /// All of them, before anything happens, so a dashboard shows a flat line rather
    /// than a gap for an outcome nobody has hit yet. A panel reading "no data" for
    /// "pushes refused because the token was dead" is indistinguishable from a panel
    /// whose query is wrong, and the difference matters during an incident.
    pub(crate) fn new(registry: &Registry) -> Self {
        let events = KINDS
            .iter()
            .map(|kind| {
                registry.counter(
                    "migo_notify_events_total",
                    "Notification events accepted, by kind.",
                    &[("kind", kind_label(*kind))],
                )
            })
            .collect();
        let stored = KINDS
            .iter()
            .map(|kind| {
                registry.counter(
                    "migo_notify_stored_total",
                    "Notification events written to the inbox, by kind.",
                    &[("kind", kind_label(*kind))],
                )
            })
            .collect();
        let woken = KINDS
            .iter()
            .map(|kind| {
                registry.counter(
                    "migo_notify_wakeups_sent_total",
                    "Devices woken by push, by kind.",
                    &[("kind", kind_label(*kind))],
                )
            })
            .collect();
        let withheld = [
            Withheld::Connected,
            Withheld::Coalesced,
            Withheld::Budget,
            Withheld::Stale,
        ]
        .iter()
        .map(|reason| {
            registry.counter(
                "migo_notify_wakeups_withheld_total",
                "Devices deliberately not woken, by reason.",
                &[("reason", reason.label())],
            )
        })
        .collect();
        let failed = [Failure::Unregistered, Failure::Throttled, Failure::Error]
            .iter()
            .map(|failure| {
                registry.counter(
                    "migo_notify_wakeups_failed_total",
                    "Wake-up attempts that failed, by reason.",
                    &[("reason", failure.label())],
                )
            })
            .collect();
        let registrations = RegistrationOutcome::ALL
            .iter()
            .map(|outcome| {
                registry.counter(
                    "migo_notify_registrations_total",
                    "Push registration changes, by outcome.",
                    &[("outcome", outcome.label())],
                )
            })
            .collect();
        Self {
            events,
            stored,
            dropped: registry.counter(
                "migo_notify_events_dropped_total",
                "Events discarded before delivery: the recipient caused them, or the kind was unknown.",
                &[],
            ),
            woken,
            withheld,
            failed,
            registrations,
            inbox_reads: registry.counter(
                "migo_notify_inbox_reads_total",
                "Inbox pages served.",
                &[],
            ),
            badge_reads: registry.counter(
                "migo_notify_badge_reads_total",
                "Badge counts served.",
                &[],
            ),
            acknowledged: registry.counter(
                "migo_notify_acknowledged_total",
                "Notifications marked read.",
                &[],
            ),
            swept: registry.counter(
                "migo_notify_swept_total",
                "Read notifications deleted by the retention sweep.",
                &[],
            ),
        }
    }

    pub(crate) fn event(&self, kind: NotificationKind) {
        if let Some(counter) = self.events.get(kind_index(kind)) {
            counter.inc();
        }
    }

    pub(crate) fn stored(&self, kind: NotificationKind) {
        if let Some(counter) = self.stored.get(kind_index(kind)) {
            counter.inc();
        }
    }

    pub(crate) fn dropped(&self) {
        self.dropped.inc();
    }

    pub(crate) fn woken(&self, kind: NotificationKind) {
        if let Some(counter) = self.woken.get(kind_index(kind)) {
            counter.inc();
        }
    }

    pub(crate) fn withheld(&self, reason: Withheld) {
        if let Some(counter) = self.withheld.get(reason as usize) {
            counter.inc();
        }
    }

    pub(crate) fn failed(&self, failure: Failure) {
        if let Some(counter) = self.failed.get(failure as usize) {
            counter.inc();
        }
    }

    pub(crate) fn registration(&self, outcome: RegistrationOutcome) {
        if let Some(counter) = self.registrations.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn inbox_read(&self) {
        self.inbox_reads.inc();
    }

    pub(crate) fn badge_read(&self) {
        self.badge_reads.inc();
    }

    pub(crate) fn acknowledged(&self, count: u32) {
        self.acknowledged.add(u64::from(count));
    }

    pub(crate) fn swept(&self, count: u64) {
        self.swept.add(count);
    }
}
