//! Injectable time source.
//!
//! Nothing in Migo outside this module and `Timestamp::now` is allowed to read
//! the host clock directly. Every component takes a `Clock`, which lets the
//! deterministic simulator advance time by hand and replay a run exactly
//! (ADR-0009). It also makes "token expires in 15 minutes" a testable claim
//! rather than a sleep.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use crate::time::Timestamp;

/// A source of the current instant.
pub trait Clock: Send + Sync + 'static {
    /// The current instant.
    fn now(&self) -> Timestamp;

    /// Milliseconds elapsed since `earlier`, clamped at zero.
    fn elapsed_since(&self, earlier: Timestamp) -> u64 {
        self.now().saturating_since(earlier)
    }
}

impl<T: Clock + ?Sized> Clock for Arc<T> {
    fn now(&self) -> Timestamp {
        (**self).now()
    }
}

/// Reads the host clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock that only moves when a test tells it to.
#[derive(Debug)]
pub struct ManualClock {
    millis: AtomicI64,
}

impl ManualClock {
    /// Starts at the given instant.
    #[must_use]
    pub fn new(start: Timestamp) -> Self {
        Self {
            millis: AtomicI64::new(start.as_millis()),
        }
    }

    /// Starts at the Migo epoch.
    #[must_use]
    pub fn at_epoch() -> Self {
        Self::new(Timestamp::ZERO)
    }

    /// Moves the clock forward.
    pub fn advance_millis(&self, millis: i64) {
        self.millis.fetch_add(millis, Ordering::SeqCst);
    }

    /// Jumps the clock to an absolute instant, forwards or backwards. Moving
    /// backwards is allowed on purpose: NTP steps happen in production and code
    /// that assumes monotonicity should be tested against that.
    pub fn set(&self, at: Timestamp) {
        self.millis.store(at.as_millis(), Ordering::SeqCst);
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::at_epoch()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.millis.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_stands_still_until_advanced() {
        let clock = ManualClock::at_epoch();
        let first = clock.now();
        assert_eq!(clock.now(), first);
        clock.advance_millis(250);
        assert_eq!(clock.now() - first, 250);
    }

    #[test]
    fn manual_clock_can_step_backwards() {
        let clock = ManualClock::new(Timestamp::from_millis(10_000));
        clock.set(Timestamp::from_millis(9_000));
        assert_eq!(clock.now().as_millis(), 9_000);
    }

    #[test]
    fn system_clock_is_after_the_epoch() {
        assert!(SystemClock.now().as_millis() > 0);
    }
}
