//! Integration coverage for the NOTIFY opcodes, driving the same service methods the handlers call.
//!
//! `migod`'s `notify` dispatch module is `pub(crate)`, so this integration test cannot reach
//! `handle_ack`/`handle_list` directly; instead it builds the identical in-memory [`SharedNotifier`]
//! the composition root would and asserts on the exact methods those handlers invoke —
//! [`Notifier::acknowledge`] for `NOTIFICATION_ACK` and [`Notifier::inbox`] for `NOTIFICATION_LIST`.

use std::sync::Arc;

use migo_cache::MemoryCache;
use migo_core::metrics::Registry;
use migo_core::{Id, Random, Secret, SeededRandom, Timestamp};
use migo_notify::{open, Caller, Event, NoPush, NotifyConfig, SharedNotifier, SharedPushSender};
use migo_protocol::NotificationKind;
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::model::NewAccount;
use migo_store::{MemoryStore, SharedStore};

const ROOT_SECRET: &[u8] = b"migo-notify spec integration test root secret v1";

/// Builds a `NewAccount` row for the one id the tests share.
fn seed_account(account: u128) -> NewAccount {
    NewAccount {
        account_id: Id::from(account),
        username: format!("user-{account}"),
        email: Some(format!("user-{account}@example.test")),
        phone: None,
        password_hash: Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
        locale: "id-ID".to_string(),
        country: Some("ID".to_string()),
        created_at: Timestamp::from_millis(1),
    }
}

async fn in_memory() -> SharedNotifier {
    let store: SharedStore = Arc::new(MemoryStore::new());
    store
        .create_account(seed_account(1))
        .await
        .expect("a seed account can be created");
    let cache: migo_cache::SharedCache = Arc::new(MemoryCache::new());
    let registry = Registry::new();
    let limiter: migo_ratelimit::SharedRateLimiter = Arc::new(CacheRateLimiter::new(
        Arc::new(MemoryCache::new()),
        Policies::default(),
        &registry,
    ));
    let sender: SharedPushSender = Arc::new(NoPush);
    open(
        store,
        cache,
        limiter,
        sender,
        Box::new(SeededRandom::new(7)) as Box<dyn Random>,
        ROOT_SECRET,
        NotifyConfig::default(),
        &registry,
    )
}

fn caller(account: u128, device: u128, now: Timestamp) -> Caller {
    Caller::new(Id::from(account), Id::from(device), TrustTier::Established, now)
}

/// `NOTIFICATION_ACK` resolves to `Notifier::acknowledge`; on an empty inbox it returns Ok with zero
/// rows changed, and on a seeded inbox it flips the unread count to zero.
#[tokio::test]
async fn ack_marks_seeded_inbox_read() {
    let svc = in_memory().await;
    let now = Timestamp::from_millis(5_000);

    let seeded = svc
        .notify(Event::new(Id::from(1), NotificationKind::Gift, now))
        .await
        .expect("the gift is stored");
    assert!(seeded.stored, "a gift becomes an inbox row");

    let inbox = svc
        .inbox(&caller(1, 2, now), 20)
        .await
        .expect("inbox reads");
    assert_eq!(inbox.unread, 1);

    let through = inbox.items[0].notification_id.timestamp();
    let changed = svc
        .acknowledge(&caller(1, 2, now), through)
        .await
        .expect("acknowledge succeeds");
    assert_eq!(changed, 1);

    let after = svc
        .inbox(&caller(1, 2, now), 20)
        .await
        .expect("inbox reads");
    assert_eq!(after.unread, 0);
}

/// `NOTIFICATION_LIST` resolves to `Notifier::inbox`; a fresh account gets an empty page.
#[tokio::test]
async fn list_returns_empty_page_for_fresh_account() {
    let svc = in_memory().await;
    let inbox = svc
        .inbox(&caller(1, 2, Timestamp::from_millis(1_000)), 50)
        .await
        .expect("inbox reads");
    assert!(inbox.items.is_empty());
    assert_eq!(inbox.unread, 0);
}
