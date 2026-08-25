//! Counters for the bot lifecycle: registered, authenticated, tokens rotated, scopes
//! changed, disabled and re-enabled — plus why an authentication was refused.
//!
//! # What may label a series here, and what may never
//!
//! Brief section 174 forbids a metric series labelled by account; this crate adds that none
//! is labelled by bot or by owner either. A counter keyed on a bot id would let a dashboard
//! rebuild which bots are busiest and who runs them straight off the metrics endpoint, which
//! is exactly the shape section 174 keeps out of it. So every series here is either unlabelled
//! or labelled by a closed enum — the one authentication-rejection reason — whose cardinality
//! is fixed at compile time and whose growth is a diff a reviewer sees.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};

/// Why an authentication was refused.
///
/// Both refusals hand the caller the same opaque error (brief section 161 — a valid-token
/// oracle is not worth handing out), but the *metric* may tell them apart, because they mean
/// different things to an operator. A spike in `Unknown` is someone probing with tokens that
/// were never issued; a spike in `Disabled` is a token that once worked being replayed after
/// its bot was paused, which is a likelier sign of a leak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthReject {
    /// No bot's stored tag matched the presented token.
    Unknown,
    /// A bot matched, but it is disabled.
    Disabled,
}

impl AuthReject {
    pub(crate) const ALL: [Self; 2] = [Self::Unknown, Self::Disabled];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Disabled => "disabled",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Every series this crate publishes.
pub(crate) struct Meters {
    registered: Arc<Counter>,
    authenticated: Arc<Counter>,
    auth_rejected: Vec<Arc<Counter>>,
    token_rotated: Arc<Counter>,
    scopes_changed: Arc<Counter>,
    disabled: Arc<Counter>,
    enabled: Arc<Counter>,
}

/// Registers one counter per variant, each tagged `key` with the variant's own label.
///
/// Registering the whole set up front is what gives a dashboard a flat line rather than a gap
/// for a rejection reason nobody has hit yet.
fn per_variant<T>(
    registry: &Registry,
    name: &'static str,
    help: &'static str,
    key: &'static str,
    variants: &[T],
    label: impl Fn(&T) -> &'static str,
) -> Vec<Arc<Counter>> {
    variants
        .iter()
        .map(|variant| registry.counter(name, help, &[(key, label(variant))]))
        .collect()
}

impl Meters {
    /// Registers every series at zero up front.
    pub(crate) fn new(registry: &Registry) -> Self {
        Self {
            registered: registry.counter("migo_bots_registered_total", "Bots registered.", &[]),
            authenticated: registry.counter(
                "migo_bots_authenticated_total",
                "Bot tokens accepted.",
                &[],
            ),
            auth_rejected: per_variant(
                registry,
                "migo_bots_auth_rejected_total",
                "Bot authentications refused, by reason.",
                "reason",
                &AuthReject::ALL,
                |reason| reason.label(),
            ),
            token_rotated: registry.counter(
                "migo_bots_token_rotated_total",
                "Bot tokens rotated.",
                &[],
            ),
            scopes_changed: registry.counter(
                "migo_bots_scopes_changed_total",
                "Bot permission sets changed.",
                &[],
            ),
            disabled: registry.counter(
                "migo_bots_disabled_total",
                "Bots disabled by their owner.",
                &[],
            ),
            enabled: registry.counter(
                "migo_bots_enabled_total",
                "Bots re-enabled by their owner.",
                &[],
            ),
        }
    }

    pub(crate) fn registered(&self) {
        self.registered.inc();
    }

    pub(crate) fn authenticated(&self) {
        self.authenticated.inc();
    }

    pub(crate) fn auth_rejected(&self, reason: AuthReject) {
        if let Some(counter) = self.auth_rejected.get(reason.index()) {
            counter.inc();
        }
    }

    pub(crate) fn token_rotated(&self) {
        self.token_rotated.inc();
    }

    pub(crate) fn scopes_changed(&self) {
        self.scopes_changed.inc();
    }

    pub(crate) fn disabled(&self) {
        self.disabled.inc();
    }

    pub(crate) fn enabled(&self) {
        self.enabled.inc();
    }
}
