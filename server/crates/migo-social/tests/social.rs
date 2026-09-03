//! The social graph, tested where getting it wrong is invisible.
//!
//! Most of this crate's surface is a listing, and a listing that returns the wrong rows
//! is caught by the first person who looks at it. The parts that are not caught that way
//! are the ones these tests are about:
//!
//! **A block is symmetric.** Every path is checked in both directions, because a block
//! written one way round that only stops contact one way round still looks like it
//! works: the blocker stops seeing the other person, and the other person keeps getting
//! through.
//!
//! **A refusal never says who refused.** Brief section 180 requires that a caller
//! cannot tell "this account blocked me" from "this account's settings exclude you", so
//! both are `PRIVACY_RESTRICTED` and a missing account is an omission rather than a
//! `NOT_FOUND`. Every test that asserts a code here is asserting an information
//! boundary, not an error message.
//!
//! **A pending request is not a friendship.** `accepted` decides, everywhere. A request
//! that read as a friendship would let anybody reach a `Friends`-only field by asking to
//! be a friend and never being answered, which is a privacy bypass with a one-line
//! exploit.
//!
//! **A gate that cannot finish refuses.** The mutual-friend scan is bounded, and past
//! the bound the answer is "no mutual friend" rather than "probably fine". The test for
//! it builds an account with more friends than the bound and the shared friend just
//! outside it, which is the shape a privacy gate that fails open gets wrong.
//!
//! The rate limiter is the real one over a real cache, so the arithmetic is part of the
//! test: a friend request costs ten and an account's burst is two hundred, which is why
//! the budget tests count to twenty.

use std::sync::Arc;

use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Result, Secret, Timestamp};
use migo_protocol::{codes, NotificationKind, Opcode, RelationshipKind};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_social::model::{
    query_is_usable, strictest, Caller, Edge, FriendOutcome, Interaction, RespondOutcome,
    SocialConfig, Standing, DEFAULT_PAGE, MAX_FAVORITES, MAX_MUTUAL_SCAN, MAX_PAGE,
    MAX_PROFILE_BATCH, MAX_QUERY_LEN,
};
use migo_social::notice::Notice;
use migo_social::service::Social;
use migo_social::traits::Graph;
use migo_store::model::{
    AccountStatus, NewAccount, Profile, ProfilePatch, Relationship, Visibility,
};
use migo_store::traits::{AccountStore, SocialStore};
use migo_store::MemoryStore;

const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const NOW: i64 = 1_700_000_000 * SECOND;

const ALICE: u128 = 1;
const BOB: u128 = 2;
const CAROL: u128 = 3;
const DAVE: u128 = 4;
const ERIN: u128 = 5;
const STRANGER: u128 = 9;

const ALICE_PHONE: u128 = 101;
const BOB_LAPTOP: u128 = 102;
const CAROL_PHONE: u128 = 103;
const DAVE_TABLET: u128 = 104;
const ERIN_PHONE: u128 = 105;

type TestSocial = Social<MemoryStore, CacheRateLimiter<MemoryCache>>;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

fn caller(account: u128, device: u128) -> Caller {
    Caller::new(id(account), id(device), TrustTier::Established, ts(NOW))
}

/// Everything a test needs, with the real limiter over a real cache.
struct Harness {
    social: TestSocial,
    store: Arc<MemoryStore>,
    registry: Registry,
}

impl Harness {
    fn new() -> Self {
        Self::configured(SocialConfig::default())
    }

    fn configured(config: SocialConfig) -> Self {
        let settings = Config::default();
        let store = Arc::new(MemoryStore::new());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(
            Arc::new(MemoryCache::new()),
            policies,
            &registry,
        ));
        let social = Social::new(Arc::clone(&store), limiter, &registry, config);
        Self {
            social,
            store,
            registry,
        }
    }

    /// An account with a profile that excludes nobody.
    async fn person(&self, account: u128, username: &str) {
        self.seed(
            account,
            username,
            Visibility::Everyone,
            Visibility::Everyone,
            Visibility::Everyone,
            true,
        )
        .await;
    }

    /// An account whose three visibility columns are set individually.
    async fn person_with(
        &self,
        account: u128,
        username: &str,
        who_can_message: Visibility,
        who_can_add: Visibility,
        show_last_seen: Visibility,
    ) {
        self.seed(
            account,
            username,
            who_can_message,
            who_can_add,
            show_last_seen,
            true,
        )
        .await;
    }

    /// An account that opted out of being findable.
    async fn unlisted_person(&self, account: u128, username: &str) {
        self.seed(
            account,
            username,
            Visibility::Everyone,
            Visibility::Everyone,
            Visibility::Everyone,
            false,
        )
        .await;
    }

    async fn seed(
        &self,
        account: u128,
        username: &str,
        who_can_message: Visibility,
        who_can_add: Visibility,
        show_last_seen: Visibility,
        searchable: bool,
    ) {
        self.store
            .create_account(NewAccount {
                account_id: id(account),
                username: username.to_string(),
                email: Some(format!("{username}@example.test")),
                phone: None,
                password_hash: Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
                locale: "id-ID".to_string(),
                country: Some("ID".to_string()),
                created_at: ts(SECOND),
            })
            .await
            .expect("a fresh username is free");
        self.store
            .create_profile(Profile {
                account_id: id(account),
                display_name: format!("{username} Nusantara"),
                bio: Some(format!("halo, saya {username}")),
                avatar_media_id: Some(id(900 + account)),
                birth_year: Some(1995),
                gender: None,
                show_last_seen,
                who_can_message,
                who_can_add,
                searchable,
                updated_at: ts(SECOND),
            })
            .await
            .expect("a new account has no profile yet");
    }

    /// An account row with no profile row, which is what mid-registration looks like.
    async fn half_registered(&self, account: u128, username: &str) {
        self.store
            .create_account(NewAccount {
                account_id: id(account),
                username: username.to_string(),
                email: None,
                phone: None,
                password_hash: Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
                locale: "id-ID".to_string(),
                country: None,
                created_at: ts(SECOND),
            })
            .await
            .expect("a fresh username is free");
    }

    /// The four people most tests need.
    async fn cast(&self) {
        self.person(ALICE, "alice").await;
        self.person(BOB, "bob").await;
        self.person(CAROL, "carol").await;
        self.person(DAVE, "dave").await;
    }

    /// A settled friendship, written straight to the store.
    ///
    /// Both rows, dated, because a friendship on one side only is the bug half these
    /// tests are looking for and a fixture must never be the thing that creates it.
    async fn friendship(&self, left: u128, right: u128, millis: i64) {
        for (owner, peer) in [(left, right), (right, left)] {
            self.store
                .put_relationship(Relationship {
                    account_id: id(owner),
                    other_id: id(peer),
                    kind: RelationshipKind::Friend,
                    created_at: ts(millis),
                    accepted_at: Some(ts(millis)),
                })
                .await
                .expect("the store takes an edge between two accounts");
        }
    }

    /// One edge of any kind, written straight to the store.
    async fn edge(&self, owner: u128, peer: u128, kind: RelationshipKind, millis: i64) {
        self.store
            .put_relationship(Relationship {
                account_id: id(owner),
                other_id: id(peer),
                kind,
                created_at: ts(millis),
                accepted_at: None,
            })
            .await
            .expect("the store takes an edge between two accounts");
    }

    /// A request waiting, in the two rows the service writes for one.
    async fn request_waiting(&self, from: u128, to: u128, millis: i64) {
        self.edge(from, to, RelationshipKind::PendingOutgoing, millis)
            .await;
        self.edge(to, from, RelationshipKind::PendingIncoming, millis)
            .await;
    }

    async fn row(&self, owner: u128, peer: u128, kind: RelationshipKind) -> Option<Relationship> {
        self.store
            .relationship(id(owner), id(peer), kind)
            .await
            .expect("the store can be read")
    }

    async fn has(&self, owner: u128, peer: u128, kind: RelationshipKind) -> bool {
        self.row(owner, peer, kind).await.is_some()
    }

    async fn count(&self, owner: u128, kind: RelationshipKind) -> u64 {
        self.store
            .count_relationships(id(owner), kind)
            .await
            .expect("the store can be counted")
    }

    /// The deployment default the call gate takes the stricter half of.
    fn social_call_default(&self) -> Visibility {
        SocialConfig::default().call_default
    }

    fn counter(&self, name: &'static str, labels: &[(&str, &str)]) -> u64 {
        self.registry.counter(name, "", labels).get()
    }

    fn requests(&self, outcome: &str) -> u64 {
        self.counter("migo_social_friend_requests_total", &[("outcome", outcome)])
    }

    fn responses(&self, outcome: &str) -> u64 {
        self.counter(
            "migo_social_friend_responses_total",
            &[("outcome", outcome)],
        )
    }

    fn added(&self, kind: &str) -> u64 {
        self.counter("migo_social_edges_added_total", &[("kind", kind)])
    }

    fn removed(&self, kind: &str) -> u64 {
        self.counter("migo_social_edges_removed_total", &[("kind", kind)])
    }

    fn gates(&self, outcome: &str) -> u64 {
        self.counter(
            "migo_social_interaction_checks_total",
            &[("outcome", outcome)],
        )
    }

    fn plain(&self, name: &'static str) -> u64 {
        self.counter(name, &[])
    }
}

#[track_caller]
fn expect_code<T>(result: Result<T>, code: u32) {
    let error = result.err().expect("this call must be refused");
    assert_eq!(
        error.code(),
        code,
        "expected code {code}, got {}: {error}",
        error.code()
    );
}

/// The other ends of a listing, in the order it returned them.
fn others(edges: &[Edge]) -> Vec<Id> {
    edges.iter().map(|edge| edge.other_id).collect()
}

// ---------------------------------------------------------------------------
// Who is asking
// ---------------------------------------------------------------------------

/// A request with no account behind it reaches no method.
///
/// Every entry point, listed one by one, because this is the check that has no
/// interesting failure mode and therefore the one most likely to be left out of a
/// method added later. A social graph is a list of who knows whom; an anonymous reader
/// of it is the whole risk.
#[tokio::test]
async fn every_method_refuses_a_caller_with_no_account() {
    let harness = Harness::new();
    harness.cast().await;
    let nobody = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));
    let subject = id(BOB);

    expect_code(
        harness.social.request_friend(&nobody, subject).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.respond_friend(&nobody, subject, true).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.remove_friend(&nobody, subject).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.follow(&nobody, subject).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.unfollow(&nobody, subject).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.block(&nobody, subject).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.unblock(&nobody, subject).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.set_favorite(&nobody, subject, true).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.friends(&nobody, None).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.pending(&nobody, None).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.following(&nobody, None).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.followers(&nobody, None).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.blocked(&nobody, None).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.favorites(&nobody, None).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.standing(&nobody, subject).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness
            .social
            .may_interact(&nobody, subject, Interaction::Message)
            .await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.suggest(&nobody, None).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.profiles(&nobody, &[subject]).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.search(&nobody, "alice", None).await,
        codes::UNAUTHENTICATED,
    );
}

/// An account is refused when it names a device that is not there.
///
/// The device id is not used to authorise anything here, but a missing one means the
/// request did not come through a session, and a graph mutation that arrived without
/// one is a bug somewhere upstream that must not be written to the database.
#[tokio::test]
async fn a_caller_with_no_device_is_not_a_caller() {
    let harness = Harness::new();
    harness.cast().await;
    let headless = Caller::new(id(ALICE), Id::NIL, TrustTier::Established, ts(NOW));

    expect_code(
        harness.social.request_friend(&headless, id(BOB)).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.social.friends(&headless, None).await,
        codes::UNAUTHENTICATED,
    );
}

/// No edge may point at the account that asked for it.
///
/// The store's primary key would refuse a self-edge anyway, but the refusal arrives as
/// a storage error rather than as a field error, and a client that sent its own id by
/// mistake deserves to be told which field was wrong.
#[tokio::test]
async fn nothing_lets_an_account_relate_to_itself() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);
    let self_id = id(ALICE);

    expect_code(
        harness.social.request_friend(&alice, self_id).await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness.social.respond_friend(&alice, self_id, true).await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness.social.remove_friend(&alice, self_id).await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness.social.follow(&alice, self_id).await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness.social.unfollow(&alice, self_id).await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness.social.block(&alice, self_id).await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness.social.unblock(&alice, self_id).await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness.social.set_favorite(&alice, self_id, true).await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness.social.standing(&alice, self_id).await,
        codes::VALIDATION_FAILED,
    );
}

/// A subject that was left unset is a missing field, not a missing account.
///
/// `NOT_FOUND` here would be a lie that costs somebody an afternoon: the account is not
/// missing, the request never named one.
#[tokio::test]
async fn an_unset_subject_is_a_missing_field() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    expect_code(
        harness.social.request_friend(&alice, Id::NIL).await,
        codes::FIELD_REQUIRED,
    );
    expect_code(
        harness.social.block(&alice, Id::NIL).await,
        codes::FIELD_REQUIRED,
    );
    expect_code(
        harness.social.standing(&alice, Id::NIL).await,
        codes::FIELD_REQUIRED,
    );
    expect_code(
        harness
            .social
            .may_interact(&alice, Id::NIL, Interaction::Message)
            .await,
        codes::FIELD_REQUIRED,
    );
    expect_code(
        harness.social.profiles(&alice, &[id(BOB), Id::NIL]).await,
        codes::FIELD_REQUIRED,
    );
}

// ---------------------------------------------------------------------------
// Asking to be a friend
// ---------------------------------------------------------------------------

/// A request writes both halves and tells the other account once.
///
/// Both halves, because a request stored only on the sender's side is invisible to the
/// person who has to answer it, and a request stored only on the receiver's side cannot
/// be withdrawn. Neither half is a friendship yet: `accepted` is false on both.
#[tokio::test]
async fn a_request_writes_both_halves_and_tells_the_other_account() {
    let harness = Harness::new();
    harness.cast().await;

    let (outcome, notice) = harness
        .social
        .request_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("a stranger who excludes nobody may be asked");

    assert_eq!(outcome, FriendOutcome::Requested);
    let outgoing = harness
        .row(ALICE, BOB, RelationshipKind::PendingOutgoing)
        .await
        .expect("the sender's half is written");
    let incoming = harness
        .row(BOB, ALICE, RelationshipKind::PendingIncoming)
        .await
        .expect("the receiver's half is written");
    assert_eq!(outgoing.accepted_at, None);
    assert_eq!(incoming.accepted_at, None);
    assert!(!Edge::of(&outgoing).accepted);
    assert!(!harness.has(ALICE, BOB, RelationshipKind::Friend).await);
    assert!(!harness.has(BOB, ALICE, RelationshipKind::Friend).await);

    let notice = notice.expect("the person being asked has to hear about it");
    assert_eq!(notice.audience, id(BOB));
    assert_eq!(notice.event.kind, NotificationKind::FriendRequest);
    assert_eq!(notice.event.actor_id, Some(id(ALICE)));
    assert_eq!(harness.requests("sent"), 1);
}

/// The notice names the actor and writes no prose.
///
/// Title and body are left empty on purpose: the device renders them from the actor id
/// in the user's own language, and a server that shipped Indonesian text to a client
/// set to English would be a translation bug nobody could fix without a deploy. The
/// conversation and room fields are empty because a friend request belongs to neither.
#[tokio::test]
async fn a_friend_notice_carries_an_actor_and_no_prose() {
    let harness = Harness::new();
    harness.cast().await;

    let (_, notice) = harness
        .social
        .request_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("a stranger who excludes nobody may be asked");
    let notice: Notice = notice.expect("the person being asked has to hear about it");

    assert_eq!(notice.opcode(), Opcode::NotificationEvent);
    assert_eq!(notice.event.title, None);
    assert_eq!(notice.event.body, None);
    assert_eq!(notice.event.conversation_id, None);
    assert_eq!(notice.event.room_id, None);
    assert_eq!(notice.event.at, ts(NOW));
}

/// Asking twice answers the same thing and writes nothing the second time.
///
/// Brief section 153 keys friend-request idempotency on the pair, so the repeat is an
/// outcome rather than an error: the client that retried never saw the first answer, and
/// an error would make it report a failure for a request that was in fact sent. The
/// second call also produces no notice, because a retry must not be able to ring
/// somebody's phone twice.
#[tokio::test]
async fn asking_twice_answers_the_same_thing_and_rings_nobody_again() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    harness
        .social
        .request_friend(&alice, id(BOB))
        .await
        .expect("a stranger who excludes nobody may be asked");
    let first = harness
        .row(ALICE, BOB, RelationshipKind::PendingOutgoing)
        .await
        .expect("the sender's half is written");

    let (outcome, notice) = harness
        .social
        .request_friend(&alice, id(BOB))
        .await
        .expect("a repeat is an answer, not a failure");

    assert_eq!(outcome, FriendOutcome::AlreadyRequested);
    assert!(notice.is_none(), "a retry must not ring the phone again");
    let second = harness
        .row(ALICE, BOB, RelationshipKind::PendingOutgoing)
        .await
        .expect("the first row is still there");
    assert_eq!(second.created_at, first.created_at);
    assert_eq!(harness.requests("sent"), 1);
    assert_eq!(harness.requests("duplicate"), 1);
}

/// Two people who asked each other end up friends rather than both waiting.
///
/// The case that decides whether the feature feels like it works. Without it, two
/// people who each tapped Add before either opened their notifications are both left
/// looking at an unanswered request, and the only way out is for one of them to work
/// out that they have to cancel theirs first.
#[tokio::test]
async fn a_crossing_request_accepts_the_one_already_waiting() {
    let harness = Harness::new();
    harness.cast().await;

    harness
        .social
        .request_friend(&caller(BOB, BOB_LAPTOP), id(ALICE))
        .await
        .expect("a stranger who excludes nobody may be asked");
    let (outcome, notice) = harness
        .social
        .request_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("the request already waiting is accepted");

    assert_eq!(outcome, FriendOutcome::Accepted);
    for (left, right) in [(ALICE, BOB), (BOB, ALICE)] {
        let row = harness
            .row(left, right, RelationshipKind::Friend)
            .await
            .expect("both sides hold a friendship");
        assert!(row.accepted_at.is_some(), "and both are settled");
        assert!(Edge::of(&row).accepted);
    }
    assert!(
        !harness
            .has(ALICE, BOB, RelationshipKind::PendingOutgoing)
            .await
            && !harness
                .has(BOB, ALICE, RelationshipKind::PendingIncoming)
                .await
            && !harness
                .has(ALICE, BOB, RelationshipKind::PendingIncoming)
                .await
            && !harness
                .has(BOB, ALICE, RelationshipKind::PendingOutgoing)
                .await,
        "and no request is left over on either side"
    );

    let notice = notice.expect("the person who asked first hears that it worked");
    assert_eq!(notice.audience, id(BOB));
    // The same kind as a request: the notification registry has one social kind, and
    // sending "accepted" under a kind that does not exist would decode to `Unknown` on
    // every client. The client tells the two apart by the standing it already holds.
    assert_eq!(notice.event.kind, NotificationKind::FriendRequest);
    assert_eq!(notice.event.actor_id, Some(id(ALICE)));
    assert_eq!(harness.requests("reciprocated"), 1);
    assert_eq!(harness.added("friend"), 1);
}

/// Asking somebody who is already a friend is answered before any setting is read.
///
/// The order matters. Somebody who narrowed `who_can_add` to nobody after the
/// friendship was made must not have their existing friends told that they are now
/// strangers, and a client that lost its cache and re-sent an old request must get an
/// answer it can act on rather than a privacy refusal about a friend it can see.
#[tokio::test]
async fn asking_an_existing_friend_is_answered_before_any_setting_is_read() {
    let harness = Harness::new();
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Everyone,
            Visibility::Nobody,
            Visibility::Everyone,
        )
        .await;
    harness.person(ALICE, "alice").await;
    harness.friendship(ALICE, BOB, NOW).await;

    let (outcome, notice) = harness
        .social
        .request_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("a friend is a friend whatever the add policy now says");

    assert_eq!(outcome, FriendOutcome::AlreadyFriends);
    assert!(notice.is_none());
    assert_eq!(harness.requests("redundant"), 1);
}

/// A caller who blocked somebody is told it was their own doing.
///
/// The one block that may be disclosed. Telling somebody what they themselves did
/// leaks nothing, and a client that can say "you blocked this person" saves them
/// working out why the button does nothing.
#[tokio::test]
async fn asking_somebody_the_caller_blocked_names_the_callers_own_block() {
    let harness = Harness::new();
    harness.cast().await;
    harness
        .social
        .block(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("blocking a stranger needs no consent");

    expect_code(
        harness
            .social
            .request_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
            .await,
        codes::BLOCKED_BY_USER,
    );
    assert_eq!(harness.requests("blocked"), 1);
}

/// Being blocked and being excluded by a setting are the same observation.
///
/// Brief section 180, and the reason every refusal in this crate is dull. If the two
/// answers differed by code, by message, or by timing, the block list would be readable
/// by anybody willing to send one request per account -- and a blocked account learning
/// that it was blocked is exactly the escalation a block exists to prevent.
#[tokio::test]
async fn being_blocked_is_indistinguishable_from_being_excluded_by_a_setting() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    // One account blocked the caller. One merely wants no requests from strangers.
    harness.person(BOB, "bob").await;
    harness
        .person_with(
            CAROL,
            "carol",
            Visibility::Everyone,
            Visibility::Nobody,
            Visibility::Everyone,
        )
        .await;
    harness
        .social
        .block(&caller(BOB, BOB_LAPTOP), id(ALICE))
        .await
        .expect("blocking a stranger needs no consent");
    let alice = caller(ALICE, ALICE_PHONE);

    let blocked = harness
        .social
        .request_friend(&alice, id(BOB))
        .await
        .expect_err("a blocked caller gets nowhere");
    let excluded = harness
        .social
        .request_friend(&alice, id(CAROL))
        .await
        .expect_err("a caller a setting excludes gets nowhere");

    assert_eq!(blocked.code(), codes::PRIVACY_RESTRICTED);
    assert_eq!(excluded.code(), codes::PRIVACY_RESTRICTED);
    assert_eq!(
        blocked.public_message(),
        excluded.public_message(),
        "the two refusals must read identically to the caller"
    );
    assert_eq!(harness.requests("restricted"), 2);
}

/// The operator can still tell a block from a privacy setting.
///
/// The caller may not, and the counter must -- otherwise nobody can answer "how much of
/// this is blocking" from a dashboard, and the only way to find out would be to read
/// individual accounts' rows. The label is taken from which side wrote the block rather
/// than from the error, because the error is deliberately the same one a privacy setting
/// produces.
#[tokio::test]
async fn the_counter_tells_a_block_from_a_setting_even_though_the_caller_cannot() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.person(BOB, "bob").await;
    harness
        .person_with(
            CAROL,
            "carol",
            Visibility::Nobody,
            Visibility::Everyone,
            Visibility::Everyone,
        )
        .await;
    harness
        .social
        .block(&caller(BOB, BOB_LAPTOP), id(ALICE))
        .await
        .expect("blocking a stranger needs no consent");
    let alice = caller(ALICE, ALICE_PHONE);

    expect_code(
        harness
            .social
            .may_interact(&alice, id(BOB), Interaction::Message)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
    expect_code(
        harness
            .social
            .may_interact(&alice, id(CAROL), Interaction::Message)
            .await,
        codes::PRIVACY_RESTRICTED,
    );

    assert_eq!(harness.gates("blocked"), 1, "the block is counted as one");
    assert_eq!(
        harness.gates("restricted"),
        1,
        "and the setting as the other"
    );
}

/// A friends-only add policy means friends of friends.
///
/// Otherwise the setting is a synonym for `Nobody`: a stranger cannot be a friend
/// already, so reading it as "must already be a friend" would mean nobody new could
/// ever ask, which is not what a user who picked the middle option asked for.
#[tokio::test]
async fn a_friends_only_add_policy_means_friends_of_friends() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Everyone,
            Visibility::Friends,
            Visibility::Everyone,
        )
        .await;
    harness.person(CAROL, "carol").await;
    harness.person(DAVE, "dave").await;
    // Carol knows both. Dave knows neither.
    harness.friendship(ALICE, CAROL, NOW).await;
    harness.friendship(BOB, CAROL, NOW).await;

    let (outcome, _) = harness
        .social
        .request_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("a friend of a friend is who the middle setting is for");
    assert_eq!(outcome, FriendOutcome::Requested);

    expect_code(
        harness
            .social
            .request_friend(&caller(DAVE, DAVE_TABLET), id(BOB))
            .await,
        codes::PRIVACY_RESTRICTED,
    );
}

/// A pending request is not a friendship, so it opens no door.
///
/// The bypass this crate is most exposed to: if an unanswered request counted as a
/// friendship, anybody could reach a friends-only field by asking to be a friend and
/// never being answered. One row, one boolean, and every gate in the crate depends on
/// it.
#[tokio::test]
async fn an_unanswered_request_is_not_a_friendship_to_any_gate() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Friends,
            Visibility::Everyone,
            Visibility::Friends,
        )
        .await;
    harness.request_waiting(ALICE, BOB, NOW).await;
    let alice = caller(ALICE, ALICE_PHONE);

    expect_code(
        harness
            .social
            .may_interact(&alice, id(BOB), Interaction::Message)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
    expect_code(
        harness
            .social
            .may_interact(&alice, id(BOB), Interaction::LastSeen)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
    let standing = harness
        .social
        .standing(&alice, id(BOB))
        .await
        .expect("standing is readable either way");
    assert!(!standing.friends, "a request is not a friendship");
    assert!(standing.requested);
}

/// A friendship that was never accepted is not a friendship either.
///
/// The store can hold a `Friend` row with no acceptance date -- a half-finished
/// migration, a partial write, a bug in a future caller -- and `accepted_at` is the only
/// thing that decides. A gate that trusted the row's existence would be one bad backfill
/// away from opening every friends-only field on the deployment.
#[tokio::test]
async fn a_friend_row_with_no_acceptance_date_opens_nothing() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Friends,
            Visibility::Everyone,
            Visibility::Everyone,
        )
        .await;
    harness
        .edge(ALICE, BOB, RelationshipKind::Friend, NOW)
        .await;
    harness
        .edge(BOB, ALICE, RelationshipKind::Friend, NOW)
        .await;
    let alice = caller(ALICE, ALICE_PHONE);

    expect_code(
        harness
            .social
            .may_interact(&alice, id(BOB), Interaction::Message)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
    assert!(
        harness
            .social
            .friends(&alice, None)
            .await
            .expect("the listing still runs")
            .is_empty(),
        "and it is not shown as a friendship either"
    );
    let standing = harness
        .social
        .standing(&alice, id(BOB))
        .await
        .expect("standing is readable either way");
    assert!(!standing.friends);
}

/// A nobody-adds policy refuses everybody, friends of friends included.
#[tokio::test]
async fn a_nobody_add_policy_refuses_even_a_friend_of_a_friend() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Everyone,
            Visibility::Nobody,
            Visibility::Everyone,
        )
        .await;
    harness.person(CAROL, "carol").await;
    harness.friendship(ALICE, CAROL, NOW).await;
    harness.friendship(BOB, CAROL, NOW).await;

    expect_code(
        harness
            .social
            .request_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
            .await,
        codes::PRIVACY_RESTRICTED,
    );
}

/// Either side's friend list being full stops the request.
///
/// Both sides, because a ceiling enforced only on the asker lets a popular account be
/// pushed past its own limit by other people's requests, and the row that breaks the
/// limit is written on their side too.
#[tokio::test]
async fn a_request_is_refused_when_either_friend_list_is_full() {
    let harness = Harness::configured(SocialConfig {
        max_friends: 1,
        ..SocialConfig::default()
    });
    harness.cast().await;
    harness.person(ERIN, "erin").await;
    // Alice is full. Bob is not.
    harness.friendship(ALICE, CAROL, NOW).await;

    expect_code(
        harness
            .social
            .request_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
            .await,
        codes::QUOTA_EXCEEDED,
    );

    // Now the other way round: Dave has room, Erin does not.
    harness.friendship(ERIN, CAROL, NOW).await;
    expect_code(
        harness
            .social
            .request_friend(&caller(DAVE, DAVE_TABLET), id(ERIN))
            .await,
        codes::QUOTA_EXCEEDED,
    );
    assert_eq!(harness.requests("full"), 2);
}

/// An account nobody registered cannot be asked.
///
/// `NOT_FOUND` and not a privacy refusal, because there is no privacy to protect: an id
/// with no account behind it discloses nothing when it is reported as missing, and a
/// client that mistyped an id needs to know which of the two happened.
#[tokio::test]
async fn asking_an_account_that_does_not_exist_is_not_found() {
    let harness = Harness::new();
    harness.cast().await;

    expect_code(
        harness
            .social
            .request_friend(&caller(ALICE, ALICE_PHONE), id(STRANGER))
            .await,
        codes::NOT_FOUND,
    );
}

/// An account halfway through registering is not there yet.
#[tokio::test]
async fn an_account_with_no_profile_row_cannot_be_asked() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.half_registered(BOB, "bob").await;

    expect_code(
        harness
            .social
            .request_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
            .await,
        codes::NOT_FOUND,
    );
}

/// Twenty requests is the burst, and the twenty-first waits.
///
/// A friend request costs ten against an account budget of two hundred, which is the
/// arithmetic that decides whether a script can spray requests across a deployment. The
/// repeats are charged too -- the price is for the attempt, not for the row -- which is
/// what stops a loop on one target from being free.
#[tokio::test]
async fn a_friend_request_budget_is_twenty_attempts() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    for attempt in 0..20 {
        harness
            .social
            .request_friend(&alice, id(BOB))
            .await
            .unwrap_or_else(|error| panic!("attempt {attempt} must fit the burst: {error}"));
    }
    expect_code(
        harness.social.request_friend(&alice, id(BOB)).await,
        codes::RATE_LIMITED,
    );
    assert_eq!(harness.requests("rate_limited"), 1);
    assert_eq!(harness.requests("sent"), 1);
    assert_eq!(harness.requests("duplicate"), 19);
}

// ---------------------------------------------------------------------------
// Answering a request
// ---------------------------------------------------------------------------

/// Accepting writes both friendships and tells the person who asked.
#[tokio::test]
async fn accepting_writes_both_friendships_and_tells_the_requester() {
    let harness = Harness::new();
    harness.cast().await;
    harness.request_waiting(BOB, ALICE, NOW).await;

    let (outcome, notice) = harness
        .social
        .respond_friend(&caller(ALICE, ALICE_PHONE), id(BOB), true)
        .await
        .expect("the person who was asked may say yes");

    assert_eq!(outcome, RespondOutcome::Accepted);
    for (left, right) in [(ALICE, BOB), (BOB, ALICE)] {
        let row = harness
            .row(left, right, RelationshipKind::Friend)
            .await
            .expect("both sides hold a friendship");
        assert_eq!(row.accepted_at, Some(ts(NOW)));
    }
    assert!(
        !harness
            .has(BOB, ALICE, RelationshipKind::PendingOutgoing)
            .await
            && !harness
                .has(ALICE, BOB, RelationshipKind::PendingIncoming)
                .await,
        "the request is spent"
    );
    let notice = notice.expect("the person who asked hears that it worked");
    assert_eq!(notice.audience, id(BOB));
    assert_eq!(notice.event.actor_id, Some(id(ALICE)));
    assert_eq!(harness.responses("accepted"), 1);
    assert_eq!(harness.added("friend"), 1);
}

/// A friendship is dated from the request, not from the answer.
///
/// "Friends since" is the date somebody reached out, which is the date a person
/// remembers. Overwriting it with the moment the notification finally got opened would
/// make every friendship look as if it started whenever the recipient last cleared their
/// badge count.
#[tokio::test]
async fn a_friendship_keeps_the_date_the_request_was_made() {
    let harness = Harness::new();
    harness.cast().await;
    harness.request_waiting(BOB, ALICE, NOW).await;
    let much_later = caller(ALICE, ALICE_PHONE);
    let much_later = Caller {
        now: ts(NOW + 30 * MINUTE),
        ..much_later
    };

    harness
        .social
        .respond_friend(&much_later, id(BOB), true)
        .await
        .expect("the person who was asked may say yes");

    for (left, right) in [(ALICE, BOB), (BOB, ALICE)] {
        let row = harness
            .row(left, right, RelationshipKind::Friend)
            .await
            .expect("both sides hold a friendship");
        assert_eq!(row.created_at, ts(NOW), "since the request");
        assert_eq!(
            row.accepted_at,
            Some(ts(NOW + 30 * MINUTE)),
            "settled at the answer"
        );
    }
}

/// Declining is silent.
///
/// No notice, deliberately. "X declined your friend request" is a sentence whose only
/// function is to make a private decision somebody else's business, and a product that
/// sends it teaches people not to decline.
#[tokio::test]
async fn declining_is_silent_and_leaves_nothing_behind() {
    let harness = Harness::new();
    harness.cast().await;
    harness.request_waiting(BOB, ALICE, NOW).await;

    let (outcome, notice) = harness
        .social
        .respond_friend(&caller(ALICE, ALICE_PHONE), id(BOB), false)
        .await
        .expect("the person who was asked may say no");

    assert_eq!(outcome, RespondOutcome::Declined);
    assert!(notice.is_none(), "declining tells nobody");
    assert!(
        !harness
            .has(BOB, ALICE, RelationshipKind::PendingOutgoing)
            .await
            && !harness
                .has(ALICE, BOB, RelationshipKind::PendingIncoming)
                .await,
        "and clears the request from both lists"
    );
    assert!(!harness.has(ALICE, BOB, RelationshipKind::Friend).await);
    assert_eq!(harness.responses("declined"), 1);
    assert_eq!(harness.added("friend"), 0);
}

/// Declining is not a way to make somebody unable to ask again.
///
/// A declined request leaves no trace, so the other person may ask once more. That is
/// the intended behaviour -- the tool for "never again" is a block -- and it is worth
/// pinning down, because a decline that quietly wrote a block would be the product
/// making a decision on the user's behalf that they cannot see or undo.
#[tokio::test]
async fn a_declined_request_may_be_made_again() {
    let harness = Harness::new();
    harness.cast().await;
    harness.request_waiting(BOB, ALICE, NOW).await;
    harness
        .social
        .respond_friend(&caller(ALICE, ALICE_PHONE), id(BOB), false)
        .await
        .expect("the person who was asked may say no");

    assert!(
        !harness.has(ALICE, BOB, RelationshipKind::Block).await,
        "a decline is not a block"
    );
    let (outcome, _) = harness
        .social
        .request_friend(&caller(BOB, BOB_LAPTOP), id(ALICE))
        .await
        .expect("nothing stops a second attempt");
    assert_eq!(outcome, FriendOutcome::Requested);
}

/// Answering a request nobody made is not found.
#[tokio::test]
async fn answering_a_request_that_was_never_made_is_not_found() {
    let harness = Harness::new();
    harness.cast().await;

    expect_code(
        harness
            .social
            .respond_friend(&caller(ALICE, ALICE_PHONE), id(BOB), true)
            .await,
        codes::NOT_FOUND,
    );
    assert_eq!(harness.responses("missing"), 1);
}

/// Only the account a request was addressed to may answer it.
///
/// The check is the caller's own incoming row, so the answer to "may I accept this"
/// cannot be borrowed from somebody else's queue. Without it, an account that knew two
/// ids could marry them to each other.
#[tokio::test]
async fn only_the_account_a_request_was_sent_to_may_answer_it() {
    let harness = Harness::new();
    harness.cast().await;
    harness.request_waiting(ALICE, BOB, NOW).await;

    expect_code(
        harness
            .social
            .respond_friend(&caller(CAROL, CAROL_PHONE), id(ALICE), true)
            .await,
        codes::NOT_FOUND,
    );
    assert!(!harness.has(ALICE, CAROL, RelationshipKind::Friend).await);
    assert!(!harness.has(CAROL, ALICE, RelationshipKind::Friend).await);
    // And the sender cannot accept their own request either.
    expect_code(
        harness
            .social
            .respond_friend(&caller(ALICE, ALICE_PHONE), id(BOB), true)
            .await,
        codes::NOT_FOUND,
    );
    assert!(!harness.has(ALICE, BOB, RelationshipKind::Friend).await);
}

/// A block written while the request waited wins, and clears the request.
///
/// A request can sit unanswered for a month, so the block is re-read at acceptance
/// rather than trusted from request time. The stale row goes too: leaving it would show
/// a blocked account's name in a pending list forever with no way to clear it, since
/// every way of clearing it is itself refused by the block.
#[tokio::test]
async fn a_block_written_while_a_request_waited_wins_and_clears_it() {
    let harness = Harness::new();
    harness.cast().await;
    harness.request_waiting(BOB, ALICE, NOW).await;
    harness
        .social
        .block(&caller(BOB, BOB_LAPTOP), id(ALICE))
        .await
        .expect("blocking a stranger needs no consent");
    // Bob's own block already cleared his side; put the request back to prove the
    // acceptance path re-checks rather than relying on that cleanup.
    harness.request_waiting(BOB, ALICE, NOW).await;

    expect_code(
        harness
            .social
            .respond_friend(&caller(ALICE, ALICE_PHONE), id(BOB), true)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
    assert!(!harness.has(ALICE, BOB, RelationshipKind::Friend).await);
    assert!(
        !harness
            .has(ALICE, BOB, RelationshipKind::PendingIncoming)
            .await,
        "the request a caller can never act on is not left in their list"
    );
    assert_eq!(harness.gates("blocked"), 1);
    assert_eq!(harness.responses("missing"), 1);
}

/// The caller's own block also clears the request it makes unanswerable.
#[tokio::test]
async fn the_callers_own_block_clears_the_request_it_refuses() {
    let harness = Harness::new();
    harness.cast().await;
    harness.request_waiting(BOB, ALICE, NOW).await;
    harness
        .social
        .block(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("blocking a stranger needs no consent");
    harness.request_waiting(BOB, ALICE, NOW).await;

    expect_code(
        harness
            .social
            .respond_friend(&caller(ALICE, ALICE_PHONE), id(BOB), true)
            .await,
        codes::BLOCKED_BY_USER,
    );
    assert!(
        !harness
            .has(ALICE, BOB, RelationshipKind::PendingIncoming)
            .await
    );
}

/// A request already in the queue survives the recipient narrowing their settings.
///
/// Acceptance reads the block and nothing else. `who_can_add` governs who may *ask*,
/// so applying it again at the answer would let a user lock themselves out of a request
/// they were about to accept -- and the person who tightened the setting is the same
/// person deciding to say yes.
#[tokio::test]
async fn narrowing_a_setting_does_not_void_a_request_already_waiting() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.person(BOB, "bob").await;
    harness
        .social
        .request_friend(&caller(BOB, BOB_LAPTOP), id(ALICE))
        .await
        .expect("a stranger who excludes nobody may be asked");
    harness
        .store
        .update_profile(
            id(ALICE),
            ProfilePatch {
                show_last_seen: Some(Visibility::Nobody),
                who_can_message: Some(Visibility::Nobody),
                who_can_add: Some(Visibility::Nobody),
                ..ProfilePatch::default()
            },
            ts(NOW),
        )
        .await
        .expect("a profile's own owner may narrow it");

    let (outcome, _) = harness
        .social
        .respond_friend(&caller(ALICE, ALICE_PHONE), id(BOB), true)
        .await
        .expect("the person who was asked may still say yes");
    assert_eq!(outcome, RespondOutcome::Accepted);
}

// ---------------------------------------------------------------------------
// Ending a friendship
// ---------------------------------------------------------------------------

/// Un-friending clears both sides and tells nobody.
///
/// Both sides in one operation, because a friendship stored on one side only is how "we
/// are not friends but you are still in my list" happens. No notice, because a
/// notification would turn a quiet exit into a confrontation.
#[tokio::test]
async fn un_friending_clears_both_sides_quietly() {
    let harness = Harness::new();
    harness.cast().await;
    harness.friendship(ALICE, BOB, NOW).await;

    harness
        .social
        .remove_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("either side may end a friendship");

    assert!(!harness.has(ALICE, BOB, RelationshipKind::Friend).await);
    assert!(
        !harness.has(BOB, ALICE, RelationshipKind::Friend).await,
        "the other side's row goes too"
    );
    assert_eq!(harness.removed("friend"), 1);
}

/// Un-friending also clears a request that arrived in the meantime.
///
/// Somebody who un-friends an account that has just asked to be friends again should
/// not be left with the request in their pending list. One gesture, one intention.
#[tokio::test]
async fn un_friending_also_clears_a_request_in_flight() {
    let harness = Harness::new();
    harness.cast().await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness.request_waiting(BOB, ALICE, NOW).await;

    harness
        .social
        .remove_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("either side may end a friendship");

    assert!(
        !harness
            .has(ALICE, BOB, RelationshipKind::PendingIncoming)
            .await
            && !harness
                .has(BOB, ALICE, RelationshipKind::PendingOutgoing)
                .await
    );
}

/// Un-friending somebody who was never a friend is not an error.
///
/// The client that retried a request it never saw the answer to gets the state it asked
/// for, which is the state it already had.
#[tokio::test]
async fn un_friending_a_stranger_is_not_an_error() {
    let harness = Harness::new();
    harness.cast().await;

    harness
        .social
        .remove_friend(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("removing what is not there is the state the caller asked for");
    assert!(!harness.has(ALICE, BOB, RelationshipKind::Friend).await);
}

// ---------------------------------------------------------------------------
// Following
// ---------------------------------------------------------------------------

/// A follow is one direction and needs no consent.
#[tokio::test]
async fn a_follow_is_one_direction_and_needs_no_consent() {
    let harness = Harness::new();
    harness.cast().await;

    harness
        .social
        .follow(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("following a public account needs nobody's permission");

    assert!(harness.has(ALICE, BOB, RelationshipKind::Follow).await);
    assert!(
        !harness.has(BOB, ALICE, RelationshipKind::Follow).await,
        "and it is not reciprocated by the server"
    );
    assert_eq!(harness.added("follow"), 1);
}

/// A follow is refused whichever side wrote the block.
///
/// The direction that matters most is the one a naive implementation misses: an account
/// that blocked somebody must not find that person in its follower list, and the block
/// lives on the other row.
#[tokio::test]
async fn a_follow_is_refused_in_both_block_directions() {
    let harness = Harness::new();
    harness.cast().await;
    harness
        .social
        .block(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("blocking a stranger needs no consent");
    harness
        .social
        .block(&caller(CAROL, CAROL_PHONE), id(DAVE))
        .await
        .expect("blocking a stranger needs no consent");

    expect_code(
        harness
            .social
            .follow(&caller(ALICE, ALICE_PHONE), id(BOB))
            .await,
        codes::BLOCKED_BY_USER,
    );
    expect_code(
        harness
            .social
            .follow(&caller(DAVE, DAVE_TABLET), id(CAROL))
            .await,
        codes::PRIVACY_RESTRICTED,
    );
    assert_eq!(harness.added("follow"), 0);
    assert_eq!(
        harness.gates("blocked"),
        2,
        "both are blocks to the operator"
    );
}

/// Following an account that does not exist is refused.
///
/// A follow of a deleted account is a row pointing at nothing that shows up in the
/// follower's list forever, with no name to render and no way to work out what it was.
#[tokio::test]
async fn following_an_account_that_does_not_exist_is_not_found() {
    let harness = Harness::new();
    harness.cast().await;

    expect_code(
        harness
            .social
            .follow(&caller(ALICE, ALICE_PHONE), id(STRANGER))
            .await,
        codes::NOT_FOUND,
    );
}

/// The following ceiling is counted rather than paged.
///
/// A count and not a listing, because the ceiling is ten thousand and a limit that had
/// to read ten thousand rows to decide would be a denial-of-service tool with a
/// friendly name.
#[tokio::test]
async fn the_following_ceiling_refuses_the_next_follow() {
    let harness = Harness::configured(SocialConfig {
        max_following: 1,
        ..SocialConfig::default()
    });
    harness.cast().await;

    harness
        .social
        .follow(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("the first fits");
    expect_code(
        harness
            .social
            .follow(&caller(ALICE, ALICE_PHONE), id(CAROL))
            .await,
        codes::QUOTA_EXCEEDED,
    );
    assert_eq!(harness.count(ALICE, RelationshipKind::Follow).await, 1);
}

/// Following twice leaves one edge.
#[tokio::test]
async fn following_twice_leaves_one_edge() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    harness.social.follow(&alice, id(BOB)).await.expect("first");
    harness
        .social
        .follow(&alice, id(BOB))
        .await
        .expect("second");

    assert_eq!(harness.count(ALICE, RelationshipKind::Follow).await, 1);
}

/// Unfollowing removes the caller's own edge and nothing else.
#[tokio::test]
async fn unfollowing_removes_only_the_callers_own_edge() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);
    harness
        .social
        .follow(&alice, id(BOB))
        .await
        .expect("follow");
    harness
        .social
        .follow(&caller(BOB, BOB_LAPTOP), id(ALICE))
        .await
        .expect("and back");

    harness
        .social
        .unfollow(&alice, id(BOB))
        .await
        .expect("a follow is the follower's to undo");

    assert!(!harness.has(ALICE, BOB, RelationshipKind::Follow).await);
    assert!(
        harness.has(BOB, ALICE, RelationshipKind::Follow).await,
        "the other direction is not the caller's to remove"
    );
    assert_eq!(harness.removed("follow"), 1);
}

/// Unfollowing somebody who was never followed is not an error.
#[tokio::test]
async fn unfollowing_a_stranger_is_not_an_error() {
    let harness = Harness::new();
    harness.cast().await;

    harness
        .social
        .unfollow(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("removing what is not there is the state the caller asked for");
}

// ---------------------------------------------------------------------------
// Blocking
// ---------------------------------------------------------------------------

/// A block undoes the friendship, the requests, the follows, and the favourite.
///
/// Five removals from one gesture, in both directions, because a block that left any of
/// them behind would leave a way through: a surviving follow keeps the blocked account
/// in a feed, a surviving request keeps their name in a list, and a surviving friendship
/// keeps every friends-only field open.
#[tokio::test]
async fn a_block_undoes_the_friendship_the_requests_the_follows_and_the_favourite() {
    let harness = Harness::new();
    harness.cast().await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness.request_waiting(BOB, ALICE, NOW).await;
    harness
        .edge(ALICE, BOB, RelationshipKind::Follow, NOW)
        .await;
    harness
        .edge(BOB, ALICE, RelationshipKind::Follow, NOW)
        .await;
    harness
        .edge(ALICE, BOB, RelationshipKind::Favorite, NOW)
        .await;

    harness
        .social
        .block(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("blocking needs no consent");

    assert!(harness.has(ALICE, BOB, RelationshipKind::Block).await);
    for (owner, peer, kind) in [
        (ALICE, BOB, RelationshipKind::Friend),
        (BOB, ALICE, RelationshipKind::Friend),
        (ALICE, BOB, RelationshipKind::PendingIncoming),
        (BOB, ALICE, RelationshipKind::PendingOutgoing),
        (ALICE, BOB, RelationshipKind::Follow),
        (BOB, ALICE, RelationshipKind::Follow),
        (ALICE, BOB, RelationshipKind::Favorite),
    ] {
        assert!(
            !harness.has(owner, peer, kind).await,
            "a block must leave no {kind:?} edge from {owner} to {peer}"
        );
    }
    assert_eq!(harness.added("block"), 1);
    // Counted as removals, because that is what they are: an operator watching an edge
    // kind's additions against its removals is watching how many exist.
    assert_eq!(harness.removed("friend"), 1);
    assert_eq!(harness.removed("follow"), 1);
    assert_eq!(harness.removed("favorite"), 1);
}

/// Blocking a stranger removes nothing, and says so.
///
/// Most blocks are of strangers. If each of them reported a friendship removal, the
/// friend series would measure blocks instead of friendships and nobody could tell how
/// many friendships a deployment actually has.
#[tokio::test]
async fn blocking_a_stranger_reports_no_removals() {
    let harness = Harness::new();
    harness.cast().await;

    harness
        .social
        .block(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("blocking needs no consent");

    assert_eq!(harness.added("block"), 1);
    assert_eq!(harness.removed("friend"), 0);
    assert_eq!(harness.removed("follow"), 0);
    assert_eq!(harness.removed("favorite"), 0);
}

/// A block does not reach into the other account's favourites.
///
/// The blocked account's own list is their own business, and quietly editing it would
/// tell them that something happened. What stops the favourite from being useful is the
/// block, not its absence from a list.
#[tokio::test]
async fn a_block_leaves_the_other_accounts_own_favourites_alone() {
    let harness = Harness::new();
    harness.cast().await;
    harness
        .edge(BOB, ALICE, RelationshipKind::Favorite, NOW)
        .await;

    harness
        .social
        .block(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("blocking needs no consent");

    assert!(harness.has(BOB, ALICE, RelationshipKind::Favorite).await);
}

/// Blocking twice leaves one row and refreshes nothing a caller can see.
#[tokio::test]
async fn blocking_twice_leaves_one_row() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    harness.social.block(&alice, id(BOB)).await.expect("first");
    harness.social.block(&alice, id(BOB)).await.expect("second");

    assert_eq!(harness.count(ALICE, RelationshipKind::Block).await, 1);
}

/// The blocklist has a ceiling.
///
/// A blocklist is a list of people somebody met and did not want to meet again. A number
/// far above the ceiling is a script, and a script filling a blocklist is filling a table
/// on somebody else's disk.
#[tokio::test]
async fn the_blocklist_ceiling_refuses_the_next_block() {
    let harness = Harness::configured(SocialConfig {
        max_blocks: 1,
        ..SocialConfig::default()
    });
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    harness.social.block(&alice, id(BOB)).await.expect("first");
    expect_code(
        harness.social.block(&alice, id(CAROL)).await,
        codes::QUOTA_EXCEEDED,
    );
}

/// Unblocking restores nothing.
///
/// The friendship, the follows, and the favourite the block removed stay removed. The
/// alternative is a server holding a shadow copy of a relationship its owner deleted, so
/// that unblocking somebody by accident would silently re-share everything a friendship
/// shares.
#[tokio::test]
async fn unblocking_restores_nothing() {
    let harness = Harness::new();
    harness.cast().await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness
        .edge(ALICE, BOB, RelationshipKind::Follow, NOW)
        .await;
    let alice = caller(ALICE, ALICE_PHONE);
    harness.social.block(&alice, id(BOB)).await.expect("block");

    harness
        .social
        .unblock(&alice, id(BOB))
        .await
        .expect("a block is the blocker's to undo");

    assert!(!harness.has(ALICE, BOB, RelationshipKind::Block).await);
    assert!(!harness.has(ALICE, BOB, RelationshipKind::Friend).await);
    assert!(!harness.has(BOB, ALICE, RelationshipKind::Friend).await);
    assert!(!harness.has(ALICE, BOB, RelationshipKind::Follow).await);
    assert_eq!(harness.removed("block"), 1);
}

/// Unblocking cannot clear a block written against the caller.
///
/// Otherwise the block is worthless: anybody who found themselves blocked would simply
/// unblock themselves. The row belongs to whoever wrote it.
#[tokio::test]
async fn unblocking_cannot_clear_a_block_written_against_the_caller() {
    let harness = Harness::new();
    harness.cast().await;
    harness
        .social
        .block(&caller(BOB, BOB_LAPTOP), id(ALICE))
        .await
        .expect("blocking needs no consent");

    harness
        .social
        .unblock(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("clearing an edge that is not there is not an error");

    assert!(
        harness.has(BOB, ALICE, RelationshipKind::Block).await,
        "the other account's block is untouched"
    );
    expect_code(
        harness
            .social
            .may_interact(&caller(ALICE, ALICE_PHONE), id(BOB), Interaction::Message)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
}

/// A block stops every interaction in both directions.
///
/// All four gates, from both sides, because a block that stopped messages but not calls
/// would be a block in name only -- and the side that wrote it is not the side most
/// likely to be tested by hand.
#[tokio::test]
async fn a_block_stops_every_interaction_from_both_sides() {
    let harness = Harness::new();
    harness.cast().await;
    harness
        .social
        .block(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("blocking needs no consent");

    for interaction in [
        Interaction::Message,
        Interaction::Call,
        Interaction::FriendRequest,
        Interaction::LastSeen,
    ] {
        expect_code(
            harness
                .social
                .may_interact(&caller(ALICE, ALICE_PHONE), id(BOB), interaction)
                .await,
            codes::BLOCKED_BY_USER,
        );
        expect_code(
            harness
                .social
                .may_interact(&caller(BOB, BOB_LAPTOP), id(ALICE), interaction)
                .await,
            codes::PRIVACY_RESTRICTED,
        );
    }
}

/// A block outranks a friendship that is still on the books.
///
/// The service clears the friendship when it writes the block, so this is the state a
/// partial failure or a direct write leaves behind. The gate must read the block first
/// regardless, because "we used to be friends" is exactly the relationship a block is
/// most often used to end.
#[tokio::test]
async fn a_block_outranks_a_friendship_that_survived_it() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Friends,
            Visibility::Friends,
            Visibility::Friends,
        )
        .await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness.edge(BOB, ALICE, RelationshipKind::Block, NOW).await;

    expect_code(
        harness
            .social
            .may_interact(&caller(ALICE, ALICE_PHONE), id(BOB), Interaction::Message)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
}

/// When both sides blocked each other, the caller hears about their own.
///
/// The disclosable answer wins. Telling somebody what they themselves did is free, and
/// hiding it behind the other account's block would leave a user staring at a refusal
/// they could have cleared themselves.
#[tokio::test]
async fn a_mutual_block_reports_the_callers_own() {
    let harness = Harness::new();
    harness.cast().await;
    harness
        .social
        .block(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("blocking needs no consent");
    harness.edge(BOB, ALICE, RelationshipKind::Block, NOW).await;

    expect_code(
        harness
            .social
            .may_interact(&caller(ALICE, ALICE_PHONE), id(BOB), Interaction::Message)
            .await,
        codes::BLOCKED_BY_USER,
    );
}

// ---------------------------------------------------------------------------
// Favourites
// ---------------------------------------------------------------------------

/// A favourite is private and needs no consent.
///
/// It is a bookmark, so an account that accepts messages from nobody may still be
/// marked, and nothing is written on their side or sent to them. Marking somebody as a
/// favourite is not a way to contact them.
#[tokio::test]
async fn a_favourite_is_a_private_bookmark() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Nobody,
            Visibility::Nobody,
            Visibility::Nobody,
        )
        .await;

    harness
        .social
        .set_favorite(&caller(ALICE, ALICE_PHONE), id(BOB), true)
        .await
        .expect("a bookmark asks nobody's permission");

    assert!(harness.has(ALICE, BOB, RelationshipKind::Favorite).await);
    assert!(
        !harness.has(BOB, ALICE, RelationshipKind::Favorite).await,
        "and writes nothing on the other side"
    );
    assert_eq!(harness.added("favorite"), 1);
}

/// Favouriting somebody the caller blocked is refused.
///
/// Not because it would leak anything, but because it is incoherent: the two rows say
/// opposite things about the same person, and a favourites list containing somebody the
/// user blocked is a list they cannot trust.
#[tokio::test]
async fn favouriting_somebody_the_caller_blocked_is_refused() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);
    harness.social.block(&alice, id(BOB)).await.expect("block");

    expect_code(
        harness.social.set_favorite(&alice, id(BOB), true).await,
        codes::BLOCKED_BY_USER,
    );
    assert!(!harness.has(ALICE, BOB, RelationshipKind::Favorite).await);
}

/// Clearing a favourite that was never set is not an error.
#[tokio::test]
async fn clearing_a_favourite_that_was_never_set_is_not_an_error() {
    let harness = Harness::new();
    harness.cast().await;

    harness
        .social
        .set_favorite(&caller(ALICE, ALICE_PHONE), id(BOB), false)
        .await
        .expect("removing what is not there is the state the caller asked for");
    assert_eq!(harness.removed("favorite"), 1);
}

/// The favourites list has a ceiling, and the row that would break it is not written.
#[tokio::test]
async fn the_favourites_ceiling_refuses_the_next_one() {
    let harness = Harness::new();
    harness.cast().await;
    // Seeded straight to the store: two hundred service calls would cost more than one
    // account's whole burst, and what is under test is the ceiling, not the price.
    for peer in 0..MAX_FAVORITES as u128 {
        harness
            .edge(
                ALICE,
                1_000 + peer,
                RelationshipKind::Favorite,
                NOW + peer as i64,
            )
            .await;
    }

    expect_code(
        harness
            .social
            .set_favorite(&caller(ALICE, ALICE_PHONE), id(BOB), true)
            .await,
        codes::QUOTA_EXCEEDED,
    );
    assert!(!harness.has(ALICE, BOB, RelationshipKind::Favorite).await);
}

/// A full favourites list can still be pruned.
///
/// The ceiling is checked only where a row is added. A limit that refused removals too
/// would be a trap: the only way out of a full list would be to ask an administrator.
#[tokio::test]
async fn a_full_favourites_list_can_still_be_pruned() {
    let harness = Harness::new();
    harness.cast().await;
    for peer in 0..MAX_FAVORITES as u128 {
        harness
            .edge(
                ALICE,
                1_000 + peer,
                RelationshipKind::Favorite,
                NOW + peer as i64,
            )
            .await;
    }

    harness
        .social
        .set_favorite(&caller(ALICE, ALICE_PHONE), id(1_000), false)
        .await
        .expect("a full list is not a locked one");
    assert_eq!(
        harness.count(ALICE, RelationshipKind::Favorite).await,
        MAX_FAVORITES as u64 - 1
    );
}

// ---------------------------------------------------------------------------
// Listings
// ---------------------------------------------------------------------------

/// The friend list shows settled friendships and nothing else.
#[tokio::test]
async fn the_friend_list_shows_only_settled_friendships() {
    let harness = Harness::new();
    harness.cast().await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness.request_waiting(ALICE, CAROL, NOW).await;
    harness
        .edge(ALICE, DAVE, RelationshipKind::Follow, NOW)
        .await;

    let friends = harness
        .social
        .friends(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may read its own friends");

    assert_eq!(others(&friends), vec![id(BOB)]);
    assert!(friends[0].accepted);
    assert_eq!(friends[0].kind, RelationshipKind::Friend);
}

/// Requests are reported from the caller's own two rows, in both directions.
///
/// Both halves of a request live on the caller's side -- one as incoming, one as
/// outgoing -- so neither list needs a scan of anybody else's rows, and the two cannot
/// disagree about who asked whom.
#[tokio::test]
async fn requests_are_reported_in_both_directions() {
    let harness = Harness::new();
    harness.cast().await;
    harness.request_waiting(ALICE, BOB, NOW).await;
    harness.request_waiting(CAROL, ALICE, NOW + SECOND).await;

    let pending = harness
        .social
        .pending(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may read its own queue");

    assert_eq!(others(&pending.outgoing), vec![id(BOB)]);
    assert_eq!(others(&pending.incoming), vec![id(CAROL)]);
    assert!(
        pending
            .outgoing
            .iter()
            .chain(&pending.incoming)
            .all(|edge| !edge.accepted),
        "nothing in a queue is settled"
    );
}

/// Following and followers are read from opposite ends of the same edge.
#[tokio::test]
async fn following_and_followers_are_read_from_opposite_ends() {
    let harness = Harness::new();
    harness.cast().await;
    harness
        .edge(ALICE, BOB, RelationshipKind::Follow, NOW)
        .await;
    harness
        .edge(CAROL, ALICE, RelationshipKind::Follow, NOW)
        .await;
    let alice = caller(ALICE, ALICE_PHONE);

    let following = harness
        .social
        .following(&alice, None)
        .await
        .expect("an account may read who it follows");
    let followers = harness
        .social
        .followers(&alice, None)
        .await
        .expect("an account may read who follows it");

    assert_eq!(others(&following), vec![id(BOB)]);
    assert_eq!(others(&followers), vec![id(CAROL)]);
}

/// The follower list names the follower, not the account reading it.
///
/// The inbound query returns rows whose *other* end is the caller, so the interesting id
/// is the row's owner. Projecting the row the same way as an outbound one would fill the
/// whole list with the caller's own id -- a listing that is the right length, sorted
/// correctly, and completely wrong.
#[tokio::test]
async fn the_follower_list_names_the_follower_and_not_the_caller() {
    let harness = Harness::new();
    harness.cast().await;
    for (follower, at) in [(BOB, NOW), (CAROL, NOW + SECOND), (DAVE, NOW + 2 * SECOND)] {
        harness
            .edge(follower, ALICE, RelationshipKind::Follow, at)
            .await;
    }

    let followers = harness
        .social
        .followers(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may read who follows it");

    assert_eq!(others(&followers), vec![id(DAVE), id(CAROL), id(BOB)]);
    assert!(
        !others(&followers).contains(&id(ALICE)),
        "the caller is not one of their own followers"
    );
}

/// The blocklist is the caller's own blocks and nobody else's.
///
/// There is no endpoint anywhere in this crate that lists who blocked you. This is the
/// one that would be mistaken for it, so it is worth stating that it is not.
#[tokio::test]
async fn the_blocklist_is_the_callers_own_and_nobody_elses() {
    let harness = Harness::new();
    harness.cast().await;
    harness.edge(ALICE, BOB, RelationshipKind::Block, NOW).await;
    harness
        .edge(CAROL, ALICE, RelationshipKind::Block, NOW)
        .await;

    let blocked = harness
        .social
        .blocked(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may read its own blocklist");

    assert_eq!(others(&blocked), vec![id(BOB)]);
}

/// The favourites list is the caller's own bookmarks.
#[tokio::test]
async fn the_favourites_list_is_the_callers_own_bookmarks() {
    let harness = Harness::new();
    harness.cast().await;
    harness
        .edge(ALICE, BOB, RelationshipKind::Favorite, NOW)
        .await;
    harness
        .edge(CAROL, ALICE, RelationshipKind::Favorite, NOW)
        .await;

    let favorites = harness
        .social
        .favorites(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may read its own favourites");

    assert_eq!(others(&favorites), vec![id(BOB)]);
    assert!(favorites[0].accepted, "a bookmark needs no consent");
}

/// Every listing is newest first.
///
/// One order for all six, because a client renders them with the same component, and a
/// list whose order depends on which endpoint filled it is a list nobody can paginate.
#[tokio::test]
async fn every_listing_is_newest_first() {
    let harness = Harness::new();
    harness.cast().await;
    harness.person(ERIN, "erin").await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness.friendship(ALICE, CAROL, NOW + SECOND).await;
    harness.friendship(ALICE, DAVE, NOW + 2 * SECOND).await;
    for (peer, at) in [(BOB, NOW), (CAROL, NOW + SECOND), (DAVE, NOW + 2 * SECOND)] {
        harness
            .edge(ALICE, peer, RelationshipKind::Follow, at)
            .await;
        harness
            .edge(ALICE, peer, RelationshipKind::Favorite, at)
            .await;
    }
    let alice = caller(ALICE, ALICE_PHONE);
    let newest_first = vec![id(DAVE), id(CAROL), id(BOB)];

    assert_eq!(
        others(&harness.social.friends(&alice, None).await.expect("friends")),
        newest_first
    );
    assert_eq!(
        others(
            &harness
                .social
                .following(&alice, None)
                .await
                .expect("following")
        ),
        newest_first
    );
    assert_eq!(
        others(
            &harness
                .social
                .favorites(&alice, None)
                .await
                .expect("favorites")
        ),
        newest_first
    );
}

/// A page size is clamped at both ends.
///
/// Zero would be a listing that returns nothing forever, and an unbounded page is a way
/// to ask one query for every row an account owns. Neither is an error, because a client
/// that sent either is not doing anything wrong enough to fail a screen over.
#[tokio::test]
async fn a_page_size_is_clamped_at_both_ends() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    // More rows than the largest page, so the upper clamp is observable.
    for peer in 0..(MAX_PAGE as u128 + 20) {
        harness
            .edge(
                ALICE,
                1_000 + peer,
                RelationshipKind::Follow,
                NOW + peer as i64,
            )
            .await;
    }
    let alice = caller(ALICE, ALICE_PHONE);

    let none = harness
        .social
        .following(&alice, None)
        .await
        .expect("a caller who named no page gets the default");
    let zero = harness
        .social
        .following(&alice, Some(0))
        .await
        .expect("zero is clamped, not refused");
    let huge = harness
        .social
        .following(&alice, Some(u16::MAX))
        .await
        .expect("an unbounded ask is clamped, not refused");

    assert_eq!(none.len(), DEFAULT_PAGE as usize);
    assert_eq!(zero.len(), 1);
    assert_eq!(huge.len(), MAX_PAGE as usize);
}

/// A listing shows nothing of anybody else's graph.
///
/// The obvious property, stated because it is the one an added `account_id` parameter
/// would quietly break: every listing here reads the caller's own rows and takes no
/// subject at all.
#[tokio::test]
async fn a_listing_shows_nothing_of_anybody_elses_graph() {
    let harness = Harness::new();
    harness.cast().await;
    harness.friendship(BOB, CAROL, NOW).await;
    harness.edge(BOB, DAVE, RelationshipKind::Follow, NOW).await;
    harness.edge(BOB, DAVE, RelationshipKind::Block, NOW).await;
    let alice = caller(ALICE, ALICE_PHONE);

    assert!(harness
        .social
        .friends(&alice, None)
        .await
        .expect("friends")
        .is_empty());
    assert!(harness
        .social
        .following(&alice, None)
        .await
        .expect("following")
        .is_empty());
    assert!(harness
        .social
        .blocked(&alice, None)
        .await
        .expect("blocked")
        .is_empty());
    let pending = harness.social.pending(&alice, None).await.expect("pending");
    assert!(pending.incoming.is_empty() && pending.outgoing.is_empty());
}

// ---------------------------------------------------------------------------
// Standing
// ---------------------------------------------------------------------------

/// Standing reports seven facts, all from the caller's own side.
#[tokio::test]
async fn standing_reports_seven_facts_from_the_callers_side() {
    let harness = Harness::new();
    harness.cast().await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness
        .edge(ALICE, BOB, RelationshipKind::Follow, NOW)
        .await;
    harness
        .edge(BOB, ALICE, RelationshipKind::Follow, NOW)
        .await;
    harness
        .edge(ALICE, BOB, RelationshipKind::Favorite, NOW)
        .await;

    let standing = harness
        .social
        .standing(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("standing is readable between two accounts");

    assert_eq!(
        standing,
        Standing {
            friends: true,
            requested: false,
            awaiting_response: false,
            following: true,
            followed_by: true,
            favorite: true,
            blocked: false,
        }
    );
}

/// Standing tells the two directions of a request apart.
#[tokio::test]
async fn standing_tells_the_two_directions_of_a_request_apart() {
    let harness = Harness::new();
    harness.cast().await;
    harness.request_waiting(ALICE, BOB, NOW).await;
    harness.request_waiting(CAROL, ALICE, NOW).await;
    let alice = caller(ALICE, ALICE_PHONE);

    let towards_bob = harness
        .social
        .standing(&alice, id(BOB))
        .await
        .expect("standing is readable between two accounts");
    let towards_carol = harness
        .social
        .standing(&alice, id(CAROL))
        .await
        .expect("standing is readable between two accounts");

    assert!(towards_bob.requested && !towards_bob.awaiting_response);
    assert!(towards_carol.awaiting_response && !towards_carol.requested);
    assert!(!towards_bob.friends && !towards_carol.friends);
}

/// Standing never reports the subject's block.
///
/// Brief section 180: there is no field for it, and there must not be. A boolean on a
/// profile response would answer in one call the question every error code in this crate
/// is arranged not to answer -- and it would be read by a script, not by a person.
#[tokio::test]
async fn standing_never_reports_the_subjects_block() {
    let harness = Harness::new();
    harness.cast().await;
    harness
        .social
        .block(&caller(BOB, BOB_LAPTOP), id(ALICE))
        .await
        .expect("blocking needs no consent");

    let standing = harness
        .social
        .standing(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("standing stays readable, or its refusal would be the disclosure");

    assert!(
        !standing.blocked,
        "`blocked` is the caller's own act and this was not it"
    );
    assert_eq!(standing, Standing::default(), "and nothing else leaked");
}

/// The caller's own block is reported, because it is theirs.
#[tokio::test]
async fn standing_reports_the_callers_own_block() {
    let harness = Harness::new();
    harness.cast().await;
    harness
        .social
        .block(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("blocking needs no consent");

    let standing = harness
        .social
        .standing(&caller(ALICE, ALICE_PHONE), id(BOB))
        .await
        .expect("standing is readable between two accounts");

    assert!(standing.blocked);
}

/// Standing on a stranger is all false rather than not found.
///
/// An id with no rows against it is a stranger, and a stranger is a legitimate answer.
/// Refusing here would make standing a way to test whether an account exists.
#[tokio::test]
async fn standing_on_a_stranger_is_all_false() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;

    let standing = harness
        .social
        .standing(&caller(ALICE, ALICE_PHONE), id(STRANGER))
        .await
        .expect("a stranger is an answer, not a failure");

    assert_eq!(standing, Standing::default());
}

/// Standing is priced from the profile-fetch opcode, so its own budget runs out first.
///
/// Three per call against an endpoint burst of a hundred, which is thirty-three calls --
/// a smaller budget than the account's own two hundred, so an account that walks the
/// deployment asking about every id it can guess runs out on this endpoint without
/// spending the budget its other work needs.
#[tokio::test]
async fn standing_has_its_own_endpoint_budget() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    for attempt in 0..33 {
        harness
            .social
            .standing(&alice, id(BOB))
            .await
            .unwrap_or_else(|error| panic!("attempt {attempt} must fit the burst: {error}"));
    }
    expect_code(
        harness.social.standing(&alice, id(BOB)).await,
        codes::RATE_LIMITED,
    );
    // And the account's own budget survived it, so other work still goes through.
    harness
        .social
        .friends(&alice, None)
        .await
        .expect("the endpoint ran out, not the account");
}

// ---------------------------------------------------------------------------
// The gate other domains ask
// ---------------------------------------------------------------------------

/// Each interaction reads the column it is about and no other.
///
/// The whole point of having four variants. A gate that read one column for everything
/// would mean somebody who turned off messages also turned off being found, and a user
/// who set one thing would have set three.
#[tokio::test]
async fn each_interaction_reads_its_own_column() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    // Messages from nobody, requests from anybody, last seen from anybody.
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Nobody,
            Visibility::Everyone,
            Visibility::Everyone,
        )
        .await;
    // The mirror image.
    harness
        .person_with(
            CAROL,
            "carol",
            Visibility::Everyone,
            Visibility::Nobody,
            Visibility::Nobody,
        )
        .await;
    let alice = caller(ALICE, ALICE_PHONE);

    expect_code(
        harness
            .social
            .may_interact(&alice, id(BOB), Interaction::Message)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
    harness
        .social
        .may_interact(&alice, id(BOB), Interaction::FriendRequest)
        .await
        .expect("bob takes requests from anybody");
    harness
        .social
        .may_interact(&alice, id(BOB), Interaction::LastSeen)
        .await
        .expect("bob shows last seen to anybody");

    harness
        .social
        .may_interact(&alice, id(CAROL), Interaction::Message)
        .await
        .expect("carol takes messages from anybody");
    expect_code(
        harness
            .social
            .may_interact(&alice, id(CAROL), Interaction::FriendRequest)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
    expect_code(
        harness
            .social
            .may_interact(&alice, id(CAROL), Interaction::LastSeen)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
}

/// An everyone policy lets a stranger through.
#[tokio::test]
async fn an_everyone_policy_lets_a_stranger_through() {
    let harness = Harness::new();
    harness.cast().await;
    let stranger = caller(DAVE, DAVE_TABLET);

    for interaction in [
        Interaction::Message,
        Interaction::FriendRequest,
        Interaction::LastSeen,
    ] {
        harness
            .social
            .may_interact(&stranger, id(BOB), interaction)
            .await
            .unwrap_or_else(|error| panic!("{interaction:?} must be allowed: {error}"));
    }
}

/// A friends policy needs a settled friendship, and a friend has it.
#[tokio::test]
async fn a_friends_policy_needs_a_settled_friendship() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Friends,
            Visibility::Everyone,
            Visibility::Friends,
        )
        .await;
    harness.person(CAROL, "carol").await;
    let alice = caller(ALICE, ALICE_PHONE);
    let carol = caller(CAROL, CAROL_PHONE);
    harness.friendship(ALICE, BOB, NOW).await;

    for interaction in [Interaction::Message, Interaction::LastSeen] {
        harness
            .social
            .may_interact(&alice, id(BOB), interaction)
            .await
            .unwrap_or_else(|error| panic!("a friend passes {interaction:?}: {error}"));
        expect_code(
            harness
                .social
                .may_interact(&carol, id(BOB), interaction)
                .await,
            codes::PRIVACY_RESTRICTED,
        );
    }
}

/// A nobody policy refuses a friend too.
///
/// It is the strictest setting, not the "strangers only" setting. Somebody who chose it
/// has decided to be unreachable, and reading it as "friends still get through" would
/// mean the strictest option was not the strictest.
#[tokio::test]
async fn a_nobody_policy_refuses_even_a_friend() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Nobody,
            Visibility::Nobody,
            Visibility::Nobody,
        )
        .await;
    harness.friendship(ALICE, BOB, NOW).await;
    let alice = caller(ALICE, ALICE_PHONE);

    for interaction in [
        Interaction::Message,
        Interaction::Call,
        Interaction::FriendRequest,
        Interaction::LastSeen,
    ] {
        expect_code(
            harness
                .social
                .may_interact(&alice, id(BOB), interaction)
                .await,
            codes::PRIVACY_RESTRICTED,
        );
    }
}

/// A friends-only message policy is not friends-of-friends.
///
/// The middle setting means two different things by design, and this is the difference:
/// for a friend request it has to mean friends-of-friends, or nobody new could ever ask;
/// for a message it has to mean an accepted friendship, or the setting would let every
/// friend of a friend into an inbox its owner closed.
#[tokio::test]
async fn a_friends_only_message_policy_is_not_friends_of_friends() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Friends,
            Visibility::Friends,
            Visibility::Friends,
        )
        .await;
    harness.person(CAROL, "carol").await;
    harness.friendship(ALICE, CAROL, NOW).await;
    harness.friendship(BOB, CAROL, NOW).await;
    let alice = caller(ALICE, ALICE_PHONE);

    harness
        .social
        .may_interact(&alice, id(BOB), Interaction::FriendRequest)
        .await
        .expect("a friend of a friend may ask");
    expect_code(
        harness
            .social
            .may_interact(&alice, id(BOB), Interaction::Message)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
    expect_code(
        harness
            .social
            .may_interact(&alice, id(BOB), Interaction::LastSeen)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
}

/// A call takes the stricter of the deployment default and the message policy.
///
/// There is no `who_can_call` column, so a call gate that read `who_can_message`
/// directly would let a deployment whose default is friends-only be talked into ringing
/// a stranger's phone, and one that read only the default would ignore a user who set
/// messages to nobody.
#[tokio::test]
async fn a_call_takes_the_stricter_of_the_default_and_the_message_policy() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    // Open to messages from anybody, but the deployment defaults calls to friends.
    harness.person(BOB, "bob").await;
    harness.person(CAROL, "carol").await;
    harness.friendship(ALICE, CAROL, NOW).await;
    let alice = caller(ALICE, ALICE_PHONE);

    assert_eq!(
        harness.social_call_default(),
        Visibility::Friends,
        "the default this test is about"
    );
    expect_code(
        harness
            .social
            .may_interact(&alice, id(BOB), Interaction::Call)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
    harness
        .social
        .may_interact(&alice, id(CAROL), Interaction::Call)
        .await
        .expect("a friend may be called under the default");
}

/// A deployment that opens calls to everybody still honours a closed inbox.
///
/// The other half of the same rule. Widening the deployment default must not override
/// the one column a user actually set.
#[tokio::test]
async fn an_open_call_default_still_honours_a_closed_inbox() {
    let harness = Harness::configured(SocialConfig {
        call_default: Visibility::Everyone,
        ..SocialConfig::default()
    });
    harness.person(ALICE, "alice").await;
    harness.person(BOB, "bob").await;
    harness
        .person_with(
            CAROL,
            "carol",
            Visibility::Nobody,
            Visibility::Everyone,
            Visibility::Everyone,
        )
        .await;
    let alice = caller(ALICE, ALICE_PHONE);

    harness
        .social
        .may_interact(&alice, id(BOB), Interaction::Call)
        .await
        .expect("a wide default reaches an open inbox");
    expect_code(
        harness
            .social
            .may_interact(&alice, id(CAROL), Interaction::Call)
            .await,
        codes::PRIVACY_RESTRICTED,
    );
}

/// An account may reach itself, except to befriend itself.
///
/// A note to self is a real feature and your own last-seen time is not a secret from
/// you. Asking to be your own friend is a client bug, and answering it with a friendship
/// would put a self-edge in a table whose primary key forbids one.
#[tokio::test]
async fn an_account_may_reach_itself_but_not_befriend_itself() {
    let harness = Harness::new();
    harness
        .person_with(
            ALICE,
            "alice",
            Visibility::Nobody,
            Visibility::Nobody,
            Visibility::Nobody,
        )
        .await;
    let alice = caller(ALICE, ALICE_PHONE);

    for interaction in [
        Interaction::Message,
        Interaction::Call,
        Interaction::LastSeen,
    ] {
        harness
            .social
            .may_interact(&alice, id(ALICE), interaction)
            .await
            .unwrap_or_else(|error| panic!("{interaction:?} with yourself is allowed: {error}"));
    }
    expect_code(
        harness
            .social
            .may_interact(&alice, id(ALICE), Interaction::FriendRequest)
            .await,
        codes::VALIDATION_FAILED,
    );
}

/// A subject with no profile is not found rather than refused.
///
/// Nothing to leak: an id with no profile behind it has no settings to protect and no
/// relationship to hide. Reporting it as a privacy refusal would send the caller looking
/// for a setting that does not exist.
#[tokio::test]
async fn a_gate_on_an_account_that_does_not_exist_is_not_found() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;

    expect_code(
        harness
            .social
            .may_interact(
                &caller(ALICE, ALICE_PHONE),
                id(STRANGER),
                Interaction::Message,
            )
            .await,
        codes::NOT_FOUND,
    );
    assert_eq!(harness.gates("unknown"), 1);
}

/// The gate is never charged.
///
/// It sits on the path of an operation that has already been charged -- a message send
/// charges the send, then asks this crate whether the recipient accepts messages -- so
/// charging again would bill one user action twice and make a send budget depend on how
/// many gates the implementation happens to consult.
#[tokio::test]
async fn the_gate_is_never_charged() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // A hundred gates. At the cheapest price anything else here pays, this would be five
    // times an account's whole burst.
    for _ in 0..100 {
        harness
            .social
            .may_interact(&alice, id(BOB), Interaction::Message)
            .await
            .expect("a gate on an open profile is allowed");
    }

    harness
        .social
        .suggest(&alice, None)
        .await
        .expect("the most expensive call here still fits, so nothing was charged");
    assert_eq!(harness.gates("allowed"), 100);
}

/// A friends-of-friends answer that cannot be completed refuses.
///
/// The mutual scan is bounded at two hundred each side. Past that the honest answer is
/// "no mutual friend found", and this test builds the shape that gets it wrong: the one
/// shared friend is the oldest edge, so it falls outside the window that a listing sorted
/// newest-first returns. A gate that fails open when the data gets large is a gate that
/// stops working exactly for the accounts with the most to lose.
#[tokio::test]
async fn a_mutual_scan_that_cannot_finish_refuses() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Everyone,
            Visibility::Friends,
            Visibility::Everyone,
        )
        .await;
    // The shared friend, and the oldest edge Alice has.
    harness.friendship(ALICE, CAROL, NOW).await;
    harness.friendship(BOB, CAROL, NOW).await;
    // Then exactly enough newer friends to push it out of the window.
    for extra in 0..MAX_MUTUAL_SCAN as u128 {
        harness
            .friendship(ALICE, 1_000 + extra, NOW + SECOND + extra as i64)
            .await;
    }

    expect_code(
        harness
            .social
            .may_interact(
                &caller(ALICE, ALICE_PHONE),
                id(BOB),
                Interaction::FriendRequest,
            )
            .await,
        codes::PRIVACY_RESTRICTED,
    );
}

/// The same shape inside the window is allowed.
///
/// The control for the test above: without it, a gate that refused everybody would pass.
#[tokio::test]
async fn a_mutual_scan_that_fits_is_allowed() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Everyone,
            Visibility::Friends,
            Visibility::Everyone,
        )
        .await;
    harness.friendship(ALICE, CAROL, NOW).await;
    harness.friendship(BOB, CAROL, NOW).await;
    for extra in 0..(MAX_MUTUAL_SCAN as u128 - 1) {
        harness
            .friendship(ALICE, 1_000 + extra, NOW + SECOND + extra as i64)
            .await;
    }

    harness
        .social
        .may_interact(
            &caller(ALICE, ALICE_PHONE),
            id(BOB),
            Interaction::FriendRequest,
        )
        .await
        .expect("the shared friend is still inside the window");
}

/// An unaccepted friendship is not a mutual friend either.
///
/// The bound is not the only way this answer goes wrong. A request in flight to the same
/// third party would otherwise let two strangers vouch for each other by both asking
/// somebody popular.
#[tokio::test]
async fn a_request_to_a_third_party_is_not_a_mutual_friend() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness
        .person_with(
            BOB,
            "bob",
            Visibility::Everyone,
            Visibility::Friends,
            Visibility::Everyone,
        )
        .await;
    harness.person(CAROL, "carol").await;
    harness.request_waiting(ALICE, CAROL, NOW).await;
    harness.request_waiting(BOB, CAROL, NOW).await;

    expect_code(
        harness
            .social
            .may_interact(
                &caller(ALICE, ALICE_PHONE),
                id(BOB),
                Interaction::FriendRequest,
            )
            .await,
        codes::PRIVACY_RESTRICTED,
    );
}

// ---------------------------------------------------------------------------
// Suggestions
// ---------------------------------------------------------------------------

/// A suggestion counts the friends two accounts share.
///
/// The only reason offered, because it is the only one the schema supports. The other
/// discovery axes in the brief -- same interests, same country, same rooms -- need either
/// a column that does not exist or a query nobody should run on a profile view.
#[tokio::test]
async fn a_suggestion_counts_the_friends_two_accounts_share() {
    let harness = Harness::new();
    harness.cast().await;
    harness.person(ERIN, "erin").await;
    // Alice knows Bob and Carol. Both know Dave; only Bob knows Erin.
    harness.friendship(ALICE, BOB, NOW).await;
    harness.friendship(ALICE, CAROL, NOW).await;
    harness.friendship(BOB, DAVE, NOW).await;
    harness.friendship(CAROL, DAVE, NOW).await;
    harness.friendship(BOB, ERIN, NOW).await;

    let suggestions = harness
        .social
        .suggest(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may ask who it might know");

    assert_eq!(
        suggestions
            .iter()
            .map(|s| (s.account_id, s.mutual_friends))
            .collect::<Vec<_>>(),
        vec![(id(DAVE), 2), (id(ERIN), 1)],
        "most shared friends first"
    );
    assert_eq!(harness.plain("migo_social_suggestions_returned_total"), 2);
}

/// Ties are broken by id, so the same graph gives the same page twice.
///
/// A suggestion list whose order depended on hash iteration would reshuffle between two
/// calls, and a user scrolling it would see the same face twice and miss another.
#[tokio::test]
async fn suggestions_with_the_same_score_are_ordered_stably() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.friendship(ALICE, BOB, NOW).await;
    for candidate in [40u128, 10, 30, 20] {
        harness.friendship(BOB, candidate, NOW).await;
    }
    let alice = caller(ALICE, ALICE_PHONE);

    let first = harness.social.suggest(&alice, None).await.expect("first");
    let second = harness.social.suggest(&alice, None).await.expect("again");

    assert_eq!(
        first.iter().map(|s| s.account_id).collect::<Vec<_>>(),
        vec![id(10), id(20), id(30), id(40)]
    );
    assert_eq!(first, second, "the same graph gives the same page");
}

/// A suggestion never offers a friend, a request in flight, or the caller.
#[tokio::test]
async fn a_suggestion_never_offers_a_friend_a_request_or_the_caller() {
    let harness = Harness::new();
    harness.cast().await;
    harness.person(ERIN, "erin").await;
    harness.friendship(ALICE, BOB, NOW).await;
    // Carol is already a friend of Alice as well as of Bob.
    harness.friendship(ALICE, CAROL, NOW).await;
    harness.friendship(BOB, CAROL, NOW).await;
    // Dave has a request in flight from Alice.
    harness.friendship(BOB, DAVE, NOW).await;
    harness.request_waiting(ALICE, DAVE, NOW).await;
    // Erin asked Alice and is waiting.
    harness.friendship(BOB, ERIN, NOW).await;
    harness.request_waiting(ERIN, ALICE, NOW).await;
    // And Bob is friends with Alice, so Alice must not be suggested to herself.
    harness.friendship(BOB, ALICE, NOW).await;

    let suggestions = harness
        .social
        .suggest(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may ask who it might know");

    assert!(
        suggestions.is_empty(),
        "everybody reachable is already known: {suggestions:?}"
    );
}

/// A suggestion never offers somebody either side blocked.
///
/// Both directions. Offering up the account somebody blocked last week would be the
/// product undoing a decision the user made deliberately; offering an account that
/// blocked *them* would be the product walking them into a wall.
#[tokio::test]
async fn a_suggestion_never_offers_a_blocked_account() {
    let harness = Harness::new();
    harness.cast().await;
    harness.person(ERIN, "erin").await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness.friendship(BOB, CAROL, NOW).await;
    harness.friendship(BOB, DAVE, NOW).await;
    harness.friendship(BOB, ERIN, NOW).await;
    harness
        .edge(ALICE, CAROL, RelationshipKind::Block, NOW)
        .await;
    harness
        .edge(DAVE, ALICE, RelationshipKind::Block, NOW)
        .await;

    let suggestions = harness
        .social
        .suggest(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may ask who it might know");

    assert_eq!(
        suggestions.iter().map(|s| s.account_id).collect::<Vec<_>>(),
        vec![id(ERIN)]
    );
}

/// Suggestions stop at one hop.
///
/// A friend of a friend of a friend is a stranger with extra steps, and following the
/// graph further is how a suggestion query turns into a crawl of the whole deployment.
#[tokio::test]
async fn suggestions_stop_at_one_hop() {
    let harness = Harness::new();
    harness.cast().await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness.friendship(BOB, CAROL, NOW).await;
    harness.friendship(CAROL, DAVE, NOW).await;

    let suggestions = harness
        .social
        .suggest(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may ask who it might know");

    assert_eq!(
        suggestions.iter().map(|s| s.account_id).collect::<Vec<_>>(),
        vec![id(CAROL)],
        "dave is two hops away"
    );
}

/// One round reads at most twenty-five friend lists.
///
/// The bound that makes the query affordable, and the counter is how an operator sees it:
/// with thirty friends who each have one other friend, a full walk would read thirty
/// lists. The scan counter proves it read twenty-five.
#[tokio::test]
async fn one_suggestion_round_reads_a_bounded_number_of_friend_lists() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    for index in 0..30u128 {
        harness
            .friendship(ALICE, 1_000 + index, NOW + index as i64)
            .await;
        harness
            .friendship(1_000 + index, 2_000 + index, NOW + index as i64)
            .await;
    }

    let suggestions = harness
        .social
        .suggest(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may ask who it might know");

    assert_eq!(suggestions.len(), 25, "one candidate per list it read");
    // Thirty of the caller's own, then two per seed list: the caller and the candidate.
    assert_eq!(
        harness.plain("migo_social_suggestion_edges_scanned_total"),
        30 + 25 * 2
    );
}

/// A suggestion page is clamped like any other.
#[tokio::test]
async fn a_suggestion_page_is_clamped() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.friendship(ALICE, BOB, NOW).await;
    for candidate in 0..5u128 {
        harness.friendship(BOB, 2_000 + candidate, NOW).await;
    }
    let alice = caller(ALICE, ALICE_PHONE);

    let two = harness
        .social
        .suggest(&alice, Some(2))
        .await
        .expect("a page of two");
    let zero = harness
        .social
        .suggest(&alice, Some(0))
        .await
        .expect("zero is clamped, not refused");

    assert_eq!(two.len(), 2);
    assert_eq!(zero.len(), 1);
}

/// An account with no friends is suggested nothing, and the query stops early.
#[tokio::test]
async fn an_account_with_no_friends_is_suggested_nothing() {
    let harness = Harness::new();
    harness.cast().await;

    let suggestions = harness
        .social
        .suggest(&caller(ALICE, ALICE_PHONE), None)
        .await
        .expect("an account may ask who it might know");

    assert!(suggestions.is_empty());
    assert_eq!(
        harness.plain("migo_social_suggestion_edges_scanned_total"),
        0
    );
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// A search matches a username from the front and a display name anywhere.
///
/// Two different rules for two different fields. A username is an identifier somebody
/// types from memory, so a prefix is what they have; a display name is prose, so the
/// word they remember may be in the middle of it.
#[tokio::test]
async fn a_search_matches_a_username_prefix_and_a_display_name_fragment() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.person(BOB, "bobby").await;
    let alice = caller(ALICE, ALICE_PHONE);

    let by_prefix = harness
        .social
        .search(&alice, "bob", None)
        .await
        .expect("a search runs");
    assert_eq!(
        by_prefix.iter().map(|f| f.account_id).collect::<Vec<_>>(),
        vec![id(BOB)]
    );
    assert_eq!(by_prefix[0].username, "bobby");
    assert_eq!(by_prefix[0].display_name, "bobby Nusantara");
    assert_eq!(by_prefix[0].avatar_media_id, Some(id(900 + BOB)));

    // Give the display name nothing in common with the username, so that the two rules
    // can be told apart at all.
    harness
        .store
        .update_profile(
            id(BOB),
            ProfilePatch {
                display_name: Some("Budi Santoso".to_string()),
                ..ProfilePatch::default()
            },
            ts(NOW),
        )
        .await
        .expect("an account may rename itself");

    // The middle of a username matches nothing.
    assert!(harness
        .social
        .search(&alice, "obby", None)
        .await
        .expect("a search runs")
        .is_empty());
    // The middle of a display name does, and case is not part of either rule.
    for fragment in ["Santoso", "santoso", "udi San"] {
        let by_fragment = harness
            .social
            .search(&alice, fragment, None)
            .await
            .expect("a search runs");
        assert_eq!(
            by_fragment.iter().map(|f| f.account_id).collect::<Vec<_>>(),
            vec![id(BOB)],
            "{fragment}"
        );
    }
}

/// A search never returns the account doing the searching.
///
/// Not a privacy rule, a usability one: a results list whose first row is yourself is a
/// row nobody wanted, and it is the row most likely to be tapped by accident.
#[tokio::test]
async fn a_search_never_returns_the_caller() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.person(BOB, "alicia").await;

    let found = harness
        .social
        .search(&caller(ALICE, ALICE_PHONE), "ali", None)
        .await
        .expect("a search runs");

    assert_eq!(
        found.iter().map(|f| f.account_id).collect::<Vec<_>>(),
        vec![id(BOB)]
    );
}

/// A search hides both directions of a block.
///
/// The direction that is easy to miss is the second: an account that blocked the caller
/// must not appear, or the block is undone by the search box.
#[tokio::test]
async fn a_search_hides_both_directions_of_a_block() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.person(BOB, "searchable-bob").await;
    harness.person(CAROL, "searchable-carol").await;
    harness.person(DAVE, "searchable-dave").await;
    harness.edge(ALICE, BOB, RelationshipKind::Block, NOW).await;
    harness
        .edge(CAROL, ALICE, RelationshipKind::Block, NOW)
        .await;

    let found = harness
        .social
        .search(&caller(ALICE, ALICE_PHONE), "searchable", None)
        .await
        .expect("a search runs");

    assert_eq!(
        found.iter().map(|f| f.account_id).collect::<Vec<_>>(),
        vec![id(DAVE)]
    );
    assert_eq!(harness.plain("migo_social_searches_total"), 1);
    assert_eq!(
        harness.plain("migo_social_search_hits_total"),
        1,
        "hits are counted after the blocklist, or the counter would leak the filtering"
    );
}

/// An account that opted out of search is not found by it.
#[tokio::test]
async fn an_account_that_opted_out_of_search_is_not_found_by_it() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.unlisted_person(BOB, "bob").await;

    assert!(harness
        .social
        .search(&caller(ALICE, ALICE_PHONE), "bob", None)
        .await
        .expect("a search runs")
        .is_empty());
}

/// A suspended account is not findable.
///
/// Somebody removed from the deployment should not be reachable through the front door,
/// and a search result is a name plus an avatar -- enough to keep an account visible long
/// after the reason it was suspended.
#[tokio::test]
async fn a_suspended_account_is_not_findable() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.person(BOB, "bob").await;
    harness
        .store
        .set_status(id(BOB), AccountStatus::Suspended, None, ts(NOW))
        .await
        .expect("moderation may suspend an account");

    assert!(harness
        .social
        .search(&caller(ALICE, ALICE_PHONE), "bob", None)
        .await
        .expect("a search runs")
        .is_empty());
}

/// A query that cannot match anything is refused before it is charged.
///
/// Both ends: nothing to search for, and more than anybody types. The refusal comes
/// before the charge, so a client with a bug in its debounce cannot spend a user's budget
/// on empty keystrokes -- a hundred bad queries here, and the next real one still runs.
#[tokio::test]
async fn an_unusable_query_is_refused_before_it_is_charged() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    harness.person(BOB, "bob").await;
    let alice = caller(ALICE, ALICE_PHONE);
    let too_long = "b".repeat(MAX_QUERY_LEN + 1);

    for _ in 0..50 {
        expect_code(
            harness.social.search(&alice, "", None).await,
            codes::VALIDATION_FAILED,
        );
        expect_code(
            harness.social.search(&alice, "   ", None).await,
            codes::VALIDATION_FAILED,
        );
        expect_code(
            harness.social.search(&alice, &too_long, None).await,
            codes::VALIDATION_FAILED,
        );
    }

    let found = harness
        .social
        .search(&alice, "bob", None)
        .await
        .expect("a hundred and fifty refusals cost nothing");
    assert_eq!(found.len(), 1);
    assert_eq!(
        harness.plain("migo_social_searches_total"),
        1,
        "a refused query is not a search"
    );
}

/// A query at the ceiling is accepted.
///
/// The boundary itself, because an off-by-one here is a search box that stops working at
/// exactly the length nobody tests by hand.
#[tokio::test]
async fn a_query_exactly_at_the_ceiling_is_accepted() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;

    harness
        .social
        .search(
            &caller(ALICE, ALICE_PHONE),
            &"b".repeat(MAX_QUERY_LEN),
            None,
        )
        .await
        .expect("the ceiling is inclusive");
}

/// A search page is clamped.
#[tokio::test]
async fn a_search_page_is_clamped() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    for index in 0..5u128 {
        harness
            .person(100 + index, &format!("target-{index}"))
            .await;
    }
    let alice = caller(ALICE, ALICE_PHONE);

    assert_eq!(
        harness
            .social
            .search(&alice, "target", Some(2))
            .await
            .expect("a search runs")
            .len(),
        2
    );
    assert_eq!(
        harness
            .social
            .search(&alice, "target", Some(0))
            .await
            .expect("zero is clamped, not refused")
            .len(),
        1
    );
}

// ---------------------------------------------------------------------------
// Profile batches
// ---------------------------------------------------------------------------

/// A batch returns the public face and nothing behind it.
///
/// Seven fields, and no visibility settings, no relationship flags, no last-seen time. A
/// profile card is what a stranger may see; the other three answers are governed by other
/// rules and a struct carrying all of them would be filled by whichever caller was
/// convenient.
#[tokio::test]
async fn a_batch_returns_the_public_face_and_nothing_behind_it() {
    let harness = Harness::new();
    harness.cast().await;

    let cards = harness
        .social
        .profiles(&caller(ALICE, ALICE_PHONE), &[id(BOB)])
        .await
        .expect("a public profile is public");

    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    assert_eq!(card.account_id, id(BOB));
    assert_eq!(card.username, "bob");
    assert_eq!(card.display_name, "bob Nusantara");
    assert_eq!(card.bio.as_deref(), Some("halo, saya bob"));
    assert_eq!(card.avatar_media_id, Some(id(900 + BOB)));
    assert_eq!(card.country.as_deref(), Some("ID"));
    assert_eq!(card.locale, "id-ID");
    assert_eq!(harness.plain("migo_social_profiles_requested_total"), 1);
    assert_eq!(harness.plain("migo_social_profiles_served_total"), 1);
}

/// A repeated id is one card and one charge.
///
/// The price is flat per call, so the ceiling is the only thing standing between it and an
/// unbounded read -- which means duplicates have to collapse before the ceiling is
/// checked, or a caller could ask for the same id sixty-four times and get one card for
/// the price of sixty-four.
#[tokio::test]
async fn a_repeated_id_is_one_card() {
    let harness = Harness::new();
    harness.cast().await;

    let cards = harness
        .social
        .profiles(
            &caller(ALICE, ALICE_PHONE),
            &[id(BOB), id(BOB), id(CAROL), id(BOB)],
        )
        .await
        .expect("a public profile is public");

    assert_eq!(cards.len(), 2);
    assert_eq!(
        harness.plain("migo_social_profiles_requested_total"),
        2,
        "counted after the duplicates collapse"
    );
}

/// A blocked id and an id that does not exist are the same observation.
///
/// Brief section 180: a missing profile in a batch is silently omitted rather than
/// reported. If the two differed -- by code, by an error field, by position -- a batch of
/// sixty-four ids would be a blocklist oracle that reads a deployment at sixty-four
/// accounts per request.
#[tokio::test]
async fn a_blocked_id_and_a_missing_id_are_the_same_observation() {
    let harness = Harness::new();
    harness.cast().await;
    harness.person(ERIN, "erin").await;
    harness.edge(ALICE, BOB, RelationshipKind::Block, NOW).await;
    harness
        .edge(CAROL, ALICE, RelationshipKind::Block, NOW)
        .await;
    harness.half_registered(6, "half").await;

    let cards = harness
        .social
        .profiles(
            &caller(ALICE, ALICE_PHONE),
            &[id(BOB), id(CAROL), id(STRANGER), id(6), id(ERIN)],
        )
        .await
        .expect("an omission is not a failure");

    assert_eq!(
        cards.iter().map(|c| c.account_id).collect::<Vec<_>>(),
        vec![id(ERIN)],
        "four ids omitted, one for each of four different reasons, all silently"
    );
    assert_eq!(harness.plain("migo_social_profiles_requested_total"), 5);
    assert_eq!(harness.plain("migo_social_profiles_served_total"), 1);
}

/// The caller can always read their own card.
///
/// A member list includes the person reading it, and a card missing from it would render
/// as a blank row on the user's own name.
#[tokio::test]
async fn the_caller_can_always_read_their_own_card() {
    let harness = Harness::new();
    harness
        .person_with(
            ALICE,
            "alice",
            Visibility::Nobody,
            Visibility::Nobody,
            Visibility::Nobody,
        )
        .await;

    let cards = harness
        .social
        .profiles(&caller(ALICE, ALICE_PHONE), &[id(ALICE)])
        .await
        .expect("your own card is yours");

    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].account_id, id(ALICE));
}

/// An empty batch is a missing field.
///
/// A request that named nothing is a client bug, and answering it with an empty list would
/// hide the bug behind a screen that renders as if the accounts had all been deleted.
#[tokio::test]
async fn an_empty_batch_is_a_missing_field() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;

    expect_code(
        harness
            .social
            .profiles(&caller(ALICE, ALICE_PHONE), &[])
            .await,
        codes::FIELD_REQUIRED,
    );
}

/// A batch past the ceiling is refused rather than truncated.
///
/// Truncating would be worse than refusing: the client would render a member list with
/// silently missing faces and no way to know which ones.
#[tokio::test]
async fn a_batch_past_the_ceiling_is_refused() {
    let harness = Harness::new();
    harness.person(ALICE, "alice").await;
    let ids: Vec<Id> = (0..(MAX_PROFILE_BATCH as u128 + 1))
        .map(|index| id(1_000 + index))
        .collect();

    expect_code(
        harness
            .social
            .profiles(&caller(ALICE, ALICE_PHONE), &ids)
            .await,
        codes::VALIDATION_FAILED,
    );
    // The ceiling itself is fine.
    harness
        .social
        .profiles(&caller(ALICE, ALICE_PHONE), &ids[..MAX_PROFILE_BATCH])
        .await
        .expect("the ceiling is inclusive");
}

/// A batch is not charged for a request it refuses.
#[tokio::test]
async fn a_refused_batch_is_not_charged() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    for _ in 0..60 {
        expect_code(
            harness.social.profiles(&alice, &[]).await,
            codes::FIELD_REQUIRED,
        );
    }

    harness
        .social
        .profiles(&alice, &[id(BOB)])
        .await
        .expect("sixty refusals cost nothing");
}

// ---------------------------------------------------------------------------
// One account's decisions are its own
// ---------------------------------------------------------------------------

/// A block is not transitive.
///
/// Somebody who blocks a person is asking not to hear from them, not asking their friends
/// to take a side. A block that propagated along the friend graph would let one account
/// quietly cut a second one off from a third, which is a harassment tool rather than a
/// safety one.
#[tokio::test]
async fn a_block_does_not_reach_past_the_two_accounts_in_it() {
    let harness = Harness::new();
    harness.cast().await;
    harness.person(ERIN, "erin").await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness.friendship(ALICE, ERIN, NOW).await;

    harness
        .social
        .block(&caller(ERIN, ERIN_PHONE), id(BOB))
        .await
        .expect("erin may block bob");

    // Alice keeps both friendships, and may still reach either one.
    assert_eq!(harness.count(ALICE, RelationshipKind::Friend).await, 2);
    for subject in [BOB, ERIN] {
        harness
            .social
            .may_interact(
                &caller(ALICE, ALICE_PHONE),
                id(subject),
                Interaction::Message,
            )
            .await
            .expect("alice took no side in it");
    }
    // And Bob, who was blocked, keeps his own view of Alice.
    harness
        .social
        .may_interact(&caller(BOB, BOB_LAPTOP), id(ALICE), Interaction::Message)
        .await
        .expect("bob lost erin and nothing else");
}

// ---------------------------------------------------------------------------
// Model helpers
// ---------------------------------------------------------------------------

/// The stricter of two settings is the numeric minimum.
///
/// Written as a minimum rather than as a match so that a fourth visibility added later
/// cannot fall through a missing arm into `Everyone` -- which is the one direction this
/// function must never fail in.
#[test]
fn the_stricter_of_two_settings_is_the_minimum() {
    let all = [
        Visibility::Nobody,
        Visibility::Friends,
        Visibility::Everyone,
    ];
    for left in all {
        for right in all {
            let picked = strictest(left, right);
            assert!(
                picked == left || picked == right,
                "the answer is one of the two"
            );
            assert!(
                picked.to_i16() <= left.to_i16() && picked.to_i16() <= right.to_i16(),
                "and never wider than either"
            );
        }
    }
    assert_eq!(
        strictest(Visibility::Everyone, Visibility::Nobody),
        Visibility::Nobody
    );
    assert_eq!(
        strictest(Visibility::Everyone, Visibility::Friends),
        Visibility::Friends
    );
}

/// A usable query is non-empty after trimming and short enough in characters.
///
/// Characters and not bytes. A query of forty-eight Javanese script characters is a
/// person typing their friend's name, and refusing it because it is a hundred and
/// ninety-two bytes long would be a limit that only applies to some alphabets.
#[test]
fn a_usable_query_is_measured_in_characters() {
    assert!(!query_is_usable(""));
    assert!(!query_is_usable("   "));
    assert!(!query_is_usable("\t\n"));
    assert!(query_is_usable("a"));
    assert!(query_is_usable("  budi  "));
    assert!(query_is_usable(&"b".repeat(MAX_QUERY_LEN)));
    assert!(!query_is_usable(&"b".repeat(MAX_QUERY_LEN + 1)));
    // Three bytes each, so this is well past the byte count and inside the character one.
    assert!(query_is_usable(&"ᮘ".repeat(MAX_QUERY_LEN)));
    assert!(!query_is_usable(&"ᮘ".repeat(MAX_QUERY_LEN + 1)));
}

/// Every kind of edge projects with the right settled flag.
#[test]
fn an_edge_projects_the_settled_flag_from_its_kind() {
    let row = |kind, accepted_at| Relationship {
        account_id: id(ALICE),
        other_id: id(BOB),
        kind,
        created_at: ts(NOW),
        accepted_at,
    };

    assert!(Edge::of(&row(RelationshipKind::Friend, Some(ts(NOW)))).accepted);
    assert!(!Edge::of(&row(RelationshipKind::Friend, None)).accepted);
    assert!(!Edge::of(&row(RelationshipKind::PendingOutgoing, None)).accepted);
    assert!(!Edge::of(&row(RelationshipKind::PendingIncoming, None)).accepted);
    for consentless in [
        RelationshipKind::Follow,
        RelationshipKind::Block,
        RelationshipKind::Favorite,
    ] {
        assert!(
            Edge::of(&row(consentless, None)).accepted,
            "{consentless:?} needs no consent, so it is settled on creation"
        );
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Every series exists before anything happens.
///
/// A dashboard built on a counter that only appears after the first occurrence shows a
/// gap where it should show a zero, and an alert on a rate of a missing series never
/// fires. Registering the whole set up front is what makes "no friend requests were
/// refused" a readable answer rather than an absent one.
#[tokio::test]
async fn every_series_exists_before_anything_happens() {
    let harness = Harness::new();

    for outcome in [
        "sent",
        "duplicate",
        "reciprocated",
        "redundant",
        "blocked",
        "restricted",
        "full",
        "invalid",
        "rate_limited",
    ] {
        assert_eq!(harness.requests(outcome), 0, "requests {outcome}");
    }
    for outcome in ["accepted", "declined", "missing"] {
        assert_eq!(harness.responses(outcome), 0, "responses {outcome}");
    }
    for kind in ["friend", "follow", "block", "favorite"] {
        assert_eq!(harness.added(kind), 0, "added {kind}");
        assert_eq!(harness.removed(kind), 0, "removed {kind}");
    }
    for outcome in ["allowed", "blocked", "restricted", "unknown"] {
        assert_eq!(harness.gates(outcome), 0, "gate {outcome}");
    }
    for series in [
        "migo_social_suggestions_returned_total",
        "migo_social_suggestion_edges_scanned_total",
        "migo_social_searches_total",
        "migo_social_search_hits_total",
        "migo_social_profiles_requested_total",
        "migo_social_profiles_served_total",
    ] {
        assert_eq!(harness.plain(series), 0, "{series}");
    }
}

/// No metric is labelled by an account.
///
/// Brief section 174. A counter labelled by account id is a social graph in the
/// monitoring system: who asked whom, who blocked whom, who searched for what -- retained
/// for as long as the metrics are, readable by anybody with a dashboard, and outside every
/// access control this crate implements.
#[tokio::test]
async fn no_metric_is_labelled_by_an_account() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    harness
        .social
        .request_friend(&alice, id(BOB))
        .await
        .expect("a request");
    harness
        .social
        .block(&alice, id(CAROL))
        .await
        .expect("a block");
    harness
        .social
        .search(&alice, "dave", None)
        .await
        .expect("a search");
    harness
        .social
        .profiles(&alice, &[id(DAVE)])
        .await
        .expect("a profile");
    harness
        .social
        .may_interact(&alice, id(DAVE), Interaction::Message)
        .await
        .expect("a gate");

    let rendered = harness.registry.render();
    for secret in [
        id(ALICE).to_text(),
        id(BOB).to_text(),
        id(CAROL).to_text(),
        id(DAVE).to_text(),
        id(ALICE_PHONE).to_text(),
    ] {
        assert!(
            !rendered.contains(&secret),
            "an id reached the metrics: {secret}"
        );
    }
    assert!(
        !rendered.contains("alice") && !rendered.contains("dave"),
        "and neither did a username or a search term"
    );
    assert!(
        rendered.contains("migo_social_searches_total"),
        "while the series themselves are still there"
    );
}

// --- mutes -----------------------------------------------------------------------
//
// A mute is a volume control and not a wall. What these tests pin is everything a
// mute must NOT do: tear down the friendship or the follows the way a block does,
// write an edge against the subject, or announce itself to anybody but the caller's
// own list.

/// A mute is one row, and the only row it touches is its own.
///
/// The contrast with a block is the whole point: a block is a falling-out and undoes
/// the graph between two accounts; a mute is "this one account is loud" and leaves
/// every edge exactly where it was. The version that quietly deleted a friendship
/// because its owner wanted somebody quieter would be this crate making a decision
/// the caller never made.
#[tokio::test]
async fn a_mute_tears_nothing_down() {
    let harness = Harness::new();
    harness.cast().await;
    harness.friendship(ALICE, BOB, NOW).await;
    harness
        .edge(ALICE, BOB, RelationshipKind::Follow, NOW)
        .await;
    harness
        .edge(BOB, ALICE, RelationshipKind::Follow, NOW)
        .await;
    harness
        .edge(ALICE, BOB, RelationshipKind::Favorite, NOW)
        .await;

    harness
        .social
        .mute(&caller(ALICE, ALICE_PHONE), id(BOB), true)
        .await
        .expect("muting needs no consent and announces nothing");

    assert!(harness.has(ALICE, BOB, RelationshipKind::Mute).await);
    // One direction only: no edge is written against the subject.
    assert!(!harness.has(BOB, ALICE, RelationshipKind::Mute).await);
    // And everything else survives, in both directions.
    for (owner, peer, kind) in [
        (ALICE, BOB, RelationshipKind::Friend),
        (BOB, ALICE, RelationshipKind::Friend),
        (ALICE, BOB, RelationshipKind::Follow),
        (BOB, ALICE, RelationshipKind::Follow),
        (ALICE, BOB, RelationshipKind::Favorite),
    ] {
        assert!(
            harness.has(owner, peer, kind).await,
            "a mute must leave the {kind:?} edge from {owner} to {peer} alone"
        );
    }
    assert_eq!(harness.added("mute"), 1);
    assert_eq!(harness.removed("friend"), 0);
    assert_eq!(harness.removed("follow"), 0);
    assert_eq!(harness.removed("favorite"), 0);
}

/// Unmuting removes the mute and only the mute.
#[tokio::test]
async fn unmuting_removes_only_the_mute() {
    let harness = Harness::new();
    harness.cast().await;
    harness.friendship(ALICE, BOB, NOW).await;
    let alice = caller(ALICE, ALICE_PHONE);

    harness
        .social
        .mute(&alice, id(BOB), true)
        .await
        .expect("the mute lands");
    harness
        .social
        .mute(&alice, id(BOB), false)
        .await
        .expect("unmuting is idempotent");

    assert!(!harness.has(ALICE, BOB, RelationshipKind::Mute).await);
    assert!(
        harness.has(ALICE, BOB, RelationshipKind::Friend).await,
        "the friendship the mute never touched is still there"
    );
    assert_eq!(harness.added("mute"), 1);
    assert_eq!(harness.removed("mute"), 1);
}

/// The mute list is the caller's own and carries the edge kind.
#[tokio::test]
async fn the_mute_list_names_its_kind() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    harness
        .social
        .mute(&alice, id(BOB), true)
        .await
        .expect("the mute lands");

    let muted = harness
        .social
        .muted(&alice, None)
        .await
        .expect("the caller's own list is readable");
    assert_eq!(muted.len(), 1);
    assert_eq!(muted[0].other_id, id(BOB));
    assert_eq!(muted[0].kind, RelationshipKind::Mute);
}

/// The mute list has the same ceiling the blocklist has, for the same reason.
#[tokio::test]
async fn the_mute_list_ceiling_refuses_the_next_mute() {
    let harness = Harness::configured(SocialConfig {
        max_mutes: 1,
        ..SocialConfig::default()
    });
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    harness
        .social
        .mute(&alice, id(BOB), true)
        .await
        .expect("first");
    expect_code(
        harness.social.mute(&alice, id(CAROL), true).await,
        codes::QUOTA_EXCEEDED,
    );
}
