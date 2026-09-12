//! The subscription-churn window: one fixed window per seat.

use migo_core::Timestamp;

/// A fixed-window request counter.
///
/// Section 165 puts a rate limit on how many stream subscriptions a
/// participant may hold, answered `RATE_LIMITED` with a retry hint when
/// crossed. The window here is fixed rather than sliding on purpose: a
/// sliding window needs timestamps per event and buys only smoother edges,
/// while the thing being defended against — a client that floods subscribe
/// and unsubscribe requests to churn the plane's state — is stopped just as
/// well by a window that says "not yet, and here is when".
///
/// A refused request still spends the window, the same deliberate arithmetic
/// the shared rate limiter uses: a rejection that costs nothing makes
/// flooding free, because an attacker whose every request is refused would
/// pay for none of them.
pub(crate) struct WindowCounter {
    window_ms: i64,
    max: u32,
    window_start: Timestamp,
    count: u32,
}

impl WindowCounter {
    /// A window of `max` requests per `window_ms`, opening at `now`.
    pub(crate) fn new(window_ms: i64, max: u32, now: Timestamp) -> Self {
        Self {
            window_ms,
            max,
            window_start: now,
            count: 0,
        }
    }

    /// Charges one request, returning how long to wait when the window is
    /// spent.
    ///
    /// The wait is to the end of the *current* window — after a roll it is
    /// the whole fresh window, which is the fixed window's honest answer
    /// even when it is pessimistic by a few milliseconds.
    pub(crate) fn charge(&mut self, now: Timestamp) -> Option<u32> {
        let interval = self.window_ms.max(1) as u64;
        if now.saturating_since(self.window_start) >= interval {
            self.window_start = now;
            self.count = 0;
        }
        self.count = self.count.saturating_add(1);
        if self.count <= self.max {
            return None;
        }
        let elapsed = now
            .saturating_since(self.window_start)
            .min(interval.saturating_sub(1));
        let remaining = interval - elapsed;
        Some(remaining.min(u32::MAX as u64).max(1) as u32)
    }
}
