//! The sign-in lockout: a progressive time-out on repeated wrong passwords.
//!
//! # The ladder
//!
//! Five consecutive failed sign-ins for one account lock the next attempt out for one minute.
//! Every further three failures (after the lockout expires) step the lockout up by two more
//! minutes — one, three, five, seven, and so on, capped by the configuration's ceiling. A
//! successful sign-in clears the whole record: the account's owner has proved themselves, and
//! the counter's only purpose was to price a guessing attack.
//!
//! The counter is keyed by **account id**, so every identifier an account owns (username, email,
//! phone) counts against the same record, and the lock follows the account across networks — a
//! distributed guessing attack cannot sidestep it by rotating addresses. The flip side, stated
//! plainly: somebody who knows an account's username can keep that account locked by feeding it
//! wrong passwords from anywhere, for at most the configured ceiling. That trade was chosen at
//! the product level.
//!
//! # Time comes from the caller
//!
//! The gate holds no clock. Every method takes the request's `now`, which keeps the gate a pure
//! state machine over `Timestamp`s and lets tests drive the whole ladder with a manual clock.
//! Attempts that arrive *during* a lockout are refused without a password check and without
//! counting — the ladder only climbs on failures that actually reached the password check.

use std::collections::HashMap;

use parking_lot::Mutex;

use migo_core::{Error, Timestamp};

/// The ladder's configuration, straight from `auth.lockout`.
#[derive(Clone, Debug)]
pub struct LockoutConfig {
    /// Whether the gate runs at all.
    pub enabled: bool,
    /// Consecutive failures at which the first lockout lands.
    pub initial_failures: u32,
    /// Further failures between one lockout level and the next.
    pub step_failures: u32,
    /// The first lockout's length.
    pub base_seconds: u64,
    /// How much longer each level above the first is.
    pub step_seconds: u64,
    /// The ceiling; the ladder climbs to here and stays.
    pub max_seconds: u64,
}

impl Default for LockoutConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            initial_failures: 5,
            step_failures: 3,
            base_seconds: 60,
            step_seconds: 120,
            max_seconds: 86_400,
        }
    }
}

/// One account's standing with the gate.
#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    /// Wrong passwords since the last success.
    failures: u32,
    /// The lockout level the failures have reached (0 = none).
    tier: u32,
    /// When the current lockout ends, if one is running.
    locked_until: Option<Timestamp>,
}

/// The gate itself: per-account state behind one mutex, in memory. A restart forgets it — the
/// same posture the captcha gate takes, and the same reason: the ladder prices a live guessing
/// attack, and a reboot is an operator's event, not an attacker's tool.
pub struct LockoutGate {
    config: LockoutConfig,
    entries: Mutex<HashMap<String, Entry>>,
}

impl LockoutGate {
    #[must_use]
    /// Builds the gate with its ladder's configuration.
    pub fn new(config: LockoutConfig) -> Self {
        Self {
            config,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Refuses the attempt while the account is locked, answering with the remaining
    /// milliseconds; otherwise lets it through untouched. A locked check counts nothing — the
    /// ladder only climbs on failures that reached a password check.
    ///
    /// # Errors
    ///
    /// Returns `Err(remaining_ms)` while the lockout is running.
    pub fn check(&self, now: Timestamp, account_id: &str) -> Result<(), u64> {
        let entries = self.entries.lock();
        let Some(entry) = entries.get(account_id) else {
            return Ok(());
        };
        let Some(until) = entry.locked_until else {
            return Ok(());
        };
        if now >= until {
            return Ok(());
        }
        Err(until.saturating_since(now))
    }

    /// Records one wrong password and answers with the lockout this failure triggered, when it
    /// crossed onto a rung of the ladder (`Some(seconds)`), or `None` when the account stays
    /// unlocked.
    #[must_use]
    pub fn record_failure(&self, now: Timestamp, account_id: &str) -> Option<u64> {
        let mut entries = self.entries.lock();
        let entry = entries.entry(account_id.to_owned()).or_default();
        // Defensive: a caller that records a failure during a live lockout (it was told not to)
        // extends nothing — the running lockout is the answer.
        if let Some(until) = entry.locked_until {
            if now < until {
                return Some(until.saturating_since(now));
            }
        }

        entry.failures = entry.failures.saturating_add(1);
        let failures = entry.failures;
        let config = &self.config;
        let crossed = failures == config.initial_failures
            || (failures > config.initial_failures
                && config.step_failures > 0
                && (failures - config.initial_failures).is_multiple_of(config.step_failures));
        if !crossed {
            return None;
        }

        entry.tier = entry.tier.saturating_add(1);
        let levels = u64::from(entry.tier.saturating_sub(1));
        let seconds = (config
            .base_seconds
            .saturating_add(levels.saturating_mul(config.step_seconds)))
        .min(config.max_seconds);
        let locked_until = Timestamp::from_unix_ms(
            now.as_unix_ms()
                .saturating_add(i64::try_from(seconds.saturating_mul(1000)).unwrap_or(i64::MAX)),
        );
        entry.locked_until = Some(locked_until);
        Some(seconds)
    }

    /// The success that ends the guessing game: the whole record goes away.
    pub fn record_success(&self, account_id: &str) {
        self.entries.lock().remove(account_id);
    }

    /// Whether an account is locked right now, and for how much longer. For tests and the
    /// curious; the sign-in path reads [`Self::check`].
    #[must_use]
    pub fn remaining_ms(&self, now: Timestamp, account_id: &str) -> Option<u64> {
        let entry = self.entries.lock().get(account_id).copied()?;
        let until = entry.locked_until?;
        (now < until).then(|| until.saturating_since(now))
    }
}

/// Returns the [`Error`] a locked-out sign-in attempt sees, carrying the wait the ladder
/// computed so a client can count down instead of guessing.
#[must_use]
pub fn error_locked(remaining_ms: u64) -> Error {
    let seconds = remaining_ms.div_ceil(1000);
    migo_protocol::fault::error(
        migo_protocol::codes::AUTH_LOCKED,
        "the account is temporarily locked after repeated sign-in failures",
    )
    .retry_after_ms(u32::try_from(remaining_ms).unwrap_or(u32::MAX))
    .public(format!("Account temporarily locked. Retry in {seconds} s"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_MS: i64 = 1_788_048_000_000;

    fn gate() -> LockoutGate {
        LockoutGate::new(LockoutConfig::default())
    }

    #[test]
    fn five_failures_lock_for_one_minute() {
        let g = gate();
        let now = Timestamp::from_unix_ms(NOW_MS);
        for _ in 0..4 {
            assert_eq!(g.record_failure(now, "acct"), None, "no lockout below five");
        }
        assert_eq!(g.record_failure(now, "acct"), Some(60));
        assert_eq!(g.remaining_ms(now, "acct"), Some(60_000));
        assert!(g.check(now, "acct").is_err(), "locked now");
    }

    #[test]
    fn three_more_failures_climb_to_three_minutes_then_five() {
        let g = gate();
        let now = Timestamp::from_unix_ms(NOW_MS);
        for _ in 0..5 {
            let _ = g.record_failure(now, "acct");
        }
        // Failures only count once the lockout has expired — the caller is expected to advance
        // past it before the next attempt reaches a password check.
        let later = Timestamp::from_unix_ms(NOW_MS + 61_000);
        // Failures 6 and 7 climb nothing; failure 8 (5 + 3) lands on the next rung.
        assert_eq!(g.record_failure(later, "acct"), None);
        assert_eq!(g.record_failure(later, "acct"), None);
        assert_eq!(g.record_failure(later, "acct"), Some(180));
        // And three past that: five minutes.
        let later2 = Timestamp::from_unix_ms(NOW_MS + 61_000 + 181_000);
        for _ in 0..2 {
            let _ = g.record_failure(later2, "acct");
        }
        assert_eq!(g.record_failure(later2, "acct"), Some(300));
    }

    #[test]
    fn the_ladder_respects_the_ceiling() {
        let g = LockoutGate::new(LockoutConfig {
            max_seconds: 300,
            ..LockoutConfig::default()
        });
        let now = Timestamp::from_unix_ms(NOW_MS);
        // Each window of failures is followed by its lockout expiring — the clock advances past
        // every rung so the ladder keeps climbing instead of stalling inside one lockout.
        let mut at = NOW_MS;
        let mut biggest: Option<u64> = None;
        for _ in 0..40 {
            if let Some(seconds) = g.record_failure(Timestamp::from_unix_ms(at), "acct") {
                biggest = Some(seconds.max(biggest.unwrap_or(0)));
            }
            at += 400_000;
        }
        let _ = now;
        assert_eq!(biggest, Some(300), "the ceiling holds the ladder down");
    }

    #[test]
    fn success_clears_the_whole_record() {
        let g = gate();
        let now = Timestamp::from_unix_ms(NOW_MS);
        for _ in 0..7 {
            let _ = g.record_failure(now, "acct");
        }
        g.record_success("acct");
        assert_eq!(g.remaining_ms(now, "acct"), None);
        // The ladder restarts: four more failures lock nothing, the fifth locks afresh.
        for _ in 0..4 {
            assert_eq!(g.record_failure(now, "acct"), None);
        }
        assert_eq!(g.record_failure(now, "acct"), Some(60));
    }

    #[test]
    fn a_lockout_refuses_fast_and_does_not_count() {
        let g = gate();
        let now = Timestamp::from_unix_ms(NOW_MS);
        for _ in 0..5 {
            let _ = g.record_failure(now, "acct");
        }
        let later = Timestamp::from_unix_ms(NOW_MS + 30_000);
        assert!(g.check(later, "acct").is_err());
        // Recording during a live lockout changes nothing: the ladder waits for real attempts.
        let during = g.record_failure(later, "acct");
        assert!(during.is_some(), "the running lockout is the answer");
        assert_eq!(g.remaining_ms(later, "acct"), Some(30_000));
        // Once the lockout has expired the ladder picks up where it left off.
        let after = Timestamp::from_unix_ms(NOW_MS + 61_000);
        assert!(g.check(after, "acct").is_ok());
    }

    #[test]
    fn accounts_lock_independently() {
        let g = gate();
        let now = Timestamp::from_unix_ms(NOW_MS);
        for _ in 0..5 {
            let _ = g.record_failure(now, "acct_a");
        }
        assert!(g.check(now, "acct_a").is_err());
        assert!(g.check(now, "acct_b").is_ok(), "b's record is b's own");
    }

    #[test]
    fn the_error_carries_the_wait() {
        let error = error_locked(95_000);
        assert_eq!(error.code(), migo_protocol::codes::AUTH_LOCKED);
        assert_eq!(error.retry_after(), Some(95_000));
        assert!(error.public_message().contains("Retry in 95 s"));
    }
}
