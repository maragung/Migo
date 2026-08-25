//! Values the cache carries, and how they are encoded.
//!
//! The encoding is [`migo_wire`], the same codec the protocol uses. Reusing it means
//! one encoder in the process rather than two, and it means cache values get the
//! benefit of the codec's own fuzzing. It is also compact, which matters: a presence
//! hash for a user with four devices is read every time one of their contacts opens
//! the app.
//!
//! Every encoded value carries its own expiry as well as being written with a TTL.
//! That looks redundant and is not: presence and typing live in Redis hashes, hash
//! fields have no individual expiry on the versions Migo supports, and the
//! alternative — one key per device — turns the hottest read in the system into a
//! fan-out. So the hash gets a TTL that bounds the leak, and the reader drops
//! individual fields that have expired.

use migo_core::{Id, Result, Timestamp};
use migo_protocol::{fault, PresenceState};
use migo_wire::{Reader, Writer};

/// Longest lifetime any cache entry may be given.
///
/// Seven days. Nothing ephemeral in Migo legitimately outlives a week, and a TTL
/// that can be set to "never" is a leak with a configuration step in front of it.
pub const MAX_TTL_MS: u32 = 7 * 24 * 60 * 60 * 1000;

/// Shortest lifetime any cache entry may be given.
///
/// One millisecond. A zero TTL means "expired on arrival", which no caller wants and
/// which Redis rejects outright, so it is clamped rather than passed through.
pub const MIN_TTL_MS: u32 = 1;

/// How long an entry lives.
///
/// A separate type from `Duration` because the range is clamped and the unit is
/// fixed: `Ttl` is always milliseconds, always inside [`MIN_TTL_MS`]`..=`[`MAX_TTL_MS`],
/// and therefore always safe to hand to `PX`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ttl(u32);

impl Ttl {
    /// Clamps `millis` into the allowed range.
    #[must_use]
    pub const fn from_millis(millis: u32) -> Self {
        if millis < MIN_TTL_MS {
            Self(MIN_TTL_MS)
        } else if millis > MAX_TTL_MS {
            Self(MAX_TTL_MS)
        } else {
            Self(millis)
        }
    }

    /// Clamps `seconds` into the allowed range.
    #[must_use]
    pub const fn from_seconds(seconds: u32) -> Self {
        Self::from_millis(seconds.saturating_mul(1000))
    }

    /// The lifetime in milliseconds.
    #[must_use]
    pub const fn as_millis(self) -> u32 {
        self.0
    }

    /// The instant an entry written at `now` stops being visible.
    #[must_use]
    pub const fn deadline(self, now: Timestamp) -> Timestamp {
        now.saturating_add_millis(self.0 as i64)
    }
}

/// One device's presence, as the cache holds it.
///
/// Presence is per device, not per account: a user online on a phone and away on a
/// laptop is two facts, and collapsing them in storage means the collapse rule can
/// never be changed without a migration. The account-level answer is computed by
/// whoever reads this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceEntry {
    /// Whose presence this is.
    pub account_id: Id,
    /// Which of their devices reported it.
    pub device_id: Id,
    /// What it reported.
    pub state: PresenceState,
    /// When the device entered this state. Survives refreshes, so "online since"
    /// does not reset every time the heartbeat lands.
    pub since: Timestamp,
    /// When this entry stops counting. See the module note on why it is stored.
    pub expires_at: Timestamp,
}

impl PresenceEntry {
    /// True when `now` is at or past [`PresenceEntry::expires_at`].
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now.is_at_or_after(self.expires_at)
    }

    /// Encodes everything except `device_id`, which is the hash field and would
    /// otherwise be stored twice.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut writer = Writer::new();
        writer.write_id(&self.account_id);
        writer.write_u32(self.state.to_wire());
        writer.write_timestamp(self.since);
        writer.write_timestamp(self.expires_at);
        writer.finish_vec().map_err(encode_failed)
    }

    /// Inverse of [`PresenceEntry::encode`]. `device_id` comes from the hash field.
    pub fn decode(device_id: Id, bytes: &[u8]) -> Result<Self> {
        let mut reader = Reader::from_slice(bytes);
        let account_id = reader.read_id().map_err(decode_failed)?;
        let state = PresenceState::from_wire(reader.read_u32().map_err(decode_failed)?);
        let since = reader.read_timestamp().map_err(decode_failed)?;
        let expires_at = reader.read_timestamp().map_err(decode_failed)?;
        Ok(Self {
            account_id,
            device_id,
            state,
            since,
            expires_at,
        })
    }
}

/// Where a connected device's socket lives.
///
/// The gateway that holds the socket is the only process that can push to it, so
/// every fan-out consults this. It is ephemeral by nature: if it is lost, the next
/// heartbeat rebuilds it, and in the meantime a push is dropped rather than
/// misdelivered — which is why losing Redis costs presence and typing but never a
/// message (brief section 138 holds the frame until it is acknowledged).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRoute {
    /// Whose session this is.
    pub account_id: Id,
    /// Which device is connected.
    pub device_id: Id,
    /// The node holding the socket, as `NodeConfig::id` spells it.
    pub node_id: String,
    /// When the socket was established.
    pub connected_at: Timestamp,
    /// When this binding stops being believed. Refreshed by the heartbeat.
    pub expires_at: Timestamp,
}

impl SessionRoute {
    /// True when `now` is at or past [`SessionRoute::expires_at`].
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now.is_at_or_after(self.expires_at)
    }

    /// Encodes the whole route, `device_id` included.
    ///
    /// Unlike [`PresenceEntry`] the device id is written out, because a route is
    /// stored twice — once under its own key and once as a field of the account
    /// index — and a value that decodes the same from either place is worth four
    /// bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut writer = Writer::new();
        writer.write_id(&self.account_id);
        writer.write_id(&self.device_id);
        writer.write_str(&self.node_id).map_err(encode_failed)?;
        writer.write_timestamp(self.connected_at);
        writer.write_timestamp(self.expires_at);
        writer.finish_vec().map_err(encode_failed)
    }

    /// Inverse of [`SessionRoute::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = Reader::from_slice(bytes);
        let account_id = reader.read_id().map_err(decode_failed)?;
        let device_id = reader.read_id().map_err(decode_failed)?;
        let node_id = reader.read_string().map_err(decode_failed)?;
        let connected_at = reader.read_timestamp().map_err(decode_failed)?;
        let expires_at = reader.read_timestamp().map_err(decode_failed)?;
        Ok(Self {
            account_id,
            device_id,
            node_id,
            connected_at,
            expires_at,
        })
    }
}

/// The result of an increment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counted {
    /// The counter's value after this increment.
    pub value: u64,
    /// When the window ends.
    ///
    /// Returned so a caller that has to reject can say `retry_after` without a
    /// second round trip, which is the whole point of returning it.
    pub expires_at: Timestamp,
}

/// The shape of a token bucket: how much it holds and how fast it refills.
///
/// Both halves are clamped to at least one. A capacity of zero rejects everything
/// forever and a refill of zero never recovers from the first rejection; neither is a
/// limit, both are outages, and an operator who types a zero means "turn this off",
/// which is done by not consulting the limiter rather than by configuring a wall.
///
/// # Why milli-tokens
///
/// The arithmetic below counts thousandths of a token, for one reason that makes the
/// whole thing exact: a bucket refilling at `r` tokens per second gains exactly `r`
/// milli-tokens per millisecond. Refill is therefore a multiplication by elapsed
/// milliseconds with no division and no rounding, so a bucket charged a thousand
/// times in a second holds precisely the same value as one charged once — which is
/// not true of any scheme that rounds per operation, and it is the difference between
/// a limit that holds and one that drifts open under load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BucketSpec {
    capacity: u32,
    refill_per_second: u32,
}

impl BucketSpec {
    /// A bucket holding `capacity` tokens, refilling at `refill_per_second`.
    #[must_use]
    pub const fn new(capacity: u32, refill_per_second: u32) -> Self {
        Self {
            capacity: if capacity == 0 { 1 } else { capacity },
            refill_per_second: if refill_per_second == 0 {
                1
            } else {
                refill_per_second
            },
        }
    }

    /// Whole tokens the bucket holds when full.
    #[must_use]
    pub const fn capacity(self) -> u32 {
        self.capacity
    }

    /// Tokens restored per second.
    #[must_use]
    pub const fn refill_per_second(self) -> u32 {
        self.refill_per_second
    }

    /// Capacity in milli-tokens.
    #[must_use]
    pub const fn capacity_milli(self) -> u64 {
        self.capacity as u64 * 1000
    }

    /// Milliseconds for an empty bucket to become full again.
    #[must_use]
    pub const fn refill_millis(self) -> u64 {
        self.capacity_milli() / self.refill_per_second as u64
    }

    /// Fails when `cost` could never fit, whatever the caller waits.
    ///
    /// A bucket that cannot hold the price of one operation refuses it forever, and
    /// every `retry_after` it could return would be a lie. That is a configuration
    /// fault — a policy tuned tighter than the operations it governs — so it is
    /// reported as validation rather than dressed up as a rate limit. Checked before
    /// any round trip, in both backends, because the answer does not depend on state.
    pub fn check_affordable(self, cost: u32) -> Result<()> {
        if cost > self.capacity {
            return Err(fault::validation(
                "cost",
                &format!(
                    "costs {cost} tokens but the bucket holds only {}: no wait would make \
                     this affordable, so the policy is too tight for the operation",
                    self.capacity
                ),
            ));
        }
        Ok(())
    }

    /// How long the backend should keep the bucket's state.
    ///
    /// Exactly as long as the state still says something, and not one millisecond
    /// longer: a bucket that has had [`BucketSpec::refill_millis`] to recover is full,
    /// and a full bucket is indistinguishable from one that was never written. So the
    /// state expires on its own, unwritten buckets cost nothing, and no sweeper is
    /// needed for the largest keyspace in the system.
    ///
    /// The extra second is for clock skew. The deadline is enforced by the backend's
    /// clock while the refill is computed from the caller's, and dropping the state a
    /// moment early would hand back a full bucket a moment early.
    #[must_use]
    pub fn state_ttl(self) -> Ttl {
        Ttl::from_millis(
            u32::try_from(self.refill_millis().saturating_add(1000)).unwrap_or(u32::MAX),
        )
    }
}

/// What one attempt to take tokens did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BucketVerdict {
    /// Whether the tokens were taken. When false, nothing was charged.
    pub taken: bool,
    /// Whole tokens left in the bucket after the attempt.
    pub remaining: u32,
    /// How long until the same request would be affordable. Zero when `taken`.
    ///
    /// A promise, not an estimate: the bucket refills on a schedule that no other
    /// caller can slow down, so waiting this long guarantees the tokens exist —
    /// though not that somebody else has not spent them first.
    pub retry_after_ms: u32,
}

impl BucketVerdict {
    /// A successful take.
    #[must_use]
    pub const fn taken(remaining: u32) -> Self {
        Self {
            taken: true,
            remaining,
            retry_after_ms: 0,
        }
    }

    /// A refusal, with the wait that fixes it.
    #[must_use]
    pub const fn refused(remaining: u32, retry_after_ms: u32) -> Self {
        Self {
            taken: false,
            remaining,
            retry_after_ms,
        }
    }
}

/// A token bucket as a backend holds it.
///
/// Public because both backends need it and because the refill rule is the one piece
/// of arithmetic in this crate that a test wants to reach directly. The Redis backend
/// stores the same two numbers as a hash rather than these bytes: Lua has to read them
/// to be atomic, and Lua cannot decode the wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BucketState {
    /// Tokens available, in thousandths.
    pub milli_tokens: u64,
    /// When `milli_tokens` was last accurate.
    pub updated_at: Timestamp,
}

impl BucketState {
    /// A bucket nobody has spent from yet.
    #[must_use]
    pub const fn full(spec: BucketSpec, now: Timestamp) -> Self {
        Self {
            milli_tokens: spec.capacity_milli(),
            updated_at: now,
        }
    }

    /// The state brought forward to `now`, without charging anything.
    ///
    /// Time running backwards refills nothing and rewinds nothing. It happens — an
    /// NTP step, a caller on another node whose clock is behind — and the safe
    /// reading of a timestamp from the future is that the bucket is already accurate
    /// up to that point, so the next call that arrives after it simply refills less.
    /// Trusting the earlier clock instead would grant tokens twice for the same
    /// millisecond.
    ///
    /// The cap is applied even when no time has passed, which is what makes the spec a
    /// parameter rather than stored state: an operator who narrows a limit has it
    /// enforced on the very next call. Clamping only alongside a refill would leave
    /// whatever the old, wider policy had banked spendable until the clock moved.
    #[must_use]
    pub fn refilled(self, spec: BucketSpec, now: Timestamp) -> Self {
        let elapsed = now.saturating_since(self.updated_at);
        let gained = elapsed.saturating_mul(u64::from(spec.refill_per_second()));
        Self {
            milli_tokens: self
                .milli_tokens
                .saturating_add(gained)
                .min(spec.capacity_milli()),
            updated_at: if elapsed == 0 { self.updated_at } else { now },
        }
    }

    /// Whole tokens available, rounding down.
    #[must_use]
    pub const fn whole_tokens(self) -> u32 {
        let whole = self.milli_tokens / 1000;
        if whole > u32::MAX as u64 {
            u32::MAX
        } else {
            whole as u32
        }
    }

    /// Refills, then spends `cost` tokens if they are there.
    ///
    /// Returns the state to persist and the verdict. On a refusal the returned state
    /// is the *original*, not the refilled one, and that is deliberate: refill is a
    /// pure function of the stored timestamp, so recomputing it next time gives the
    /// same answer, and skipping the write makes a refusal one round trip instead of
    /// two. Under a flood — precisely when refusals are the common case — that halves
    /// the load the limiter itself puts on Redis.
    #[must_use]
    pub fn charge(self, spec: BucketSpec, cost: u32, now: Timestamp) -> (Self, BucketVerdict) {
        let refilled = self.refilled(spec, now);
        let cost_milli = u64::from(cost) * 1000;
        if refilled.milli_tokens < cost_milli {
            let deficit = cost_milli - refilled.milli_tokens;
            let wait = deficit.div_ceil(u64::from(spec.refill_per_second()));
            return (
                self,
                BucketVerdict::refused(
                    refilled.whole_tokens(),
                    u32::try_from(wait).unwrap_or(u32::MAX),
                ),
            );
        }
        let spent = Self {
            milli_tokens: refilled.milli_tokens - cost_milli,
            updated_at: refilled.updated_at,
        };
        (spent, BucketVerdict::taken(spent.whole_tokens()))
    }
}

/// A cache value with its deadline, as the in-memory backend stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expiring<T> {
    /// The value.
    pub value: T,
    /// When it stops being visible.
    pub expires_at: Timestamp,
}

impl<T> Expiring<T> {
    /// Pairs a value with its deadline.
    pub const fn new(value: T, expires_at: Timestamp) -> Self {
        Self { value, expires_at }
    }

    /// True when `now` is at or past the deadline.
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now.is_at_or_after(self.expires_at)
    }
}

/// A value that would not encode. Not the caller's fault, so it reports as a cache
/// failure rather than as validation.
fn encode_failed(source: migo_wire::WireError) -> migo_core::Error {
    fault::cache(format!("encoding a cache value: {source}"))
}

/// A value that would not decode.
///
/// Reachable in production without anybody doing anything wrong: a rolling deploy
/// that changed a layout leaves the old shape in Redis for one TTL. Callers treat
/// this the same as a miss, which is why it must not be fatal.
fn decode_failed(source: migo_wire::WireError) -> migo_core::Error {
    fault::cache(format!("decoding a cache value: {source}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    #[test]
    fn a_ttl_is_clamped_at_both_ends() {
        assert_eq!(Ttl::from_millis(0).as_millis(), MIN_TTL_MS);
        assert_eq!(Ttl::from_millis(u32::MAX).as_millis(), MAX_TTL_MS);
        assert_eq!(Ttl::from_seconds(30).as_millis(), 30_000);
        // Seconds that would overflow the millisecond product clamp, not wrap.
        assert_eq!(Ttl::from_seconds(u32::MAX).as_millis(), MAX_TTL_MS);
    }

    #[test]
    fn a_deadline_is_the_ttl_after_now() {
        assert_eq!(Ttl::from_seconds(60).deadline(ts(1_000)), ts(61_000));
    }

    #[test]
    fn presence_survives_a_round_trip() {
        let entry = PresenceEntry {
            account_id: Id::from(7u128),
            device_id: Id::from(9u128),
            state: PresenceState::Away,
            since: ts(1_000),
            expires_at: ts(61_000),
        };
        let bytes = entry.encode().unwrap();
        assert_eq!(
            PresenceEntry::decode(entry.device_id, &bytes).unwrap(),
            entry
        );
    }

    #[test]
    fn a_route_survives_a_round_trip() {
        let route = SessionRoute {
            account_id: Id::from(7u128),
            device_id: Id::from(9u128),
            node_id: "gateway-sg-1".to_string(),
            connected_at: ts(1_000),
            expires_at: ts(61_000),
        };
        let bytes = route.encode().unwrap();
        assert_eq!(SessionRoute::decode(&bytes).unwrap(), route);
    }

    #[test]
    fn a_truncated_value_is_a_cache_error_not_a_panic() {
        let route = SessionRoute {
            account_id: Id::from(7u128),
            device_id: Id::from(9u128),
            node_id: "gateway-sg-1".to_string(),
            connected_at: ts(1_000),
            expires_at: ts(61_000),
        };
        let bytes = route.encode().unwrap();
        for cut in 0..bytes.len() {
            // Every prefix, because a rolling deploy or a half-written value can
            // produce any of them and none of them may take the process down.
            let _ = SessionRoute::decode(&bytes[..cut]);
        }
        assert!(SessionRoute::decode(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn an_unknown_presence_state_decodes_rather_than_failing() {
        // Forward compatibility: an older node reading a state a newer node wrote
        // must see `Unknown`, not an error, or a rolling deploy becomes an outage.
        let mut writer = Writer::new();
        writer.write_id(&Id::from(7u128));
        writer.write_u32(250);
        writer.write_timestamp(ts(1_000));
        writer.write_timestamp(ts(61_000));
        let bytes = writer.finish_vec().unwrap();
        let entry = PresenceEntry::decode(Id::from(9u128), &bytes).unwrap();
        assert_eq!(entry.state, PresenceState::Unknown);
    }

    #[test]
    fn expiry_is_inclusive_of_the_deadline() {
        let entry = Expiring::new((), ts(1_000));
        assert!(!entry.is_expired(ts(999)));
        // At the deadline, not after it: a TTL of 60s means the entry is gone at
        // t+60s, which is what a caller comparing against a wall clock assumes.
        assert!(entry.is_expired(ts(1_000)));
    }
}
