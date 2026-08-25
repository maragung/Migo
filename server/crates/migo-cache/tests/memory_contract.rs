//! The cache contract, run against the in-process backend.
//!
//! Almost nothing is asserted here on purpose: the file binds the suite in
//! `contract/mod.rs` to one backend, so the list of cases cannot drift between
//! backends — the macro names them, and both runners expand the same macro. The
//! two tests at the bottom are the exceptions, and each says why it cannot be a
//! contract case.

use std::sync::Arc;

use migo_cache::key::{CacheKey, SCOPE_BUCKET, SCOPE_COUNTER, SCOPE_KV};
use migo_cache::model::{BucketSpec, PresenceEntry, SessionRoute, Ttl};
use migo_cache::traits::{
    Cache, CounterCache, KeyValueCache, PresenceCache, RoutingCache, TokenBucketCache, TypingCache,
};
use migo_cache::{MemoryCache, SharedCache};
use migo_core::{Id, Timestamp};
use migo_protocol::PresenceState;

#[macro_use]
mod contract;

use contract::Fixture;

fn fixture() -> Fixture {
    Fixture::new(Arc::new(MemoryCache::new()) as SharedCache)
}

macro_rules! case {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            contract::$name(&fixture()).await;
        }
    };
}

for_each_contract_case!(case);

#[tokio::test]
async fn the_backend_says_what_it_is() {
    let cache = MemoryCache::new();
    assert_eq!(cache.backend_name(), "memory");
    assert!(cache.is_empty());
}

/// What `sweep` is for, and the one thing only this backend can be asked.
///
/// Redis reclaims expired keys itself and answers zero, so a contract case cannot
/// assert that anything was dropped. Here it must be: an entry nobody reads again is
/// never lazily expired, and a long-running simulation would grow without bound.
#[tokio::test]
async fn sweeping_reclaims_what_lazy_expiry_never_would() {
    let cache = MemoryCache::new();
    let now = Timestamp::from_millis(1_000_000);
    let ttl = Ttl::from_seconds(10);
    let account = Id::from(1u128);
    let device = Id::from(2u128);
    let conversation = Id::from(3u128);

    cache
        .set(&CacheKey::new(SCOPE_KV, "abandoned"), b"x", ttl, now)
        .await
        .unwrap();
    cache
        .increment(&CacheKey::new(SCOPE_COUNTER, "abandoned"), 1, ttl, now)
        .await
        .unwrap();
    cache
        .set_presence(
            PresenceEntry {
                account_id: account,
                device_id: device,
                state: PresenceState::Online,
                since: now,
                expires_at: ttl.deadline(now),
            },
            ttl,
            now,
        )
        .await
        .unwrap();
    cache
        .set_typing(conversation, account, ttl, now)
        .await
        .unwrap();
    cache
        .bind_session(
            SessionRoute {
                account_id: account,
                device_id: device,
                node_id: "gateway-sg-1".to_string(),
                connected_at: now,
                expires_at: ttl.deadline(now),
            },
            ttl,
            now,
        )
        .await
        .unwrap();
    // Five tokens at one per second, so this bucket's state lives six seconds — well
    // inside the eleven the sweep is run at.
    cache
        .take_tokens(
            &CacheKey::new(SCOPE_BUCKET, "abandoned"),
            BucketSpec::new(5, 1),
            1,
            now,
        )
        .await
        .unwrap();
    assert_eq!(cache.len(), 6);

    let later = now.saturating_add_millis(11_000);
    assert_eq!(
        cache.sweep(later).await.unwrap(),
        6,
        "every namespace has to be swept, not just the ones easy to reach"
    );
    assert!(cache.is_empty());

    // Idempotent: the janitor runs on a timer and must not report work it did not do.
    assert_eq!(cache.sweep(later).await.unwrap(), 0);
}
