//! Time on the wire.
//!
//! Migo stores and transmits time as milliseconds since **2024-01-01T00:00:00Z**
//! rather than the Unix epoch. The reason is bandwidth: a Unix millisecond
//! timestamp needs 6 varint bytes for the next few centuries, while a
//! Migo-epoch one needs 5 until 2058. On a busy room every event carries at
//! least one timestamp, so one byte is not a rounding error — see
//! `docs/05-bandwidth-budget.md`.
//!
//! The type is deliberately not `SystemTime`: it is `Copy`, `Ord`, exactly the
//! width of the wire representation, and has no failure mode when arithmetic
//! crosses the epoch.

use std::fmt;
use std::ops::{Add, Sub};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The Migo epoch, in milliseconds since the Unix epoch: 2024-01-01T00:00:00Z.
pub const MIGO_EPOCH_MS: i64 = 1_704_067_200_000;

/// A millisecond instant, measured from [`MIGO_EPOCH_MS`].
///
/// Negative values are representable (they denote instants before 2024) so that
/// subtraction never panics, but the wire encoding clamps at zero: nothing in
/// Migo legitimately predates its own epoch.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The epoch itself.
    pub const ZERO: Timestamp = Timestamp(0);

    /// Largest value the wire encoding can carry.
    pub const MAX: Timestamp = Timestamp(i64::MAX);

    /// Wraps a raw Migo-epoch millisecond count.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// The raw Migo-epoch millisecond count.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// Converts from milliseconds since the Unix epoch.
    ///
    /// Saturating, because the shift is 54 years and the ends of the range are
    /// real values: a permanent ban is stored as [`Timestamp::MAX`], and a
    /// conversion that panicked on it would be a panic in production the first
    /// time anybody was banned forever.
    #[must_use]
    pub const fn from_unix_ms(unix_ms: i64) -> Self {
        Self(unix_ms.saturating_sub(MIGO_EPOCH_MS))
    }

    /// Converts to milliseconds since the Unix epoch. Saturating, for the reason
    /// given on [`Timestamp::from_unix_ms`].
    #[must_use]
    pub const fn as_unix_ms(self) -> i64 {
        self.0.saturating_add(MIGO_EPOCH_MS)
    }

    /// Reads the host clock. Prefer injecting a [`crate::Clock`] instead —
    /// direct calls are untestable and unreplayable (ADR-0009).
    #[must_use]
    pub fn now() -> Self {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            // A clock set before 1970 is a broken machine, not a supported state.
            .unwrap_or(MIGO_EPOCH_MS);
        Self::from_unix_ms(unix_ms)
    }

    /// The value used on the wire: clamped to non-negative and widened to
    /// unsigned so it varint-encodes without zigzag.
    #[must_use]
    pub const fn to_wire(self) -> u64 {
        if self.0 < 0 {
            0
        } else {
            self.0 as u64
        }
    }

    /// Inverse of [`Timestamp::to_wire`].
    #[must_use]
    pub const fn from_wire(value: u64) -> Self {
        // Values beyond i64::MAX cannot be produced by to_wire; saturate rather
        // than wrap so a hostile peer cannot fabricate a negative instant.
        if value > i64::MAX as u64 {
            Self(i64::MAX)
        } else {
            Self(value as i64)
        }
    }

    /// Milliseconds elapsed from `earlier` to `self`, clamped at zero.
    #[must_use]
    pub const fn saturating_since(self, earlier: Timestamp) -> u64 {
        let delta = self.0 - earlier.0;
        if delta < 0 {
            0
        } else {
            delta as u64
        }
    }

    /// Adds milliseconds without panicking on overflow.
    #[must_use]
    pub const fn saturating_add_millis(self, millis: i64) -> Self {
        Self(self.0.saturating_add(millis))
    }

    /// True when `self` is at or after `other`.
    #[must_use]
    pub const fn is_at_or_after(self, other: Timestamp) -> bool {
        self.0 >= other.0
    }

    /// Renders RFC 3339 in UTC, for logs and JSON APIs.
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        let unix_ms = self.as_unix_ms();
        let seconds = unix_ms.div_euclid(1000);
        let millis = unix_ms.rem_euclid(1000);
        match time::OffsetDateTime::from_unix_timestamp(seconds) {
            Ok(dt) => format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                dt.year(),
                u8::from(dt.month()),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second(),
                millis
            ),
            // Only reachable for instants far outside the representable range.
            Err(_) => format!("+{unix_ms}ms"),
        }
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({} /* {} */)", self.0, self.to_rfc3339())
    }
}

impl Add<Duration> for Timestamp {
    type Output = Timestamp;

    fn add(self, rhs: Duration) -> Timestamp {
        Timestamp(
            self.0
                .saturating_add(rhs.as_millis().min(i64::MAX as u128) as i64),
        )
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Timestamp;

    fn sub(self, rhs: Duration) -> Timestamp {
        Timestamp(
            self.0
                .saturating_sub(rhs.as_millis().min(i64::MAX as u128) as i64),
        )
    }
}

impl Sub<Timestamp> for Timestamp {
    type Output = i64;

    /// Signed millisecond difference. Signed on purpose: clock skew between a
    /// client and a server is a real quantity and hiding its sign hides bugs.
    fn sub(self, rhs: Timestamp) -> i64 {
        self.0 - rhs.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_maps_to_zero() {
        assert_eq!(Timestamp::from_unix_ms(MIGO_EPOCH_MS), Timestamp::ZERO);
        assert_eq!(Timestamp::ZERO.as_unix_ms(), MIGO_EPOCH_MS);
        assert_eq!(Timestamp::ZERO.to_rfc3339(), "2024-01-01T00:00:00.000Z");
    }

    #[test]
    fn wire_encoding_clamps_negatives() {
        let before_epoch = Timestamp::from_millis(-5000);
        assert_eq!(before_epoch.to_wire(), 0);
        assert_eq!(Timestamp::from_wire(u64::MAX), Timestamp::MAX);
    }

    #[test]
    fn wire_round_trip() {
        for ms in [0i64, 1, 999, 86_400_000, 4_000_000_000] {
            let t = Timestamp::from_millis(ms);
            assert_eq!(Timestamp::from_wire(t.to_wire()), t);
        }
    }

    #[test]
    fn differences_keep_their_sign() {
        let a = Timestamp::from_millis(1000);
        let b = Timestamp::from_millis(2500);
        assert_eq!(b - a, 1500);
        assert_eq!(a - b, -1500);
        assert_eq!(a.saturating_since(b), 0);
        assert_eq!(b.saturating_since(a), 1500);
    }

    #[test]
    fn rfc3339_matches_a_known_instant() {
        // 2024-03-05T06:07:08.090Z
        let unix_ms = 1_709_618_828_090;
        assert_eq!(
            Timestamp::from_unix_ms(unix_ms).to_rfc3339(),
            "2024-03-05T06:07:08.090Z"
        );
    }

    #[test]
    fn duration_arithmetic_saturates() {
        let t = Timestamp::MAX;
        assert_eq!(t + Duration::from_secs(1), Timestamp::MAX);
        assert_eq!(
            Timestamp::ZERO - Duration::from_secs(1),
            Timestamp::from_millis(-1000)
        );
    }
}
