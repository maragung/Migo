//! The engine.

use std::sync::Arc;

use async_trait::async_trait;
use migo_cache::traits::TokenBucketCache;
use migo_core::metrics::Registry;
use migo_core::{ErrorKind, Result, Timestamp};
use migo_protocol::fault;
use tracing::warn;

use crate::local::LocalBuckets;
use crate::metrics::Meters;
use crate::policy::{Policies, TrustTier};
use crate::scope::BucketKey;
use crate::traits::{RateLimiter, Verdict};

/// Most surfaces one charge may name.
///
/// Seven, which is every surface there is (brief section 120). The bound exists because
/// each key is a cache round trip and a caller that built a list in a loop would otherwise
/// turn one request into as many round trips as the loop had iterations. Exceeding it is a
/// caller bug, so it is reported rather than truncated: truncating would silently drop
/// whichever surface came last.
pub const MAX_KEYS_PER_CHARGE: usize = 7;

/// Token buckets in the shared cache, with local ones behind them.
///
/// Generic over an unsized cache handle rather than taking `Arc<dyn TokenBucketCache>`
/// outright, which lets the composition root pass its `Arc<dyn Cache>` straight in: a
/// bound of `C: TokenBucketCache + ?Sized` is satisfied by `dyn Cache`, because
/// `dyn Cache` implements every supertrait of `Cache`. Trait upcasting (stable in 1.86,
/// and so available under the declared 1.94) would narrow the handle too, but only
/// through an explicit coercion at every call site, and it would erase a concrete cache
/// that the caller had no reason to erase. The default type parameter keeps
/// `CacheRateLimiter` spellable without naming it.
pub struct CacheRateLimiter<C: TokenBucketCache + ?Sized = dyn TokenBucketCache> {
    cache: Arc<C>,
    local: LocalBuckets,
    policies: Policies,
    meters: Meters,
}

impl<C: TokenBucketCache + ?Sized> CacheRateLimiter<C> {
    /// Builds a limiter over `cache`, reporting into `registry`.
    #[must_use]
    pub fn new(cache: Arc<C>, policies: Policies, registry: &Registry) -> Self {
        Self {
            cache,
            local: LocalBuckets::new(),
            policies,
            meters: Meters::new(registry),
        }
    }

    /// The same, with a smaller local fallback. For exercising saturation.
    #[cfg(test)]
    fn with_fallback_capacity(
        cache: Arc<C>,
        policies: Policies,
        registry: &Registry,
        max: usize,
    ) -> Self {
        Self {
            cache,
            local: LocalBuckets::with_capacity(max),
            policies,
            meters: Meters::new(registry),
        }
    }

    /// How many subjects the local fallback is currently tracking. Zero unless the cache
    /// has failed at some point.
    #[must_use]
    pub fn local_buckets(&self) -> usize {
        self.local.len()
    }

    /// Charges one key, falling back to a local bucket when the cache cannot answer.
    ///
    /// The error polarity here is the important part, and it is the opposite of the
    /// obvious one: a validation error propagates and *everything else* degrades. Written
    /// the other way round — degrade on the known cache error codes, propagate the rest —
    /// a backend error nobody had thought of would fail the request, and the rule this
    /// whole layer is built on (`migo-cache`'s `traits` module: a cache error must never
    /// fail a request that could have succeeded without the cache) would hold only for
    /// the failures already enumerated. The failures already enumerated are not the ones
    /// that take a system down at three in the morning.
    ///
    /// A validation error is not a cache failure at all. It means the caller asked for
    /// something impossible — a cost larger than the bucket — and degrading would hide a
    /// misconfiguration behind a limiter that quietly does something else instead.
    async fn charge_one(
        &self,
        key: &BucketKey,
        cost: u32,
        tier: TrustTier,
        now: Timestamp,
    ) -> Result<Option<BucketOutcome>> {
        let spec = self.policies.resolve(key.scope(), tier);
        match self
            .cache
            .take_tokens(key.cache_key(), spec, cost, now)
            .await
        {
            Ok(verdict) => Ok(Some(BucketOutcome {
                taken: verdict.taken,
                remaining: verdict.remaining,
                retry_after_ms: verdict.retry_after_ms,
            })),
            Err(error) if error.kind() == ErrorKind::Validation => Err(error),
            Err(error) => {
                self.meters.degrade();
                warn!(
                    scope = key.scope().label(),
                    error = %error,
                    "rate limit falling back to local buckets"
                );
                let degraded = self.policies.degraded(key.scope(), tier, cost);
                match self.local.charge(key.cache_key(), degraded, cost, now) {
                    Some(verdict) => Ok(Some(BucketOutcome {
                        taken: verdict.taken,
                        remaining: verdict.remaining,
                        retry_after_ms: verdict.retry_after_ms,
                    })),
                    None => {
                        self.meters.saturate();
                        Ok(None)
                    }
                }
            }
        }
    }
}

/// One bucket's answer, from whichever store answered.
struct BucketOutcome {
    taken: bool,
    remaining: u32,
    retry_after_ms: u32,
}

/// How long to tell a caller to wait when the fallback has no room for it.
///
/// One second. The condition is not about this subject's budget — the subject has no
/// bucket, that is the problem — so there is no honest per-subject number to compute. A
/// short wait keeps a legitimate client retrying rather than giving up, and the node is by
/// then reporting `migo_ratelimit_fallback_saturated_total`, which is the signal an
/// operator acts on.
const SATURATED_RETRY_AFTER_MS: u32 = 1_000;

#[async_trait]
impl<C: TokenBucketCache + ?Sized + 'static> RateLimiter for CacheRateLimiter<C> {
    async fn charge(
        &self,
        keys: &[BucketKey],
        cost: u32,
        tier: TrustTier,
        now: Timestamp,
    ) -> Result<Verdict> {
        self.meters.check();
        if keys.is_empty() {
            return Err(fault::validation(
                "keys",
                "a charge must name at least one surface: charging none of them limits \
                 nothing, and reporting that as allowed would make the path unmetered",
            ));
        }
        if keys.len() > MAX_KEYS_PER_CHARGE {
            return Err(fault::validation(
                "keys",
                &format!(
                    "a charge may name at most {MAX_KEYS_PER_CHARGE} surfaces, got {}: each one \
                     is a round trip, and there are only that many surfaces to name",
                    keys.len()
                ),
            ));
        }
        if cost == 0 {
            return Ok(Verdict::Free);
        }

        let mut tightest = u32::MAX;
        for key in keys {
            let Some(outcome) = self.charge_one(key, cost, tier, now).await? else {
                self.meters.reject(key.scope());
                return Ok(Verdict::Rejected {
                    scope: key.scope(),
                    retry_after_ms: SATURATED_RETRY_AFTER_MS,
                });
            };
            if !outcome.taken {
                self.meters.reject(key.scope());
                return Ok(Verdict::Rejected {
                    scope: key.scope(),
                    retry_after_ms: outcome.retry_after_ms,
                });
            }
            tightest = tightest.min(outcome.remaining);
        }
        Ok(Verdict::Allowed {
            remaining: tightest,
        })
    }

    async fn peek(&self, key: &BucketKey, tier: TrustTier, now: Timestamp) -> Result<u32> {
        let spec = self.policies.resolve(key.scope(), tier);
        match self.cache.peek_bucket(key.cache_key(), spec, now).await {
            Ok(remaining) => Ok(remaining),
            Err(error) if error.kind() == ErrorKind::Validation => Err(error),
            Err(_) => {
                // The degraded shape, because that is the bucket the fallback is
                // actually holding. Reporting the healthy capacity here would tell an
                // operator investigating an outage a number that is not in force.
                let degraded = self.policies.degraded(key.scope(), tier, 0);
                Ok(self.local.peek(key.cache_key(), degraded, now))
            }
        }
    }

    async fn clear(&self, key: &BucketKey) -> Result<()> {
        // Both stores, and the local one unconditionally: an operator lifting a limit by
        // hand during an outage is exactly the case where the local copy is the one that
        // matters, and it would be a poor tool that cleared only the store that was down.
        self.local.clear(key.cache_key());
        self.cache.clear_bucket(key.cache_key()).await
    }

    fn policies(&self) -> &Policies {
        &self.policies
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_cache::key::CacheKey;
    use migo_cache::model::{BucketSpec, BucketVerdict};
    use migo_core::Id;

    /// A cache that never answers. The saturation path only exists behind an outage, so
    /// there is no way to reach it with a working one.
    struct Down;

    #[async_trait]
    impl TokenBucketCache for Down {
        async fn take_tokens(
            &self,
            _key: &CacheKey,
            _spec: BucketSpec,
            _cost: u32,
            _now: Timestamp,
        ) -> Result<BucketVerdict> {
            Err(fault::cache("down"))
        }

        async fn peek_bucket(
            &self,
            _key: &CacheKey,
            _spec: BucketSpec,
            _now: Timestamp,
        ) -> Result<u32> {
            Err(fault::cache("down"))
        }

        async fn clear_bucket(&self, _key: &CacheKey) -> Result<()> {
            Err(fault::cache("down"))
        }
    }

    #[tokio::test]
    async fn a_fallback_with_no_room_refuses_rather_than_stops_counting() {
        let registry = Registry::new();
        let limiter = CacheRateLimiter::with_fallback_capacity(
            Arc::new(Down),
            Policies::default(),
            &registry,
            2,
        );
        let now = Timestamp::from_millis(1_000_000);

        for n in 0..2u128 {
            let keys = [BucketKey::account(Id::from(n))];
            assert!(limiter
                .charge(&keys, 1, TrustTier::Established, now)
                .await
                .unwrap()
                .is_allowed());
        }

        let verdict = limiter
            .charge(
                &[BucketKey::account(Id::from(99u128))],
                1,
                TrustTier::Established,
                now,
            )
            .await
            .unwrap();

        assert_eq!(
            verdict,
            Verdict::Rejected {
                scope: crate::scope::Scope::Account,
                retry_after_ms: SATURATED_RETRY_AFTER_MS,
            },
            "with nowhere to record the charge the only honest answers are refuse or \
             lie, and ADR-0006 says degraded but never open"
        );
        assert!(registry
            .render()
            .contains("migo_ratelimit_fallback_saturated_total 1"));
    }
}
