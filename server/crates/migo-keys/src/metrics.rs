//! Series this crate publishes.
//!
//! Every label here is a closed enum. Section 174 keeps a metric from being labelled by
//! account, device, or conversation, and key material is the most sensitive thing in the
//! system to build a time series about: a counter keyed by device id would say which
//! devices are being messaged and how often, which is the social graph written in
//! cardinality.
//!
//! The one number worth watching is the ratio of exhausted bundles to bundles served. It
//! rising means client top-up is not keeping pace, and every exhausted bundle is a first
//! message that formed a session without a one-time prekey. Nothing in a log will show
//! that, because nothing in a log is allowed to name the device it happened to.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};

/// Why a publication was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublishRejection {
    /// A length or a count was structurally wrong.
    Malformed,
    /// The identity key was not a valid pair of points.
    BadIdentity,
    /// The signed prekey's signature did not verify.
    BadSignature,
    /// A one-time prekey was not a valid X25519 public key.
    BadPrekey,
    /// The signed prekey was already expired when it was published.
    Expired,
}

impl PublishRejection {
    pub(crate) const ALL: [Self; 5] = [
        Self::Malformed,
        Self::BadIdentity,
        Self::BadSignature,
        Self::BadPrekey,
        Self::Expired,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::BadIdentity => "bad_identity",
            Self::BadSignature => "bad_signature",
            Self::BadPrekey => "bad_prekey",
            Self::Expired => "expired",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Every series this crate publishes.
pub(crate) struct Meters {
    published: Arc<Counter>,
    prekeys_accepted: Arc<Counter>,
    prekeys_skipped: Arc<Counter>,
    rejections: Vec<Arc<Counter>>,
    bundles_served: Arc<Counter>,
    bundles_exhausted: Arc<Counter>,
    fetches_refused: Arc<Counter>,
}

impl Meters {
    /// Registers every series at zero.
    ///
    /// Including the rejection counters, which are the ones most likely to be looked at
    /// during an incident and least likely to have been hit before it. A panel that reads
    /// "no data" for "publications refused for a bad signature" cannot be told apart from
    /// a panel whose query is wrong.
    pub(crate) fn new(registry: &Registry) -> Self {
        let rejections = PublishRejection::ALL
            .iter()
            .map(|reason| {
                registry.counter(
                    "migo_keys_publish_rejected_total",
                    "Key publications refused, by reason.",
                    &[("reason", reason.label())],
                )
            })
            .collect();
        Self {
            published: registry.counter(
                "migo_keys_published_total",
                "Key publications accepted.",
                &[],
            ),
            prekeys_accepted: registry.counter(
                "migo_keys_one_time_prekeys_accepted_total",
                "One-time prekeys stored.",
                &[],
            ),
            prekeys_skipped: registry.counter(
                "migo_keys_one_time_prekeys_skipped_total",
                "One-time prekeys refused because the key id was already published.",
                &[],
            ),
            rejections,
            bundles_served: registry.counter(
                "migo_keys_bundles_served_total",
                "Key bundles returned to senders.",
                &[],
            ),
            bundles_exhausted: registry.counter(
                "migo_keys_bundles_without_one_time_prekey_total",
                "Bundles served without a one-time prekey, weakening the first message.",
                &[],
            ),
            fetches_refused: registry.counter(
                "migo_keys_fetches_refused_exhausted_total",
                "Bundle fetches refused because a device had no one-time prekey left.",
                &[],
            ),
        }
    }

    /// Records one accepted publication.
    pub(crate) fn published(&self, accepted: u32, skipped: u32) {
        self.published.inc();
        self.prekeys_accepted.add(u64::from(accepted));
        self.prekeys_skipped.add(u64::from(skipped));
    }

    /// Records one refused publication.
    pub(crate) fn rejected(&self, reason: PublishRejection) {
        if let Some(counter) = self.rejections.get(reason.index()) {
            counter.inc();
        }
    }

    /// Records one fetch.
    ///
    /// Both numbers, because the ratio is the signal. See the module docs.
    pub(crate) fn served(&self, bundles: usize, exhausted: usize) {
        self.bundles_served.add(bundles as u64);
        self.bundles_exhausted.add(exhausted as u64);
    }

    /// Records a fetch refused by policy.
    pub(crate) fn refused(&self) {
        self.fetches_refused.inc();
    }
}
