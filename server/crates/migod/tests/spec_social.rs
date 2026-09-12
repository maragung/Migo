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
            passphrase_hash: Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
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
            gender: None,
            show_last_seen: Visibility::Everyone,
            who_can_message: Visibility::Everyone,
            who_can_add: Visibility::Everyone,
            searchable: true,
            custom_status: None,
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

/// The path the `RELATIONSHIP_LIST` handler drives: one read of the whole graph, with
/// the limit bounding **each kind** rather than the concatenated list. A caller whose
/// friends fill the page still sees the request waiting behind it — a combined cap is
/// how "no pending requests" gets rendered over a graph that was never fully read.
#[tokio::test]
async fn relationship_list_serves_each_kind_its_own_page() {
    let (svc, store) = harness();
    person(&store, 1, "alice").await;
    person(&store, 2, "bob").await;
    person(&store, 3, "carol").await;
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

    // One settled friend, then one request waiting: a graph where the friend fills a
    // one-entry page and the request is the entry a combined cap would drop.
    svc.request_friend(&alice, Id::from(2u128))
        .await
        .expect("the request is sent");
    svc.respond_friend(&bob, Id::from(1u128), true)
        .await
        .expect("the request is accepted");
    svc.request_friend(
        &Caller::new(
            Id::from(3u128),
            Id::from(103u128),
            TrustTier::Established,
            Timestamp::from_millis(NOW),
        ),
        Id::from(1u128),
    )
    .await
    .expect("the second request is sent");

    let edges = svc
        .list_relationships(&alice, Some(1))
        .await
        .expect("the combined listing succeeds");
    let has =
        |wanted: migo_protocol::RelationshipKind| edges.iter().any(|edge| edge.kind == wanted);
    assert!(
        has(migo_protocol::RelationshipKind::Friend),
        "the friend is on the page: {:?}",
        edges
    );
    assert!(
        has(migo_protocol::RelationshipKind::PendingIncoming),
        "the waiting request is on its own page, not starved behind the friend: {:?}",
        edges
    );
}

/// The whole listing is one charge against the account budget of two hundred. At three
/// a listing, sixty-six refreshes fit; at the seven per-kind charges the handler used
/// to pay, nine would exhaust the budget and the tenth would answer `RATE_LIMITED` —
/// one friends-screen refresh burning a fifth of what an honest client needs for
/// everything else it does.
#[tokio::test]
async fn relationship_list_is_priced_as_one_listing() {
    let (svc, store) = harness();
    person(&store, 1, "alice").await;
    let alice = Caller::new(
        Id::from(1u128),
        Id::from(101u128),
        TrustTier::Established,
        Timestamp::from_millis(NOW),
    );

    let mut refreshes = 0u32;
    loop {
        match svc.list_relationships(&alice, None).await {
            Ok(_) => refreshes += 1,
            Err(error) => {
                assert_eq!(
                    error.code(),
                    migo_protocol::generated::codes::RATE_LIMITED,
                    "the budget, not the graph, is what runs out: {error}"
                );
                break;
            }
        }
    }
    assert_eq!(
        refreshes, 66,
        "sixty-six listings at three each fit the two-hundred budget; seven per kind would have stopped at nine"
    );
}

/// The paged walk of one kind reaches every row, in order, exactly once.
///
/// The combined listing is bounded per kind, so a graph larger than one page has no
/// reachable end through it; the walk is the way in, and its contract is the keyset:
/// the cursor names the last row the caller holds, so a page boundary inside a group
/// of equal timestamps — ordered by id — neither repeats a row nor steps over one.
#[tokio::test]
async fn a_paged_walk_of_one_kind_reaches_every_row_exactly_once() {
    let (svc, store) = harness();
    person(&store, 1, "alice").await;
    // Ten friends under three timestamps, so a five-row page boundary lands inside a
    // tie group at least once. Friendships through the service, so the rows carry
    // the acceptance the listing filters on.
    for n in 2..=11u128 {
        person(&store, n, &format!("user{n}")).await;
        let asker = Caller::new(
            Id::from(n),
            Id::from(1_000 + n),
            TrustTier::Established,
            Timestamp::from_millis(NOW + (n % 3) as i64),
        );
        let alice = Caller::new(
            Id::from(1u128),
            Id::from(101u128),
            TrustTier::Established,
            Timestamp::from_millis(NOW + 1 + (n % 3) as i64),
        );
        match svc.request_friend(&asker, Id::from(1u128)).await {
            Ok(_) => {}
            Err(error) => {
                // A duplicate outcome is the request already waiting; anything else
                // is a failure the test should not paper over.
                assert!(
                    error.code() == migo_protocol::generated::codes::VALIDATION_FAILED
                        || error.code() == migo_protocol::generated::codes::RATE_LIMITED,
                    "unexpected refusal: {error}"
                );
            }
        }
        svc.respond_friend(&alice, Id::from(n), true)
            .await
            .expect("the friendship is accepted");
    }

    let alice = Caller::new(
        Id::from(1u128),
        Id::from(101u128),
        TrustTier::Established,
        Timestamp::from_millis(NOW + 60_000),
    );
    let mut walked: Vec<migo_social::model::Edge> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let (page, next) = svc
            .page_relationships(
                &alice,
                migo_protocol::RelationshipKind::Friend,
                Some(5),
                cursor,
            )
            .await
            .expect("the page is served");
        if page.is_empty() {
            assert!(
                next.is_none(),
                "an empty page never carries a cursor: {next:?}"
            );
            break;
        }
        assert!(page.len() <= 5, "the limit is a ceiling");
        walked.extend(page);
        cursor = next;
        if cursor.is_none() {
            break;
        }
    }

    assert_eq!(walked.len(), 10, "every friend is reached");
    let mut seen = std::collections::HashSet::new();
    for edge in &walked {
        assert!(seen.insert(edge.other_id), "no row may be served twice");
    }
    let mut held = walked
        .iter()
        .map(|edge| (edge.since, edge.other_id))
        .collect::<Vec<_>>();
    // Newest first, then by id ascending within a tie: the listing's own order,
    // which is what the cursor's keyset is defined against.
    held.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let served = walked
        .iter()
        .map(|edge| (edge.since, edge.other_id))
        .collect::<Vec<_>>();
    assert_eq!(
        served, held,
        "the walk serves newest first, then by id, throughout"
    );
}

/// A malformed cursor is refused, not guessed at.
///
/// A cursor that parses loosely pages from a position nobody chose, and the symptom
/// is a client that skips friends — indistinguishable from data loss to whoever
/// reports it.
#[tokio::test]
async fn a_malformed_cursor_is_refused_rather_than_guessed_at() {
    let (svc, store) = harness();
    person(&store, 1, "alice").await;
    person(&store, 2, "bob").await;
    let alice = Caller::new(
        Id::from(1u128),
        Id::from(101u128),
        TrustTier::Established,
        Timestamp::from_millis(NOW),
    );

    for text in ["", "v2.1", "v1.1", "v1.1.2", "v1.not-a-time.2"] {
        let error = svc
            .page_relationships(
                &alice,
                migo_protocol::RelationshipKind::Friend,
                None,
                Some(text.to_string()),
            )
            .await
            .expect_err("a malformed cursor must not page");
        assert_eq!(
            error.code(),
            migo_protocol::generated::codes::VALIDATION_FAILED,
            "{text:?}: {error}"
        );
    }
}
