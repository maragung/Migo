//! The cache contract.
//!
//! Split by domain for the same reason the storage traits are (`migo-store`'s
//! `traits` module): a crate should be able to say exactly what it touches.
//! `migo-ratelimit` takes `Arc<dyn CounterCache>` and cannot read anybody's
//! presence.
//!
//! # What "cache" means here
//!
//! Everything behind these traits is *reconstructible*. Losing all of it must cost
//! nothing but freshness: presence goes unknown and rebuilds on the next heartbeat,
//! typing indicators vanish, session routes are re-registered on reconnect, rate
//! limit counters reset to zero. Nothing durable may live here — that is
//! `migo-store`'s job, and the split is ADR-0004.
//!
//! The consequence for callers is a rule, not a suggestion: **a cache error must
//! never fail a request that could have succeeded without the cache.** A read that
//! fails is a miss; a write that fails is a lost refresh. Both are worth a metric and
//! a log line, neither is worth a 500. The traits return `Result` because silently
//! swallowing an error inside the backend would hide a Redis outage from the very
//! metrics that are supposed to reveal it — the decision to degrade belongs to the
//! caller, who is the only one who knows what degrading means for that request.
//!
//! # Conventions
//!
//! * Every method takes `now: Timestamp` rather than reading a clock. Same reason as
//!   the storage traits: the deterministic simulator (ADR-0009) has to be able to
//!   move time by hand. The Redis backend hands the TTL to Redis, which uses its own
//!   clock, so the two backends can disagree about expiry by however much the
//!   caller's clock and Redis's differ. That is acceptable for reconstructible state
//!   and is not acceptable for anything else, which is another reason nothing
//!   durable belongs here.
//! * A lookup that finds nothing returns `Ok(None)` or an empty list. An expired
//!   entry is indistinguishable from an absent one, by design.
//! * A write is idempotent. Setting the same presence twice is one presence.
//! * A delete of something absent returns `Ok(false)`, not an error.

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};

use crate::key::CacheKey;
use crate::model::{BucketSpec, BucketVerdict, Counted, PresenceEntry, SessionRoute, Ttl};

/// Largest value the cache will accept in one entry.
///
/// 256 KiB. Large enough for a rendered leaderboard page, small enough that a
/// thousand of them do not become a Redis incident. A cache is not a file store; an
/// entry that does not fit belongs in object storage with a signed URL.
pub const MAX_VALUE_BYTES: usize = 256 * 1024;

/// Most accounts one presence read may name at once.
///
/// A contact list screen asks for everyone visible on it. 512 is above any real
/// screenful and below the point where one request's fan-out starves the others.
pub const MAX_PRESENCE_FANOUT: usize = 512;

/// Opaque bytes, keyed.
///
/// The escape hatch: anything a domain crate wants to cache for itself and that does
/// not deserve a trait of its own. Values are opaque here on purpose — a cache that
/// knows the shape of what it stores acquires opinions about it, and then two crates
/// have to agree with those opinions.
#[async_trait]
pub trait KeyValueCache: Send + Sync {
    /// Reads a value. `Ok(None)` for absent, expired, or too large to have been
    /// stored in the first place.
    async fn get(&self, key: &CacheKey, now: Timestamp) -> Result<Option<Vec<u8>>>;

    /// Writes a value, replacing any previous one and resetting its lifetime.
    ///
    /// Fails with `VALIDATION_FAILED` when `value` exceeds [`MAX_VALUE_BYTES`]. That
    /// one is the caller's bug rather than a cache outage, so it is not a
    /// degradation case: it is reported.
    async fn set(&self, key: &CacheKey, value: &[u8], ttl: Ttl, now: Timestamp) -> Result<()>;

    /// Writes only if the key is absent. Returns whether this call wrote.
    ///
    /// This is the lock and the idempotency marker. `true` means the caller holds it
    /// for `ttl`; `false` means somebody else got there first. A lock with no expiry
    /// is a deadlock waiting for a crash, which is why there is no TTL-less form.
    async fn set_if_absent(
        &self,
        key: &CacheKey,
        value: &[u8],
        ttl: Ttl,
        now: Timestamp,
    ) -> Result<bool>;

    /// Writes only if the current value is exactly `expected`. Returns whether this
    /// call wrote.
    ///
    /// `expected: None` means "only if absent", which makes this a superset of
    /// [`KeyValueCache::set_if_absent`]; both exist because the absent case is by far
    /// the most common and `SET NX` is one command where a compare is a script.
    ///
    /// The primitive that makes a read-modify-write safe across processes. Callers
    /// retry on `false` with a bounded number of attempts — unbounded retry against
    /// a hot key is a livelock.
    async fn compare_and_set(
        &self,
        key: &CacheKey,
        expected: Option<&[u8]>,
        value: &[u8],
        ttl: Ttl,
        now: Timestamp,
    ) -> Result<bool>;

    /// Removes a key. Returns whether it was there.
    async fn delete(&self, key: &CacheKey) -> Result<bool>;
}

/// Monotonic counters with a window.
///
/// The substrate for rate limiting (ADR-0006). Kept separate from
/// [`KeyValueCache`] because the operation is different in kind: an increment has to
/// be atomic across every gateway in the region, and a counter that is read, added
/// to, and written back is a counter that undercounts exactly when it matters.
#[async_trait]
pub trait CounterCache: Send + Sync {
    /// Adds `by` and returns the new value with the end of the window.
    ///
    /// The window is set when the counter is created and is *not* extended by later
    /// increments. A sliding window would let a steady stream of requests hold a
    /// counter alive forever, so that the limit never resets and a legitimate user
    /// stays blocked; a fixed window resets on schedule, which is the behaviour the
    /// `retry_after` on `RATE_LIMITED` promises.
    async fn increment(
        &self,
        key: &CacheKey,
        by: u64,
        window: Ttl,
        now: Timestamp,
    ) -> Result<Counted>;

    /// Reads a counter without touching it. Zero when absent or expired.
    async fn count(&self, key: &CacheKey, now: Timestamp) -> Result<u64>;

    /// Drops a counter. For an operator clearing a limit by hand, and for tests.
    async fn reset(&self, key: &CacheKey) -> Result<()>;
}

/// Token buckets, charged atomically.
///
/// The primitive rate limiting is built on (ADR-0006), and the reason it is a method
/// here rather than a loop in `migo-ratelimit`: taking tokens is a read-modify-write,
/// and the honest ways to do one from outside the server are a `WATCH`/`MULTI`
/// transaction or a compare-and-set retry loop. Both work and both fail in the same
/// place — a hot subject. A busy room's bucket is charged by every member sending at
/// once, so with N concurrent writers a compare-and-set succeeds about one time in N
/// and the limiter starts refusing traffic it should have allowed, which is a limiter
/// that gets *less* accurate exactly as load rises. One script is one round trip and
/// cannot contend with itself.
///
/// The bucket's shape is a parameter rather than stored state, so the caller owns the
/// policy: changing a limit takes effect on the next call with no migration and no
/// stale copy in Redis. What is stored is only the pair of numbers that cannot be
/// recomputed — how much is left, and when that was true.
#[async_trait]
pub trait TokenBucketCache: Send + Sync {
    /// Spends `cost` tokens from the bucket at `key`, if they are there.
    ///
    /// An absent bucket is a full one, so a subject that has been quiet for
    /// [`crate::model::BucketSpec::refill_millis`] starts fresh and costs no storage in
    /// the meantime.
    ///
    /// Fails with `VALIDATION_FAILED` when `cost` exceeds the bucket's capacity. That
    /// is not a refusal but an unsatisfiable request — the bucket can never hold
    /// enough, so no `retry_after` would be truthful — and it means a policy has been
    /// configured tighter than the operations it governs. Like an oversized value it is
    /// the caller's bug, so it is reported rather than degraded.
    async fn take_tokens(
        &self,
        key: &CacheKey,
        spec: BucketSpec,
        cost: u32,
        now: Timestamp,
    ) -> Result<BucketVerdict>;

    /// Reads a bucket's level without charging it. Full when absent or expired.
    ///
    /// For the operator asking why a subject is being refused, and for tests. Not for
    /// a caller deciding whether to proceed: between the peek and the charge the
    /// answer can change, and code that trusts it has reintroduced the race
    /// [`TokenBucketCache::take_tokens`] exists to remove.
    async fn peek_bucket(&self, key: &CacheKey, spec: BucketSpec, now: Timestamp) -> Result<u32>;

    /// Refills a bucket to full. For an operator lifting a limit by hand, and for
    /// tests.
    async fn clear_bucket(&self, key: &CacheKey) -> Result<()>;
}

/// Who is online, per device.
#[async_trait]
pub trait PresenceCache: Send + Sync {
    /// Records or refreshes one device's presence.
    ///
    /// The caller supplies `entry.expires_at`; `ttl` is what the backend gives the
    /// containing key. They are usually the same span and the caller is not required
    /// to keep them so: a backend TTL longer than the entry's own deadline only
    /// means the storage is reclaimed a little later than the fact expires.
    async fn set_presence(&self, entry: PresenceEntry, ttl: Ttl, now: Timestamp) -> Result<()>;

    /// Every live presence entry for one account, in no guaranteed order.
    async fn presence(&self, account_id: Id, now: Timestamp) -> Result<Vec<PresenceEntry>>;

    /// The same for several accounts at once.
    ///
    /// One call rather than a loop because this is the contact-list read and a loop
    /// there is one round trip per contact. Ids beyond [`MAX_PRESENCE_FANOUT`] are
    /// ignored rather than rejected: a truncated contact list renders, a failed one
    /// does not.
    async fn presence_many(&self, account_ids: &[Id], now: Timestamp)
        -> Result<Vec<PresenceEntry>>;

    /// Forgets one device's presence. Called on a clean disconnect, so that going
    /// offline is immediate rather than a TTL away.
    async fn clear_presence(&self, account_id: Id, device_id: Id) -> Result<()>;
}

/// Who is typing, per conversation.
///
/// Typing is the shortest-lived state in the system and the highest-volume: a
/// keystroke-driven event with a TTL measured in seconds. It gets its own trait
/// because it needs none of presence's structure — there is no state to carry, only
/// the fact and its deadline.
#[async_trait]
pub trait TypingCache: Send + Sync {
    /// Marks an account as typing in a conversation until `ttl` elapses.
    async fn set_typing(
        &self,
        conversation_id: Id,
        account_id: Id,
        ttl: Ttl,
        now: Timestamp,
    ) -> Result<()>;

    /// Everyone currently typing in a conversation, in no guaranteed order.
    async fn typing(&self, conversation_id: Id, now: Timestamp) -> Result<Vec<Id>>;

    /// Clears the mark, for the client that stopped typing before its TTL ran out.
    async fn clear_typing(&self, conversation_id: Id, account_id: Id) -> Result<()>;
}

/// Which node holds which socket.
#[async_trait]
pub trait RoutingCache: Send + Sync {
    /// Registers or refreshes a device's socket location.
    ///
    /// Last writer wins. A device that reconnects to another gateway while the old
    /// binding is still live must end up pointing at the new one, because the old
    /// socket is already gone.
    async fn bind_session(&self, route: SessionRoute, ttl: Ttl, now: Timestamp) -> Result<()>;

    /// Where one device's socket is, if anywhere.
    async fn route(&self, device_id: Id, now: Timestamp) -> Result<Option<SessionRoute>>;

    /// Every live route for one account. The fan-out set for a push.
    async fn routes_of_account(&self, account_id: Id, now: Timestamp) -> Result<Vec<SessionRoute>>;

    /// Removes a binding, but only if `node_id` still owns it. Returns whether it
    /// removed anything.
    ///
    /// The guard is the point of the method. A gateway that loses a socket and then
    /// unbinds it unconditionally will, if the device has meanwhile reconnected
    /// elsewhere, delete the *new* binding and silently strand a connected device.
    /// Checking the owner first turns that race into a no-op.
    async fn unbind_session(&self, device_id: Id, account_id: Id, node_id: &str) -> Result<bool>;
}

/// Everything, for the composition root.
///
/// Domain crates take the narrow traits. This one exists so `migod` can build one
/// object, hand out slices of it, and ask it whether it is up.
#[async_trait]
pub trait Cache:
    KeyValueCache
    + CounterCache
    + TokenBucketCache
    + PresenceCache
    + TypingCache
    + RoutingCache
    + Send
    + Sync
{
    /// Backend name, for the startup banner and metric labels.
    fn backend_name(&self) -> &'static str;

    /// Round-trips one command. Used by the readiness probe.
    ///
    /// Cache health is reported separately from overall readiness on purpose: a node
    /// whose cache is down is degraded, not unready. Taking it out of the load
    /// balancer would turn a Redis outage into a total outage, which is exactly the
    /// failure this whole layer is designed not to have (brief section 5240).
    async fn health(&self) -> Result<()>;

    /// Drops entries that expired at or before `now`, returning how many.
    ///
    /// Redis does this itself and returns zero. The in-memory backend needs a caller:
    /// a presence entry for a device that never comes back is read by nobody, so
    /// lazy expiry on read never reclaims it, and a long-running simulation or a
    /// single-node development deployment would grow without bound.
    async fn sweep(&self, now: Timestamp) -> Result<usize>;
}
