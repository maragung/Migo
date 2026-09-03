//! What messaging reports.
//!
//! # What is deliberately absent
//!
//! No series here is labelled by account, conversation, or device. A counter
//! labelled by conversation is a directory of who is talking to whom, exported to
//! whatever scrapes the metrics endpoint — which is the social graph, and this is
//! a product whose whole premise is that the server cannot read the messages. A
//! label that reconstructs the envelope from the outside is not an improvement on
//! reading the contents.
//!
//! No series counts bytes of an envelope either. Ciphertext length is a side
//! channel: a histogram of message sizes per conversation distinguishes a "yes"
//! from a paragraph, and aggregated over a two-party conversation that is a
//! transcript at low resolution.
//!
//! What is left is the shape of the traffic — how many sends, how many were
//! retries, how many syncs had to report a hole — which is what an operator
//! actually pages on.

use std::sync::Arc;

use migo_core::metrics::{Counter, Histogram, Registry};

/// Buckets for "how many messages did one sync return".
///
/// Linear at the bottom and coarse at the top, because the interesting questions
/// live at both ends and nowhere in between: near zero it separates "a client
/// polling for nothing" from "a client that missed one message", and at the top it
/// separates a normal catch-up from one that keeps hitting the page cap and is
/// therefore a client stuck in a loop it cannot page out of.
const SYNC_BUCKETS: &[f64] = &[0.0, 1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 200.0];

/// How a send ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SendOutcome {
    /// Appended, sequenced, and ready to fan out.
    Accepted,
    /// The same `message_id` had already been accepted. Success, not an error.
    Duplicate,
    /// Same `message_id`, different payload.
    Mismatch,
    /// No such conversation, or the caller is not in it. One outcome, because
    /// the caller cannot tell the two apart and neither should this counter
    /// pretend to more certainty than the response carries.
    Unknown,
    /// One of the two parties has blocked the other.
    Blocked,
    /// The conversation is archived and takes no more messages.
    Archived,
    /// The caller is muted in this group. Speech, not citizenship: they keep
    /// their vote, and the moment the mute expires passes on its own.
    Muted,
    /// Refused on shape before anything was read.
    Invalid,
    /// Refused by the rate limiter.
    RateLimited,
}

impl SendOutcome {
    const ALL: [Self; 9] = [
        Self::Accepted,
        Self::Duplicate,
        Self::Mismatch,
        Self::Unknown,
        Self::Blocked,
        Self::Archived,
        Self::Muted,
        Self::Invalid,
        Self::RateLimited,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
            Self::Mismatch => "idempotency_mismatch",
            Self::Unknown => "unknown_conversation",
            Self::Blocked => "blocked",
            Self::Archived => "archived",
            Self::Muted => "muted",
            Self::Invalid => "invalid",
            Self::RateLimited => "rate_limited",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Accepted => 0,
            Self::Duplicate => 1,
            Self::Mismatch => 2,
            Self::Unknown => 3,
            Self::Blocked => 4,
            Self::Archived => 5,
            Self::Muted => 6,
            Self::Invalid => 7,
            Self::RateLimited => 8,
        }
    }
}

/// How a sync ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    /// The client is now contiguous with the server.
    Complete,
    /// A full page came back and there is more behind it.
    More,
    /// Messages the client asked for are gone, and it was told so.
    Truncated,
}

impl SyncOutcome {
    const ALL: [Self; 3] = [Self::Complete, Self::More, Self::Truncated];

    const fn label(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::More => "more",
            Self::Truncated => "truncated",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Complete => 0,
            Self::More => 1,
            Self::Truncated => 2,
        }
    }
}

/// The series, resolved once at construction.
pub(crate) struct Meters {
    send: Vec<Arc<Counter>>,
    sync: Vec<Arc<Counter>>,
    sync_messages: Arc<Histogram>,
    receipts: Arc<Counter>,
    receipts_ignored: Arc<Counter>,
    deletes: Arc<Counter>,
    edits: Arc<Counter>,
    typing: Arc<Counter>,
    conversations_created: Arc<Counter>,
    conversations_listed: Arc<Counter>,
    expired: Arc<Counter>,
}

impl Meters {
    /// Registers every series, including the outcomes that have not happened yet.
    ///
    /// A counter that springs into existence on its first occurrence cannot be
    /// alerted on beforehand: `rate(migo_messaging_send_total{outcome="idempotency_mismatch"}[5m]) > 0`
    /// against a series that does not exist does not evaluate to false, it fails
    /// to evaluate — so the alert could only be written after the first incident
    /// it was supposed to catch. Everything is created at zero for that reason.
    pub(crate) fn new(registry: &Registry) -> Self {
        Self {
            send: SendOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_messaging_send_total",
                        "Message sends, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            sync: SyncOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_messaging_sync_total",
                        "Sync requests, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            sync_messages: registry.histogram(
                "migo_messaging_sync_messages",
                "Messages returned by one sync request.",
                &[],
                SYNC_BUCKETS,
            ),
            receipts: registry.counter(
                "migo_messaging_receipts_total",
                "Delivery and read receipts that moved a cursor forward.",
                &[],
            ),
            receipts_ignored: registry.counter(
                "migo_messaging_receipts_ignored_total",
                "Receipts that moved nothing, and so produced no frame.",
                &[],
            ),
            deletes: registry.counter(
                "migo_messaging_deletes_total",
                "Messages tombstoned for everyone.",
                &[],
            ),
            edits: registry.counter(
                "migo_messaging_edits_total",
                "Messages edited in place.",
                &[],
            ),
            typing: registry.counter(
                "migo_messaging_typing_total",
                "Typing marks set or cleared.",
                &[],
            ),
            conversations_created: registry.counter(
                "migo_messaging_conversations_created_total",
                "Conversations created, direct and group.",
                &[],
            ),
            conversations_listed: registry.counter(
                "migo_messaging_conversation_pages_total",
                "Pages of the conversation list served.",
                &[],
            ),
            expired: registry.counter(
                "migo_messaging_expired_total",
                "Disappearing messages deleted after their deadline.",
                &[],
            ),
        }
    }

    pub(crate) fn send(&self, outcome: SendOutcome) {
        if let Some(counter) = self.send.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn synced(&self, outcome: SyncOutcome, messages: usize) {
        if let Some(counter) = self.sync.get(outcome.index()) {
            counter.inc();
        }
        // `as f64` on a count that the page cap holds under 201.
        self.sync_messages.observe(messages as f64);
    }

    pub(crate) fn receipt(&self, moved: bool) {
        if moved {
            self.receipts.inc();
        } else {
            self.receipts_ignored.inc();
        }
    }

    pub(crate) fn deleted(&self) {
        self.deletes.inc();
    }

    pub(crate) fn edited(&self) {
        self.edits.inc();
    }

    pub(crate) fn typed(&self) {
        self.typing.inc();
    }

    pub(crate) fn conversation_created(&self) {
        self.conversations_created.inc();
    }

    pub(crate) fn conversations_listed(&self) {
        self.conversations_listed.inc();
    }

    pub(crate) fn expired(&self, count: u64) {
        self.expired.add(count);
    }
}
