//! The nonce window: the memory that stops a captured handshake being replayed.
//!
//! Section 169 requires a handshake nonce to be random 32 bytes that must not repeat within
//! a tolerance window. [`verify_proof`](migo_crypto::node::verify_proof) already rejects a
//! proof whose timestamp is outside the clock-skew band, which bounds *how old* a replay can
//! be; this closes the remaining gap — a proof captured and replayed while its timestamp is
//! still fresh — by remembering the nonce and refusing it the second time.
//!
//! The window is global, not per-peer: a 32-byte random nonce is unique across the whole
//! mesh with overwhelming probability, so one shared memory is both correct and simpler than
//! one per link. It is pruned on every check, so it holds only nonces still inside the
//! window and cannot grow without bound.

use std::collections::HashMap;

use parking_lot::Mutex;

use migo_core::Timestamp;
use migo_crypto::node::NONCE_LEN;

/// A time-pruned memory of recently seen handshake nonces.
pub(crate) struct NonceWindow {
    /// How long a nonce is remembered, in milliseconds. Must exceed twice the accepted clock
    /// skew, or a replay could pass the timestamp check in a window the nonce memory has
    /// already forgotten.
    window_ms: i64,
    /// Each seen nonce and when it was recorded.
    seen: Mutex<HashMap<[u8; NONCE_LEN], Timestamp>>,
}

impl NonceWindow {
    /// A fresh window that remembers a nonce for `window_ms` milliseconds.
    pub(crate) fn new(window_ms: i64) -> Self {
        Self {
            window_ms,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Records `nonce` and reports whether it was fresh.
    ///
    /// Returns `true` if the nonce had not been seen inside the window — it is now recorded —
    /// and `false` if it is a replay. Prunes expired entries first, so a `true` means "not
    /// seen recently", never "not seen since the map was last cleared".
    #[must_use]
    pub(crate) fn check_and_record(&self, nonce: &[u8; NONCE_LEN], now: Timestamp) -> bool {
        // A negative or absurd window disables protection rather than panicking; the service
        // validates the configured value, so this is belt-and-braces.
        let window = u64::try_from(self.window_ms).unwrap_or(0);
        let mut seen = self.seen.lock();
        // Drop everything older than the window. Afterwards the map holds only live nonces,
        // so a present key is by definition a replay.
        seen.retain(|_, &mut recorded| now.saturating_since(recorded) <= window);
        if seen.contains_key(nonce) {
            return false;
        }
        seen.insert(*nonce, now);
        true
    }
}
