//! ECONOMY SPEC opcodes: the service methods the dispatch handlers delegate to.
//!
//! These run against the real economy service over the in-memory store and the real rate
//! limiter, so they exercise the same `Treasurer` methods the `migod` handlers call — and
//! assert the behaviour those handlers rely on: a fresh wallet reads as zero, and sending a
//! gift spends the sender's coins.

use std::sync::Arc;

use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Secret, Timestamp};
use migo_economy::{Catalogue, EconomyConfig, Gift, Grant, Reason, SendGift};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::model::{Currency, NewAccount};
use migo_store::traits::AccountStore;
use migo_store::MemoryStore;

const NOW: i64 = 1_700_000_000_000;

fn harness() -> (migo_economy::SharedTreasurer, Arc<MemoryStore>) {
    let settings = Config::default();
    let mem = Arc::new(MemoryStore::new());
    let registry = Registry::new();
    let policies = Policies::from_config(&settings.rate_limit).expect("default policies are valid");
    let limiter = Arc::new(CacheRateLimiter::new(
        Arc::new(MemoryCache::new()),
        policies,
        &registry,
    ));
    let cache: migo_cache::SharedCache = Arc::new(MemoryCache::new());
    let announcer = Arc::new(migo_economy::Silent);
    let store: migo_store::SharedStore = mem.clone();
    let svc = migo_economy::open(
        store,
        cache,
        limiter,
        announcer,
        Catalogue::with_default_gifts(),
        EconomyConfig::default(),
        &registry,
    );
    (svc, mem)
}

/// Seeds the minimal account rows the economy methods need (a gift sender and a recipient).
async fn seed_account(store: &Arc<MemoryStore>, account: u128, username: &str) {
    store
        .create_account(NewAccount {
            account_id: Id::from(account),
            username: username.to_string(),
            email: None,
            phone: None,
            passphrase_hash: Secret::new("unused"),
            locale: "id-ID".to_string(),
            country: Some("ID".to_string()),
            created_at: Timestamp::from_millis(NOW),
        })
        .await
        .expect("seed account");
}

fn caller(account: u128, device: u128) -> migo_economy::Caller {
    migo_economy::Caller {
        account_id: Id::from(account),
        device_id: Id::from(device),
        tier: TrustTier::Established,
        now: Timestamp::from_millis(NOW),
        request_id: None,
    }
}

/// The path `BALANCE_FETCH` drives: a wallet that has never been touched reads as zero.
#[tokio::test]
async fn balance_fetch_on_fresh_wallet_is_zero() {
    let (svc, _) = harness();
    let wallet = svc.wallet(&caller(1, 101)).await.expect("wallet read");
    assert_eq!(wallet.coins, 0);
    assert_eq!(wallet.points, 0);
}

/// The path `GIFT_SEND` drives: funding a sender, then sending a gift, leaves the sender
/// poorer by the gift's price. This is the same `send_gift` method the handler calls.
#[tokio::test]
async fn gift_send_spends_the_sender_coins() {
    let (svc, store) = harness();
    seed_account(&store, 1, "sender").await;
    seed_account(&store, 2, "recipient").await;
    let sender = caller(1, 101);

    svc.grant(Grant {
        account_id: Id::from(1u128),
        currency: Currency::Coins,
        amount: 1000,
        reason: Reason::Grant,
        ref_id: None,
        idempotency_key: "seed:1".to_string(),
        created_by: None,
        at: Timestamp::from_millis(NOW),
    })
    .await
    .expect("grant succeeds");

    let before = svc.wallet(&sender).await.expect("wallet read").coins;
    let outcome = svc
        .send_gift(
            &sender,
            SendGift {
                recipient_id: Id::from(2u128),
                gift: Gift::Rose,
                conversation_id: None,
                client_key: "spec:1".to_string(),
            },
        )
        .await
        .expect("gift sent");
    assert!(!outcome.duplicate);
    let after = svc.wallet(&sender).await.expect("wallet read").coins;
    assert!(
        after < before,
        "the sender's coins decreased by the gift price"
    );

    // The wire now carries a client key per intent, so a genuine retry — same
    // key, the network having eaten the first answer — is the first send
    // again: same gift row, no second charge, and `duplicate` set so the
    // handler can report the gift standing instead of failing.
    let retry = svc
        .send_gift(
            &sender,
            SendGift {
                recipient_id: Id::from(2u128),
                gift: Gift::Rose,
                conversation_id: None,
                client_key: "spec:1".to_string(),
            },
        )
        .await
        .expect("retry is answered, not failed");
    assert!(retry.duplicate, "the retry returns the first send");
    assert_eq!(
        retry.gift_id, outcome.gift_id,
        "the retry names the same gift row"
    );
    let after_retry = svc.wallet(&sender).await.expect("wallet read").coins;
    assert_eq!(
        after_retry, after,
        "the retry charges nothing on top of the first send"
    );
}

/// The path `STORE_PURCHASE` drives with coins: a funded buyer pays the catalogue price
/// once, owns the item, and a retry with the same key is the first purchase again — this
/// is the same `purchase` method the handler calls.
#[tokio::test]
async fn store_purchase_charges_once_and_grants_the_entitlement() {
    let (svc, store) = harness();
    seed_account(&store, 1, "buyer").await;
    let buyer = caller(1, 101);
    let sku = migo_economy::Sku::parse("gift.rose").expect("the default catalogue prices it");

    svc.grant(Grant {
        account_id: Id::from(1u128),
        currency: Currency::Coins,
        amount: 1000,
        reason: Reason::Grant,
        ref_id: None,
        idempotency_key: "seed:buyer".to_string(),
        created_by: None,
        at: Timestamp::from_millis(NOW),
    })
    .await
    .expect("grant succeeds");

    let first = svc
        .purchase(&buyer, &sku, "spec:rose", None)
        .await
        .expect("purchase succeeds");
    assert!(!first.duplicate);
    assert_eq!(first.price.amount, 10);
    let paid = 1000 - svc.wallet(&buyer).await.expect("wallet read").coins;
    assert_eq!(paid, 10, "the buyer paid exactly the catalogue price");

    let owned = svc.entitlements(&buyer).await.expect("entitlements read");
    assert_eq!(owned.len(), 1, "the buyer owns the item they paid for");
    assert_eq!(owned[0].sku, "gift.rose");

    let retry = svc
        .purchase(&buyer, &sku, "spec:rose", None)
        .await
        .expect("retry is answered, not failed");
    assert!(
        retry.duplicate,
        "a retry with the same key is the first purchase again"
    );
    let paid_twice = 1000 - svc.wallet(&buyer).await.expect("wallet read").coins;
    assert_eq!(paid_twice, 10, "the retry charged nothing");
}

/// The trap the on-chain path used to spring: a client that had already paid real currency
/// on the chain was then charged the full coin price too — and refused for unaffordable
/// coins after the money had left. A purchase claiming on-chain settlement is now refused
/// outright (`FEATURE_DISABLED`) before anything is written, whatever the buyer's balance,
/// because a hash this node cannot verify is not a payment method it will honour.
#[tokio::test]
async fn an_on_chain_claim_is_refused_before_anything_is_written() {
    let (svc, store) = harness();
    seed_account(&store, 1, "buyer").await;
    let buyer = caller(1, 101);
    let sku = migo_economy::Sku::parse("gift.rose").expect("the default catalogue prices it");

    // Funded on purpose: the refusal must not depend on the buyer being unable to pay.
    svc.grant(Grant {
        account_id: Id::from(1u128),
        currency: Currency::Coins,
        amount: 10_000,
        reason: Reason::Grant,
        ref_id: None,
        idempotency_key: "seed:rich-buyer".to_string(),
        created_by: None,
        at: Timestamp::from_millis(NOW),
    })
    .await
    .expect("grant succeeds");

    let error = svc
        .purchase(&buyer, &sku, "spec:chain", Some("0xdeadbeef"))
        .await
        .expect_err("an unverifiable on-chain claim is refused");
    assert_eq!(error.code(), migo_protocol::codes::FEATURE_DISABLED);

    let wallet = svc.wallet(&buyer).await.expect("wallet read");
    assert_eq!(wallet.coins, 10_000, "a refused settlement moves no money");
    let owned = svc.entitlements(&buyer).await.expect("entitlements read");
    assert!(
        owned.is_empty(),
        "a refused settlement grants no entitlement"
    );
}

/// One XP award intent, shared by the cap tests below.
fn xp_award(amount: i64, key: &str, at: i64) -> migo_economy::Award {
    migo_economy::Award {
        account_id: Id::from(1u128),
        source: migo_economy::Source::Game,
        amount,
        ref_id: None,
        idempotency_key: Some(key.to_string()),
        at: Timestamp::from_millis(at),
    }
}

/// The daily caps bind inside the award write: the smaller remaining headroom is granted,
/// the rest is refused without a write, and the day's window is rolling — the same
/// `award` method the dispatch path calls. Defaults: 2,000 per game source, 3,000 global.
#[tokio::test]
async fn xp_daily_caps_clamp_inside_the_award_write() {
    let (svc, store) = harness();
    seed_account(&store, 1, "player").await;

    let first = svc
        .award(xp_award(1_500, "spec:xp:1", NOW))
        .await
        .expect("the first award lands");
    assert_eq!(first.granted, 1_500);
    assert!(!first.capped);
    assert_eq!(first.after - first.before, 1_500);

    // Only 500 of the source cap's headroom remains: the request is cut, not refused,
    // and the cut is reported so a client can say "capped", not "nothing".
    let second = svc
        .award(xp_award(1_000, "spec:xp:2", NOW))
        .await
        .expect("the second award lands");
    assert_eq!(second.granted, 500, "the source cap binds at its remainder");
    assert!(second.capped);

    // The cap is met: nothing granted, nothing written, standing unchanged.
    let third = svc
        .award(xp_award(1, "spec:xp:3", NOW))
        .await
        .expect("a capped award answers, not fails");
    assert_eq!(third.granted, 0);
    assert!(third.capped);
    assert_eq!(third.before, third.after, "nothing moved");

    // The window is rolling: a day later the same source has its headroom again.
    let next_day = svc
        .award(xp_award(1, "spec:xp:4", NOW + 25 * 60 * 60 * 1000))
        .await
        .expect("a new day grants again");
    assert_eq!(next_day.granted, 1);
}

/// The race the capped write exists to close: two awards arriving together must not each
/// read the same headroom and spend the cap twice. Against the memory store the write
/// lock serialises them; against PostgreSQL the advisory lock does. The property under
/// test is the total: whatever interleaving, the day's source cap is not exceeded.
#[tokio::test]
async fn concurrent_xp_awards_cannot_spend_the_cap_twice() {
    let (svc, store) = harness();
    seed_account(&store, 1, "racer").await;

    let (first, second) = tokio::join!(
        svc.award(xp_award(2_000, "spec:xp:race:1", NOW)),
        svc.award(xp_award(2_000, "spec:xp:race:2", NOW)),
    );
    let first = first.expect("the first award lands");
    let second = second.expect("the second award lands");
    let granted = first.granted + second.granted;
    assert!(
        granted <= 2_000,
        "two concurrent awards cannot spend the source cap twice: {granted} granted"
    );
    assert!(
        granted > 0,
        "the race must not refuse the first award outright either"
    );
    assert_eq!(
        first.after, second.after,
        "both answers see the same final progression row"
    );
}
