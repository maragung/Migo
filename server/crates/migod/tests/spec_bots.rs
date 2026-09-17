//! Integration coverage for the BOTS domain SPEC opcodes.
//!
//! The dispatch handlers in `migod::dispatch::bots` are `pub(crate)`, so an integration test
//! in a separate crate cannot call them directly; the unit test inside that module already
//! drives `migo_bots::open` with an in-memory backend and asserts `register` returns a bot
//! with an id and a one-time token. This crate-level test builds the same in-memory bot
//! service and calls the very `Bots::register` method the `handle_register` handler delegates
//! to, asserting the behaviour that handler relies on.

use std::sync::Arc;

use async_trait::async_trait;
use migo_bots::model::{BotsConfig, Caller, NewBotSpec, Scopes};
use migo_bots::open;
use migo_bots::traits::Webhook;
use migo_core::Result;

/// A webhook sink that does nothing, for tests that never command a bot.
struct NoopWebhook;

#[async_trait]
impl Webhook for NoopWebhook {
    async fn deliver(&self, _url: &str, _payload: &[u8]) -> Result<()> {
        Ok(())
    }
}
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
    // A webhook sink that records instead of speaking HTTPS; the register path here never
    // commands a bot, so an always-succeeding no-op is exactly the stand-in it needs.
    let sink: migo_bots::SharedWebhook = Arc::new(NoopWebhook);
    let svc = open(
        store,
        limiter,
        BotsConfig::default(),
        TOKEN_ROOT,
        &registry,
        sink,
    )
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

/// Registers a bot the way every management test below needs one: owned by `account`, named,
/// and holding nothing.
async fn a_bot(
    svc: &migo_bots::SharedBots,
    account: u128,
    username: &str,
) -> migo_bots::Registered {
    svc.register(
        &owner(account),
        NewBotSpec {
            username: username.to_string(),
            display_name: username.to_string(),
            scopes: Scopes::NONE,
            webhook_url: None,
            locale: None,
        },
    )
    .await
    .expect("registration succeeds")
}

/// The method `handle_list` delegates to: the caller's own bots, and only those.
///
/// The negative half is the point. A list that answered with every bot in the store would
/// pass a test that registered one bot and read it back, and would hand every owner the names
/// of every other owner's integrations — so this registers for two accounts and asserts the
/// answer is partitioned.
#[tokio::test]
async fn list_answers_with_the_callers_own_bots_and_no_others() {
    let (svc, _store) = harness();
    let mine = a_bot(&svc, 1, "weather").await;
    let also_mine = a_bot(&svc, 1, "clock").await;
    let theirs = a_bot(&svc, 2, "theirs").await;

    let listed = svc.list(&owner(1)).await.expect("the owner can list");

    let ids: Vec<_> = listed.iter().map(|bot| bot.bot_id).collect();
    assert!(ids.contains(&mine.bot.bot_id));
    assert!(ids.contains(&also_mine.bot.bot_id));
    assert!(
        !ids.contains(&theirs.bot.bot_id),
        "an owner's list must not name another account's bot"
    );
    assert_eq!(ids.len(), 2);
}

/// The method `handle_rotate` delegates to: the pair it replies with is the bot it rotated.
///
/// The handler replies with `rotated.bot` and `rotated.token`, so a rotation that returned a
/// default view — or the view of some other row — would answer the owner with a bot that is
/// not the one whose credential they just replaced.
#[tokio::test]
async fn rotate_hands_back_the_bot_whose_token_it_replaced() {
    let (svc, _store) = harness();
    let registered = a_bot(&svc, 1, "weather").await;

    let rotated = svc
        .rotate_token(&owner(1), registered.bot.bot_id)
        .await
        .expect("the owner can rotate");

    assert_eq!(rotated.bot.bot_id, registered.bot.bot_id);
    assert_eq!(rotated.bot.name, registered.bot.name);
    assert_ne!(
        rotated.token.expose(),
        registered.token.expose(),
        "a rotation that returned the same secret would not be a rotation"
    );
}

/// The method `handle_pause` delegates to: pausing is visible on the view and on the next
/// list, and resuming is the same call with the flag the other way.
#[tokio::test]
async fn pausing_shows_on_the_view_and_survives_into_the_next_list() {
    let (svc, _store) = harness();
    let registered = a_bot(&svc, 1, "weather").await;
    assert!(!registered.bot.disabled, "a new bot is not paused");

    let paused = svc
        .set_paused(&owner(1), registered.bot.bot_id, true)
        .await
        .expect("the owner can pause");
    assert!(paused.disabled);
    assert!(paused.disabled_at.is_some(), "pausing records when");

    let listed = svc.list(&owner(1)).await.expect("the owner can list");
    assert_eq!(listed.len(), 1);
    assert!(
        listed[0].disabled,
        "the pause is a stored row and not a value returned once"
    );

    let resumed = svc
        .set_paused(&owner(1), registered.bot.bot_id, false)
        .await
        .expect("the owner can resume");
    assert!(!resumed.disabled);
    assert!(resumed.disabled_at.is_none());
}

/// The method `handle_scopes` delegates to: a replacement, and the slug vocabulary the
/// handler parses the wire's strings through.
///
/// `scopes_from_slugs` is private to the dispatch module, so the rule it enforces is pinned
/// here at the two halves it is built from: every slug a client may send round-trips, and a
/// slug no build defines is refused rather than folded into the set as nothing. That second
/// half is the one that matters — a handler that dropped unknown slugs would answer a request
/// for `moderate` with a bot that silently lacks it, and the owner would find out when the
/// bot failed to do the thing they granted.
#[tokio::test]
async fn scopes_are_replaced_wholesale_and_unknown_slugs_are_refused() {
    let (svc, _store) = harness();
    let registered = a_bot(&svc, 1, "weather").await;
    let bot_id = registered.bot.bot_id;

    // Every slug the wire may carry round-trips, and the set built from them is exactly the
    // set the view reports back.
    let wanted = [Scopes::SEND_MESSAGES, Scopes::READ_MEMBERS];
    let mut requested = Scopes::NONE;
    for scope in wanted {
        let slug = scope.slug().expect("a named scope has a slug");
        let parsed = Scopes::from_slug(slug).expect("a slug this build emits parses back");
        assert_eq!(parsed, scope);
        requested = requested.with(parsed);
    }

    let widened = svc
        .set_scopes(&owner(1), bot_id, requested)
        .await
        .expect("the owner can set scopes");
    assert!(widened.scopes.contains(Scopes::SEND_MESSAGES));
    assert!(widened.scopes.contains(Scopes::READ_MEMBERS));
    assert!(!widened.scopes.contains(Scopes::MODERATE));

    // A replacement, not a union: the second call drops what the first granted.
    let narrowed = svc
        .set_scopes(&owner(1), bot_id, Scopes::NONE)
        .await
        .expect("the owner can clear scopes");
    assert!(
        narrowed.scopes.is_empty(),
        "setting scopes replaces the set rather than widening it"
    );

    // The rule the handler refuses on.
    assert!(
        Scopes::from_slug("moderate_extra").is_none(),
        "a slug no build defines must be refused, not folded in as nothing"
    );
    assert!(Scopes::from_slug("").is_none());
}
