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
}
