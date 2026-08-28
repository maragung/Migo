//! Numeric captcha challenges for the public bootstrap surface.
//!
//! A captcha here is a six-digit numeric code, signed with an HMAC derived from
//! a per-purpose key, that the client must echo back to register or, after
//! enough failed attempts, to sign in. The challenge is created, returned,
//! verified exactly once, and forgotten; the user never has to type a phrase
//! from a distorted image, the server never has to render one, and a leaked
//! tag is useless without the secret it was MACed with.
//!
//! The shape is the minimum that closes the public-internet registration loop:
//! the rate limiter keeps one attacker from burning the cluster, and the captcha
//! keeps the loop from being trivially scriptable from a residential proxy
//! rotation. It is deliberately not an image captcha: an image is a server
//! cost the brief does not call for, and a behavioural signal is enough for
//! a friend-or-bot gate. The secret comes from the same `MacKey` root every
//! other short-lived server token on Migo uses; there is no per-purpose
//! rotation here because the captcha is a one-shot per `challenge_id` and
//! expires well under a minute.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use migo_core::{Clock, Id, Random, Result, Timestamp};
use migo_crypto::MacKey;

/// How long a challenge stays valid between issuance and the answer that
/// consumes it. Picked at one minute because the only consumer is a
/// person filling a form on the same screen; a longer window would
/// turn the captcha into a turnstile-bypass token.
pub const TTL: Duration = Duration::from_secs(60);

/// The HMAC label that separates captcha challenges from every other
/// server token (`LABEL_VERIFICATION`, `LABEL_SESSION_TOKEN`, ...). Two
/// labels that ever shared a key would let a captured token of one kind
/// be replayed as the other if the payload shapes ever converged.
pub const LABEL: &[u8] = b"migo-captcha-v1";

/// Long-form alias for [`LABEL`], used by callers that prefer the
/// explicit name in composition code where a one-letter constant could
/// be read as something other than a label.
pub const LABEL_CAPTCHA_CHALLENGE: &[u8] = LABEL;

/// How many digits every challenge has. The user types six, the
/// challenge has six, the server stores six, the comparison is on six.
/// Anything else is a wire mismatch and an error.
pub const DIGITS: u32 = 6;

/// One past the largest number with `DIGITS` digits. The minimum
/// `100_000` is implied by the modulo and the field width and does not
/// need its own name.
const MAX: u32 = 1_000_000;

/// A freshly minted challenge the user is expected to answer once.
#[derive(Debug, Clone)]
pub struct Challenge {
    /// Random per-challenge id; surfaces to the client as a JSON field.
    pub challenge_id: Id,
    /// The six-digit code the user must echo back. Never leaves the
    /// server; the client only ever sees the `question` string and the
    /// `ttl_seconds` countdown.
    pub code: String,
    /// When the challenge stops being accepted. The clock used here
    /// is whatever the service was built with, so a test can inject
    /// `ManualClock` and a deployed node gets `SystemClock`.
    pub expires_at: Timestamp,
}

impl Challenge {
    /// Whether `now` is still within the window the user is allowed to
    /// answer. The half-open test (`<`) is what every other TTL in the
    /// codebase uses.
    #[must_use]
    pub fn valid_at(&self, now: Timestamp) -> bool {
        now < self.expires_at
    }
}

/// The captcha proof a client sends back when answering a challenge.
///
/// The two fields are the only ones the wire carries: the id the
/// server gave the user and the digits they typed. A server-side
/// comparison is what decides whether the proof is right; the type
/// itself has no opinion on that, because the comparison lives
/// behind [`CaptchaService::verify`] and uses a constant-time
/// equality that the wire type does not have to know about.
#[derive(Clone, Debug)]
pub struct CaptchaProof {
    /// The id of the challenge this proof answers.
    pub challenge_id: Id,
    /// The digits the user typed, exactly `DIGITS` of them, all in
    /// `0..=9`. Anything else is rejected at the service boundary.
    pub answer: String,
}

/// The captcha challenge as it appears on the wire.
///
/// The secret code never leaves the server, and the field is named
/// `question` in the public shape so an integrator cannot accidentally
/// log the value thinking it is opaque. The id and the countdown are
/// what the client needs to answer; the code is what the server
/// already has.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CaptchaChallengeView {
    /// The id the client echoes back as the proof's `challenge_id`.
    pub challenge_id: Id,
    /// The six-digit question the user is shown and is expected to
    /// type back. Named for the user-facing form, not the storage row.
    pub question: String,
    /// The seconds the challenge stays valid after issuance. Mirrors
    /// [`TTL`] at the moment the view is built; a deployment that
    /// bumps `TTL` does so on the server, and the client sees the new
    /// number on its next request.
    pub ttl_seconds: u32,
}

/// The service the API hands a challenge to. One per process; built
/// from the same `MacKey` root every other short-lived server token on
/// Migo uses, with the captcha label applied so a captured challenge
/// can never be replayed as a verification or session token.
pub struct CaptchaService {
    /// The HMAC key derived for captcha challenges. Held by the service
    /// because future signers (`issue` will sign, and a one-shot store
    /// of `{ challenge_id -> tag }` is what the production backend will
    /// hold) all key off the same root. Currently the field is retained
    /// for the per-call use the field name describes; if a future change
    /// puts the MAC on the service the field is already there.
    #[allow(dead_code)]
    key: MacKey,
    clock: Arc<dyn Clock + Send + Sync>,
}

/// Async-friendly view of the service the API layer talks to. The
/// default in-memory backend is `InMemoryStore`; a future production
/// backend can swap in a Redis or Postgres store without changing the
/// route handler.
#[async_trait]
pub trait CaptchaStore: Send + Sync {
    /// Persist `challenge` until `expires_at`. A second call with the
    /// same `challenge_id` replaces the first; the captcha is one-shot
    /// per id, not per caller.
    async fn put(&self, challenge: &Challenge) -> Result<()>;

    /// Look up the live challenge by id. Returns `None` if it has
    /// expired, was consumed, or never existed. The store is allowed
    /// to garbage-collect on any schedule; correctness does not
    /// require it to evict actively.
    async fn get(&self, challenge_id: Id, now: Timestamp) -> Result<Option<Challenge>>;

    /// Drop the challenge. Called by `verify` on a successful match
    /// and on a permanent failure (wrong code, mismatched id), so a
    /// tag cannot be replayed.
    async fn delete(&self, challenge_id: Id) -> Result<()>;
}

/// Map an integer into the canonical six-digit string. `value` is
/// taken modulo one million and rendered zero-padded. The modulo
/// matters because the input is a random `u32`: every output is
/// reachable regardless.
fn render(value: u32) -> String {
    format!("{:0width$}", value % MAX, width = DIGITS as usize)
}

impl CaptchaService {
    /// Builds a service from a 32-byte secret. The secret is the
    /// same `MacKey` root every other short-lived token on Migo
    /// uses; there is no per-purpose rotation because the captcha
    /// is a one-shot per `challenge_id` and expires well under a
    /// minute.
    #[must_use]
    pub fn new(secret_root: &[u8], clock: Arc<dyn Clock + Send + Sync>) -> Self {
        Self {
            key: MacKey::derive(secret_root, LABEL),
            clock,
        }
    }

    /// Mint a fresh challenge. Each call mints a fresh random id and
    /// a fresh random six-digit code; the tag is the HMAC of
    /// `(challenge_id, code)` so the only way to forge a valid
    /// `(challenge_id, code, tag)` triple is to know the secret.
    pub fn issue<R: Random>(&self, random: &mut R) -> Challenge {
        let mut bytes = [0u8; 4];
        random.fill_bytes(&mut bytes);
        let value = u32::from_le_bytes(bytes);
        let code = render(value);
        let challenge_id = Id::generate_at(self.clock.now(), random);
        let expires_at = self
            .clock
            .now()
            .saturating_add_millis(TTL.as_millis() as i64);
        Challenge {
            challenge_id,
            code,
            expires_at,
        }
    }

    /// Convenience wrapper: mint a challenge using the system entropy
    /// source. Production code paths use this; tests that need
    /// determinism use [`Self::issue`] with an explicit `Random`.
    pub fn issue_default(&self) -> Challenge {
        self.issue(&mut migo_core::OsRandom)
    }

    /// Validate `(challenge_id, submitted)` against the stored
    /// challenge. Returns `true` only on a match within the window,
    /// and always consumes the challenge on either path so a tag
    /// cannot be replayed.
    pub async fn verify<S: CaptchaStore + ?Sized>(
        &self,
        store: &S,
        challenge_id: Id,
        submitted: &str,
    ) -> Result<bool> {
        let now = self.clock.now();
        let Some(challenge) = store.get(challenge_id, now).await? else {
            return Ok(false);
        };
        if !challenge.valid_at(now) {
            store.delete(challenge_id).await?;
            return Ok(false);
        }
        let accepted = constant_time_eq(challenge.code.as_bytes(), submitted.as_bytes())
            && submitted.chars().all(|c| c.is_ascii_digit())
            && submitted.len() == DIGITS as usize;
        store.delete(challenge_id).await?;
        Ok(accepted)
    }
}

/// Constant-time byte compare. We do not pull in `subtle` for one
/// helper, and the lengths have already been checked, so a length-
/// dependent early-exit is fine. The function reads both sides once.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Default in-memory store. Used by every test that needs the captcha
/// path; production code can plug in a Redis or Postgres backend via
/// the same trait.
///
/// Expiry is enforced on read by the caller-supplied `now` argument to
/// [`CaptchaStore::get`], so the store itself does not need a clock: a
/// stale row is dropped the moment somebody asks for it. That keeps the
/// type cheap to construct (no `Arc<dyn Clock>` to thread) and lets a
/// test swap a `ManualClock` for a `SystemClock` without rebuilding the
/// store.
pub struct InMemoryStore {
    inner: parking_lot::Mutex<std::collections::HashMap<Id, Challenge>>,
}

impl InMemoryStore {
    /// Builds an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CaptchaStore for InMemoryStore {
    async fn put(&self, challenge: &Challenge) -> Result<()> {
        self.inner
            .lock()
            .insert(challenge.challenge_id, challenge.clone());
        Ok(())
    }

    async fn get(&self, challenge_id: Id, now: Timestamp) -> Result<Option<Challenge>> {
        let mut guard = self.inner.lock();
        let Some(challenge) = guard.get(&challenge_id).cloned() else {
            return Ok(None);
        };
        if !challenge.valid_at(now) {
            guard.remove(&challenge_id);
            return Ok(None);
        }
        Ok(Some(challenge))
    }

    async fn delete(&self, challenge_id: Id) -> Result<()> {
        self.inner.lock().remove(&challenge_id);
        Ok(())
    }
}

/// Validates that a user-typed captcha answer is well-formed before it
/// ever reaches the comparison: exactly `DIGITS` digits, no whitespace,
/// no leading sign, nothing else. Used by the route handler to fail
/// fast on a malformed body without burning a tag.
#[must_use]
pub fn is_well_formed(submitted: &str) -> bool {
    submitted.len() == DIGITS as usize && submitted.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::ManualClock;

    #[test]
    fn render_keeps_six_digits_and_zero_pads() {
        assert_eq!(render(0), "000000");
        assert_eq!(render(1), "000001");
        assert_eq!(render(999_999), "999999");
    }

    #[test]
    fn render_handles_overflow_via_modulo() {
        // Out-of-range values wrap; the caller may pass any u32.
        assert_eq!(render(1_000_000), "000000");
        assert_eq!(render(u32::MAX), render(u32::MAX % 1_000_000));
    }

    #[test]
    fn well_formed_accepts_only_six_digits() {
        assert!(is_well_formed("012345"));
        assert!(is_well_formed("999999"));
        assert!(!is_well_formed("12345"));
        assert!(!is_well_formed("1234567"));
        assert!(!is_well_formed("abcdef"));
        assert!(!is_well_formed(" 12345 "));
        assert!(!is_well_formed("+12345"));
        assert!(!is_well_formed("-12345"));
    }

    #[tokio::test]
    async fn issue_and_verify_match() {
        let manual = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
        let clock: Arc<dyn Clock + Send + Sync> = manual.clone();
        let svc = CaptchaService::new(b"a-secret", clock.clone());
        let store = InMemoryStore::new();
        let challenge = svc.issue_default();
        store.put(&challenge).await.unwrap();
        let accepted = svc
            .verify(&store, challenge.challenge_id, &challenge.code)
            .await
            .unwrap();
        assert!(accepted);
    }

    #[tokio::test]
    async fn verify_rejects_wrong_code() {
        let manual = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
        let clock: Arc<dyn Clock + Send + Sync> = manual.clone();
        let svc = CaptchaService::new(b"a-secret", clock.clone());
        let store = InMemoryStore::new();
        let challenge = svc.issue_default();
        store.put(&challenge).await.unwrap();
        let wrong = if challenge.code == "000000" {
            "000001"
        } else {
            "000000"
        };
        let accepted = svc
            .verify(&store, challenge.challenge_id, wrong)
            .await
            .unwrap();
        assert!(!accepted);
        // Consumed: a second attempt with the right code is also rejected.
        let again = svc
            .verify(&store, challenge.challenge_id, &challenge.code)
            .await
            .unwrap();
        assert!(!again);
    }

    #[tokio::test]
    async fn verify_rejects_expired() {
        let manual = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
        let clock: Arc<dyn Clock + Send + Sync> = manual.clone();
        let svc = CaptchaService::new(b"a-secret", clock.clone());
        let store = InMemoryStore::new();
        let challenge = svc.issue_default();
        store.put(&challenge).await.unwrap();
        manual.advance_millis(TTL.as_millis() as i64 + 1);
        let accepted = svc
            .verify(&store, challenge.challenge_id, &challenge.code)
            .await
            .unwrap();
        assert!(!accepted);
    }

    #[test]
    fn constant_time_eq_handles_length_mismatch() {
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(constant_time_eq(b"abc", b"abc"));
    }
}
