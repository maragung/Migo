//! Buckets that live in this process, for when the shared ones cannot be reached.
//!
//! ADR-0006: buckets live in Redis when it is there and fall back to conservative local
//! buckets when it is not — "degraded but never open". This is the degraded half.
//!
//! # What is degraded about it
//!
//! Two things, and both are visible in the name.
//!
//! *It is local.* Every node keeps its own copy, so a subject spread across N nodes gets
//! N budgets. That is why [`crate::policy::Policies::degraded`] divides the rate before
//! it gets here: without the division, losing Redis would make the system permit *more*
//! traffic than it does healthy, and a limiter that loosens under stress is worse than no
//! limiter, because nobody expects it.
//!
//! *It starts empty.* At the moment Redis goes away every local bucket is absent, and an
//! absent bucket is a full one, so the first burst after the failure gets one full
//! tightened bucket per subject before the local limits bite. The alternative is writing
//! to both stores on every request forever so that the local copy is warm on the day it
//! is needed — doubling the work of the healthy path, which is nearly all of the time, to
//! improve the first second of an outage. It is the wrong trade and this is the reasoning
//! behind not making it.
//!
//! # The arithmetic is not duplicated
//!
//! `BucketState::charge` from `migo-cache` does the refill and the spend here exactly as
//! it does for the in-memory backend and as the Lua script does inside Redis. Three
//! copies of a token bucket would be three chances to round differently, and a fallback
//! that disagrees with the primary is a fallback that changes the answer when it engages.

use std::collections::HashMap;

use migo_cache::key::CacheKey;
use migo_cache::model::{BucketSpec, BucketState, BucketVerdict, Expiring};
use migo_core::Timestamp;
use parking_lot::Mutex;

/// Most subjects one node will track locally.
///
/// A request charges up to five surfaces, so this is roughly fifty thousand concurrent
/// subjects — the order of magnitude of one gateway's socket budget. The memory is about
/// twenty megabytes at the cap, which is affordable for a mode that only exists during an
/// incident.
pub(crate) const MAX_LOCAL_BUCKETS: usize = 262_144;

/// The fallback store.
///
/// One mutex over one map rather than a sharded structure: this is only ever on the path
/// while Redis is down, and a node in that state is not throughput-bound on a hash map —
/// it is doing everything else degraded too. A lock-free design here would be complexity
/// paid for permanently to speed up something rare.
pub(crate) struct LocalBuckets {
    inner: Mutex<HashMap<String, Expiring<BucketState>>>,
    max: usize,
}

impl LocalBuckets {
    pub(crate) fn new() -> Self {
        Self::with_capacity(MAX_LOCAL_BUCKETS)
    }

    /// The same, with a different ceiling.
    ///
    /// Exists so the full-map behaviour can be exercised with two buckets instead of a
    /// quarter of a million. A ceiling that can only be reached by allocating twenty
    /// megabytes is a ceiling nobody tests, and the code behind it is the code that runs
    /// when a node is already having its worst day.
    pub(crate) fn with_capacity(max: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max: max.max(1),
        }
    }

    /// Charges a local bucket, or reports that there is no room to track a new one.
    ///
    /// `None` means the map is full of buckets that have not expired yet, which takes
    /// either a very large node or a flood from a very large number of distinct subjects.
    /// The caller refuses the request in that case. Refusing is the conservative
    /// direction and it is also the honest one: the alternative is to stop tracking
    /// subjects, and a limiter that has stopped tracking is a limiter that has stopped
    /// limiting while still reporting success.
    ///
    /// Subjects already in the map keep working when it is full, so an outage that
    /// saturates the fallback degrades for arrivals rather than for everybody.
    pub(crate) fn charge(
        &self,
        key: &CacheKey,
        spec: BucketSpec,
        cost: u32,
        now: Timestamp,
    ) -> Option<BucketVerdict> {
        let mut map = self.inner.lock();
        let existing = map
            .get(key.as_str())
            .filter(|entry| !entry.is_expired(now))
            .map(|entry| entry.value);
        let stored = existing.unwrap_or_else(|| BucketState::full(spec, now));
        let (next, verdict) = stored.charge(spec, cost, now);
        if verdict.taken {
            // Nothing is written on a refusal, the same as the shared backends. A flood
            // therefore cannot fill this map with the buckets that are bouncing it.
            if existing.is_none() && map.len() >= self.max {
                // Sweeping is O(n) and runs only when a new subject arrives at a full
                // map, which after one sweep leaves room for many more arrivals. There
                // is no background sweeper because there is no clock here: `now` is the
                // caller's, by the same convention as every cache method (ADR-0009).
                map.retain(|_, entry| !entry.is_expired(now));
                if map.len() >= self.max {
                    return None;
                }
            }
            map.insert(
                key.as_str().to_string(),
                Expiring::new(next, spec.state_ttl().deadline(now)),
            );
        }
        Some(verdict)
    }

    /// A bucket's balance, without charging it. Full when absent.
    pub(crate) fn peek(&self, key: &CacheKey, spec: BucketSpec, now: Timestamp) -> u32 {
        let map = self.inner.lock();
        map.get(key.as_str())
            .filter(|entry| !entry.is_expired(now))
            .map_or(spec.capacity(), |entry| {
                entry.value.refilled(spec, now).whole_tokens()
            })
    }

    /// Forgets a bucket, which refills it.
    pub(crate) fn clear(&self, key: &CacheKey) {
        self.inner.lock().remove(key.as_str());
    }

    /// How many buckets are held, expired ones included. For tests and for a debug
    /// endpoint.
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().len()
    }
}

impl Default for LocalBuckets {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_cache::key::{CacheKey, SCOPE_BUCKET};

    fn key(name: &str) -> CacheKey {
        CacheKey::new(SCOPE_BUCKET, name)
    }

    fn start() -> Timestamp {
        Timestamp::from_millis(1_000_000)
    }

    /// Ten tokens, one per second, so a bucket's state lives eleven seconds.
    fn spec() -> BucketSpec {
        BucketSpec::new(10, 1)
    }

    #[test]
    fn an_absent_bucket_is_a_full_one() {
        let buckets = LocalBuckets::new();
        assert_eq!(buckets.peek(&key("a"), spec(), start()), 10);
        assert_eq!(
            buckets.len(),
            0,
            "peeking must not create state: a metrics scrape that walked every subject \
             would otherwise populate the map it was measuring"
        );
    }

    #[test]
    fn nothing_is_written_on_a_refusal() {
        let buckets = LocalBuckets::with_capacity(8);
        let subject = key("spender");

        let verdict = buckets
            .charge(&subject, spec(), 10, start())
            .expect("room for one");
        assert!(verdict.taken);
        assert_eq!(buckets.len(), 1);

        let refused = buckets
            .charge(&subject, spec(), 10, start())
            .expect("a refusal is still an answer");
        assert!(!refused.taken);
        assert_eq!(
            buckets.len(),
            1,
            "a refusal must not touch the map, or a flood would fill it with the very \
             buckets that are bouncing the flood"
        );
    }

    #[test]
    fn a_full_map_keeps_serving_the_subjects_it_already_knows() {
        let buckets = LocalBuckets::with_capacity(2);
        for name in ["first", "second"] {
            assert!(
                buckets
                    .charge(&key(name), spec(), 1, start())
                    .expect("room")
                    .taken
            );
        }

        assert!(
            buckets.charge(&key("third"), spec(), 1, start()).is_none(),
            "an arrival that cannot be tracked must be reported, not waved through: a \
             limiter that has stopped tracking has stopped limiting"
        );
        assert!(
            buckets
                .charge(&key("first"), spec(), 1, start())
                .expect("a known subject is unaffected")
                .taken,
            "saturation degrades for arrivals, not for everybody"
        );
    }

    #[test]
    fn a_full_map_makes_room_when_its_buckets_have_recovered() {
        let buckets = LocalBuckets::with_capacity(2);
        for name in ["first", "second"] {
            buckets
                .charge(&key(name), spec(), 1, start())
                .expect("room");
        }
        assert!(buckets.charge(&key("third"), spec(), 1, start()).is_none());

        // Eleven seconds is this spec's state lifetime: by then both buckets have
        // refilled, so their stored state says nothing a full bucket would not.
        let later = start().saturating_add_millis(11_001);
        assert!(
            buckets
                .charge(&key("third"), spec(), 1, later)
                .expect("the recovered buckets were reclaimed")
                .taken
        );
        assert!(
            buckets.len() <= 2,
            "the sweep has to actually remove them, not just count them"
        );
    }

    #[test]
    fn clearing_a_bucket_refills_it() {
        let buckets = LocalBuckets::new();
        let subject = key("cleared");
        buckets.charge(&subject, spec(), 10, start()).expect("room");
        assert_eq!(buckets.peek(&subject, spec(), start()), 0);

        buckets.clear(&subject);

        assert_eq!(buckets.peek(&subject, spec(), start()), 10);
    }
}
