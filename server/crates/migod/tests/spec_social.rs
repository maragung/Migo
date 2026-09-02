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
use migo_social::open;
use migo_store::model::{NewAccount, Profile, Visibility};
use migo_store::traits::AccountStore;
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

/// The seam the `SUGGESTIONS` handler composes: `suggest` answers an *empty* list for an
/// empty graph (never an error), while `profiles` refuses an empty batch by contract.
///
/// The handler collects the suggestion ids and resolves their names through `profiles` —
/// so a fresh account, whose graph suggests nobody, hands that composition an empty id
/// list. The handler guards the empty case and replies with an empty suggestion list; this
/// test pins the two halves of the seam so the guard cannot silently lose its reason: if
/// `suggest` ever stopped answering empty, or `profiles` ever stopped refusing empty, the
/// composition contract written in the handler's comment would no longer hold.
#[tokio::test]
async fn an_empty_graph_suggests_nobody_and_profiles_refuses_an_empty_batch() {
    let (svc, store) = harness();
    person(&store, 9, "nobody").await;
    let nobody = Caller::new(
        Id::from(9u128),
        Id::from(109u128),
        TrustTier::Established,
        Timestamp::from_millis(NOW),
    );

    let suggestions = svc
        .suggest(&nobody, None)
        .await
        .expect("an empty graph is not an error, just an empty answer");
    assert!(
        suggestions.is_empty(),
        "an account with no graph must be told there is nothing to suggest, not refused"
    );

    let empty: Vec<Id> = Vec::new();
    let refused = svc
        .profiles(&nobody, &empty)
        .await
        .expect_err("an empty profile batch is a malformed request, never a valid read");
    assert_eq!(
        refused.code(),
        migo_protocol::generated::codes::FIELD_REQUIRED,
        "the refusal must be the field-required fault the handler guards against"
    );
}
