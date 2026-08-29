//! Integration coverage for the BOTS domain SPEC opcodes.
//!
//! The dispatch handlers in `migod::dispatch::bots` are `pub(crate)`, so an integration test
//! in a separate crate cannot call them directly; the unit test inside that module already
//! drives `migo_bots::open` with an in-memory backend and asserts `register` returns a bot
//! with an id and a one-time token. This crate-level test builds the same in-memory bot
//! service and calls the very `Bots::register` method the `handle_register` handler delegates
//! to, asserting the behaviour that handler relies on.

use std::sync::Arc;

use migo_bots::model::{BotsConfig, Caller, NewBotSpec, Scopes};
use migo_bots::open;
use migo_bots::traits::Bots;
use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Timestamp};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::MemoryStore;

const NOW: i64 = 1_700_000_000_000;
const TOKEN_ROOT: &[u8] = b"migo-bots integration-test token root key material";

/// Builds the bot service over the in-memory store and the real rate limiter, the same way
/// the `handle_register` path wires it.
fn harness() -> (migo_bots::SharedBots, Arc<MemoryStore>) {
    let settings = Config::default();
    let mem = Arc::new(MemoryStore::new());
    let registry = Registry::new();
    let policies =
        Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
    let limiter = Arc::new(CacheRateLimiter::new(
        Arc::new(MemoryCache::new()),
        policies,
        &registry,
    ));
    let store: migo_store::SharedStore = mem.clone();
    let svc = open(store, limiter, BotsConfig::default(), TOKEN_ROOT, &registry)
        .expect("the bot service opens");
    (svc, mem)
}

fn owner(account: u128) -> Caller {
    Caller {
        account_id: Id::from(account),
        device_id: Id::from(account + 1_000_000),
        tier: TrustTier::Established,
        now: Timestamp::from_millis(NOW),
        request_id: None,
    }
}

/// The method `handle_register` delegates to: registering returns a bot with a real id.
#[tokio::test]
async fn register_returns_a_bot_with_an_id() {
    let (svc, _store) = harness();
    let spec = NewBotSpec {
        username: "weather".to_string(),
        display_name: "Weather".to_string(),
        scopes: Scopes::NONE,
        webhook_url: None,
        locale: None,
    };
    let registered = svc
        .register(&owner(1), spec)
        .await
        .expect("registration succeeds");
    assert!(
        !registered.bot.bot_id.is_nil(),
        "a registered bot is given a real id"
    );
    assert_eq!(registered.bot.name, "Weather");
}
