//! What authentication reports.
//!
//! # What is deliberately absent
//!
//! No metric here is labelled by account, username, IP, or device. A counter labelled
//! by username is a list of usernames in a system that operators scrape, alert on, and
//! ship to third-party dashboards — which is to say it is a user directory with extra
//! steps. The registry's cardinality cap would drop most of the series anyway, keeping
//! whichever arrived first, so the exported answer would be both a privacy leak and
//! wrong.
//!
//! Per-account questions are answered from the audit log, which is access-controlled,
//! retained on purpose, and readable by a named human rather than by a scrape endpoint.
//!
//! # Why failures are labelled by outcome
//!
//! `migo_auth_signin_total{outcome="..."}` distinguishes a wrong passphrase from an
//! unknown user, even though the *response* to the client is identical for both
//! (see the `service` module, and `fault::invalid_credentials`). That is not a
//! contradiction. The client must not learn which one happened, because that answer
//! enumerates accounts. The operator must, because "unknown user" climbing while "bad
//! passphrase" stays flat is credential stuffing against a leaked email list, and the two
//! incidents call for different responses.
//!
//! The distinction is safe here because the label is on an aggregate count with no
//! subject attached: a rate is visible, an individual attempt is not.

use std::sync::Arc;

use migo_core::metrics::{Counter, Histogram, Registry, LATENCY_BUCKETS_MS};

/// Why a sign-in ended the way it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignInOutcome {
    /// Credentials matched and a session was opened.
    Success,
    /// No account for the identifier presented.
    UnknownUser,
    /// Account exists, passphrase did not match.
    BadPassphrase,
    /// Account exists and is not permitted to sign in.
    Suspended,
    /// Refused by the rate limiter before credentials were checked.
    RateLimited,
    /// The account already has as many devices as it may have.
    DeviceLimit,
}

impl SignInOutcome {
    const ALL: [Self; 6] = [
        Self::Success,
        Self::UnknownUser,
        Self::BadPassphrase,
        Self::Suspended,
        Self::RateLimited,
        Self::DeviceLimit,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::UnknownUser => "unknown_user",
            Self::BadPassphrase => "bad_passphrase",
            Self::Suspended => "suspended",
            Self::RateLimited => "rate_limited",
            Self::DeviceLimit => "device_limit",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Success => 0,
            Self::UnknownUser => 1,
            Self::BadPassphrase => 2,
            Self::Suspended => 3,
            Self::RateLimited => 4,
            Self::DeviceLimit => 5,
        }
    }
}

/// Why a refresh ended the way it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefreshOutcome {
    /// Rotated into a new generation.
    Success,
    /// No session matched the presented token.
    Unknown,
    /// The session was already rotated or revoked. This is the theft signal.
    Reuse,
    /// The refresh window has closed.
    Expired,
    /// Presented from a device other than the one it was minted for.
    DeviceMismatch,
    /// The account may no longer sign in.
    Suspended,
    /// Refused by the rate limiter.
    RateLimited,
}

impl RefreshOutcome {
    const ALL: [Self; 7] = [
        Self::Success,
        Self::Unknown,
        Self::Reuse,
        Self::Expired,
        Self::DeviceMismatch,
        Self::Suspended,
        Self::RateLimited,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Unknown => "unknown",
            Self::Reuse => "reuse_detected",
            Self::Expired => "expired",
            Self::DeviceMismatch => "device_mismatch",
            Self::Suspended => "suspended",
            Self::RateLimited => "rate_limited",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Success => 0,
            Self::Unknown => 1,
            Self::Reuse => 2,
            Self::Expired => 3,
            Self::DeviceMismatch => 4,
            Self::Suspended => 5,
            Self::RateLimited => 6,
        }
    }
}

/// The counters, resolved once at construction.
pub(crate) struct Meters {
    registrations: Arc<Counter>,
    registrations_reconciled: Arc<Counter>,
    registrations_refused: Arc<Counter>,
    signin: Vec<Arc<Counter>>,
    refresh: Vec<Arc<Counter>>,
    families_revoked: Arc<Counter>,
    sessions_revoked: Arc<Counter>,
    devices_revoked: Arc<Counter>,
    rehashes: Arc<Counter>,
    verify_failures: Arc<Counter>,
    hash_latency: Arc<Histogram>,
}

impl Meters {
    /// Registers every series, including the ones that have not happened yet.
    ///
    /// A counter that appears the first time it fires cannot be alerted on in advance:
    /// `rate(migo_auth_refresh_total{outcome="reuse_detected"}[5m]) > 0` against a
    /// series that does not exist does not evaluate to false, it fails to evaluate. So
    /// the alert would have to be written after the first token theft it was meant to
    /// catch. Every outcome is created at zero for that reason.
    pub(crate) fn new(registry: &Registry) -> Self {
        Self {
            registrations: registry.counter(
                "migo_auth_registrations_total",
                "Accounts created.",
                &[],
            ),
            registrations_refused: registry.counter(
                "migo_auth_registrations_refused_total",
                "Registration attempts refused, for any reason.",
                &[],
            ),
            registrations_reconciled: registry.counter(
                "migo_auth_registrations_reconciled_total",
                "Registration retries folded into an existing account (brief section 12).",
                &[],
            ),
            signin: SignInOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_auth_signin_total",
                        "Sign-in attempts, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            refresh: RefreshOutcome::ALL
                .iter()
                .map(|outcome| {
                    registry.counter(
                        "migo_auth_refresh_total",
                        "Refresh attempts, by outcome.",
                        &[("outcome", outcome.label())],
                    )
                })
                .collect(),
            families_revoked: registry.counter(
                "migo_auth_families_revoked_total",
                "Session families revoked because a refresh token was reused.",
                &[],
            ),
            sessions_revoked: registry.counter(
                "migo_auth_sessions_revoked_total",
                "Sessions revoked, for any reason.",
                &[],
            ),
            devices_revoked: registry.counter(
                "migo_auth_devices_revoked_total",
                "Devices revoked by their owner.",
                &[],
            ),
            rehashes: registry.counter(
                "migo_auth_passphrase_rehash_total",
                "Passphrases transparently rehashed to current parameters on sign-in.",
                &[],
            ),
            verify_failures: registry.counter(
                "migo_auth_token_verify_failures_total",
                "Access tokens rejected during verification.",
                &[],
            ),
            hash_latency: registry.histogram(
                "migo_auth_passphrase_hash_ms",
                "Time spent hashing or verifying a passphrase.",
                &[],
                LATENCY_BUCKETS_MS,
            ),
        }
    }

    pub(crate) fn registered(&self) {
        self.registrations.inc();
    }

    pub(crate) fn registration_refused(&self) {
        self.registrations_refused.inc();
    }

    pub(crate) fn reconciled_registration(&self) {
        self.registrations_reconciled.inc();
    }

    pub(crate) fn signin(&self, outcome: SignInOutcome) {
        if let Some(counter) = self.signin.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn refresh(&self, outcome: RefreshOutcome) {
        if let Some(counter) = self.refresh.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn family_revoked(&self, sessions: u64) {
        self.families_revoked.inc();
        self.sessions_revoked.add(sessions);
    }

    pub(crate) fn sessions_revoked(&self, count: u64) {
        self.sessions_revoked.add(count);
    }

    pub(crate) fn device_revoked(&self) {
        self.devices_revoked.inc();
    }

    pub(crate) fn rehashed(&self) {
        self.rehashes.inc();
    }

    pub(crate) fn verify_failed(&self) {
        self.verify_failures.inc();
    }

    pub(crate) fn hash_took(&self, millis: f64) {
        self.hash_latency.observe(millis);
    }
}
