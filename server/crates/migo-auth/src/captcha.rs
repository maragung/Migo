//! The captcha gate.
//!
//! Authentication against the public internet is gated by three layers in
//! order: a per-network rate limit (the one [`migo_ratelimit`] charges
//! before any work), a captcha on the bootstrap surface after enough
//! failures from the same network, and the password check itself. The
//! captcha is the layer this module owns.
//!
//! The route layer issues challenges (one ticket per `POST
//! /v1/auth/captcha`); the authenticator consults the gate to decide
//! whether the next register or sign-in attempt has to carry a proof,
//! and to verify the proof the client sends back. The two halves talk
//! through one struct, [`CaptchaGate`], which holds the service that
//! mints and verifies, the store that persists the challenge, and the
//! per-IP counter that drives the "is it required now" decision.
//!
//! # Why a counter, not a sliding window
//!
//! The threshold fires on *consecutive* failures from the same network:
//! one correct sign-in clears the count, and the assumption is that a
//! person who just proved they own the account is not a bot. The
//! counter is bounded by the network bucket the rate limiter maintains
//! and reset to zero on every successful authentication, which keeps the
//! state machine small enough to reason about without a clock.
//!
//! # What the gate is not
//!
//! The gate does not decide *which* proof to accept — a captcha is a
//! captcha, and the service is the only thing that knows whether the
//! digits typed match the digits it issued. The gate also does not
//! throttle the issuance route; the network rate-limit bucket on the
//! route handler is the right place to do that, and a per-IP issuance
//! limit would belong in the rate limiter too. The gate counts failures
//! and verifies proofs, and nothing else.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use parking_lot::Mutex;

use migo_captcha::{CaptchaChallengeView, CaptchaProof, CaptchaService, CaptchaStore, TTL};
use migo_core::{Error, Result};

/// The captcha gate the authenticator consults and the route layer
/// mints challenges through.
///
/// One gate per process: the service and the store behind it are
/// shared by the `register` and `sign_in` paths, the per-IP failure
/// counter is held in the gate (not in the rate limiter, because the
/// rate limiter's charges are about budget and the captcha counter is
/// about state), and the threshold is a single configuration value
/// that the deployment sets once and forgets.
///
/// `Send + Sync` because every field is independently: `Arc`s are
/// `Send + Sync` when their contents are, and the per-IP counter
/// guards a `HashMap` with a parking-lot mutex whose critical section
/// is a counter increment and never an `await`.
pub struct CaptchaGate {
    /// Issues and verifies the cryptographic challenges.
    service: Arc<CaptchaService>,
    /// Persists the challenges between issue and verify.
    store: Arc<dyn CaptchaStore + Send + Sync>,
    /// Consecutive failure count per network. Cleared on success and
    /// ticked up on a sign-in that hit a wrong password past the
    /// threshold. See the module docs for the model.
    attempts: Mutex<HashMap<IpAddr, u32>>,
    /// How many failures in a row from the same network force the next
    /// attempt to carry a captcha proof. The deployment sets this from
    /// `AuthConfig::captcha_threshold`; the gate has no opinion of its
    /// own on what the number is.
    threshold: u32,
}

impl CaptchaGate {
    /// Builds a gate over the given service, store, and threshold.
    ///
    /// `threshold == 0` means *the next attempt must carry a captcha*,
    /// which is rarely what an operator wants; the configuration layer
    /// rejects that value at startup and the gate does not duplicate the
    /// check here. The intended "captcha off" posture is
    /// `AuthConfig::captcha_threshold = None`, in which case the gate
    /// is not built at all and the authenticator sees a `None`.
    #[must_use]
    pub fn new(
        service: Arc<CaptchaService>,
        store: Arc<dyn CaptchaStore + Send + Sync>,
        threshold: u32,
    ) -> Self {
        Self {
            service,
            store,
            attempts: Mutex::new(HashMap::new()),
            threshold,
        }
    }

    /// The TTL, in seconds, the route layer puts on the wire.
    fn ttl_seconds(&self) -> u32 {
        // The captcha crate's `TTL` is a `Duration`; the public value is
        // seconds. Saturating to zero would only happen for an
        // unrealistic constant, but `u64 -> u32` would silently wrap
        // past 136 years, so the conversion is the narrower one.
        u32::try_from(TTL.as_secs()).unwrap_or(u32::MAX)
    }

    /// Issues a fresh challenge and returns the public view of it.
    ///
    /// The route layer is the only caller; it puts the returned
    /// `challenge_id` and `question` on the wire and the gate does not
    /// hold on to them. The challenge is stored under the id, so a
    /// later `verify` can find it.
    pub async fn request(&self) -> Result<CaptchaChallengeView> {
        let challenge = self.service.issue_default();
        self.store.put(&challenge).await?;
        let ttl_seconds = self.ttl_seconds();
        Ok(CaptchaChallengeView {
            challenge_id: challenge.challenge_id,
            question: challenge.code,
            ttl_seconds,
        })
    }

    /// Verifies a captcha proof against the stored challenge and, on
    /// either branch, consumes the challenge so the proof cannot be
    /// replayed.
    ///
    /// Returns `Ok(true)` only on a fresh match, `Ok(false)` on a wrong
    /// or expired proof, and `Err(_)` only on a store fault — a wrong
    /// answer is the user's mistake, not the server's.
    pub async fn verify(&self, proof: &CaptchaProof) -> Result<bool> {
        // The service stamps `now` from the clock it was built with,
        // which is the same clock the rest of the authenticator uses
        // (composition pins the two). A divergence here would be a
        // composition bug, not something the gate can paper over.
        self.service
            .verify(self.store.as_ref(), proof.challenge_id, &proof.answer)
            .await
    }

    /// Notes a failed sign-in from `ip`. Past the threshold, the next
    /// attempt from that network is required to carry a captcha.
    pub fn record_failure(&self, ip: IpAddr) {
        let mut attempts = self.attempts.lock();
        let counter = attempts.entry(ip).or_insert(0);
        *counter = counter.saturating_add(1);
    }

    /// Notes a successful sign-in from `ip`, clearing the counter. A
    /// streak of successes does not remember a prior failure: the user
    /// just proved they own the account, and the gate's job is to
    /// suspect a network, not a person.
    pub fn record_success(&self, ip: IpAddr) {
        let mut attempts = self.attempts.lock();
        attempts.remove(&ip);
    }

    /// Whether the next attempt from `ip` must carry a captcha proof.
    ///
    /// Read without taking a lock against the `record_*` calls: the
    /// parking-lot mutex serialises the lot, and the decision is
    /// monotonic in the counter so a transient value never moves the
    /// answer in the wrong direction.
    #[must_use]
    pub fn needs_captcha(&self, ip: IpAddr) -> bool {
        let attempts = self.attempts.lock();
        attempts.get(&ip).copied().unwrap_or(0) >= self.threshold
    }

    /// The threshold the gate is enforcing. For tests and metrics; the
    /// route layer has no business asking.
    #[must_use]
    pub fn threshold(&self) -> u32 {
        self.threshold
    }
}

/// Reads the threshold the gate is enforcing, defaulting to "captcha
/// off" when the configuration is absent.
///
/// Kept as a free function so the authenticator's `Auth::new` can
/// resolve the configuration once and not have to thread `Option<...>`
/// into the type of the gate field. Returning `None` (rather than
/// building a gate with a sentinel threshold) makes the "captcha off"
/// branch compile-time obvious at the call site.
#[must_use]
pub fn threshold_from(config_threshold: Option<u32>) -> Option<u32> {
    config_threshold
}

/// Returns the [`Error`] an unauthenticated caller sees when the gate
/// demands a captcha and the body has none.
///
/// The `feature_disabled` shape rather than a custom one: a captcha
/// is not a feature flag, but the wire contract is the same — the
/// caller has to satisfy a precondition they did not.
pub fn error_required() -> Error {
    migo_protocol::fault::error(
        migo_protocol::codes::CAPTCHA_REQUIRED,
        "a captcha proof is required for this attempt",
    )
}

/// Returns the [`Error`] the caller sees when the proof they sent
/// does not verify.
pub fn error_invalid(why: impl Into<String>) -> Error {
    migo_protocol::fault::error(migo_protocol::codes::INVALID_CAPTCHA, why)
}

/// Returns the [`Error`] the caller sees when the proof they sent
/// refers to a challenge that has been consumed or that was never
/// issued, which the service reports by returning `false` from
/// `verify`. The gate has no clock of its own, so a "gone" challenge
/// and a "never existed" challenge are the same wire error; a fresh
/// request for a new one is the only honest answer.
pub fn error_expired() -> Error {
    migo_protocol::fault::error(
        migo_protocol::codes::CAPTCHA_EXPIRED,
        "the captcha challenge has expired or was never issued",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_captcha::InMemoryStore;
    use migo_core::{Clock, ManualClock, SystemClock};

    fn test_gate() -> (Arc<CaptchaService>, Arc<InMemoryStore>, CaptchaGate) {
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(ManualClock::at_epoch());
        let service = Arc::new(CaptchaService::new(b"a-test-root", clock));
        let store = Arc::new(InMemoryStore::new());
        let gate = CaptchaGate::new(service.clone(), store.clone(), 3);
        (service, store, gate)
    }

    fn ip() -> IpAddr {
        "203.0.113.1".parse().expect("literal address")
    }

    #[tokio::test]
    async fn a_fresh_request_returns_a_challenge_with_a_question_and_a_ttl() {
        let (_service, _store, gate) = test_gate();
        let challenge = gate.request().await.expect("gate issues");
        assert!(!challenge.question.is_empty());
        assert_eq!(challenge.ttl_seconds, 60);
    }

    #[test]
    fn needs_captcha_is_false_until_the_threshold_is_reached() {
        let (_service, _store, gate) = test_gate();
        let ip = ip();
        assert!(!gate.needs_captcha(ip));
        gate.record_failure(ip);
        assert!(!gate.needs_captcha(ip));
        gate.record_failure(ip);
        assert!(!gate.needs_captcha(ip));
        gate.record_failure(ip);
        assert!(gate.needs_captcha(ip));
    }

    #[test]
    fn record_success_clears_the_counter() {
        let (_service, _store, gate) = test_gate();
        let ip = ip();
        for _ in 0..5 {
            gate.record_failure(ip);
        }
        assert!(gate.needs_captcha(ip));
        gate.record_success(ip);
        assert!(!gate.needs_captcha(ip));
    }

    #[test]
    fn one_networks_failures_do_not_count_for_another() {
        let (_service, _store, gate) = test_gate();
        let noisy: IpAddr = "203.0.113.7".parse().unwrap();
        let quiet: IpAddr = "198.51.100.7".parse().unwrap();
        for _ in 0..5 {
            gate.record_failure(noisy);
        }
        assert!(gate.needs_captcha(noisy));
        assert!(!gate.needs_captcha(quiet));
    }

    #[tokio::test]
    async fn verify_consumes_a_challenge() {
        let (_service, _store, gate) = test_gate();
        let challenge = gate.request().await.expect("gate issues");
        let proof = CaptchaProof {
            challenge_id: challenge.challenge_id,
            answer: challenge.question,
        };
        assert!(gate.verify(&proof).await.expect("verify"));
        // A second attempt with the same proof is now rejected: the
        // store evicted it on the first verify.
        assert!(!gate.verify(&proof).await.expect("verify"));
    }

    #[tokio::test]
    async fn verify_rejects_a_wrong_answer() {
        let (_service, _store, gate) = test_gate();
        let challenge = gate.request().await.expect("gate issues");
        let wrong = if challenge.question == "000000" {
            "000001".to_string()
        } else {
            "000000".to_string()
        };
        let proof = CaptchaProof {
            challenge_id: challenge.challenge_id,
            answer: wrong,
        };
        assert!(!gate.verify(&proof).await.expect("verify"));
    }

    #[test]
    fn threshold_zero_means_captcha_required_immediately() {
        // The default validator rejects 0, but the gate itself is
        // permissive: a deployment that bypasses the validator still
        // gets the documented behaviour. The point of the test is to
        // pin the model, not to bless the configuration.
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(SystemClock);
        let service = Arc::new(CaptchaService::new(b"a-test-root", clock));
        let store = Arc::new(InMemoryStore::new());
        let gate = CaptchaGate::new(service, store, 0);
        let ip = ip();
        // The first failure ticks the counter to 1, which is >= 0.
        gate.record_failure(ip);
        assert!(gate.needs_captcha(ip));
    }
}
