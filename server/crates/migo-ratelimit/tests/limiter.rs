//! What the limiter promises, checked against a real backend and a broken one.
//!
//! The in-memory cache is the backend under test rather than a mock, because the
//! interesting behaviour is arithmetic and the arithmetic is shared: the same
//! `BucketState::charge` runs here, inside the Redis Lua script, and inside the local
//! fallback. A mock would prove the limiter calls the cache; these prove it gets the
//! right answer.
//!
//! The broken backend is a mock, and has to be — there is no way to ask a working Redis
//! to fail on demand, and the degraded path is the one nobody exercises until the night
//! it matters.

use std::net::IpAddr;
use std::slice;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use migo_cache::key::CacheKey;
use migo_cache::model::{BucketSpec, BucketVerdict};
use migo_cache::traits::TokenBucketCache;
use migo_cache::MemoryCache;
use migo_core::config::RateLimitConfig;
use migo_core::metrics::Registry;
use migo_core::{ErrorKind, Id, Result, Timestamp};
use migo_protocol::{fault, Opcode};
use migo_ratelimit::{
    BucketKey, CacheRateLimiter, Policies, RateLimiter, Scope, TrustTier, Verdict,
    FALLBACK_DIVISOR, MAX_KEYS_PER_CHARGE,
};

/// A fixed instant. Nothing here reads a clock; see ADR-0009.
fn start() -> Timestamp {
    Timestamp::from_millis(1_000_000)
}

fn id(n: u64) -> Id {
    Id::from(u128::from(n))
}

fn address(last: u8) -> IpAddr {
    IpAddr::from([203, 0, 113, last])
}

/// A limiter over a working cache.
fn working() -> (CacheRateLimiter<MemoryCache>, Registry) {
    let registry = Registry::new();
    let limiter =
        CacheRateLimiter::new(Arc::new(MemoryCache::new()), Policies::default(), &registry);
    (limiter, registry)
}

/// A cache that cannot answer, and counts how often it was asked.
struct BrokenCache {
    calls: AtomicU64,
}

impl BrokenCache {
    fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
        }
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl TokenBucketCache for BrokenCache {
    async fn take_tokens(
        &self,
        _key: &CacheKey,
        _spec: BucketSpec,
        _cost: u32,
        _now: Timestamp,
    ) -> Result<BucketVerdict> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(fault::cache("redis is not there"))
    }

    async fn peek_bucket(
        &self,
        _key: &CacheKey,
        _spec: BucketSpec,
        _now: Timestamp,
    ) -> Result<u32> {
        Err(fault::cache("redis is not there"))
    }

    async fn clear_bucket(&self, _key: &CacheKey) -> Result<()> {
        Err(fault::cache("redis is not there"))
    }
}

fn broken() -> (CacheRateLimiter<BrokenCache>, Arc<BrokenCache>, Registry) {
    let cache = Arc::new(BrokenCache::new());
    let registry = Registry::new();
    let limiter = CacheRateLimiter::new(Arc::clone(&cache), Policies::default(), &registry);
    (limiter, cache, registry)
}

// --- the ordinary path ---

#[tokio::test]
async fn a_free_opcode_touches_no_bucket() {
    let (limiter, _registry) = working();
    let keys = [BucketKey::account(id(1))];

    // ACK costs nothing in the IDL, and the server sends more of them than anything
    // else. Charging them would have the server rate limiting its own replies.
    let verdict = limiter
        .charge_opcode(&keys, Opcode::Ack, TrustTier::Established, start())
        .await
        .unwrap();

    assert_eq!(verdict, Verdict::Free);
    assert_eq!(
        limiter
            .peek(&keys[0], TrustTier::Established, start())
            .await
            .unwrap(),
        limiter
            .policies()
            .resolve(Scope::Account, TrustTier::Established)
            .capacity(),
        "a free charge must leave the bucket untouched"
    );
}

#[tokio::test]
async fn the_reported_balance_comes_from_the_tightest_surface() {
    let (limiter, _registry) = working();
    let account = id(7);
    let keys = [
        BucketKey::endpoint_of_account(account, Opcode::MessageSend),
        BucketKey::account(account),
        BucketKey::ip(address(9)),
    ];

    let verdict = limiter
        .charge_opcode(&keys, Opcode::MessageSend, TrustTier::Established, start())
        .await
        .unwrap();

    // The endpoint surface is half a user's budget and the IP surface is four times it,
    // so the number a client should pace itself against is the endpoint's.
    let endpoint = limiter
        .policies()
        .resolve(Scope::Endpoint, TrustTier::Established);
    assert_eq!(
        verdict,
        Verdict::Allowed {
            remaining: endpoint.capacity() - Opcode::MessageSend.cost(),
        }
    );
}

#[tokio::test]
async fn costs_come_from_the_protocol_and_not_the_caller() {
    let (limiter, _registry) = working();
    let keys = [BucketKey::account(id(3))];
    let capacity = limiter
        .policies()
        .resolve(Scope::Account, TrustTier::Established)
        .capacity();

    limiter
        .charge_opcode(&keys, Opcode::KeyPublish, TrustTier::Established, start())
        .await
        .unwrap();

    assert_eq!(
        limiter
            .peek(&keys[0], TrustTier::Established, start())
            .await
            .unwrap(),
        capacity - Opcode::KeyPublish.cost(),
        "a publish must cost what the IDL says it costs, not one"
    );
    assert!(
        Opcode::KeyPublish.cost() > Opcode::MessageSend.cost(),
        "the point of cost-based limiting is that operations differ in price"
    );
}

#[tokio::test]
async fn a_drained_surface_refuses_and_names_itself() {
    let (limiter, registry) = working();
    let keys = [BucketKey::account(id(11))];
    let spec = limiter
        .policies()
        .resolve(Scope::Account, TrustTier::Established);

    for _ in 0..spec.capacity() {
        assert!(limiter
            .charge(&keys, 1, TrustTier::Established, start())
            .await
            .unwrap()
            .is_allowed());
    }

    let verdict = limiter
        .charge(&keys, 1, TrustTier::Established, start())
        .await
        .unwrap();

    assert_eq!(verdict.rejected_by(), Some(Scope::Account));
    assert_eq!(
        verdict.retry_after_ms(),
        Some(1000 / spec.refill_per_second()),
        "the wait must be the time to earn exactly what was asked for"
    );
    assert!(registry
        .render()
        .contains("migo_ratelimit_rejections_total{scope=\"account\"} 1"));
}

#[tokio::test]
async fn a_refusal_does_not_refund_what_was_already_charged() {
    let (limiter, _registry) = working();
    let account = id(13);
    // Ordered tightest first, which is the order a caller should build: the endpoint
    // bucket is drained below, so it refuses before the account bucket is reached.
    let endpoint = BucketKey::endpoint_of_account(account, Opcode::MessageSend);
    let keys = [BucketKey::account(account), endpoint.clone()];

    let endpoint_spec = limiter
        .policies()
        .resolve(Scope::Endpoint, TrustTier::Established);
    for _ in 0..endpoint_spec.capacity() {
        limiter
            .charge(
                slice::from_ref(&endpoint),
                1,
                TrustTier::Established,
                start(),
            )
            .await
            .unwrap();
    }
    let account_before = limiter
        .peek(&keys[0], TrustTier::Established, start())
        .await
        .unwrap();

    let verdict = limiter
        .charge(&keys, 1, TrustTier::Established, start())
        .await
        .unwrap();

    assert_eq!(verdict.rejected_by(), Some(Scope::Endpoint));
    assert_eq!(
        limiter
            .peek(&keys[0], TrustTier::Established, start())
            .await
            .unwrap(),
        account_before - 1,
        "the account surface was charged before the endpoint refused, and must stay \
         charged: a refusal that costs nothing makes flooding free"
    );
}

#[tokio::test]
async fn a_bucket_refills_and_the_wait_it_promised_is_enough() {
    let (limiter, _registry) = working();
    let keys = [BucketKey::account(id(17))];
    let spec = limiter
        .policies()
        .resolve(Scope::Account, TrustTier::Established);

    for _ in 0..spec.capacity() {
        limiter
            .charge(&keys, 1, TrustTier::Established, start())
            .await
            .unwrap();
    }
    let wait = limiter
        .charge(&keys, 1, TrustTier::Established, start())
        .await
        .unwrap()
        .retry_after_ms()
        .expect("a drained bucket must refuse");

    let later = start().saturating_add_millis(i64::from(wait));
    assert!(
        limiter
            .charge(&keys, 1, TrustTier::Established, later)
            .await
            .unwrap()
            .is_allowed(),
        "waiting exactly as long as the limiter asked must be enough, or the \
         retry_after it sends clients is a lie that produces a second rejection"
    );
}

#[tokio::test]
async fn surfaces_are_charged_independently() {
    let (limiter, _registry) = working();
    let account = BucketKey::account(id(19));
    let other = BucketKey::account(id(23));

    limiter
        .charge(
            slice::from_ref(&account),
            5,
            TrustTier::Established,
            start(),
        )
        .await
        .unwrap();

    let capacity = limiter
        .policies()
        .resolve(Scope::Account, TrustTier::Established)
        .capacity();
    assert_eq!(
        limiter
            .peek(&other, TrustTier::Established, start())
            .await
            .unwrap(),
        capacity,
        "one account's spending must not touch another's"
    );
}

// --- keys ---

#[tokio::test]
async fn an_address_is_truncated_to_its_network() {
    let (limiter, _registry) = working();
    let neighbour = BucketKey::ip(address(1));
    let same_network = BucketKey::ip(address(200));

    assert_eq!(
        neighbour.cache_key(),
        same_network.cache_key(),
        "two addresses in one /24 must share a bucket: a per-address budget hands an \
         attacker with a subnet one budget per address"
    );

    limiter
        .charge(
            slice::from_ref(&neighbour),
            4,
            TrustTier::Anonymous,
            start(),
        )
        .await
        .unwrap();
    let capacity = limiter
        .policies()
        .resolve(Scope::Ip, TrustTier::Anonymous)
        .capacity();
    assert_eq!(
        limiter
            .peek(&same_network, TrustTier::Anonymous, start())
            .await
            .unwrap(),
        capacity - 4
    );

    let elsewhere = BucketKey::ip(IpAddr::from([198, 51, 100, 1]));
    assert_ne!(neighbour.cache_key(), elsewhere.cache_key());
}

#[test]
fn an_ipv6_address_is_truncated_to_its_prefix() {
    let inside = BucketKey::ip("2001:db8:0:1:ffff:ffff:ffff:ffff".parse().unwrap());
    let also_inside = BucketKey::ip("2001:db8:0:1::1".parse().unwrap());
    let outside = BucketKey::ip("2001:db8:0:2::1".parse().unwrap());

    assert_eq!(inside.cache_key(), also_inside.cache_key());
    assert_ne!(inside.cache_key(), outside.cache_key());
    assert_eq!(
        migo_ratelimit::network("2001:db8:0:1::1".parse().unwrap()),
        "2001:db8:0:1::/64"
    );
}

#[test]
fn a_credential_never_appears_in_its_own_key() {
    let mut fingerprint = [0u8; migo_ratelimit::TOKEN_FINGERPRINT_BYTES];
    for (index, byte) in fingerprint.iter_mut().enumerate() {
        *byte = index as u8;
    }
    let key = BucketKey::token(&fingerprint);
    let text = key.cache_key().as_str();

    assert!(text.starts_with("m:tb:tk/"));
    let hex = text.trim_start_matches("m:tb:tk/");
    assert_eq!(hex, "000102030405060708090a0b0c0d0e0f");
    assert!(
        hex.len() < migo_ratelimit::TOKEN_FINGERPRINT_BYTES * 2,
        "only part of the fingerprint belongs in a key: a key is a distinguisher, not \
         a verifier, and cache keys turn up in operator dumps"
    );
}

#[test]
fn every_surface_is_distinguishable() {
    let codes: Vec<&str> = Scope::ALL.iter().map(|scope| scope.code()).collect();
    let mut sorted = codes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        codes.len(),
        "two surfaces sharing a key prefix share buckets"
    );

    // Brief section 120 lists seven surfaces. The count is asserted so that a surface
    // added here without being added there, or the reverse, is visible.
    assert_eq!(Scope::ALL.len(), 7);
    for (index, scope) in Scope::ALL.iter().enumerate() {
        assert_eq!(
            scope.index(),
            index,
            "Scope::index must agree with Scope::ALL"
        );
    }
}

#[test]
fn the_endpoint_surface_separates_opcodes() {
    let account = id(29);
    let send = BucketKey::endpoint_of_account(account, Opcode::MessageSend);
    let publish = BucketKey::endpoint_of_account(account, Opcode::KeyPublish);
    assert_ne!(send.cache_key(), publish.cache_key());
    assert_eq!(send.scope(), Scope::Endpoint);
}

// --- policy ---

#[test]
fn the_shipped_configuration_is_usable() {
    Policies::from_config(&RateLimitConfig::default())
        .expect("the defaults must pass their own validation, or nothing boots");
}

#[test]
fn standing_scales_both_the_burst_and_the_rate() {
    let policies = Policies::default();
    let new = policies.resolve(Scope::Account, TrustTier::New);
    let established = policies.resolve(Scope::Account, TrustTier::Established);
    let trusted = policies.resolve(Scope::Account, TrustTier::Trusted);

    assert!(new.capacity() < established.capacity());
    assert!(trusted.capacity() > established.capacity());
    assert!(
        new.refill_per_second() < established.refill_per_second(),
        "scaling the burst alone would let a brand new account send at a trusted \
         account's sustained rate, and the sustained rate is the one abuse needs"
    );
    assert!(trusted.refill_per_second() > established.refill_per_second());
}

#[test]
fn a_shared_surface_ignores_the_callers_standing() {
    let policies = Policies::default();
    for scope in [Scope::Ip, Scope::Room] {
        assert!(!scope.scales_with_tier());
        let anonymous = policies.resolve(scope, TrustTier::Anonymous);
        let trusted = policies.resolve(scope, TrustTier::Trusted);
        assert_eq!(
            anonymous, trusted,
            "a surface strangers share must not widen because one trusted caller used \
             it: the next hundred requests could come from a hundred new accounts"
        );
    }
}

#[test]
fn a_budget_too_small_to_pay_for_an_operation_is_refused_at_startup() {
    // Eighty is chosen so that exactly one resolved bucket falls short, which is what
    // makes this a test of the calculation and not just of the loop. A new account gets a
    // quarter of the burst (twenty, enough for one KEY_PUBLISH) and the endpoint surface
    // then halves it to ten, which can never hold one. Every other surface at every other
    // tier clears the bar, so the message has to name the narrowest.
    let config = RateLimitConfig {
        user_burst: 80,
        ..RateLimitConfig::default()
    };
    let error = Policies::from_config(&config)
        .expect_err("a bucket that refuses forever is a misconfiguration, not a limit");
    assert_eq!(error.kind(), ErrorKind::Validation);

    let message = error.to_string();
    assert!(message.contains("endpoint"), "{message}");
    assert!(message.contains("new"), "{message}");
    assert!(
        message.contains("costs 20"),
        "the message has to say what it could not afford, or an operator cannot tell \
         which number to raise: {message}"
    );

    // One notch up and the same configuration is fine, which is the useful half of the
    // check: validation that rejects a wide range of workable settings gets disabled.
    Policies::from_config(&RateLimitConfig {
        user_burst: 160,
        ..RateLimitConfig::default()
    })
    .expect("160 resolves to twenty on the narrowest surface, which is exactly enough");
}

#[test]
fn the_degraded_shape_is_tighter_but_still_payable() {
    let policies = Policies::default();
    let cost = Opcode::Authenticate.cost();
    let healthy = policies.resolve(Scope::Endpoint, TrustTier::Anonymous);
    let degraded = policies.degraded(Scope::Endpoint, TrustTier::Anonymous, cost);

    assert!(
        degraded.refill_per_second() < healthy.refill_per_second(),
        "N nodes each enforcing the full rate would together enforce N times it, so \
         losing the shared store must tighten the rate, not keep it"
    );
    assert!(
        degraded.capacity() >= cost,
        "dividing the capacity by {FALLBACK_DIVISOR} would leave an anonymous endpoint \
         bucket too small to pay for AUTHENTICATE, and a bucket that cannot pay refuses \
         forever: an outage would lock everybody out of logging in"
    );
    assert!(degraded.capacity() <= healthy.capacity());
}

// --- degradation ---

#[tokio::test]
async fn a_cache_outage_still_lets_a_first_request_through() {
    let (limiter, cache, registry) = broken();
    let keys = [BucketKey::endpoint_of_ip(address(4), Opcode::Authenticate)];

    let verdict = limiter
        .charge_opcode(&keys, Opcode::Authenticate, TrustTier::Anonymous, start())
        .await
        .unwrap();

    assert!(
        verdict.is_allowed(),
        "logging in must survive a Redis outage: {verdict:?}"
    );
    assert_eq!(
        cache.calls(),
        1,
        "the shared store is tried first every time"
    );
    assert_eq!(
        limiter.local_buckets(),
        1,
        "and the local bucket now exists"
    );
    assert!(registry
        .render()
        .contains("migo_ratelimit_degraded_total 1"));
}

#[tokio::test]
async fn a_cache_outage_does_not_open_the_limiter() {
    let (limiter, _cache, _registry) = broken();
    let keys = [BucketKey::endpoint_of_ip(address(5), Opcode::Authenticate)];

    // One AUTHENTICATE empties the tightened anonymous endpoint bucket exactly.
    limiter
        .charge_opcode(&keys, Opcode::Authenticate, TrustTier::Anonymous, start())
        .await
        .unwrap();
    let verdict = limiter
        .charge_opcode(&keys, Opcode::Authenticate, TrustTier::Anonymous, start())
        .await
        .unwrap();

    assert_eq!(verdict.rejected_by(), Some(Scope::Endpoint));
    let degraded_wait = verdict.retry_after_ms().unwrap();

    // The same flood against a working cache waits less, because the shared buckets
    // refill at the configured rate rather than the tightened one.
    let (healthy, _registry) = working();
    healthy
        .charge_opcode(&keys, Opcode::Authenticate, TrustTier::Anonymous, start())
        .await
        .unwrap();
    let healthy_wait = healthy
        .charge_opcode(&keys, Opcode::Authenticate, TrustTier::Anonymous, start())
        .await
        .unwrap()
        .retry_after_ms()
        .unwrap();

    assert!(
        degraded_wait > healthy_wait,
        "degraded must mean stricter, not looser: {degraded_wait} vs {healthy_wait}"
    );
}

#[tokio::test]
async fn a_caller_bug_is_reported_rather_than_degraded() {
    let (limiter, _registry) = working();
    let keys = [BucketKey::account(id(31))];

    // Nothing in the IDL costs this much; a caller passing it has a bug, and no wait
    // would ever make the charge affordable.
    let error = limiter
        .charge(&keys, 10_000, TrustTier::Established, start())
        .await
        .expect_err("an unsatisfiable charge is not a refusal");

    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        limiter.local_buckets(),
        0,
        "a validation error must not be mistaken for an outage: degrading here would \
         hide a misconfiguration behind a limiter quietly doing something else"
    );
}

#[tokio::test]
async fn a_peek_during_an_outage_reports_the_limit_in_force() {
    let (limiter, _cache, _registry) = broken();
    let key = BucketKey::account(id(37));

    let remaining = limiter
        .peek(&key, TrustTier::Established, start())
        .await
        .expect("a peek must degrade rather than fail");

    assert_eq!(
        remaining,
        limiter
            .policies()
            .degraded(Scope::Account, TrustTier::Established, 0)
            .capacity(),
        "reporting the healthy capacity during an outage tells an operator a number \
         that is not the one being enforced"
    );
}

// --- the caller's own mistakes ---

#[tokio::test]
async fn charging_no_surface_is_a_bug_and_not_an_allowance() {
    let (limiter, _registry) = working();
    let error = limiter
        .charge(&[], 1, TrustTier::Established, start())
        .await
        .expect_err("charging nothing limits nothing");
    assert_eq!(error.kind(), ErrorKind::Validation);
}

#[tokio::test]
async fn charging_more_surfaces_than_exist_is_a_bug() {
    let (limiter, _registry) = working();
    let keys: Vec<BucketKey> = (0..=MAX_KEYS_PER_CHARGE as u64)
        .map(|n| BucketKey::account(id(100 + n)))
        .collect();
    let error = limiter
        .charge(&keys, 1, TrustTier::Established, start())
        .await
        .expect_err("each key is a round trip and there are only seven surfaces");
    assert_eq!(error.kind(), ErrorKind::Validation);
}

// --- operator surface ---

#[tokio::test]
async fn clearing_a_bucket_lifts_the_limit() {
    let (limiter, _registry) = working();
    let keys = [BucketKey::account(id(41))];
    let spec = limiter
        .policies()
        .resolve(Scope::Account, TrustTier::Established);

    for _ in 0..spec.capacity() {
        limiter
            .charge(&keys, 1, TrustTier::Established, start())
            .await
            .unwrap();
    }
    assert!(!limiter
        .charge(&keys, 1, TrustTier::Established, start())
        .await
        .unwrap()
        .is_allowed());

    limiter.clear(&keys[0]).await.unwrap();

    assert!(limiter
        .charge(&keys, 1, TrustTier::Established, start())
        .await
        .unwrap()
        .is_allowed());
}

#[tokio::test]
async fn peeking_does_not_charge() {
    let (limiter, _registry) = working();
    let key = BucketKey::account(id(43));
    let capacity = limiter
        .policies()
        .resolve(Scope::Account, TrustTier::Established)
        .capacity();

    for _ in 0..5 {
        assert_eq!(
            limiter
                .peek(&key, TrustTier::Established, start())
                .await
                .unwrap(),
            capacity
        );
    }
}

#[test]
fn every_rejection_series_exists_before_anything_is_rejected() {
    let registry = Registry::new();
    let _limiter =
        CacheRateLimiter::new(Arc::new(MemoryCache::new()), Policies::default(), &registry);
    let rendered = registry.render();

    for scope in Scope::ALL {
        let series = format!(
            "migo_ratelimit_rejections_total{{scope=\"{}\"}} 0",
            scope.label()
        );
        assert!(
            rendered.contains(&series),
            "a counter that appears only once it fires cannot be alerted on \
             beforehand; missing {series}"
        );
    }
    assert!(rendered.contains("migo_ratelimit_checks_total 0"));
    assert!(rendered.contains("migo_ratelimit_fallback_saturated_total 0"));
}

// --- the verdict ---

#[test]
fn a_refusal_becomes_a_rate_limited_error_carrying_the_wait() {
    let verdict = Verdict::Rejected {
        scope: Scope::Room,
        retry_after_ms: 2_500,
    };
    let error = verdict
        .into_result()
        .expect_err("a refusal must be an error");
    assert_eq!(error.kind(), ErrorKind::RateLimit);
    assert!(
        !error.to_string().contains("room"),
        "which of seven buckets refused is an operator's business: telling the caller \
         tells an attacker which one to work around"
    );

    assert!(Verdict::Free.into_result().is_ok());
    assert!(Verdict::Allowed { remaining: 3 }.into_result().is_ok());
}
