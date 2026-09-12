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
use migo_store::model::{Currency, EntitlementPosition, LedgerPosition, NewAccount};
use migo_store::traits::{AccountStore, EconomyStore};
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

    let owned = svc
        .entitlements(&buyer, 200, None)
        .await
        .expect("entitlements read");
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
    let owned = svc
        .entitlements(&buyer, 200, None)
        .await
        .expect("entitlements read");
    assert!(
        owned.is_empty(),
        "a refused settlement grants no entitlement"
    );
}

/// The overdraft floor: a spend the balance cannot cover is refused whole —
/// `INSUFFICIENT_BALANCE`, nothing written, the balance unmoved at zero. The floor is the
/// ledger's, not the caller's arithmetic: a buyer with nothing is refused exactly like a
/// buyer five coins short of a ten-coin gift.
#[tokio::test]
async fn a_spend_beyond_the_balance_is_refused_whole() {
    let (svc, store) = harness();
    seed_account(&store, 1, "poor-buyer").await;
    seed_account(&store, 2, "recipient").await;
    let buyer = caller(1, 101);
    let sku = migo_economy::Sku::parse("gift.rose").expect("the default catalogue prices it");

    // Zero coins: the purchase is refused before anything is written.
    let error = svc
        .purchase(&buyer, &sku, "spec:broke", None)
        .await
        .expect_err("a purchase without the balance is refused");
    assert_eq!(error.code(), migo_protocol::codes::INSUFFICIENT_BALANCE);
    assert_eq!(
        svc.wallet(&buyer).await.expect("wallet read").coins,
        0,
        "a refused purchase moves no money"
    );
    assert!(
        svc.entitlements(&buyer, 200, None)
            .await
            .expect("entitlements read")
            .is_empty(),
        "a refused purchase grants no entitlement"
    );

    // Five of ten coins: short is short, whatever the shortfall.
    svc.grant(Grant {
        account_id: Id::from(1u128),
        currency: Currency::Coins,
        amount: 5,
        reason: Reason::Grant,
        ref_id: None,
        idempotency_key: "seed:short".to_string(),
        created_by: None,
        at: Timestamp::from_millis(NOW),
    })
    .await
    .expect("grant succeeds");
    let error = svc
        .purchase(&buyer, &sku, "spec:short", None)
        .await
        .expect_err("a five-coin wallet does not cover a ten-coin gift");
    assert_eq!(error.code(), migo_protocol::codes::INSUFFICIENT_BALANCE);
    assert_eq!(
        svc.wallet(&buyer).await.expect("wallet read").coins,
        5,
        "the refusal leaves the five coins alone"
    );

    // A gift send meets the same floor, on the sender's side.
    let error = svc
        .send_gift(
            &buyer,
            SendGift {
                recipient_id: Id::from(2u128),
                gift: Gift::Rose,
                conversation_id: None,
                client_key: "spec:broke-gift".to_string(),
            },
        )
        .await
        .expect_err("a gift the sender cannot afford is refused");
    assert_eq!(error.code(), migo_protocol::codes::INSUFFICIENT_BALANCE);
    assert_eq!(
        svc.wallet(&buyer).await.expect("wallet read").coins,
        5,
        "the refused gift leaves the balance unmoved"
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

/// A grant helper for coins, so the seeds below say what they mean.
async fn grant(svc: &migo_economy::SharedTreasurer, account: u128, coins: i64) {
    svc.grant(Grant {
        account_id: Id::from(account),
        currency: Currency::Coins,
        amount: coins,
        reason: Reason::Grant,
        ref_id: None,
        idempotency_key: format!("seed:coins:{account}"),
        created_by: None,
        at: Timestamp::from_millis(NOW),
    })
    .await
    .expect("coin grant succeeds");
}

/// A grant helper for KP, mirroring the coins seed above.
async fn grant_kp(svc: &migo_economy::SharedTreasurer, account: u128, kp: i64, key: &str) {
    svc.grant(Grant {
        account_id: Id::from(account),
        currency: Currency::KickPoints,
        amount: kp,
        reason: Reason::Grant,
        ref_id: None,
        idempotency_key: key.to_string(),
        created_by: None,
        at: Timestamp::from_millis(NOW),
    })
    .await
    .expect("kp grant succeeds");
}

/// The kick price, prepaid path: a kicker holding one Kick Point spends it and no coin
/// moves. This is the same `charge_kick` method the messaging tariff calls.
#[tokio::test]
async fn charge_kick_spends_a_held_kick_point_before_any_coin() {
    let (svc, store) = harness();
    seed_account(&store, 1, "kicker").await;
    grant(&svc, 1, 5).await;
    grant_kp(&svc, 1, 1, "seed:kp:1").await;

    let charge = svc
        .charge_kick(
            Id::from(1u128),
            Id::from(50u128),
            Id::from(2u128),
            Timestamp::from_millis(NOW),
        )
        .await
        .expect("the kick is priced");
    assert!(charge.spent_kick_point, "the held point settles the kick");
    assert_eq!(charge.paid_coins, 0);

    let wallet = svc.wallet(&caller(1, 101)).await.expect("wallet read");
    assert_eq!(wallet.kick_points, 0, "the point was spent");
    assert_eq!(wallet.coins, 5, "no coin moved while a point was held");
}

/// The kick price, coin path: a kicker holding no Kick Point pays exactly one coin.
#[tokio::test]
async fn charge_kick_falls_back_to_one_coin_when_no_point_is_held() {
    let (svc, store) = harness();
    seed_account(&store, 1, "kicker").await;
    grant(&svc, 1, 5).await;

    let charge = svc
        .charge_kick(
            Id::from(1u128),
            Id::from(50u128),
            Id::from(2u128),
            Timestamp::from_millis(NOW),
        )
        .await
        .expect("the kick is priced");
    assert!(!charge.spent_kick_point);
    assert_eq!(charge.paid_coins, 1);

    let wallet = svc.wallet(&caller(1, 101)).await.expect("wallet read");
    assert_eq!(wallet.coins, 4, "the coin path costs exactly one coin");
    assert_eq!(wallet.kick_points, 0);
}

/// The kick price, refusal: a kicker holding neither a Kick Point nor a coin is refused,
/// and the refusal names both doors out.
#[tokio::test]
async fn charge_kick_refuses_a_kicker_who_holds_neither() {
    let (svc, store) = harness();
    seed_account(&store, 1, "kicker").await;

    let error = svc
        .charge_kick(
            Id::from(1u128),
            Id::from(50u128),
            Id::from(2u128),
            Timestamp::from_millis(NOW),
        )
        .await
        .expect_err("the kick is refused");
    assert_eq!(error.code(), migo_protocol::codes::INSUFFICIENT_BALANCE);
    let detail = error.to_string();
    assert!(
        detail.contains("Kick Point") && detail.contains("MGO"),
        "the refusal names both the point and the coin: {detail}"
    );
}

/// A retried kick does not pay twice: the (kicker, conversation, target) triple keys the
/// idempotency, so the retry reads back the first settlement. The accepted edge — a
/// founder re-kicking the same rejoined member later is not charged again — is the price
/// of a kick that can never double-charge, and it is paid deliberately.
#[tokio::test]
async fn charge_kick_retry_does_not_double_charge() {
    let (svc, store) = harness();
    seed_account(&store, 1, "kicker").await;
    grant(&svc, 1, 5).await;

    for _ in 0..2 {
        svc.charge_kick(
            Id::from(1u128),
            Id::from(50u128),
            Id::from(2u128),
            Timestamp::from_millis(NOW),
        )
        .await
        .expect("both the kick and its retry succeed");
    }
    let wallet = svc.wallet(&caller(1, 101)).await.expect("wallet read");
    assert_eq!(wallet.coins, 4, "the retry charged nothing further");
}

/// Each pack on the table buys: coins out, points in, at the table's price.
#[tokio::test]
async fn kick_point_packs_buy_at_their_table_price() {
    let (svc, store) = harness();
    seed_account(&store, 1, "buyer").await;
    grant(&svc, 1, 100).await;
    let buyer = caller(1, 101);

    for (pack_kp, price) in migo_economy::KP_PACKS {
        let outcome = svc
            .buy_kick_points(&buyer, *pack_kp, &format!("spec:kp:{pack_kp}"))
            .await
            .expect("the pack buys");
        assert_eq!(
            outcome.price, *price,
            "pack {pack_kp} costs its table price"
        );
        assert!(!outcome.duplicate);
    }

    let wallet = svc.wallet(&buyer).await.expect("wallet read");
    let expected_kp: i64 = migo_economy::KP_PACKS
        .iter()
        .map(|(kp, _)| i64::from(*kp))
        .sum();
    assert_eq!(
        wallet.kick_points, expected_kp,
        "every point of every pack arrived"
    );
    let expected_coins = 100
        - migo_economy::KP_PACKS
            .iter()
            .map(|(_, price)| price)
            .sum::<i64>();
    assert_eq!(wallet.coins, expected_coins, "every pack was paid in full");
}

/// A size the deployment does not sell is refused before anything moves.
#[tokio::test]
async fn kick_point_pack_unknown_size_is_refused_before_anything_moves() {
    let (svc, store) = harness();
    seed_account(&store, 1, "buyer").await;
    grant(&svc, 1, 100).await;

    let error = svc
        .buy_kick_points(&caller(1, 101), 7, "spec:kp:7")
        .await
        .expect_err("an unsold size is refused");
    assert_eq!(error.code(), migo_protocol::codes::VALIDATION_FAILED);

    let wallet = svc.wallet(&caller(1, 101)).await.expect("wallet read");
    assert_eq!(wallet.coins, 100, "nothing was charged for the refusal");
    assert_eq!(wallet.kick_points, 0, "nothing was minted for the refusal");
}

/// An unaffordable pack is refused and no points arrive: the coin leg refuses before the
/// mint leg runs, so a caller is never left holding points they did not pay for.
#[tokio::test]
async fn kick_point_pack_unaffordable_is_refused_before_the_mint_leg() {
    let (svc, store) = harness();
    seed_account(&store, 1, "buyer").await;
    grant(&svc, 1, 1).await;

    let error = svc
        .buy_kick_points(&caller(1, 101), 50, "spec:kp:poor")
        .await
        .expect_err("an unaffordable pack is refused");
    assert_eq!(error.code(), migo_protocol::codes::INSUFFICIENT_BALANCE);

    let wallet = svc.wallet(&caller(1, 101)).await.expect("wallet read");
    assert_eq!(wallet.kick_points, 0, "the mint leg never ran");
    assert_eq!(wallet.coins, 1, "the single coin was not spent");
}

/// A repeated client key is the first buy answered again: no second charge, no second
/// mint, and `duplicate` says which of the two calls this was.
#[tokio::test]
async fn kick_point_buy_retry_is_a_duplicate_and_does_not_double_mint() {
    let (svc, store) = harness();
    seed_account(&store, 1, "buyer").await;
    grant(&svc, 1, 100).await;
    let buyer = caller(1, 101);

    let first = svc
        .buy_kick_points(&buyer, 10, "spec:kp:retry")
        .await
        .expect("the first buy lands");
    assert!(!first.duplicate);
    let second = svc
        .buy_kick_points(&buyer, 10, "spec:kp:retry")
        .await
        .expect("the retry answers the first buy");
    assert!(second.duplicate);
    assert_eq!(
        second.kick_points, first.kick_points,
        "the retry minted nothing further"
    );

    let wallet = svc.wallet(&buyer).await.expect("wallet read");
    assert_eq!(wallet.kick_points, 10, "one buy's worth of points, not two");
    assert_eq!(wallet.coins, 100 - 9, "one pack's price, not two");
}

/// The zero-sum invariant the nightly audit re-asserts over the whole ledger, held here
/// for the new currency across both of its movements: a grant issues from the Mint, a
/// kick returns the point to it, and the sum stays zero whatever a user holds.
#[tokio::test]
async fn kick_points_sum_to_zero_across_a_grant_and_a_spend() {
    let (svc, store) = harness();
    seed_account(&store, 1, "kicker").await;
    grant_kp(&svc, 1, 3, "seed:kp:sum").await;

    for target in 2u128..=3u128 {
        svc.charge_kick(
            Id::from(1u128),
            Id::from(50u128),
            Id::from(target),
            Timestamp::from_millis(NOW),
        )
        .await
        .expect("each kick is priced");
    }

    let sum = store
        .currency_sum(Currency::KickPoints)
        .await
        .expect("the sum reads");
    assert_eq!(
        sum, 0,
        "spent points return to the Mint; nothing is created or destroyed"
    );

    let wallet = svc.wallet(&caller(1, 101)).await.expect("wallet read");
    assert_eq!(
        wallet.kick_points, 1,
        "the kicker holds what the grants left them"
    );
}

/// The path `LEDGER_HISTORY` drives when the statement is longer than one page:
/// page two continues after page one's last line, and walking five seeded grants
/// in pages of two returns each line once, newest first. The position handed
/// between pages is the same `LedgerPosition` the handler decodes from and
/// encodes into the wire cursor, so this walks exactly the walk a paging client
/// takes.
#[tokio::test]
async fn ledger_history_pages_by_position_without_repeating_or_dropping() {
    const MINUTE: i64 = 60_000;
    let (svc, store) = harness();
    seed_account(&store, 1, "spender").await;
    for n in 0..5i64 {
        svc.grant(Grant {
            account_id: Id::from(1u128),
            currency: Currency::Coins,
            amount: 10,
            reason: Reason::Grant,
            ref_id: None,
            idempotency_key: format!("spec:page:{n}"),
            created_by: None,
            at: Timestamp::from_millis(NOW + n * MINUTE),
        })
        .await
        .expect("grant succeeds");
    }

    let reader = caller(1, 101);
    let mut seen: Vec<i64> = Vec::new();
    let mut after: Option<LedgerPosition> = None;
    for _ in 0..5 {
        let page = svc
            .statement(&reader, Currency::Coins, 2, after)
            .await
            .expect("the statement reads");
        if page.is_empty() {
            // The last full page still carried a cursor, so one request past
            // the end comes back empty — that is how the walk stops, not a bug.
            break;
        }
        seen.extend(page.iter().map(|entry| entry.at.as_millis()));
        let last = page.last().expect("a non-empty page has a last row");
        after = Some(LedgerPosition {
            created_at: last.at,
            tx_id: last.tx_id,
        });
    }
    let expected: Vec<i64> = (0..5).rev().map(|n| NOW + n * MINUTE).collect();
    assert_eq!(
        seen, expected,
        "five lines, newest first, each exactly once"
    );
}

/// The path `ENTITLEMENTS` drives when the shelf is longer than one page: five
/// purchases a minute apart, walked in pages of two, come back oldest first with
/// each item once. The position is the same `EntitlementPosition` the handler
/// decodes from and encodes into the wire cursor.
#[tokio::test]
async fn entitlements_page_by_position_without_repeating_or_dropping() {
    const MINUTE: i64 = 60_000;
    let (svc, store) = harness();
    seed_account(&store, 1, "collector").await;
    grant(&svc, 1, 10_000).await;
    let slugs = [
        "gift.rose",
        "gift.heart",
        "gift.cake",
        "gift.star",
        "gift.diamond",
    ];
    for (n, slug) in slugs.iter().enumerate() {
        // Each purchase posts at its own instant — the caller's `now` is the
        // posting time — so the five acquired times are distinct and the walk
        // is decided by the data, not by a tie broken on the sku.
        let mut buyer = caller(1, 101);
        buyer.now = Timestamp::from_millis(NOW + n as i64 * MINUTE);
        let sku = migo_economy::Sku::parse(slug).expect("the default catalogue prices it");
        svc.purchase(&buyer, &sku, &format!("spec:shelf:{n}"), None)
            .await
            .expect("purchase succeeds");
    }

    let buyer = caller(1, 101);
    let mut seen: Vec<String> = Vec::new();
    let mut after: Option<EntitlementPosition> = None;
    for _ in 0..5 {
        let page = svc
            .entitlements(&buyer, 2, after)
            .await
            .expect("the shelf reads");
        if page.is_empty() {
            break;
        }
        seen.extend(page.iter().map(|held| held.sku.clone()));
        let last = page.last().expect("a non-empty page has a last row");
        after = Some(EntitlementPosition {
            acquired_at: last.acquired_at,
            sku: last.sku.clone(),
        });
    }
    assert_eq!(
        seen,
        slugs.map(str::to_string),
        "five items, oldest first, each exactly once"
    );
}
