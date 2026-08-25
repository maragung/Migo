//! What the limiter reports.
//!
//! Four families, all counters. There is no gauge for "current bucket level" and no
//! histogram of remaining tokens, because both would need one series per subject and the
//! subject is an IP address or an account id — the registry's own cardinality cap
//! (`MAX_SERIES_PER_FAMILY`) would silently drop most of them, and the ones that survived
//! would be whichever arrived first. A per-subject question is answered by
//! [`crate::RateLimiter::peek`], on demand, for the one subject being asked about.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};

use crate::scope::Scope;

/// The counters, resolved once at construction.
///
/// Held as `Arc<Counter>` rather than looked up per increment: a registry lookup takes a
/// read lock and hashes a label set, and this is on the hottest path in the server —
/// every single request passes through it, several times.
pub(crate) struct Meters {
    checks: Arc<Counter>,
    rejections: Vec<Arc<Counter>>,
    degraded: Arc<Counter>,
    saturated: Arc<Counter>,
}

impl Meters {
    /// Registers every series, including the ones that have not happened.
    ///
    /// All seven rejection series are created at zero. A counter that springs into
    /// existence the first time it fires cannot be alerted on beforehand — `rate(...) >
    /// 0` on a series that does not exist yet does not fire, it errors — so the alert
    /// would have to be written after the first incident it was supposed to catch.
    pub(crate) fn new(registry: &Registry) -> Self {
        Self {
            checks: registry.counter(
                "migo_ratelimit_checks_total",
                "Rate limit decisions made.",
                &[],
            ),
            rejections: Scope::ALL
                .iter()
                .map(|scope| {
                    registry.counter(
                        "migo_ratelimit_rejections_total",
                        "Operations refused, by the surface that refused them.",
                        &[("scope", scope.label())],
                    )
                })
                .collect(),
            degraded: registry.counter(
                "migo_ratelimit_degraded_total",
                "Charges served from local buckets because the cache was unreachable.",
                &[],
            ),
            saturated: registry.counter(
                "migo_ratelimit_fallback_saturated_total",
                "Charges refused because the local fallback had no room to track them.",
                &[],
            ),
        }
    }

    pub(crate) fn check(&self) {
        self.checks.inc();
    }

    pub(crate) fn reject(&self, scope: Scope) {
        if let Some(counter) = self.rejections.get(scope.index()) {
            counter.inc();
        }
    }

    pub(crate) fn degrade(&self) {
        self.degraded.inc();
    }

    pub(crate) fn saturate(&self) {
        self.saturated.inc();
    }
}
