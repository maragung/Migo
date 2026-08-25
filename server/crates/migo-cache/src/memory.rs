//! The in-process cache backend.
//!
//! Used by tests, by the deterministic simulator (ADR-0009), and by a single-node
//! development deployment that should not need a Redis to boot. It is also the
//! reference implementation: when the two backends disagree, this one is the
//! statement of what the contract meant, because it is the one you can read.
//!
//! One lock over one struct, rather than a lock per map. The maps are small, the
//! critical sections are a few hash lookups, and a shared cache backend is not where
//! a single-node deployment's contention lives. `parking_lot` because its `RwLock`
//! has no poisoning: a panic while holding this lock should not turn every later
//! cache read into an error.

use std::collections::HashMap;

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::fault;
use parking_lot::RwLock;

use crate::key::CacheKey;
use crate::model::{
    BucketSpec, BucketState, BucketVerdict, Counted, Expiring, PresenceEntry, SessionRoute, Ttl,
};
use crate::traits::{
    Cache, CounterCache, KeyValueCache, PresenceCache, RoutingCache, TokenBucketCache, TypingCache,
    MAX_PRESENCE_FANOUT, MAX_VALUE_BYTES,
};

/// A group of fields that share a reclamation deadline.
///
/// This is the in-memory shape of a Redis hash, and it exists to reproduce Redis's
/// behaviour rather than to improve on it. Visibility of a field is decided by the
/// field's own deadline; `reclaim_at` decides only when the storage goes away. In
/// Redis those are two different mechanisms — a value inside the field, and the key's
/// TTL — and a backend that collapsed them would be visibly better on its own and
/// wrong as half of a pair.
#[derive(Debug)]
struct Bucket<V> {
    fields: HashMap<Id, V>,
    reclaim_at: Timestamp,
}

impl<V> Bucket<V> {
    fn new(reclaim_at: Timestamp) -> Self {
        Self {
            fields: HashMap::new(),
            reclaim_at,
        }
    }

    /// Pushes the reclamation deadline out, never in.
    ///
    /// A device with a 10-second presence TTL must not shorten the lifetime of the
    /// hash a sibling device just refreshed for 60.
    fn extend_to(&mut self, deadline: Timestamp) {
        if deadline.is_at_or_after(self.reclaim_at) {
            self.reclaim_at = deadline;
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    kv: HashMap<String, Expiring<Vec<u8>>>,
    counters: HashMap<String, Expiring<u64>>,
    /// Token buckets. The deadline is the bucket's own recovery time, so an entry
    /// disappears at the moment it would have read as full anyway.
    token_buckets: HashMap<String, Expiring<BucketState>>,
    /// account -> device -> presence.
    presence: HashMap<Id, Bucket<PresenceEntry>>,
    /// conversation -> account -> when the mark expires.
    typing: HashMap<Id, Bucket<Timestamp>>,
    /// device -> route. The authoritative copy.
    routes: HashMap<Id, SessionRoute>,
    /// account -> devices. An index, holding ids only, so a route cannot be stale in
    /// one place and current in the other.
    routes_by_account: HashMap<Id, Bucket<()>>,
}

/// A cache that lives in this process and dies with it.
#[derive(Debug, Default)]
pub struct MemoryCache {
    inner: RwLock<Inner>,
}

impl MemoryCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live entries across every namespace. For tests and for the
    /// development metrics endpoint.
    #[must_use]
    pub fn len(&self) -> usize {
        let inner = self.inner.read();
        inner.kv.len()
            + inner.counters.len()
            + inner.token_buckets.len()
            + inner
                .presence
                .values()
                .map(|b| b.fields.len())
                .sum::<usize>()
            + inner.typing.values().map(|b| b.fields.len()).sum::<usize>()
            + inner.routes.len()
    }

    /// True when nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Rejects an oversized value.
///
/// A separate function because both `set` and the compare-and-set path need it and
/// the message has to be identical: a caller diagnosing a rejection should not have
/// to work out which entry point produced it.
fn check_value(value: &[u8]) -> Result<()> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(fault::validation(
            "value",
            &format!(
                "is {} bytes, over the {MAX_VALUE_BYTES} byte cache limit",
                value.len()
            ),
        ));
    }
    Ok(())
}

#[async_trait]
impl KeyValueCache for MemoryCache {
    async fn get(&self, key: &CacheKey, now: Timestamp) -> Result<Option<Vec<u8>>> {
        let inner = self.inner.read();
        Ok(inner
            .kv
            .get(key.as_str())
            .filter(|entry| !entry.is_expired(now))
            .map(|entry| entry.value.clone()))
    }

    async fn set(&self, key: &CacheKey, value: &[u8], ttl: Ttl, now: Timestamp) -> Result<()> {
        check_value(value)?;
        let mut inner = self.inner.write();
        inner.kv.insert(
            key.as_str().to_string(),
            Expiring::new(value.to_vec(), ttl.deadline(now)),
        );
        Ok(())
    }

    async fn set_if_absent(
        &self,
        key: &CacheKey,
        value: &[u8],
        ttl: Ttl,
        now: Timestamp,
    ) -> Result<bool> {
        check_value(value)?;
        let mut inner = self.inner.write();
        let occupied = inner
            .kv
            .get(key.as_str())
            .is_some_and(|entry| !entry.is_expired(now));
        if occupied {
            return Ok(false);
        }
        inner.kv.insert(
            key.as_str().to_string(),
            Expiring::new(value.to_vec(), ttl.deadline(now)),
        );
        Ok(true)
    }

    async fn compare_and_set(
        &self,
        key: &CacheKey,
        expected: Option<&[u8]>,
        value: &[u8],
        ttl: Ttl,
        now: Timestamp,
    ) -> Result<bool> {
        check_value(value)?;
        let mut inner = self.inner.write();
        let current = inner
            .kv
            .get(key.as_str())
            .filter(|entry| !entry.is_expired(now))
            .map(|entry| entry.value.as_slice());
        if current != expected {
            return Ok(false);
        }
        inner.kv.insert(
            key.as_str().to_string(),
            Expiring::new(value.to_vec(), ttl.deadline(now)),
        );
        Ok(true)
    }

    async fn delete(&self, key: &CacheKey) -> Result<bool> {
        let mut inner = self.inner.write();
        Ok(inner.kv.remove(key.as_str()).is_some())
    }
}

#[async_trait]
impl CounterCache for MemoryCache {
    async fn increment(
        &self,
        key: &CacheKey,
        by: u64,
        window: Ttl,
        now: Timestamp,
    ) -> Result<Counted> {
        let mut inner = self.inner.write();
        let entry = inner
            .counters
            .entry(key.as_str().to_string())
            .or_insert_with(|| Expiring::new(0, window.deadline(now)));
        if entry.is_expired(now) {
            // A new window, not a continuation. Resetting the deadline here rather
            // than extending it is what keeps the window fixed; see the note on
            // `CounterCache::increment`.
            entry.value = 0;
            entry.expires_at = window.deadline(now);
        }
        // Saturating rather than wrapping: a counter that wraps to zero grants an
        // attacker a fresh quota at a predictable moment.
        entry.value = entry.value.saturating_add(by);
        Ok(Counted {
            value: entry.value,
            expires_at: entry.expires_at,
        })
    }

    async fn count(&self, key: &CacheKey, now: Timestamp) -> Result<u64> {
        let inner = self.inner.read();
        Ok(inner
            .counters
            .get(key.as_str())
            .filter(|entry| !entry.is_expired(now))
            .map_or(0, |entry| entry.value))
    }

    async fn reset(&self, key: &CacheKey) -> Result<()> {
        let mut inner = self.inner.write();
        inner.counters.remove(key.as_str());
        Ok(())
    }
}

#[async_trait]
impl TokenBucketCache for MemoryCache {
    async fn take_tokens(
        &self,
        key: &CacheKey,
        spec: BucketSpec,
        cost: u32,
        now: Timestamp,
    ) -> Result<BucketVerdict> {
        spec.check_affordable(cost)?;
        let mut inner = self.inner.write();
        let stored = inner
            .token_buckets
            .get(key.as_str())
            .filter(|entry| !entry.is_expired(now))
            .map_or_else(|| BucketState::full(spec, now), |entry| entry.value);
        let (next, verdict) = stored.charge(spec, cost, now);
        if verdict.taken {
            inner.token_buckets.insert(
                key.as_str().to_string(),
                Expiring::new(next, spec.state_ttl().deadline(now)),
            );
        }
        Ok(verdict)
    }

    async fn peek_bucket(&self, key: &CacheKey, spec: BucketSpec, now: Timestamp) -> Result<u32> {
        let inner = self.inner.read();
        Ok(inner
            .token_buckets
            .get(key.as_str())
            .filter(|entry| !entry.is_expired(now))
            .map_or(spec.capacity(), |entry| {
                entry.value.refilled(spec, now).whole_tokens()
            }))
    }

    async fn clear_bucket(&self, key: &CacheKey) -> Result<()> {
        let mut inner = self.inner.write();
        inner.token_buckets.remove(key.as_str());
        Ok(())
    }
}

#[async_trait]
impl PresenceCache for MemoryCache {
    async fn set_presence(&self, entry: PresenceEntry, ttl: Ttl, now: Timestamp) -> Result<()> {
        let mut inner = self.inner.write();
        let bucket = inner
            .presence
            .entry(entry.account_id)
            .or_insert_with(|| Bucket::new(ttl.deadline(now)));
        bucket.extend_to(ttl.deadline(now));
        bucket.fields.insert(entry.device_id, entry);
        Ok(())
    }

    async fn presence(&self, account_id: Id, now: Timestamp) -> Result<Vec<PresenceEntry>> {
        let inner = self.inner.read();
        Ok(inner
            .presence
            .get(&account_id)
            .map(|bucket| {
                bucket
                    .fields
                    .values()
                    .filter(|entry| !entry.is_expired(now))
                    .copied()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn presence_many(
        &self,
        account_ids: &[Id],
        now: Timestamp,
    ) -> Result<Vec<PresenceEntry>> {
        let inner = self.inner.read();
        let mut out = Vec::new();
        for account_id in account_ids.iter().take(MAX_PRESENCE_FANOUT) {
            if let Some(bucket) = inner.presence.get(account_id) {
                out.extend(
                    bucket
                        .fields
                        .values()
                        .filter(|entry| !entry.is_expired(now))
                        .copied(),
                );
            }
        }
        Ok(out)
    }

    async fn clear_presence(&self, account_id: Id, device_id: Id) -> Result<()> {
        let mut inner = self.inner.write();
        if let Some(bucket) = inner.presence.get_mut(&account_id) {
            bucket.fields.remove(&device_id);
            if bucket.fields.is_empty() {
                inner.presence.remove(&account_id);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl TypingCache for MemoryCache {
    async fn set_typing(
        &self,
        conversation_id: Id,
        account_id: Id,
        ttl: Ttl,
        now: Timestamp,
    ) -> Result<()> {
        let mut inner = self.inner.write();
        let deadline = ttl.deadline(now);
        let bucket = inner
            .typing
            .entry(conversation_id)
            .or_insert_with(|| Bucket::new(deadline));
        bucket.extend_to(deadline);
        bucket.fields.insert(account_id, deadline);
        Ok(())
    }

    async fn typing(&self, conversation_id: Id, now: Timestamp) -> Result<Vec<Id>> {
        let inner = self.inner.read();
        Ok(inner
            .typing
            .get(&conversation_id)
            .map(|bucket| {
                bucket
                    .fields
                    .iter()
                    .filter(|(_, expires_at)| !now.is_at_or_after(**expires_at))
                    .map(|(account_id, _)| *account_id)
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn clear_typing(&self, conversation_id: Id, account_id: Id) -> Result<()> {
        let mut inner = self.inner.write();
        if let Some(bucket) = inner.typing.get_mut(&conversation_id) {
            bucket.fields.remove(&account_id);
            if bucket.fields.is_empty() {
                inner.typing.remove(&conversation_id);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl RoutingCache for MemoryCache {
    async fn bind_session(&self, route: SessionRoute, ttl: Ttl, now: Timestamp) -> Result<()> {
        let mut inner = self.inner.write();
        let deadline = ttl.deadline(now);
        let index = inner
            .routes_by_account
            .entry(route.account_id)
            .or_insert_with(|| Bucket::new(deadline));
        index.extend_to(deadline);
        index.fields.insert(route.device_id, ());
        inner.routes.insert(route.device_id, route);
        Ok(())
    }

    async fn route(&self, device_id: Id, now: Timestamp) -> Result<Option<SessionRoute>> {
        let inner = self.inner.read();
        Ok(inner
            .routes
            .get(&device_id)
            .filter(|route| !route.is_expired(now))
            .cloned())
    }

    async fn routes_of_account(&self, account_id: Id, now: Timestamp) -> Result<Vec<SessionRoute>> {
        let inner = self.inner.read();
        let Some(index) = inner.routes_by_account.get(&account_id) else {
            return Ok(Vec::new());
        };
        Ok(index
            .fields
            .keys()
            .filter_map(|device_id| inner.routes.get(device_id))
            .filter(|route| !route.is_expired(now))
            // A device that rebound to another account would otherwise show up here
            // through the stale index. Nothing in Migo moves a device between
            // accounts, which is exactly why the check is cheap to keep.
            .filter(|route| route.account_id == account_id)
            .cloned()
            .collect())
    }

    async fn unbind_session(&self, device_id: Id, account_id: Id, node_id: &str) -> Result<bool> {
        let mut inner = self.inner.write();
        let owned = inner
            .routes
            .get(&device_id)
            .is_some_and(|route| route.node_id == node_id);
        if !owned {
            return Ok(false);
        }
        inner.routes.remove(&device_id);
        if let Some(index) = inner.routes_by_account.get_mut(&account_id) {
            index.fields.remove(&device_id);
            if index.fields.is_empty() {
                inner.routes_by_account.remove(&account_id);
            }
        }
        Ok(true)
    }
}

#[async_trait]
impl Cache for MemoryCache {
    fn backend_name(&self) -> &'static str {
        "memory"
    }

    async fn health(&self) -> Result<()> {
        Ok(())
    }

    async fn sweep(&self, now: Timestamp) -> Result<usize> {
        let mut inner = self.inner.write();
        let mut dropped = 0usize;

        inner.kv.retain(|_, entry| {
            let keep = !entry.is_expired(now);
            dropped += usize::from(!keep);
            keep
        });
        inner.counters.retain(|_, entry| {
            let keep = !entry.is_expired(now);
            dropped += usize::from(!keep);
            keep
        });
        inner.token_buckets.retain(|_, entry| {
            let keep = !entry.is_expired(now);
            dropped += usize::from(!keep);
            keep
        });
        inner.presence.retain(|_, bucket| {
            bucket.fields.retain(|_, entry| {
                let keep = !entry.is_expired(now);
                dropped += usize::from(!keep);
                keep
            });
            !bucket.fields.is_empty() && !now.is_at_or_after(bucket.reclaim_at)
        });
        inner.typing.retain(|_, bucket| {
            bucket.fields.retain(|_, expires_at| {
                let keep = !now.is_at_or_after(*expires_at);
                dropped += usize::from(!keep);
                keep
            });
            !bucket.fields.is_empty() && !now.is_at_or_after(bucket.reclaim_at)
        });

        // Routes first, then the index, so the index can be pruned against what
        // survived rather than against what used to be there.
        inner.routes.retain(|_, route| {
            let keep = !route.is_expired(now);
            dropped += usize::from(!keep);
            keep
        });
        let live: Vec<Id> = inner.routes.keys().copied().collect();
        inner.routes_by_account.retain(|_, index| {
            index
                .fields
                .retain(|device_id, ()| live.contains(device_id));
            !index.fields.is_empty()
        });

        Ok(dropped)
    }
}
