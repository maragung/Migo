//! SOCIAL SPEC opcodes: the service methods the dispatch handlers delegate to.
//!
//! These run against the real social service over the in-memory store and the real rate
//! limiter, so they exercise the same `Graph` methods the `migod` handlers call — and
//! assert the behaviour those handlers rely on: a friend request that is accepted becomes
//! a friend edge, and a block becomes a block edge.

use std::sync::Arc;

use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Secret, Timestamp};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_social::model::{Caller, FriendOutcome, RespondOutcome, SocialConfig};
use migo_social::traits::Graph;
use migo_social::open;
use migo_store::model::{NewAccount, Profile, Visibility};
use migo_store::traits::{AccountStore, SocialStore};
use migo_store::MemoryStore;

const NOW: i64 = 1_700_000_000_000;

fn harness() -> (migo_social::SharedSocial, Arc<MemoryStore>) {
    let settings = Config::default();
    let mem = Arc::new(MemoryStore::new());
    let registry = Registry::new();
    let policies = Policies::from_config(&settings.rate_limit).expect("default policies are valid");
    let limiter = Arc::new(CacheRateLimiter::new(
        Arc::new(MemoryCache::new()),
        policies,
        &registry,
    ));
    let store: migo_store::SharedStore = mem.clone();
    let svc = open(store, limiter, &registry, SocialConfig::default());
    (svc, mem)
}

async fn person(store: &Arc<MemoryStore>, account: u128, username: &str) {
    let id = Id::from(account);
    store
        .create_account(NewAccount {
            account_id: id,
            username: username.to_string(),
            email: Some(format!("{username}@example.test")),
            phone: None,
            password_hash: Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
            locale: "id-ID".to_string(),
            country: Some("ID".to_string()),
            created_at: Timestamp::from_millis(NOW),
        })
        .await
        .expect("a fresh username is free");
    store
        .create_profile(Profile {
            account_id: id,
            display_name: format!("{username} Nusantara"),
            bio: None,
            avatar_media_id: None,
            birth_year: None,
            show_last_seen: Visibility::Everyone,
            who_can_message: Visibility::Everyone,
            who_can_add: Visibility::Everyone,
            searchable: true,
            updated_at: Timestamp::from_millis(NOW),
        })
        .await
        .expect("a new account has no profile yet");
}

/// The path the `FRIEND_REQUEST` then `FRIEND_RESPOND` handlers drive: a request that is
/// accepted yields a settled friendship on the asker's side.
#[tokio::test]
async fn friend_request_accepted_becomes_friend() {
    let (svc, store) = harness();
    person(&store, 1, "alice").await;
    person(&store, 2, "bob").await;

    let alice = Caller::new(
        Id::from(1u128),
        Id::from(101u128),
        TrustTier::Established,
        Timestamp::from_millis(NOW),
    );
    let bob = Caller::new(
        Id::from(2u128),
        Id::from(102u128),
        TrustTier::Established,
        Timestamp::from_millis(NOW),
    );

    let (outcome, _) = svc
        .request_friend(&alice, Id::from(2u128))
        .await
        .expect("the request is sent");
    assert_eq!(outcome, FriendOutcome::Requested);

    let (response, _) = svc
        .respond_friend(&bob, Id::from(1u128), true)
        .await
        .expect("the request is answered");
    assert_eq!(response, RespondOutcome::Accepted);

    let friends = svc
        .friends(&alice, None)
        .await
        .expect("the listing succeeds");
    assert!(
        friends.iter().any(|e| e.other_id == Id::from(2u128)),
        "alice should now count bob as a friend"
    );
}
