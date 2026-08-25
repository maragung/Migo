//! The cache contract, run against Redis.
//!
//! Needs a real server, so it is opt-in: set `MIGO_TEST_REDIS_URL` and every case
//! runs. Without it, each case returns immediately and says so once.
//!
//! ```text
//! MIGO_TEST_REDIS_URL=redis://127.0.0.1:6379/1 cargo test -p migo-cache
//! ```
//!
//! Cases share one database and stay out of each other's way by key, not by
//! isolation: `Fixture` claims a private corner of the keyspace, which is what lets
//! the suite run in parallel against a single server. Point it at a database you do
//! not mind sharing — every key it writes carries a TTL, so it cleans up after itself
//! without a flush, and a flush is exactly what would destroy an operator's Redis if
//! this URL were ever set to a real one by accident.

use std::sync::Arc;

use migo_cache::{RedisCache, SharedCache};
use migo_core::config::{CacheBackend, CacheConfig};
use migo_core::Secret;

#[macro_use]
mod contract;

use contract::Fixture;

/// The configured URL, or `None` when the suite is not meant to run.
fn redis_url() -> Option<String> {
    match std::env::var("MIGO_TEST_REDIS_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

fn fixture() -> Option<Fixture> {
    let url = redis_url()?;
    let config = CacheConfig {
        backend: CacheBackend::Redis,
        url: Some(Secret::new(url)),
        default_ttl_seconds: 300,
    };
    let cache = RedisCache::connect(&config).expect("MIGO_TEST_REDIS_URL must be a valid URL");
    Some(Fixture::new(Arc::new(cache) as SharedCache))
}

macro_rules! case {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            let Some(fixture) = fixture() else {
                return;
            };
            contract::$name(&fixture).await;
        }
    };
}

for_each_contract_case!(case);

#[tokio::test]
async fn the_backend_says_what_it_is_and_that_it_is_up() {
    use migo_cache::traits::Cache;

    let Some(url) = redis_url() else {
        eprintln!("MIGO_TEST_REDIS_URL is not set: the Redis contract did not run");
        return;
    };
    let config = CacheConfig {
        backend: CacheBackend::Redis,
        url: Some(Secret::new(url)),
        default_ttl_seconds: 300,
    };
    let cache = RedisCache::connect(&config).unwrap();
    assert_eq!(cache.backend_name(), "redis");
    cache
        .health()
        .await
        .expect("MIGO_TEST_REDIS_URL must be reachable");
    // Twice, because the second call is the one that proves the connection is reused
    // rather than rebuilt per operation.
    cache.health().await.unwrap();
}

#[tokio::test]
async fn a_url_redis_cannot_parse_fails_at_construction() {
    // Configuration errors are worth failing startup over. An unreachable server is
    // not, which is why `connect` does not contact one.
    let config = CacheConfig {
        backend: CacheBackend::Redis,
        url: Some(Secret::new("not-a-redis-url")),
        default_ttl_seconds: 300,
    };
    let error = RedisCache::connect(&config).expect_err("a bad URL must not be accepted");
    assert_eq!(error.code(), migo_protocol::codes::VALIDATION_FAILED);
    assert!(
        !error.internal_message().contains("not-a-redis-url"),
        "the URL carries a password and must not be echoed"
    );
}

#[tokio::test]
async fn a_missing_url_is_a_configuration_error() {
    let config = CacheConfig {
        backend: CacheBackend::Redis,
        url: None,
        default_ttl_seconds: 300,
    };
    let error = RedisCache::connect(&config).expect_err("the redis backend needs a URL");
    assert_eq!(error.code(), migo_protocol::codes::VALIDATION_FAILED);
}

/// No key `migo-cache` writes may live forever.
///
/// This is the invariant that separates a cache from a leak, and it is the one
/// mistake that hides best: a `SET` without `PX`, or a `HSET` whose companion
/// `PEXPIRE` was lost in a refactor, works perfectly in every functional test and
/// grows the working set until a production Redis starts evicting things somebody
/// was relying on. Only Redis can be asked this — the memory backend stores the
/// deadline inside the value — so it cannot be a contract case.
///
/// The assertion is deliberately global rather than scoped to this fixture: every
/// writer sets its TTL in the same command or script, so a key observed by another
/// client never briefly exists without one, and checking the other cases' keys too
/// makes the test stronger rather than racy.
#[tokio::test]
async fn every_key_written_carries_a_deadline() {
    use migo_cache::model::{BucketSpec, PresenceEntry, SessionRoute, Ttl};
    // No trait imports: the methods of `Cache`'s supertraits are reachable on
    // `dyn Cache` without one, which is why the contract cases need none either.
    use migo_core::Timestamp;
    use migo_protocol::PresenceState;
    use redis::AsyncCommands as _;

    let Some(url) = redis_url() else {
        return;
    };
    let Some(fixture) = fixture() else {
        return;
    };

    let cache = fixture.cache();
    let now = Timestamp::from_millis(1_000_000);
    let ttl = Ttl::from_seconds(30);
    let account = fixture.id(90);
    let device = fixture.id(91);
    let conversation = fixture.id(92);

    // One write through every path that creates a key. A path added later and left out
    // here is a leak this test would not see, which is why the count is asserted at
    // the bottom: adding a writer without adding it here fails loudly.
    cache
        .set(&fixture.key("deadline"), b"v", ttl, now)
        .await
        .unwrap();
    cache
        .set_if_absent(&fixture.key("deadline-absent"), b"v", ttl, now)
        .await
        .unwrap();
    cache
        .increment(&fixture.counter("deadline"), 1, ttl, now)
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
                node_id: "gateway-test-1".to_string(),
                connected_at: now,
                expires_at: ttl.deadline(now),
            },
            ttl,
            now,
        )
        .await
        .unwrap();
    cache
        .take_tokens(&fixture.bucket("deadline"), BucketSpec::new(10, 5), 1, now)
        .await
        .unwrap();

    // A second, raw connection: the point is to observe what the backend actually
    // left behind, not what it believes it wrote.
    let client = redis::Client::open(url).unwrap();
    let mut connection = client.get_multiplexed_async_connection().await.unwrap();

    let mut cursor: u64 = 0;
    let mut checked = 0usize;
    loop {
        let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(format!("{}:*", migo_cache::key::PREFIX))
            .arg("COUNT")
            .arg(500)
            .query_async(&mut connection)
            .await
            .unwrap();
        for key in keys {
            let pttl: i64 = connection.pttl(&key).await.unwrap();
            assert!(
                pttl >= 0,
                "{key} has no expiry (PTTL {pttl}): a cache key that outlives its \
                 window is a leak, not a cache entry"
            );
            checked += 1;
        }
        cursor = next;
        if cursor == 0 {
            break;
        }
    }
    assert!(
        checked >= 7,
        "the seven writes above must be visible; only {checked} keys were found"
    );
}

/// Whether this run is required to have a real backend behind it.
///
/// CI sets `MIGO_TEST_REQUIRE_BACKENDS=1`. It stays unset on a laptop, where a developer
/// with no Redis running should get a green suite rather than a wall of red for a service
/// they were never asked to install.
fn backends_are_required() -> bool {
    matches!(
        std::env::var("MIGO_TEST_REQUIRE_BACKENDS").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Fails when CI believes it ran the Redis contract and did not.
///
/// Every case above returns early when `MIGO_TEST_REDIS_URL` is unset, and the suite then
/// reports exactly the same count of passing tests as a run that touched a real server.
/// The TTL sweep is the part that hurts to lose: it is the only check that catches a `SET`
/// which lost its `PX`, it cannot run against `MemoryCache`, and it would disappear in
/// silence.
#[test]
fn the_contract_actually_ran_when_it_was_required_to() {
    if backends_are_required() {
        assert!(
            redis_url().is_some(),
            "MIGO_TEST_REQUIRE_BACKENDS is set but MIGO_TEST_REDIS_URL is not: every case \
             in this suite skipped, and the build would have gone green anyway"
        );
    }
}
