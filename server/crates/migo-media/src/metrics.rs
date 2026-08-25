//! Counters for the media pipeline.
//!
//! # No account labels, no conversation labels, and no keys
//!
//! Brief section 174 forbids a metric series labelled by account, device, or
//! conversation. Media adds two labels of its own that would be just as bad and are
//! more tempting: the storage key, because it is right there in the function that
//! failed, and the signed URL, because a failing URL is the thing an engineer wants to
//! see. Section 69 settles the second one — *"Signed URL TIDAK BOLEH ditulis ke log, ke
//! analytics, atau ke crash report"* — and analytics is exactly what a metric is.
//!
//! So every series here is labelled by kind, by outcome, or by identified format, all
//! three of which are closed enums. The cardinality of this module is fixed at compile
//! time, and no series can grow with traffic.
//!
//! # Why byte counts are here and sizes are not
//!
//! `migo_media_bytes_committed_total` is a counter of bytes, which is a capacity signal
//! an operator needs and which says nothing about any one upload. A *histogram* of
//! object sizes would be a different thing: with a small enough population, a bucket
//! with one observation in it is one person's file, and the timestamps that come with a
//! scrape make it that person's file at that minute. Aggregate throughput answers the
//! capacity question without building that.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};

use crate::model::{MediaKind, Scan};
use crate::sniff::Identity;

/// Why an upload was refused.
///
/// One series per reason, because these are not interchangeable operationally: a spike
/// in `too_large` is a client that shipped a bad compression setting, a spike in
/// `ticket_invalid` is somebody probing the MAC, and a spike in `bytes_missing` is
/// object storage dropping writes. Collapsing them into one "refused" counter would
/// hide all three behind each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The limiter refused it.
    RateLimited,
    /// The declared size is over the ceiling for its kind.
    TooLarge,
    /// The caller may not upload to that destination, or it does not exist.
    Denied,
    /// A voice note longer than the configured ceiling.
    TooLong,
    /// The declared MIME type or size is malformed.
    Invalid,
    /// The ticket ran out.
    TicketExpired,
    /// The ticket did not verify, or was for another account or device.
    TicketInvalid,
    /// Storage holds fewer bytes than the client committed.
    BytesMissing,
    /// Storage holds a different number of bytes than the client committed.
    SizeMismatch,
    /// The leading bytes are not what the declared kind must contain.
    WrongContent,
    /// Object storage failed.
    Storage,
}

impl Refused {
    pub(crate) const ALL: [Self; 11] = [
        Self::RateLimited,
        Self::TooLarge,
        Self::Denied,
        Self::TooLong,
        Self::Invalid,
        Self::TicketExpired,
        Self::TicketInvalid,
        Self::BytesMissing,
        Self::SizeMismatch,
        Self::WrongContent,
        Self::Storage,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::TooLarge => "too_large",
            Self::Denied => "denied",
            Self::TooLong => "too_long",
            Self::Invalid => "invalid",
            Self::TicketExpired => "ticket_expired",
            Self::TicketInvalid => "ticket_invalid",
            Self::BytesMissing => "bytes_missing",
            Self::SizeMismatch => "size_mismatch",
            Self::WrongContent => "content_refused",
            Self::Storage => "storage",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// What a request for a download URL answered.
///
/// `Denied` and `Missing` are separate series even though a caller is told the same
/// thing for both — brief section 161 has an unauthorised caller told `NOT_FOUND` so
/// that existence does not leak. The rule constrains what the *caller* learns; the
/// operator needs to know whether a spike is people asking for objects that were
/// deleted or people asking for objects that belong to somebody else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Granted {
    /// A URL was issued.
    Issued,
    /// The caller is not a member of the object's conversation.
    Denied,
    /// No such object, or it is tombstoned.
    Missing,
    /// The object exists and the caller may have it, but it is not cleared yet.
    NotCleared,
    /// The limiter refused it.
    RateLimited,
    /// Object storage could not sign a URL.
    Storage,
}

impl Granted {
    pub(crate) const ALL: [Self; 6] = [
        Self::Issued,
        Self::Denied,
        Self::Missing,
        Self::NotCleared,
        Self::RateLimited,
        Self::Storage,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Issued => "issued",
            Self::Denied => "denied",
            Self::Missing => "missing",
            Self::NotCleared => "not_cleared",
            Self::RateLimited => "rate_limited",
            Self::Storage => "storage",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Every series this crate publishes.
pub(crate) struct Meters {
    begun: Vec<Arc<Counter>>,
    committed: Vec<Arc<Counter>>,
    bytes: Vec<Arc<Counter>>,
    refused: Vec<Arc<Counter>>,
    grants: Vec<Arc<Counter>>,
    scans: Vec<Arc<Counter>>,
    formats: Vec<Arc<Counter>>,
    unidentified: Arc<Counter>,
    deleted: Arc<Counter>,
    aborted: Arc<Counter>,
}

impl Meters {
    /// Registers every series at zero.
    ///
    /// All of them, before anything happens. A dashboard panel that renders "no data"
    /// for "uploads refused because the ticket did not verify" is indistinguishable
    /// from a panel whose query is wrong, and the difference matters when somebody is
    /// deciding at three in the morning whether the MAC key rotated correctly.
    pub(crate) fn new(registry: &Registry) -> Self {
        let begun = MediaKind::ALL
            .iter()
            .map(|kind| {
                registry.counter(
                    "migo_media_uploads_begun_total",
                    "Upload tickets issued, by kind.",
                    &[("kind", kind.label())],
                )
            })
            .collect();
        let committed = MediaKind::ALL
            .iter()
            .map(|kind| {
                registry.counter(
                    "migo_media_uploads_committed_total",
                    "Uploads that became objects, by kind.",
                    &[("kind", kind.label())],
                )
            })
            .collect();
        let bytes = MediaKind::ALL
            .iter()
            .map(|kind| {
                registry.counter(
                    "migo_media_bytes_committed_total",
                    "Bytes accepted into object storage, by kind.",
                    &[("kind", kind.label())],
                )
            })
            .collect();
        let refused = Refused::ALL
            .iter()
            .map(|reason| {
                registry.counter(
                    "migo_media_upload_refusals_total",
                    "Uploads refused, by reason.",
                    &[("reason", reason.label())],
                )
            })
            .collect();
        let grants = Granted::ALL
            .iter()
            .map(|outcome| {
                registry.counter(
                    "migo_media_url_grants_total",
                    "Requests for a download URL, by outcome.",
                    &[("outcome", outcome.label())],
                )
            })
            .collect();
        let scans = Scan::ALL
            .iter()
            .map(|status| {
                registry.counter(
                    "migo_media_scan_results_total",
                    "Scan verdicts recorded, by resulting status.",
                    &[("status", status.label())],
                )
            })
            .collect();
        let formats = FORMATS
            .iter()
            .map(|identity| {
                registry.counter(
                    "migo_media_content_identified_total",
                    "Objects whose leading bytes named a format, by format.",
                    &[("format", identity.label())],
                )
            })
            .collect();
        Self {
            begun,
            committed,
            bytes,
            refused,
            grants,
            scans,
            formats,
            unidentified: registry.counter(
                "migo_media_content_unidentified_total",
                "Objects committed with the MIME type the client declared, because the bytes named no format and the kind allows that.",
                &[],
            ),
            deleted: registry.counter(
                "migo_media_objects_deleted_total",
                "Objects tombstoned at their owner's request.",
                &[],
            ),
            aborted: registry.counter(
                "migo_media_uploads_aborted_total",
                "Upload tickets abandoned by the client.",
                &[],
            ),
        }
    }

    pub(crate) fn begun(&self, kind: MediaKind) {
        if let Some(counter) = self.begun.get(kind.index()) {
            counter.inc();
        }
    }

    pub(crate) fn committed(&self, kind: MediaKind, byte_size: u64) {
        if let Some(counter) = self.committed.get(kind.index()) {
            counter.inc();
        }
        if let Some(counter) = self.bytes.get(kind.index()) {
            counter.add(byte_size);
        }
    }

    pub(crate) fn refused(&self, reason: Refused) {
        if let Some(counter) = self.refused.get(reason.index()) {
            counter.inc();
        }
    }

    pub(crate) fn granted(&self, outcome: Granted) {
        if let Some(counter) = self.grants.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn scanned(&self, status: Scan) {
        if let Some(counter) = self.scans.get(status.index()) {
            counter.inc();
        }
    }

    /// Records what the sniffer decided about one object's leading bytes.
    pub(crate) fn identified(&self, identity: Option<Identity>) {
        match identity {
            Some(identity) => {
                if let Some(index) = FORMATS.iter().position(|known| *known == identity) {
                    if let Some(counter) = self.formats.get(index) {
                        counter.inc();
                    }
                }
            }
            None => self.unidentified.inc(),
        }
    }

    pub(crate) fn deleted(&self) {
        self.deleted.inc();
    }

    pub(crate) fn aborted(&self) {
        self.aborted.inc();
    }
}

/// Every format the sniffer can name, in the order their series are registered.
///
/// A `const` array rather than an `Identity::ALL` on the enum itself: the sniffer's
/// list is a private implementation detail of content validation, and publishing it as
/// part of the enum's API would invite a caller to switch on it exhaustively and then
/// break when a format is added.
const FORMATS: [Identity; 13] = [
    Identity::Png,
    Identity::Jpeg,
    Identity::Webp,
    Identity::Gif,
    Identity::Avif,
    Identity::OggAudio,
    Identity::MpegAudio,
    Identity::Mp4Audio,
    Identity::Wav,
    Identity::Mp4Video,
    Identity::Webm,
    Identity::Mov,
    Identity::Pdf,
];
