//! MODERATION SPEC opcodes: the service methods the dispatch handlers delegate to.
//!
//! These run against the real moderation service over the in-memory store and the real rate
//! limiter, so they exercise the same `Warden` methods `handle_report` and `handle_action` call —
//! and assert the behaviour those handlers rely on: a report that is filed returns a fresh id, and
//! a re-authenticated operator's decision closes the case.

use std::sync::Arc;

use async_trait::async_trait;
use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::random::OsRandom;
use migo_core::{Id, Result, Timestamp};
use migo_moderation::Caller as WardenCaller;
use migo_moderation::{
    open, Filing, ModerationConfig, Operator, Powers, Reason, Resolution, Roster, SharedRoster,
    SharedWarden, Subject, Warden,
};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::MemoryStore;

const NOW: i64 = 1_700_000_000_000;

/// A roster granting every power, so the action path is exercisable without a deployment.
struct AllStaff;

#[async_trait]
impl Roster for AllStaff {
    async fn powers(&self, _account_id: Id) -> Result<Powers> {
        Ok(Powers::ALL)
    }
}

/// Builds a fully in-memory warden, exactly as the production composition root would wire it.
fn warden() -> SharedWarden {
    let store: Arc<MemoryStore> = Arc::new(MemoryStore::new());
    let registry = Registry::new();
    let policies =
        Policies::from_config(&Config::default().rate_limit).expect("default policies are valid");
    let limiter: Arc<CacheRateLimiter<MemoryCache>> = Arc::new(CacheRateLimiter::new(
        Arc::new(MemoryCache::new()),
        policies,
        &registry,
    ));
    let roster: SharedRoster = Arc::new(AllStaff);
    open(
        store,
        limiter,
        roster,
        Box::new(OsRandom),
        ModerationConfig::default(),
        &registry,
    )
}

/// The path `handle_report` drives: a user files a report and the service mints a case.
#[tokio::test]
async fn report_creates_a_case() {
    let svc = warden();
    let reporter = WardenCaller::new(
        Id::from(1u128),
        Id::from(101u128),
        TrustTier::Established,
        Timestamp::from_millis(NOW),
    );
    let filed = svc
        .file_report(
            &reporter,
            Filing::new(Subject::User(Id::from(2u128)), Reason::Spam),
        )
        .await
        .expect("the report files");
    assert!(!filed.duplicate);
}

/// The path `handle_action` drives: a re-authenticated operator resolves the case shut.
#[tokio::test]
async fn action_resolves_the_case() {
    let svc = warden();
    let reporter = WardenCaller::new(
        Id::from(1u128),
        Id::from(101u128),
        TrustTier::Established,
        Timestamp::from_millis(NOW),
    );
    let filed = svc
        .file_report(
            &reporter,
            Filing::new(Subject::User(Id::from(2u128)), Reason::Spam),
        )
        .await
        .expect("the report files");

    let operator = Operator::new(
        Id::from(3u128),
        Id::from(103u128),
        Powers::NONE,
        Timestamp::from_millis(NOW),
    )
    .reauthenticated();
    let case = svc
        .resolve(&operator, filed.report_id, Resolution::NoAction, None)
        .await
        .expect("the case resolves");
    assert!(!case.is_open());
}
