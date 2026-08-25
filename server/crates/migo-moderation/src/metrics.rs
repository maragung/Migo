//! Counters for moderation.
//!
//! # No account labels, and no reporter labels
//!
//! Brief section 174 forbids a metric series labelled by account, device, or
//! conversation. Moderation is where that rule is easiest to break with good intentions:
//! "reports filed per reporter" and "actions taken per operator" are both things an
//! operations team genuinely wants, and both are a named list of people published on an
//! unauthenticated endpoint.
//!
//! So every series here is labelled by a closed enum, and the cardinality of this whole
//! module is fixed at compile time. Who did what is in the audit table, behind
//! `Powers::AUDIT`, which is where a question about a person belongs.
//!
//! # Why a report reason *is* a label, when a ban reason is not
//!
//! `migo_rooms` refuses to label any series by ban reason, and this module labels reports
//! by [`Reason`](crate::model::Reason). The difference is not a relaxation.
//!
//! A room ban reason is free text an operator typed. Its cardinality is unbounded, it
//! contains whatever somebody wrote — including, on a bad day, an account name or a quote
//! from the thing being banned for — and a metric label is a string that gets scraped,
//! stored, and indexed for as long as the time series lives.
//!
//! A report reason is one of thirteen numbers defined in this crate. Aggregated across a
//! deployment it says "spam reports tripled this week", which is exactly what a metric is
//! for and says nothing whatever about a person.
//!
//! # Why the abuse score is not a histogram
//!
//! Because a score is computed per account, and a histogram bucket with one observation
//! in it is one person's score. The level is counted instead — four values, each of which
//! needs thousands of accounts before it means anything.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};

use crate::model::{Reason, Resolution, Risk};

/// The seven actions, in the order [`Action::index`](crate::model::Action::index) gives.
const ACTION_LABELS: [&str; 7] = [
    "warn",
    "suspend",
    "reinstate",
    "remove_message",
    "remove_media",
    "archive_room",
    "disable_bot",
];

/// The five report subjects, in `report.subject_kind` order.
const SUBJECT_LABELS: [&str; 5] = ["user", "message", "room", "media", "bot"];

/// Why a call into this crate was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The limiter refused it.
    RateLimited,
    /// Malformed, self-addressed, or aimed at nothing.
    Invalid,
    /// The caller is not staff, or is staff without this power.
    Denied,
    /// A staff session that had not proved a factor recently.
    Stale,
    /// The report, account, room, or object is not there.
    Missing,
    /// The report was already resolved.
    Settled,
}

impl Refused {
    pub(crate) const ALL: [Self; 6] = [
        Self::RateLimited,
        Self::Invalid,
        Self::Denied,
        Self::Stale,
        Self::Missing,
        Self::Settled,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::Invalid => "invalid",
            Self::Denied => "denied",
            Self::Stale => "reauth_required",
            Self::Missing => "missing",
            Self::Settled => "already_resolved",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Every series this crate publishes.
pub(crate) struct Meters {
    filed: Vec<Arc<Counter>>,
    reasons: Vec<Arc<Counter>>,
    duplicates: Arc<Counter>,
    resolved: Vec<Arc<Counter>>,
    actions: Vec<Arc<Counter>>,
    refused: Vec<Arc<Counter>>,
    assessed: Vec<Arc<Counter>>,
    auto_suspended: Arc<Counter>,
    queue_reads: Arc<Counter>,
    queue_items: Arc<Counter>,
    audit_reads: Arc<Counter>,
    audit_written: Arc<Counter>,
}

impl Meters {
    /// Registers every series at zero.
    ///
    /// All of them, before anything happens, so a dashboard shows a flat line rather than
    /// a gap for an outcome nobody has hit yet. A panel reading "no data" for "reports
    /// about child safety" is indistinguishable from a panel whose query is wrong, and
    /// that is not a distinction to be discovering during an incident.
    pub(crate) fn new(registry: &Registry) -> Self {
        let filed = SUBJECT_LABELS
            .iter()
            .map(|subject| {
                registry.counter(
                    "migo_moderation_reports_filed_total",
                    "Reports filed, by what they are about.",
                    &[("subject", subject)],
                )
            })
            .collect();
        let reasons = Reason::ALL
            .iter()
            .map(|reason| {
                registry.counter(
                    "migo_moderation_reports_by_reason_total",
                    "Reports filed, by reason code.",
                    &[("reason", reason.label())],
                )
            })
            .collect();
        let resolved = Resolution::ALL
            .iter()
            .map(|resolution| {
                registry.counter(
                    "migo_moderation_reports_resolved_total",
                    "Reports closed, by what they came to.",
                    &[("resolution", resolution.label())],
                )
            })
            .collect();
        let actions = ACTION_LABELS
            .iter()
            .map(|action| {
                registry.counter(
                    "migo_moderation_actions_total",
                    "Moderator actions taken, by kind.",
                    &[("action", action)],
                )
            })
            .collect();
        let refused = Refused::ALL
            .iter()
            .map(|reason| {
                registry.counter(
                    "migo_moderation_refusals_total",
                    "Calls into moderation that were refused, by reason.",
                    &[("reason", reason.label())],
                )
            })
            .collect();
        let assessed = Risk::ALL
            .iter()
            .map(|risk| {
                registry.counter(
                    "migo_moderation_assessments_total",
                    "Abuse assessments, by resulting level.",
                    &[("risk", risk.label())],
                )
            })
            .collect();
        Self {
            filed,
            reasons,
            duplicates: registry.counter(
                "migo_moderation_reports_duplicate_total",
                "Reports that repeated one the same reporter already had open.",
                &[],
            ),
            resolved,
            actions,
            refused,
            assessed,
            auto_suspended: registry.counter(
                "migo_moderation_auto_suspensions_total",
                "Accounts suspended by the scorer rather than by a person.",
                &[],
            ),
            queue_reads: registry.counter(
                "migo_moderation_queue_reads_total",
                "Reads of the report queue.",
                &[],
            ),
            queue_items: registry.counter(
                "migo_moderation_queue_items_returned_total",
                "Reports handed to a triager.",
                &[],
            ),
            audit_reads: registry.counter(
                "migo_moderation_audit_reads_total",
                "Reads of the audit trail.",
                &[],
            ),
            audit_written: registry.counter(
                "migo_moderation_audit_entries_total",
                "Audit entries appended by this crate.",
                &[],
            ),
        }
    }

    pub(crate) fn filed(&self, subject_kind: i16, reason: Reason) {
        if let Ok(index) = usize::try_from(subject_kind) {
            if let Some(counter) = self.filed.get(index) {
                counter.inc();
            }
        }
        if let Some(counter) = self.reasons.get(reason.index()) {
            counter.inc();
        }
    }

    pub(crate) fn duplicate(&self) {
        self.duplicates.inc();
    }

    pub(crate) fn resolved(&self, resolution: Resolution) {
        if let Some(counter) = self.resolved.get(resolution.index()) {
            counter.inc();
        }
    }

    pub(crate) fn acted(&self, index: usize) {
        if let Some(counter) = self.actions.get(index) {
            counter.inc();
        }
    }

    pub(crate) fn refused(&self, reason: Refused) {
        if let Some(counter) = self.refused.get(reason.index()) {
            counter.inc();
        }
    }

    pub(crate) fn assessed(&self, risk: Risk) {
        if let Some(counter) = self.assessed.get(risk.index()) {
            counter.inc();
        }
    }

    pub(crate) fn auto_suspended(&self) {
        self.auto_suspended.inc();
    }

    /// Records one queue read.
    ///
    /// Both numbers, because the ratio is the signal: a queue read that returns a full
    /// page every time is a queue that is not being kept up with, and neither counter
    /// alone shows that.
    pub(crate) fn queue_read(&self, returned: usize) {
        self.queue_reads.inc();
        self.queue_items.add(returned as u64);
    }

    pub(crate) fn audit_read(&self) {
        self.audit_reads.inc();
    }

    pub(crate) fn audit_written(&self) {
        self.audit_written.inc();
    }
}
