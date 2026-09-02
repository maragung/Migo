//! Integration tests for the rooms service.
//!
//! Everything runs against `MemoryStore`, `MemoryCache`, and the real
//! `CacheRateLimiter` over that cache, with a seeded `SeededRandom` and hand-written
//! timestamps. No clock is read and no socket is opened, so a failure here is a
//! failure in the code and not in the machine.
//!
//! The limiter is the real one rather than a fake because rooms has nothing
//! asymmetric to prove about charge ordering: every method charges before it acts,
//! and the one that deliberately does not charge at all -- `authorize` -- is proved
//! by making sixty calls that would have emptied the bucket if they cost anything.
//!
//! The properties under test are the ones the crate's own documentation calls
//! load-bearing, because those are the ones that are expensive to get wrong in
//! production:
//!
//! * effective permissions are the role default plus the grant minus the deny, and
//!   the subtraction is last;
//! * a ban is read before a departure, so the person who was banned is told they
//!   were banned;
//! * nothing is sent when nothing changed -- a join into a room already joined, an
//!   unchanged settings submit, and a role set to the one already held all produce
//!   no fanout;
//! * the owner cannot be sanctioned, demoted, or overridden by anybody at any rank;
//! * a refusal names which of `NOT_A_MEMBER`, `BANNED`, `MUTED`, and
//!   `PERMISSION_DENIED` happened rather than collapsing four situations into one;
//! * a filter, a cursor, an invite code, and an approval queue this build cannot
//!   honour are refused rather than silently ignored.

use std::sync::Arc;

use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, PublicId, Random, Result, SeededRandom, Timestamp};
use migo_protocol::{
    codes, EncryptionMode, Opcode, RelationshipKind, RoomJoinRequest, RoomKind, RoomLeaveRequest,
    RoomListRequest, RoomMemberEvent, RoomRole, RoomStateEvent,
};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_rooms::fanout::{Broadcast, Fanout};
use migo_rooms::model::{
    slug_is_valid, Caller, NewRoomRequest, RoomsConfig, Sanction, Settings, TopicChange,
    BASE_ROOM_CAPACITY, MANAGED_ROOM_MAX_MEMBERS, MAX_LIST_LIMIT, MAX_MUTE_MS, MAX_NAME_LEN,
    MAX_QUERY_LEN, MAX_REASON_LEN, MAX_ROSTER_PAGE, MAX_SLOW_MODE_SECONDS, MAX_TOPIC_LEN,
    PERMANENT_BAN_MS, PUBLIC_ROOM_MAX_MEMBERS,
};
use migo_rooms::permission;
use migo_rooms::service::Rooms;
use migo_rooms::traits::Roomkeeper;
use migo_rooms::view::ONLINE_COUNT_UNSET;
use migo_store::model::{join_policy, Relationship, Room, RoomMember};
use migo_store::traits::{RoomStore, SocialStore};
use migo_store::MemoryStore;

/// One second in milliseconds.
const SECOND: i64 = 1_000;
/// One minute.
const MINUTE: i64 = 60 * SECOND;
/// One day.
const DAY: i64 = 24 * 60 * MINUTE;

/// When a fixture is built.
const NOW: i64 = 1_700_000_000 * SECOND;
/// When a test acts. A minute later, so every bucket the fixture touched is full
/// again and no test measures the setup's spending.
const LATER: i64 = NOW + MINUTE;

/// Alice, who owns the room in every fixture.
const ALICE: u128 = 1;
/// Bob, the ordinary member things are done to.
const BOB: u128 = 2;
/// Carol, the second member, for the tests that need a bystander.
const CAROL: u128 = 3;
/// Dave, who holds a role in the tests about rank.
const DAVE: u128 = 4;
/// Somebody with no membership anywhere.
const STRANGER: u128 = 9;

/// Alice's phone, which a fanout she caused excludes.
const ALICE_PHONE: u128 = 101;
/// Bob's laptop.
const BOB_LAPTOP: u128 = 102;
/// Carol's phone.
const CAROL_PHONE: u128 = 103;
/// Dave's tablet.
const DAVE_TABLET: u128 = 104;
/// The stranger's phone.
const STRANGER_PHONE: u128 = 109;

/// The slug every fixture uses.
const LOBBY: &str = "jakarta-lobby";

type TestRooms = Rooms<MemoryStore, CacheRateLimiter<MemoryCache>>;

/// A timestamp from milliseconds.
fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

/// An id from a small number, so a failure message names the fixture.
fn id(value: u128) -> Id {
    Id::from(value)
}

/// An established caller who has not proved a second factor.
fn caller(account: u128, device: u128, millis: i64) -> Caller {
    Caller::new(id(account), id(device), TrustTier::Established, ts(millis))
}

/// An established caller who proved one recently.
fn proven(account: u128, device: u128, millis: i64) -> Caller {
    caller(account, device, millis).reauthenticated()
}

/// A room request with nothing unusual about it.
fn request(slug: &str, name: &str) -> NewRoomRequest {
    NewRoomRequest {
        slug: slug.to_string(),
        name: name.to_string(),
        topic: None,
        kind: RoomKind::Public,
        max_members: None,
    }
}

/// A join with no invite code.
fn join_request(room_id: Id) -> RoomJoinRequest {
    RoomJoinRequest {
        room_id,
        invite_code: None,
    }
}

/// A listing that asks for nothing in particular.
fn list_request(limit: u32) -> RoomListRequest {
    RoomListRequest {
        limit,
        query: None,
        category: None,
        language: None,
        country: None,
        cursor: None,
    }
}

/// Everything a test needs, built the way `migod` builds it.
struct Harness {
    rooms: TestRooms,
    store: Arc<MemoryStore>,
    registry: Registry,
}

impl Harness {
    fn new() -> Self {
        Self::configured(RoomsConfig::default())
    }

    fn configured(config: RoomsConfig) -> Self {
        let settings = Config::default();
        let store = Arc::new(MemoryStore::new());
        let cache = Arc::new(MemoryCache::new());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(cache, policies, &registry));
        let rooms = Rooms::new(
            Arc::clone(&store),
            limiter,
            &registry,
            config,
            Box::new(SeededRandom::new(0x5eed_2001)) as Box<dyn Random>,
        );
        Self {
            rooms,
            store,
            registry,
        }
    }

    /// A public room owned by Alice, with no members but her.
    async fn empty_room(&self) -> Id {
        self.rooms
            .create(
                &caller(ALICE, ALICE_PHONE, NOW),
                request(LOBBY, "Jakarta Lobby"),
            )
            .await
            .expect("a well-formed room is created")
            .room_id
    }

    /// Alice's room with Bob and Carol in it.
    async fn founded(&self) -> Id {
        let room = self.empty_room().await;
        self.join(BOB, BOB_LAPTOP, room, NOW + SECOND).await;
        self.join(CAROL, CAROL_PHONE, room, NOW + 2 * SECOND).await;
        room
    }

    /// Joins, discarding the fanout, for a fixture rather than an assertion.
    async fn join(&self, account: u128, device: u128, room: Id, millis: i64) {
        self.rooms
            .join(&caller(account, device, millis), join_request(room))
            .await
            .expect("an open room admits a stranger");
    }

    /// Writes an accepted friendship, both directions, the way `accept_friend` does.
    ///
    /// The pending rows are skipped because the service reads `Friend` rows only —
    /// `count_relationships` never sees a pending edge, and neither does the capacity
    /// rule built on it.
    async fn befriend(&self, left: u128, right: u128, millis: i64) {
        for (owner, other) in [(left, right), (right, left)] {
            self.store
                .put_relationship(Relationship {
                    account_id: id(owner),
                    other_id: id(other),
                    kind: RelationshipKind::Friend,
                    created_at: ts(millis),
                    accepted_at: Some(ts(millis)),
                })
                .await
                .expect("the edge is written");
        }
    }

    /// Sets a role as Alice, who owns the room.
    async fn promote(&self, room: Id, subject: u128, role: RoomRole, millis: i64) {
        self.rooms
            .set_role(&caller(ALICE, ALICE_PHONE, millis), room, id(subject), role)
            .await
            .expect("the owner outranks everybody");
    }

    /// The stored room row.
    async fn room_row(&self, room: Id) -> Room {
        self.store
            .room(room)
            .await
            .expect("the store answers")
            .expect("the room exists")
    }

    /// The stored membership row, lapsed or banned included.
    async fn member_row(&self, room: Id, account: u128) -> RoomMember {
        self.store
            .room_member(room, id(account))
            .await
            .expect("the store answers")
            .expect("the membership row exists")
    }

    /// One counter's current value.
    fn counter(&self, name: &'static str, labels: &[(&str, &str)]) -> u64 {
        self.registry.counter(name, "", labels).get()
    }

    fn creations(&self, outcome: &str) -> u64 {
        self.counter("migo_rooms_creations_total", &[("outcome", outcome)])
    }

    fn joins(&self, outcome: &str) -> u64 {
        self.counter("migo_rooms_joins_total", &[("outcome", outcome)])
    }

    fn leaves(&self, outcome: &str) -> u64 {
        self.counter("migo_rooms_leaves_total", &[("outcome", outcome)])
    }

    fn authorizations(&self, outcome: &str) -> u64 {
        self.counter("migo_rooms_authorizations_total", &[("outcome", outcome)])
    }

    fn sanctions(&self, action: &str) -> u64 {
        self.counter("migo_rooms_sanctions_total", &[("action", action)])
    }

    fn settings_changes(&self, outcome: &str) -> u64 {
        self.counter("migo_rooms_settings_total", &[("outcome", outcome)])
    }

    fn role_changes(&self, outcome: &str) -> u64 {
        self.counter("migo_rooms_role_changes_total", &[("outcome", outcome)])
    }

    fn overrides(&self, outcome: &str) -> u64 {
        self.counter(
            "migo_rooms_permission_overrides_total",
            &[("outcome", outcome)],
        )
    }

    fn archives(&self) -> u64 {
        self.counter("migo_rooms_archives_total", &[])
    }

    fn transfers(&self) -> u64 {
        self.counter("migo_rooms_ownership_transfers_total", &[])
    }

    fn listings(&self) -> u64 {
        self.counter("migo_rooms_listings_total", &[])
    }
}

/// Asserts the failure class rather than the message, which is not a contract.
#[track_caller]
fn expect_code<T>(result: Result<T>, code: u32) {
    match result {
        Ok(_) => panic!("expected error {code}, got success"),
        Err(error) => assert_eq!(error.code(), code, "wrong failure class: {error}"),
    }
}

/// The member event a fanout carries, and whose socket it skips.
#[track_caller]
fn expect_member(fanout: Option<Fanout>) -> (Fanout, RoomMemberEvent) {
    let fanout = fanout.expect("the room should have been told");
    match &fanout.event {
        Broadcast::Member(event) => (fanout.clone(), event.clone()),
        Broadcast::State(event) => panic!("expected a member event, got {event:?}"),
    }
}

/// The state event a fanout carries.
#[track_caller]
fn expect_state(fanout: Option<Fanout>) -> (Fanout, RoomStateEvent) {
    let fanout = fanout.expect("the room should have been told");
    match &fanout.event {
        Broadcast::State(event) => (fanout.clone(), event.clone()),
        Broadcast::Member(event) => panic!("expected a state event, got {event:?}"),
    }
}

/// A string of `count` ASCII characters.
fn long(count: usize) -> String {
    "a".repeat(count)
}

// --- identity ------------------------------------------------------------------
//
// The gateway never produces a half-identified caller, so these guard against a
// future caller that is not the gateway: a nil account id would be one membership row
// shared by every anonymous request, and a nil device id would aim a fanout exclusion
// at somebody else's socket.

#[tokio::test]
async fn a_nil_account_cannot_create_a_room() {
    let harness = Harness::new();
    let caller = Caller::new(Id::NIL, id(ALICE_PHONE), TrustTier::Established, ts(NOW));
    expect_code(
        harness.rooms.create(&caller, request(LOBBY, "Lobby")).await,
        codes::UNAUTHENTICATED,
    );
    assert_eq!(harness.creations("accepted"), 0);
    assert_eq!(
        harness.creations("invalid"),
        0,
        "an unidentified caller is not a malformed request"
    );
}

#[tokio::test]
async fn a_nil_device_cannot_join() {
    let harness = Harness::new();
    let room = harness.empty_room().await;
    let caller = Caller::new(id(BOB), Id::NIL, TrustTier::Established, ts(LATER));
    expect_code(
        harness.rooms.join(&caller, join_request(room)).await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn every_method_refuses_an_unidentified_caller() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let nil = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(LATER)).reauthenticated();
    expect_code(
        harness
            .rooms
            .leave(&nil, RoomLeaveRequest { room_id: room })
            .await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.rooms.list(&nil, list_request(10)).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.rooms.summary(&nil, room).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.rooms.resolve(&nil, LOBBY).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(harness.rooms.mine(&nil).await, codes::UNAUTHENTICATED);
    expect_code(
        harness.rooms.roster(&nil, room, 10, None).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.rooms.update(&nil, room, Settings::default()).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.rooms.archive(&nil, room).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness
            .rooms
            .set_role(&nil, room, id(BOB), RoomRole::Helper)
            .await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness
            .rooms
            .set_permissions(&nil, room, id(BOB), permission::CHAT_PIN, 0)
            .await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness
            .rooms
            .sanction(&nil, room, id(BOB), Sanction::Kick)
            .await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.rooms.transfer_ownership(&nil, room, id(BOB)).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.rooms.authorize(&nil, room, 0).await,
        codes::UNAUTHENTICATED,
    );
}

// --- the slug namespace --------------------------------------------------------

#[test]
fn a_slug_is_lowercase_hyphenated_and_of_bounded_length() {
    assert!(slug_is_valid("abc"));
    assert!(slug_is_valid("jakarta-lobby"));
    assert!(slug_is_valid("room-42"));
    assert!(slug_is_valid(&long(32)));

    assert!(!slug_is_valid(""), "empty");
    assert!(!slug_is_valid("ab"), "two characters is under the floor");
    assert!(!slug_is_valid(&long(33)), "one over the ceiling");
    assert!(!slug_is_valid("Jakarta"), "uppercase");
    assert!(!slug_is_valid("jakarta lobby"), "a space");
    assert!(!slug_is_valid("jakarta_lobby"), "an underscore");
    assert!(!slug_is_valid("-jakarta"), "a leading hyphen");
    assert!(!slug_is_valid("jakarta-"), "a trailing hyphen");
    assert!(!slug_is_valid("jakarta--lobby"), "a doubled hyphen");
    assert!(!slug_is_valid("kafe-jakarta-ü"), "not ASCII");
}

#[test]
fn a_slug_may_never_be_an_id() {
    // The two namespaces must not overlap, because `resolve` tries the id form first:
    // a slug that parsed as an id would be a slug nobody could ever reach.
    let text = id(1).to_text();
    assert_eq!(text.len(), 26);
    assert!(
        !slug_is_valid(&text),
        "the canonical id text must be refused as a slug"
    );
    assert!(
        !slug_is_valid(&text.to_lowercase()),
        "and so must its lowercase form, which `Id::parse` also accepts"
    );
}

// --- creation ------------------------------------------------------------------

#[tokio::test]
async fn creating_a_room_makes_its_maker_the_owner() {
    let harness = Harness::new();
    let summary = harness
        .rooms
        .create(
            &caller(ALICE, ALICE_PHONE, NOW),
            NewRoomRequest {
                slug: LOBBY.to_string(),
                name: "  Jakarta Lobby  ".to_string(),
                topic: Some("  ngobrol santai  ".to_string()),
                kind: RoomKind::Public,
                max_members: None,
            },
        )
        .await
        .expect("a well-formed room is created");

    assert_eq!(summary.name, "Jakarta Lobby", "the name is trimmed");
    assert_eq!(summary.topic.as_deref(), Some("ngobrol santai"));
    assert_eq!(summary.my_role, Some(RoomRole::Owner));
    assert_eq!(summary.member_count, 1, "the owner is the first member");
    assert_eq!(
        summary.online_count, ONLINE_COUNT_UNSET,
        "this crate does not count who is connected"
    );
    assert_eq!(summary.slow_mode_ms, None, "slow mode starts off");
    assert_eq!(
        summary.public_id,
        summary.room_id.public_id(PublicId::Room),
        "the shareable alias is derived from the id"
    );
    assert_eq!(summary.kind, RoomKind::Public);
    assert_eq!(harness.creations("accepted"), 1);

    let room = harness.room_row(summary.room_id).await;
    assert_eq!(room.owner_id, id(ALICE));
    assert_eq!(room.home_region, "local", "stamped from the config");
    assert_eq!(room.join_policy, join_policy::OPEN);
    assert_eq!(room.encryption, EncryptionMode::Transport);
    assert!(room.archived_at.is_none());
    assert_ne!(
        room.conversation_id, room.room_id,
        "the conversation is its own object"
    );

    let owner = harness.member_row(summary.room_id, ALICE).await;
    assert_eq!(owner.role, RoomRole::Owner);
    assert!(owner.is_active());
    assert_eq!(owner.permissions_grant, 0, "the role carries the power");
    assert_eq!(owner.permissions_deny, 0);
}

#[tokio::test]
async fn an_all_whitespace_topic_becomes_no_topic() {
    let harness = Harness::new();
    let summary = harness
        .rooms
        .create(
            &caller(ALICE, ALICE_PHONE, NOW),
            NewRoomRequest {
                topic: Some("   ".to_string()),
                ..request(LOBBY, "Lobby")
            },
        )
        .await
        .expect("whitespace is not a validation failure");
    assert_eq!(
        summary.topic, None,
        "a topic-shaped gap in every client is worse than no topic"
    );
}

#[tokio::test]
async fn a_taken_slug_is_refused_by_name() {
    let harness = Harness::new();
    harness.empty_room().await;
    expect_code(
        harness
            .rooms
            .create(&caller(BOB, BOB_LAPTOP, LATER), request(LOBBY, "Another"))
            .await,
        codes::ALREADY_EXISTS,
    );
    assert_eq!(harness.creations("taken"), 1);
    assert_eq!(harness.creations("accepted"), 1);
}

#[tokio::test]
async fn creation_refuses_what_it_cannot_store() {
    let harness = Harness::new();
    let alice = caller(ALICE, ALICE_PHONE, NOW);

    expect_code(
        harness.rooms.create(&alice, request("Ab", "Lobby")).await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness.rooms.create(&alice, request(LOBBY, "   ")).await,
        codes::FIELD_REQUIRED,
    );
    expect_code(
        harness
            .rooms
            .create(&alice, request(LOBBY, &long(MAX_NAME_LEN + 1)))
            .await,
        codes::FIELD_TOO_LONG,
    );
    expect_code(
        harness
            .rooms
            .create(
                &alice,
                NewRoomRequest {
                    topic: Some(long(MAX_TOPIC_LEN + 1)),
                    ..request(LOBBY, "Lobby")
                },
            )
            .await,
        codes::FIELD_TOO_LONG,
    );
    expect_code(
        harness
            .rooms
            .create(
                &alice,
                NewRoomRequest {
                    kind: RoomKind::Unknown,
                    ..request(LOBBY, "Lobby")
                },
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness
            .rooms
            .create(
                &alice,
                NewRoomRequest {
                    max_members: Some(1),
                    ..request(LOBBY, "Lobby")
                },
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness
            .rooms
            .create(
                &alice,
                NewRoomRequest {
                    max_members: Some(1_000_000),
                    ..request(LOBBY, "Lobby")
                },
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(harness.creations("invalid"), 7);
    assert_eq!(harness.creations("accepted"), 0);
}

#[tokio::test]
async fn a_name_is_measured_in_characters_and_not_bytes() {
    let harness = Harness::new();
    // Sixty-four characters of three bytes each. A byte limit would give an
    // Indonesian or Arabic name a third of the room an English one gets.
    let name: String = "あ".repeat(MAX_NAME_LEN);
    assert!(
        name.len() > MAX_NAME_LEN,
        "the byte length is over the limit"
    );
    let summary = harness
        .rooms
        .create(
            &caller(ALICE, ALICE_PHONE, NOW),
            NewRoomRequest {
                name: name.clone(),
                ..request(LOBBY, "unused")
            },
        )
        .await
        .expect("sixty-four characters is sixty-four characters");
    assert_eq!(summary.name, name);
}

#[tokio::test]
async fn a_malformed_creation_is_never_charged_for() {
    // A client bug must not exhaust its user's allowance and take their working
    // devices down with it. Four creations fit in one burst, so a hundred refused ones
    // followed by a successful one proves nothing was spent.
    let harness = Harness::new();
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    for _ in 0..100 {
        expect_code(
            harness.rooms.create(&alice, request("no", "Lobby")).await,
            codes::VALIDATION_FAILED,
        );
    }
    harness
        .rooms
        .create(&alice, request(LOBBY, "Lobby"))
        .await
        .expect("the budget was never touched");
    assert_eq!(harness.creations("rate_limited"), 0);
}

#[tokio::test]
async fn creation_runs_out_of_budget_eventually() {
    // The other half of the previous test: a well-formed request does cost something.
    let harness = Harness::new();
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    let mut made = 0;
    for index in 0..20 {
        let slug = format!("room-{index:02}");
        if harness
            .rooms
            .create(&alice, request(&slug, "Room"))
            .await
            .is_ok()
        {
            made += 1;
        } else {
            break;
        }
    }
    assert_eq!(made, 4, "a burst of two hundred pays for four rooms");
    assert_eq!(harness.creations("rate_limited"), 1);
}

#[tokio::test]
async fn a_strangers_room_is_small() {
    let harness = Harness::new();
    let alice = caller(ALICE, ALICE_PHONE, NOW);
    // Nobody vouches for Alice: the base capacity, and not a seat more.
    let summary = harness
        .rooms
        .create(&alice, request(LOBBY, "Lobby"))
        .await
        .expect("a stranger may still found a room");
    let room = harness.room_row(summary.room_id).await;
    assert_eq!(room.max_members, BASE_ROOM_CAPACITY);
    assert_eq!(room.home_region, "local");
    expect_code(
        harness
            .rooms
            .create(
                &alice,
                NewRoomRequest {
                    max_members: Some(BASE_ROOM_CAPACITY + 1),
                    slug: "second-room".to_string(),
                    ..request(LOBBY, "Lobby")
                },
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(harness.creations("invalid"), 1);
}

#[tokio::test]
async fn capacity_grows_ten_seats_per_friend() {
    let harness = Harness::new();
    let alice = caller(ALICE, ALICE_PHONE, NOW);
    harness.befriend(ALICE, BOB, NOW).await;
    harness.befriend(ALICE, CAROL, NOW).await;
    // 5 + 10 × 2 = 25 seats, and an explicit ask may claim all of them.
    let summary = harness
        .rooms
        .create(
            &alice,
            NewRoomRequest {
                max_members: Some(25),
                ..request(LOBBY, "Lobby")
            },
        )
        .await
        .expect("two friendships earn twenty-five seats");
    assert_eq!(
        harness.room_row(summary.room_id).await.max_members,
        25,
        "an explicit capacity within the allowance is honoured"
    );
    expect_code(
        harness
            .rooms
            .create(
                &alice,
                NewRoomRequest {
                    max_members: Some(26),
                    slug: "greedy-room".to_string(),
                    ..request(LOBBY, "Lobby")
                },
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    // A pending request is not a friendship: it earns nothing. Dave's outgoing request
    // adds a row Alice's friend count never reads.
    harness
        .store
        .put_relationship(Relationship {
            account_id: id(DAVE),
            other_id: id(ALICE),
            kind: RelationshipKind::PendingOutgoing,
            created_at: ts(NOW),
            accepted_at: None,
        })
        .await
        .expect("the request is written");
    let summary = harness
        .rooms
        .create(
            &alice,
            NewRoomRequest {
                slug: "third-room".to_string(),
                ..request(LOBBY, "Lobby")
            },
        )
        .await
        .expect("the request changes nothing about the allowance");
    assert_eq!(
        harness.room_row(summary.room_id).await.max_members,
        25,
        "a pending request earns no seats"
    );
}

#[tokio::test]
async fn the_kind_bounds_the_ceiling() {
    let harness = Harness::new();
    let alice = caller(ALICE, ALICE_PHONE, NOW);
    // Five friendships would earn 55 seats; the kinds stop earlier.
    for other in [BOB, CAROL, DAVE, STRANGER, 12] {
        harness.befriend(ALICE, other, NOW).await;
    }
    let public = harness
        .rooms
        .create(&alice, request(LOBBY, "Lobby"))
        .await
        .expect("the public room is created");
    assert_eq!(
        harness.room_row(public.room_id).await.max_members,
        PUBLIC_ROOM_MAX_MEMBERS,
        "a public room stops at its own ceiling"
    );
    let managed = harness
        .rooms
        .create(
            &alice,
            NewRoomRequest {
                kind: RoomKind::Managed,
                slug: "managed-hall".to_string(),
                ..request(LOBBY, "Lobby")
            },
        )
        .await
        .expect("the managed room is created");
    assert_eq!(
        harness.room_row(managed.room_id).await.max_members,
        MANAGED_ROOM_MAX_MEMBERS,
        "a managed room stops at its own, larger ceiling"
    );
}

// --- joining ---------------------------------------------------------------------

#[tokio::test]
async fn joining_an_open_room_admits_the_caller_and_tells_the_room() {
    let harness = Harness::new();
    let room = harness.empty_room().await;
    let (response, fanout) = harness
        .rooms
        .join(&caller(BOB, BOB_LAPTOP, LATER), join_request(room))
        .await
        .expect("an open room admits a stranger");

    assert_eq!(response.room.room_id, room);
    assert_eq!(response.room.my_role, Some(RoomRole::Member));
    assert_eq!(
        response.room.member_count, 2,
        "the count is re-read after the join, not guessed before it"
    );
    assert_eq!(response.encryption, EncryptionMode::Transport);
    assert_eq!(
        response.last_seq, 0,
        "a room with no messages starts at zero"
    );
    assert_eq!(
        response.conversation_id,
        harness.room_row(room).await.conversation_id
    );

    let (fanout, event) = expect_member(fanout);
    assert_eq!(fanout.room_id, room);
    assert_eq!(
        fanout.exclude_device,
        Some(id(BOB_LAPTOP)),
        "the joiner's own socket already has the answer"
    );
    assert_eq!(fanout.opcode(), Opcode::RoomMemberEvent);
    assert_eq!(event.user_id, id(BOB));
    assert!(event.joined);
    assert_eq!(event.role, Some(RoomRole::Member));
    assert_eq!(event.member_count, Some(2));
    assert_eq!(harness.joins("accepted"), 1);
}

#[tokio::test]
async fn joining_a_room_already_joined_says_nothing_to_the_room() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let (response, fanout) = harness
        .rooms
        .join(&caller(BOB, BOB_LAPTOP, LATER), join_request(room))
        .await
        .expect("a second join is not an error");
    assert_eq!(response.room.member_count, 3, "nobody was added twice");
    assert!(
        fanout.is_none(),
        "brief section 156: nothing is sent when nothing changed"
    );
    assert_eq!(harness.joins("already"), 1);
}

#[tokio::test]
async fn a_rejoin_keeps_the_role_and_the_mute_it_left_with() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness.promote(room, BOB, RoomRole::Moderator, LATER).await;
    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Mute {
                duration_ms: 10 * MINUTE,
                reason: Some("calm down".to_string()),
            },
        )
        .await
        .expect("a moderator may be muted by the owner");
    harness
        .rooms
        .leave(
            &caller(BOB, BOB_LAPTOP, LATER),
            RoomLeaveRequest { room_id: room },
        )
        .await
        .expect("a moderator may walk out");

    let (response, fanout) = harness
        .rooms
        .join(&caller(BOB, BOB_LAPTOP, LATER + MINUTE), join_request(room))
        .await
        .expect("leaving does not bar a return");
    assert_eq!(
        response.room.my_role,
        Some(RoomRole::Moderator),
        "a fresh row would silently demote somebody who stepped out for an hour"
    );
    let (_, event) = expect_member(fanout);
    assert_eq!(event.role, Some(RoomRole::Moderator));
    assert_eq!(harness.joins("rejoined"), 1);

    let member = harness.member_row(room, BOB).await;
    assert!(
        member.is_muted(ts(LATER + MINUTE)),
        "and would clear the mute they were under"
    );
}

#[tokio::test]
async fn a_ban_survives_leaving_and_rejoining() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Ban {
                duration_ms: Some(DAY),
                reason: Some("spam".to_string()),
            },
        )
        .await
        .expect("the owner may ban a member");
    // The store's own `join_room` does not check bans, so this is the service's job.
    expect_code(
        harness
            .rooms
            .join(&caller(BOB, BOB_LAPTOP, LATER + MINUTE), join_request(room))
            .await,
        codes::BANNED,
    );
    assert_eq!(harness.joins("banned"), 1);
    // And it lapses on its own once the clock passes the expiry.
    harness
        .rooms
        .join(
            &caller(BOB, BOB_LAPTOP, LATER + 2 * DAY),
            join_request(room),
        )
        .await
        .expect("a timed ban expires without anybody lifting it");
}

#[tokio::test]
async fn an_invite_code_is_refused_rather_than_ignored() {
    let harness = Harness::new();
    let room = harness.empty_room().await;
    expect_code(
        harness
            .rooms
            .join(
                &caller(BOB, BOB_LAPTOP, LATER),
                RoomJoinRequest {
                    room_id: room,
                    invite_code: Some("MIGO-1234".to_string()),
                },
            )
            .await,
        codes::FEATURE_DISABLED,
    );
    assert_eq!(harness.joins("not_admitted"), 1);
    assert_eq!(
        harness.joins("accepted"),
        0,
        "admitting the holder of a string nobody read is worse than saying the feature is off"
    );
}

#[tokio::test]
async fn an_approval_queue_is_refused_and_invitation_only_is_denied() {
    let harness = Harness::new();
    let room = harness.empty_room().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);

    harness
        .rooms
        .update(
            &alice,
            room,
            Settings {
                join_policy: Some(join_policy::APPROVAL),
                ..Settings::default()
            },
        )
        .await
        .expect("the owner may set the policy");
    expect_code(
        harness
            .rooms
            .join(&caller(BOB, BOB_LAPTOP, LATER), join_request(room))
            .await,
        codes::FEATURE_DISABLED,
    );

    harness
        .rooms
        .update(
            &alice,
            room,
            Settings {
                join_policy: Some(join_policy::INVITE),
                ..Settings::default()
            },
        )
        .await
        .expect("the owner may set the policy");
    expect_code(
        harness
            .rooms
            .join(&caller(CAROL, CAROL_PHONE, LATER), join_request(room))
            .await,
        // Not a missing feature: invitation-only without invitations is the policy
        // working exactly as configured.
        codes::PERMISSION_DENIED,
    );
    assert_eq!(harness.joins("not_admitted"), 2);
}

#[tokio::test]
async fn a_policy_change_does_not_evict_the_people_already_in() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .update(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            Settings {
                join_policy: Some(join_policy::INVITE),
                ..Settings::default()
            },
        )
        .await
        .expect("the owner may close the room");
    let (_, fanout) = harness
        .rooms
        .join(&caller(BOB, BOB_LAPTOP, LATER), join_request(room))
        .await
        .expect("the policy is only consulted for somebody not already in");
    assert!(fanout.is_none());
}

#[tokio::test]
async fn a_full_room_refuses_the_next_arrival() {
    let harness = Harness::new();
    let room = harness
        .rooms
        .create(
            &caller(ALICE, ALICE_PHONE, NOW),
            NewRoomRequest {
                max_members: Some(2),
                ..request(LOBBY, "Two Seats")
            },
        )
        .await
        .expect("two is the floor, not below it")
        .room_id;
    harness.join(BOB, BOB_LAPTOP, room, LATER).await;
    expect_code(
        harness
            .rooms
            .join(&caller(CAROL, CAROL_PHONE, LATER), join_request(room))
            .await,
        codes::ROOM_FULL,
    );
    assert_eq!(harness.joins("full"), 1);
    assert_eq!(harness.room_row(room).await.member_count, 2);
}

#[tokio::test]
async fn a_missing_or_nil_room_is_refused_before_anything_else() {
    let harness = Harness::new();
    expect_code(
        harness
            .rooms
            .join(&caller(BOB, BOB_LAPTOP, LATER), join_request(Id::NIL))
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness
            .rooms
            .join(&caller(BOB, BOB_LAPTOP, LATER), join_request(id(777)))
            .await,
        codes::NOT_FOUND,
    );
    assert_eq!(harness.joins("not_found"), 2);
}

#[tokio::test]
async fn an_archived_room_takes_no_more_members() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .archive(&caller(ALICE, ALICE_PHONE, LATER), room)
        .await
        .expect("the owner may archive");
    expect_code(
        harness
            .rooms
            .join(&caller(DAVE, DAVE_TABLET, LATER), join_request(room))
            .await,
        codes::ROOM_ARCHIVED,
    );
    assert_eq!(harness.joins("archived"), 1);
}

#[tokio::test]
async fn joining_runs_out_of_budget_on_the_joiner_and_not_the_room() {
    // A shared room bucket would be a denial of service with a two-line script: join
    // and leave a popular room until nobody else can get in.
    let harness = Harness::new();
    let room = harness.empty_room().await;
    let bob = caller(BOB, BOB_LAPTOP, LATER);
    let mut attempts = 0;
    loop {
        let joined = harness.rooms.join(&bob, join_request(room)).await.is_ok();
        if !joined {
            break;
        }
        attempts += 1;
        harness
            .rooms
            .leave(&bob, RoomLeaveRequest { room_id: room })
            .await
            .expect("leaving works until the leave budget runs out too");
        assert!(attempts < 20, "the budget must be finite");
    }
    assert!(attempts > 0, "the first join must have succeeded");
    assert_eq!(harness.joins("rate_limited"), 1);
    // Carol, who has spent nothing, is unaffected.
    harness
        .rooms
        .join(&caller(CAROL, CAROL_PHONE, LATER), join_request(room))
        .await
        .expect("one loud joiner must not close the door on everybody else");
}

// --- leaving ---------------------------------------------------------------------

#[tokio::test]
async fn leaving_removes_the_member_and_tells_the_room() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let fanout = harness
        .rooms
        .leave(
            &caller(BOB, BOB_LAPTOP, LATER),
            RoomLeaveRequest { room_id: room },
        )
        .await
        .expect("a member may walk out");
    let (fanout, event) = expect_member(fanout);
    assert_eq!(fanout.exclude_device, Some(id(BOB_LAPTOP)));
    assert_eq!(event.user_id, id(BOB));
    assert!(!event.joined);
    assert_eq!(event.role, None, "a departure carries no role");
    assert_eq!(event.member_count, Some(2));
    assert_eq!(harness.leaves("applied"), 1);

    let member = harness.member_row(room, BOB).await;
    assert!(!member.is_active(), "the row is kept and marked");
    assert_eq!(
        member.role,
        RoomRole::Member,
        "the row is kept so a rejoin can find it"
    );
}

#[tokio::test]
async fn leaving_a_room_never_joined_says_nothing() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let fanout = harness
        .rooms
        .leave(
            &caller(STRANGER, STRANGER_PHONE, LATER),
            RoomLeaveRequest { room_id: room },
        )
        .await
        .expect("a stranger leaving is not an error worth showing anybody");
    assert!(fanout.is_none());

    // And neither does leaving twice.
    let bob = caller(BOB, BOB_LAPTOP, LATER);
    harness
        .rooms
        .leave(&bob, RoomLeaveRequest { room_id: room })
        .await
        .expect("the first leave applies");
    let second = harness
        .rooms
        .leave(&bob, RoomLeaveRequest { room_id: room })
        .await
        .expect("the second is a no-op");
    assert!(second.is_none());
    assert_eq!(harness.leaves("unchanged"), 2);
    assert_eq!(harness.leaves("applied"), 1);
}

#[tokio::test]
async fn the_owner_cannot_walk_out() {
    // A room whose owner is gone has nobody who can transfer it, archive it, or
    // appoint a Manager -- and promoting somebody automatically hands a community to
    // whoever happens to be next in the roster.
    let harness = Harness::new();
    let room = harness.founded().await;
    expect_code(
        harness
            .rooms
            .leave(
                &caller(ALICE, ALICE_PHONE, LATER),
                RoomLeaveRequest { room_id: room },
            )
            .await,
        codes::CONFLICT,
    );
    assert_eq!(harness.leaves("denied"), 1);
    assert!(harness.member_row(room, ALICE).await.is_active());
}

#[tokio::test]
async fn leaving_refuses_a_nil_or_missing_room() {
    let harness = Harness::new();
    expect_code(
        harness
            .rooms
            .leave(
                &caller(BOB, BOB_LAPTOP, LATER),
                RoomLeaveRequest { room_id: Id::NIL },
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness
            .rooms
            .leave(
                &caller(BOB, BOB_LAPTOP, LATER),
                RoomLeaveRequest { room_id: id(777) },
            )
            .await,
        codes::NOT_FOUND,
    );
}

// --- browsing and searching ------------------------------------------------------

/// Four rooms with distinct member counts, creation times, and names.
///
/// Created a second apart so the browse ordering is decided by the data and not by a
/// tie broken on a random id, and so each creation's cost has refilled by the next.
async fn browsable(harness: &Harness) -> [Id; 4] {
    let mut rooms = [Id::NIL; 4];
    for (index, (slug, name)) in [
        ("warung-kopi", "Warung Kopi"),
        ("kafe-jakarta", "Kafe Jakarta"),
        ("kelas-rust", "Kelas Rust"),
        ("arsip-lama", "Arsip Lama"),
    ]
    .into_iter()
    .enumerate()
    {
        rooms[index] = harness
            .rooms
            .create(
                &caller(ALICE, ALICE_PHONE, NOW + index as i64 * SECOND),
                request(slug, name),
            )
            .await
            .expect("one creation per second stays inside the budget")
            .room_id;
    }
    harness
        .join(BOB, BOB_LAPTOP, rooms[0], NOW + 10 * SECOND)
        .await;
    harness
        .join(CAROL, CAROL_PHONE, rooms[0], NOW + 11 * SECOND)
        .await;
    harness
        .join(BOB, BOB_LAPTOP, rooms[1], NOW + 12 * SECOND)
        .await;
    harness
        .rooms
        .archive(&caller(ALICE, ALICE_PHONE, NOW + 13 * SECOND), rooms[3])
        .await
        .expect("the owner may archive");
    rooms
}

#[tokio::test]
async fn a_listing_ranks_by_member_count_and_hides_archived_rooms() {
    let harness = Harness::new();
    let rooms = browsable(&harness).await;
    let response = harness
        .rooms
        .list(&caller(STRANGER, STRANGER_PHONE, LATER), list_request(10))
        .await
        .expect("anybody may browse");

    let ids: Vec<Id> = response.rooms.iter().map(|room| room.room_id).collect();
    assert_eq!(ids, vec![rooms[0], rooms[1], rooms[2]]);
    assert_eq!(
        response.rooms[0].member_count, 3,
        "the busiest room comes first"
    );
    assert!(
        !ids.contains(&rooms[3]),
        "an archived room is not on offer to join"
    );
    assert_eq!(
        response.next_cursor, None,
        "there is no stable page token over a ranking that moves"
    );
    assert!(
        response.rooms.iter().all(|room| room.my_role.is_none()),
        "nobody looked up a membership for a browse row"
    );
    assert_eq!(harness.listings(), 1);
}

#[tokio::test]
async fn a_listing_reports_no_role_even_for_a_member() {
    let harness = Harness::new();
    let rooms = browsable(&harness).await;
    let response = harness
        .rooms
        .list(&caller(ALICE, ALICE_PHONE, LATER), list_request(10))
        .await
        .expect("the owner may browse too");
    assert!(response.rooms.iter().any(|room| room.room_id == rooms[0]));
    assert!(
        response.rooms.iter().all(|room| room.my_role.is_none()),
        "a listing that guessed would claim a role the caller does not have"
    );
}

#[tokio::test]
async fn a_search_matches_the_name_or_the_slug_case_insensitively() {
    let harness = Harness::new();
    let rooms = browsable(&harness).await;
    let stranger = caller(STRANGER, STRANGER_PHONE, LATER);

    for (query, expected) in [
        ("kopi", rooms[0]),
        ("WARUNG", rooms[0]),
        ("jakarta", rooms[1]),
        ("kelas-rust", rooms[2]),
    ] {
        let response = harness
            .rooms
            .list(
                &stranger,
                RoomListRequest {
                    query: Some(query.to_string()),
                    ..list_request(10)
                },
            )
            .await
            .expect("a search is a browse with a filter");
        let ids: Vec<Id> = response.rooms.iter().map(|room| room.room_id).collect();
        assert_eq!(ids, vec![expected], "searching for {query}");
    }

    let empty = harness
        .rooms
        .list(
            &stranger,
            RoomListRequest {
                query: Some("tidak-ada".to_string()),
                ..list_request(10)
            },
        )
        .await
        .expect("a miss is an honest short answer");
    assert!(empty.rooms.is_empty());
}

#[tokio::test]
async fn a_blank_search_is_a_browse() {
    let harness = Harness::new();
    let rooms = browsable(&harness).await;
    let response = harness
        .rooms
        .list(
            &caller(STRANGER, STRANGER_PHONE, LATER),
            RoomListRequest {
                query: Some("   ".to_string()),
                ..list_request(10)
            },
        )
        .await
        .expect("whitespace filters nothing");
    assert_eq!(response.rooms.len(), 3, "all three live rooms: {rooms:?}");
}

#[tokio::test]
async fn a_listing_clamps_its_own_page_size() {
    let harness = Harness::new();
    browsable(&harness).await;
    let stranger = caller(STRANGER, STRANGER_PHONE, LATER);

    let one = harness
        .rooms
        .list(&stranger, list_request(1))
        .await
        .expect("a page of one is honoured");
    assert_eq!(one.rooms.len(), 1);

    let defaulted = harness
        .rooms
        .list(&stranger, list_request(0))
        .await
        .expect("zero means the default");
    assert_eq!(defaulted.rooms.len(), 3, "not zero rows");

    let huge = harness
        .rooms
        .list(&stranger, list_request(MAX_LIST_LIMIT + 10_000))
        .await
        .expect("an absurd page size is clamped, not refused");
    assert_eq!(huge.rooms.len(), 3);
}

#[tokio::test]
async fn a_filter_this_build_cannot_apply_is_refused() {
    // Silently returning unfiltered content under a filtered heading would keep
    // happening, because nothing in the response says the filter was dropped.
    let harness = Harness::new();
    let stranger = caller(STRANGER, STRANGER_PHONE, LATER);
    for request in [
        RoomListRequest {
            category: Some("music".to_string()),
            ..list_request(10)
        },
        RoomListRequest {
            language: Some("id".to_string()),
            ..list_request(10)
        },
        RoomListRequest {
            country: Some("ID".to_string()),
            ..list_request(10)
        },
        RoomListRequest {
            cursor: Some("page-2".to_string()),
            ..list_request(10)
        },
    ] {
        expect_code(
            harness.rooms.list(&stranger, request).await,
            codes::FEATURE_DISABLED,
        );
    }
    assert_eq!(
        harness.listings(),
        0,
        "a refused listing is not a listing served"
    );
}

#[tokio::test]
async fn an_overlong_query_is_refused_by_length() {
    let harness = Harness::new();
    expect_code(
        harness
            .rooms
            .list(
                &caller(STRANGER, STRANGER_PHONE, LATER),
                RoomListRequest {
                    query: Some(long(MAX_QUERY_LEN + 1)),
                    ..list_request(10)
                },
            )
            .await,
        codes::FIELD_TOO_LONG,
    );
    harness
        .rooms
        .list(
            &caller(STRANGER, STRANGER_PHONE, LATER),
            RoomListRequest {
                query: Some(long(MAX_QUERY_LEN)),
                ..list_request(10)
            },
        )
        .await
        .expect("exactly at the limit is inside it");
}

// --- reading one room ------------------------------------------------------------

#[tokio::test]
async fn a_summary_reports_the_callers_own_role() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let owner = harness
        .rooms
        .summary(&caller(ALICE, ALICE_PHONE, LATER), room)
        .await
        .expect("the owner may read the room");
    assert_eq!(owner.my_role, Some(RoomRole::Owner));

    let member = harness
        .rooms
        .summary(&caller(BOB, BOB_LAPTOP, LATER), room)
        .await
        .expect("a member may read the room");
    assert_eq!(member.my_role, Some(RoomRole::Member));

    let stranger = harness
        .rooms
        .summary(&caller(STRANGER, STRANGER_PHONE, LATER), room)
        .await
        .expect("a public room is readable by anybody holding its id");
    assert_eq!(stranger.my_role, None);
    assert_eq!(stranger.member_count, 3);
}

#[tokio::test]
async fn a_summary_hides_the_role_of_somebody_who_left_or_was_banned() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .leave(
            &caller(BOB, BOB_LAPTOP, LATER),
            RoomLeaveRequest { room_id: room },
        )
        .await
        .expect("a member may walk out");
    let left = harness
        .rooms
        .summary(&caller(BOB, BOB_LAPTOP, LATER), room)
        .await
        .expect("the room is still readable");
    assert_eq!(left.my_role, None);

    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(CAROL),
            Sanction::Ban {
                duration_ms: None,
                reason: None,
            },
        )
        .await
        .expect("the owner may ban");
    let banned = harness
        .rooms
        .summary(&caller(CAROL, CAROL_PHONE, LATER), room)
        .await
        .expect("the room is still readable");
    assert_eq!(
        banned.my_role, None,
        "telling a banned member their old role would say the ban had not happened"
    );
}

#[tokio::test]
async fn an_archived_room_still_resolves() {
    // Brief section 85 archives rather than deletes precisely so links keep working.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .archive(&caller(ALICE, ALICE_PHONE, LATER), room)
        .await
        .expect("the owner may archive");
    let summary = harness
        .rooms
        .summary(&caller(STRANGER, STRANGER_PHONE, LATER), room)
        .await
        .expect("an archived room is still a room");
    assert_eq!(summary.room_id, room);
    harness
        .rooms
        .resolve(&caller(STRANGER, STRANGER_PHONE, LATER), LOBBY)
        .await
        .expect("and its slug still points at it");
}

#[tokio::test]
async fn resolve_accepts_a_slug_and_the_canonical_id_text() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let bob = caller(BOB, BOB_LAPTOP, LATER);

    let by_slug = harness
        .rooms
        .resolve(&bob, LOBBY)
        .await
        .expect("a slug names a room");
    assert_eq!(by_slug.room_id, room);
    assert_eq!(by_slug.my_role, Some(RoomRole::Member));

    let by_id = harness
        .rooms
        .resolve(&bob, &room.to_text())
        .await
        .expect("so does the id");
    assert_eq!(by_id.room_id, room);

    let padded = harness
        .rooms
        .resolve(&bob, &format!("  {LOBBY}  "))
        .await
        .expect("a pasted link carries whitespace");
    assert_eq!(padded.room_id, room);

    let lowercased = harness
        .rooms
        .resolve(&bob, &room.to_text().to_lowercase())
        .await
        .expect("`Id::parse` accepts both cases");
    assert_eq!(lowercased.room_id, room);
}

#[tokio::test]
async fn resolve_answers_not_found_rather_than_naming_the_shape_of_the_input() {
    // A deep link that names no room is indistinguishable, from outside, from one
    // naming a room the caller may not see. Two different answers would make the
    // endpoint a slug oracle.
    let harness = Harness::new();
    let room = harness.founded().await;
    let bob = caller(BOB, BOB_LAPTOP, LATER);
    for reference in [
        "",
        "no",
        "Jakarta Lobby",
        "--broken--",
        "tidak-pernah-ada",
        &id(999).to_text(),
        &room.public_id(PublicId::Room),
    ] {
        expect_code(
            harness.rooms.resolve(&bob, reference).await,
            codes::NOT_FOUND,
        );
    }
}

#[tokio::test]
async fn the_shareable_alias_is_not_a_lookup_key() {
    // `MGO-ROOM-XXXXXX` is six hex digits of the id and cannot be reversed, so it is
    // for display and support tickets, not for resolution.
    let harness = Harness::new();
    let room = harness.founded().await;
    let alias = room.public_id(PublicId::Room);
    assert!(alias.starts_with("MGO-ROOM-"));
    assert!(Id::parse(&alias).is_err(), "it is not an id");
    assert!(!slug_is_valid(&alias), "and it is not a slug");
}

#[tokio::test]
async fn mine_lists_the_rooms_the_caller_is_in_with_the_role_held() {
    let harness = Harness::new();
    let first = harness.founded().await;
    let second = harness
        .rooms
        .create(
            &caller(ALICE, ALICE_PHONE, NOW + SECOND),
            request("kelas-rust", "Kelas Rust"),
        )
        .await
        .expect("one creation per second stays inside the budget")
        .room_id;
    harness
        .join(BOB, BOB_LAPTOP, second, NOW + 2 * SECOND)
        .await;
    harness
        .promote(second, BOB, RoomRole::Moderator, LATER)
        .await;

    let mine = harness
        .rooms
        .mine(&caller(BOB, BOB_LAPTOP, LATER))
        .await
        .expect("a member may list their own rooms");
    assert_eq!(mine.len(), 2);
    let roles: Vec<(Id, Option<RoomRole>)> = mine
        .iter()
        .map(|room| (room.room_id, room.my_role))
        .collect();
    assert!(roles.contains(&(first, Some(RoomRole::Member))));
    assert!(
        roles.contains(&(second, Some(RoomRole::Moderator))),
        "the screen that exists to show what you are must show it"
    );

    let stranger = harness
        .rooms
        .mine(&caller(STRANGER, STRANGER_PHONE, LATER))
        .await
        .expect("a stranger has an empty list, not an error");
    assert!(stranger.is_empty());
}

// --- the roster ------------------------------------------------------------------

#[tokio::test]
async fn a_roster_is_ranked_and_members_only() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness.promote(room, BOB, RoomRole::Moderator, LATER).await;
    let members = harness
        .rooms
        .roster(&caller(CAROL, CAROL_PHONE, LATER), room, 10, None)
        .await
        .expect("every member may see who is here");
    let order: Vec<(Id, RoomRole)> = members
        .iter()
        .map(|member| (member.account_id, member.role))
        .collect();
    assert_eq!(
        order,
        vec![
            (id(ALICE), RoomRole::Owner),
            (id(BOB), RoomRole::Moderator),
            (id(CAROL), RoomRole::Member),
        ],
        "highest role first"
    );

    expect_code(
        harness
            .rooms
            .roster(&caller(STRANGER, STRANGER_PHONE, LATER), room, 10, None)
            .await,
        // There is no permission bit for "may see who is here" because every member
        // may -- and handing the list to anybody holding a room id would make every
        // public room a directory of the people in it.
        codes::NOT_A_MEMBER,
    );
}

#[tokio::test]
async fn a_roster_hides_the_people_who_left() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .leave(
            &caller(BOB, BOB_LAPTOP, LATER),
            RoomLeaveRequest { room_id: room },
        )
        .await
        .expect("a member may walk out");
    let members = harness
        .rooms
        .roster(&caller(CAROL, CAROL_PHONE, LATER), room, 10, None)
        .await
        .expect("a member may read the roster");
    let ids: Vec<Id> = members.iter().map(|member| member.account_id).collect();
    assert_eq!(ids, vec![id(ALICE), id(CAROL)]);
}

#[tokio::test]
async fn a_roster_clamps_its_page_size_upward_and_downward() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let carol = caller(CAROL, CAROL_PHONE, LATER);
    let zero = harness
        .rooms
        .roster(&carol, room, 0, None)
        .await
        .expect("zero is clamped to one, not to an empty page");
    assert_eq!(zero.len(), 1);
    let huge = harness
        .rooms
        .roster(&carol, room, MAX_ROSTER_PAGE + 1, None)
        .await
        .expect("an absurd page size is clamped");
    assert_eq!(huge.len(), 3);
}

#[tokio::test]
async fn a_banned_member_cannot_read_the_roster() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Ban {
                duration_ms: None,
                reason: None,
            },
        )
        .await
        .expect("the owner may ban");
    expect_code(
        harness
            .rooms
            .roster(&caller(BOB, BOB_LAPTOP, LATER), room, 10, None)
            .await,
        codes::BANNED,
    );
}

// --- settings --------------------------------------------------------------------

#[tokio::test]
async fn a_rename_applies_without_a_frame() {
    // `RoomStateEvent` carries a count, a topic, and an interval, and nothing else, so
    // a renamed room is learned from the next summary. Inventing a field for it is a
    // schema change, and a domain crate is not where the packet registry gets edited.
    let harness = Harness::new();
    let room = harness.founded().await;
    let (summary, fanout) = harness
        .rooms
        .update(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            Settings {
                name: Some("  Ruang Jakarta  ".to_string()),
                ..Settings::default()
            },
        )
        .await
        .expect("the owner may rename the room");
    assert_eq!(summary.name, "Ruang Jakarta", "trimmed on the way in");
    assert!(fanout.is_none());
    assert_eq!(harness.settings_changes("applied"), 1);
    assert_eq!(harness.room_row(room).await.name, "Ruang Jakarta");
}

#[tokio::test]
async fn a_topic_change_is_broadcast_and_a_removal_is_an_empty_string() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);

    let (summary, fanout) = harness
        .rooms
        .update(
            &alice,
            room,
            Settings {
                topic: TopicChange::Set("  diskusi rust  ".to_string()),
                ..Settings::default()
            },
        )
        .await
        .expect("the owner may set the topic");
    assert_eq!(summary.topic.as_deref(), Some("diskusi rust"));
    let (fanout, event) = expect_state(fanout);
    assert_eq!(fanout.exclude_device, Some(id(ALICE_PHONE)));
    assert_eq!(fanout.opcode(), Opcode::RoomStateEvent);
    assert_eq!(event.room_id, room);
    assert_eq!(event.topic.as_deref(), Some("diskusi rust"));
    assert_eq!(event.slow_mode_ms, None, "nothing else moved");
    assert_eq!(event.member_count, None);
    assert_eq!(event.online_count, None);

    let (summary, fanout) = harness
        .rooms
        .update(
            &alice,
            room,
            Settings {
                topic: TopicChange::Clear,
                ..Settings::default()
            },
        )
        .await
        .expect("the owner may clear the topic");
    assert_eq!(summary.topic, None);
    let (_, event) = expect_state(fanout);
    assert_eq!(
        event.topic.as_deref(),
        Some(""),
        "`None` on this field already means unchanged, so a removal has to say something"
    );
}

#[tokio::test]
async fn an_all_whitespace_topic_submit_is_a_removal() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    harness
        .rooms
        .update(
            &alice,
            room,
            Settings {
                topic: TopicChange::Set("ngobrol".to_string()),
                ..Settings::default()
            },
        )
        .await
        .expect("the owner may set the topic");
    let (summary, fanout) = harness
        .rooms
        .update(
            &alice,
            room,
            Settings {
                topic: TopicChange::Set("   ".to_string()),
                ..Settings::default()
            },
        )
        .await
        .expect("whitespace is a removal, matching what creation does with one");
    assert_eq!(summary.topic, None);
    let (_, event) = expect_state(fanout);
    assert_eq!(event.topic.as_deref(), Some(""));
}

#[tokio::test]
async fn slow_mode_is_broadcast_in_milliseconds_and_zero_when_turned_off() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);

    let (summary, fanout) = harness
        .rooms
        .update(
            &alice,
            room,
            Settings {
                slow_mode_seconds: Some(30),
                ..Settings::default()
            },
        )
        .await
        .expect("the owner may slow the room down");
    assert_eq!(summary.slow_mode_ms, Some(30_000));
    let (_, event) = expect_state(fanout);
    assert_eq!(event.slow_mode_ms, Some(30_000));
    assert_eq!(event.topic, None);

    let (summary, fanout) = harness
        .rooms
        .update(
            &alice,
            room,
            Settings {
                slow_mode_seconds: Some(0),
                ..Settings::default()
            },
        )
        .await
        .expect("and speed it up again");
    assert_eq!(summary.slow_mode_ms, None, "off is absent in a summary");
    let (_, event) = expect_state(fanout);
    assert_eq!(
        event.slow_mode_ms,
        Some(0),
        "absent would mean unchanged and leave every client showing a dead interval"
    );
}

#[tokio::test]
async fn a_settings_screen_that_changed_nothing_writes_nothing() {
    let harness = Harness::new();
    let room = harness.empty_room().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    harness
        .rooms
        .update(
            &alice,
            room,
            Settings {
                topic: TopicChange::Set("ngobrol".to_string()),
                slow_mode_seconds: Some(15),
                ..Settings::default()
            },
        )
        .await
        .expect("the first submit applies");
    let before = harness.room_row(room).await.updated_at;

    // Every input resubmitted with the value it already holds.
    let (summary, fanout) = harness
        .rooms
        .update(
            &caller(ALICE, ALICE_PHONE, LATER + MINUTE),
            room,
            Settings {
                name: Some("Jakarta Lobby".to_string()),
                topic: TopicChange::Set("ngobrol".to_string()),
                slow_mode_seconds: Some(15),
                join_policy: Some(join_policy::OPEN),
            },
        )
        .await
        .expect("a resubmit is not an error");
    assert!(fanout.is_none(), "and does not broadcast");
    assert_eq!(summary.my_role, Some(RoomRole::Owner));
    assert_eq!(harness.settings_changes("unchanged"), 1);
    assert_eq!(
        harness.room_row(room).await.updated_at,
        before,
        "and does not appear in an audit trail as a change"
    );
}

#[tokio::test]
async fn clearing_a_topic_that_is_already_absent_changes_nothing() {
    let harness = Harness::new();
    let room = harness.empty_room().await;
    let (_, fanout) = harness
        .rooms
        .update(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            Settings {
                topic: TopicChange::Clear,
                ..Settings::default()
            },
        )
        .await
        .expect("clearing nothing is not an error");
    assert!(fanout.is_none());
    assert_eq!(harness.settings_changes("unchanged"), 1);
}

#[tokio::test]
async fn settings_refuse_what_they_cannot_store() {
    let harness = Harness::new();
    let room = harness.empty_room().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    for (settings, code) in [
        (
            Settings {
                name: Some("   ".to_string()),
                ..Settings::default()
            },
            codes::FIELD_REQUIRED,
        ),
        (
            Settings {
                name: Some(long(MAX_NAME_LEN + 1)),
                ..Settings::default()
            },
            codes::FIELD_TOO_LONG,
        ),
        (
            Settings {
                topic: TopicChange::Set(long(MAX_TOPIC_LEN + 1)),
                ..Settings::default()
            },
            codes::FIELD_TOO_LONG,
        ),
        (
            Settings {
                slow_mode_seconds: Some(-1),
                ..Settings::default()
            },
            codes::VALIDATION_FAILED,
        ),
        (
            Settings {
                slow_mode_seconds: Some(MAX_SLOW_MODE_SECONDS + 1),
                ..Settings::default()
            },
            codes::VALIDATION_FAILED,
        ),
        (
            Settings {
                join_policy: Some(9),
                ..Settings::default()
            },
            codes::VALIDATION_FAILED,
        ),
    ] {
        expect_code(harness.rooms.update(&alice, room, settings).await, code);
    }
    assert_eq!(harness.settings_changes("invalid"), 6);
    // An hour is the ceiling, and an hour exactly is inside it: longer than that is a
    // read-only room, not slow mode.
    harness
        .rooms
        .update(
            &alice,
            room,
            Settings {
                slow_mode_seconds: Some(MAX_SLOW_MODE_SECONDS),
                ..Settings::default()
            },
        )
        .await
        .expect("exactly at the ceiling is allowed");
}

#[tokio::test]
async fn editing_needs_the_edit_bit_and_the_policy_needs_the_manage_bit() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness.promote(room, BOB, RoomRole::Admin, LATER).await;
    harness
        .promote(room, CAROL, RoomRole::Moderator, LATER)
        .await;

    // A Moderator has no `ROOM_EDIT`.
    expect_code(
        harness
            .rooms
            .update(
                &caller(CAROL, CAROL_PHONE, LATER),
                room,
                Settings {
                    name: Some("Diambil Alih".to_string()),
                    ..Settings::default()
                },
            )
            .await,
        codes::PERMISSION_DENIED,
    );

    // An Administrator has `ROOM_EDIT` and may rename.
    harness
        .rooms
        .update(
            &caller(BOB, BOB_LAPTOP, LATER),
            room,
            Settings {
                name: Some("Ruang Baru".to_string()),
                ..Settings::default()
            },
        )
        .await
        .expect("an administrator may rename a room");

    // Turning it invitation-only is membership management, so it costs the higher bit
    // and an Administrator does not hold it.
    expect_code(
        harness
            .rooms
            .update(
                &caller(BOB, BOB_LAPTOP, LATER),
                room,
                Settings {
                    join_policy: Some(join_policy::INVITE),
                    ..Settings::default()
                },
            )
            .await,
        codes::PERMISSION_DENIED,
    );
    assert_eq!(harness.settings_changes("denied"), 2);
    assert_eq!(harness.room_row(room).await.join_policy, join_policy::OPEN);
}

#[tokio::test]
async fn a_stranger_cannot_edit_a_room_they_are_not_in() {
    let harness = Harness::new();
    let room = harness.founded().await;
    expect_code(
        harness
            .rooms
            .update(
                &caller(STRANGER, STRANGER_PHONE, LATER),
                room,
                Settings {
                    name: Some("Diambil Alih".to_string()),
                    ..Settings::default()
                },
            )
            .await,
        codes::NOT_A_MEMBER,
    );
}

#[tokio::test]
async fn an_archived_room_takes_no_more_settings() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    harness
        .rooms
        .archive(&alice, room)
        .await
        .expect("the owner may archive");
    expect_code(
        harness
            .rooms
            .update(
                &alice,
                room,
                Settings {
                    name: Some("Dibuka Lagi".to_string()),
                    ..Settings::default()
                },
            )
            .await,
        codes::ROOM_ARCHIVED,
    );
}

// --- archiving -------------------------------------------------------------------

#[tokio::test]
async fn only_the_owner_may_archive_and_doing_it_twice_is_not_an_error() {
    let harness = Harness::new();
    let room = harness.founded().await;
    // A Manager holds every permission bit there is, including `ROOM_MANAGE`, and
    // still may not do this: archiving ends the room for everybody in it, and it is
    // the one settings action a Manager appointed this morning should not take alone.
    harness.promote(room, BOB, RoomRole::Manager, LATER).await;
    expect_code(
        harness
            .rooms
            .archive(&caller(BOB, BOB_LAPTOP, LATER), room)
            .await,
        codes::PERMISSION_DENIED,
    );
    assert_eq!(harness.archives(), 0);

    let alice = caller(ALICE, ALICE_PHONE, LATER);
    harness
        .rooms
        .archive(&alice, room)
        .await
        .expect("the owner may archive");
    assert_eq!(harness.archives(), 1);
    let archived_at = harness.room_row(room).await.archived_at;
    assert_eq!(archived_at, Some(ts(LATER)));

    harness
        .rooms
        .archive(&caller(ALICE, ALICE_PHONE, LATER + MINUTE), room)
        .await
        .expect("the second press of a button whose first press worked is not an error");
    assert_eq!(
        harness.archives(),
        1,
        "and is not counted as a second archive"
    );
    assert_eq!(
        harness.room_row(room).await.archived_at,
        archived_at,
        "nor does it move the timestamp"
    );
}

#[tokio::test]
async fn archiving_a_room_that_does_not_exist_is_not_found() {
    let harness = Harness::new();
    expect_code(
        harness
            .rooms
            .archive(&caller(ALICE, ALICE_PHONE, LATER), id(777))
            .await,
        codes::NOT_FOUND,
    );
}

// --- the permission algebra ------------------------------------------------------

#[test]
fn a_role_carries_everything_the_roles_below_it_do() {
    let ladder = [
        RoomRole::Member,
        RoomRole::Helper,
        RoomRole::Moderator,
        RoomRole::Admin,
        RoomRole::Manager,
        RoomRole::Owner,
    ];
    for pair in ladder.windows(2) {
        let (lower, higher) = (permission::of_role(pair[0]), permission::of_role(pair[1]));
        assert_eq!(
            lower & higher,
            lower,
            "{:?} must keep everything {:?} has",
            pair[1],
            pair[0]
        );
        if pair[1] != RoomRole::Owner {
            assert_ne!(lower, higher, "{:?} must add something", pair[1]);
        }
    }
    assert_eq!(
        permission::of_role(RoomRole::Member),
        permission::MEMBER_DEFAULT
    );
    // Ownership is not a permission bit, so the top two roles carry the same mask and
    // differ only in rank and in the room's owner column. Anything reserved to the
    // owner -- archiving, handing the room over -- has to be checked against that
    // column, because there is no bit to check and no bit that could be granted.
    assert_eq!(permission::of_role(RoomRole::Manager), permission::ALL);
    assert_eq!(permission::of_role(RoomRole::Owner), permission::ALL);
    assert_eq!(
        permission::of_role(RoomRole::Unknown),
        0,
        "a role this build does not know grants nothing"
    );
}

#[test]
fn a_new_member_may_talk_but_not_moderate() {
    let member = permission::of_role(RoomRole::Member);
    for bit in [
        permission::CHAT_SEND,
        permission::VOICE_NOTE_SEND,
        permission::VOICE_NOTE_PLAY,
        permission::VOICE_NOTE_FORWARD,
        permission::CALL_JOIN,
        permission::BOT_USE,
    ] {
        assert!(permission::allows(member, bit), "a member may use {bit:#x}");
    }
    for bit in [
        permission::CHAT_DELETE,
        permission::CHAT_PIN,
        permission::USER_MUTE,
        permission::USER_KICK,
        permission::USER_BAN,
        permission::ROOM_EDIT,
        permission::ROOM_MANAGE,
        permission::ROOM_MODERATE,
        permission::ROOM_ANNOUNCE,
        permission::CALL_START,
        permission::BOT_MANAGE,
    ] {
        assert!(!permission::allows(member, bit), "and not {bit:#x}");
    }
}

#[test]
fn deny_wins_and_it_wins_last() {
    // A moderator who takes `CHAT_SEND` away needs that to hold whatever else the
    // member accumulates, or the moderation tool is advisory.
    let denied = permission::resolve(
        RoomRole::Member,
        permission::CHAT_SEND,
        permission::CHAT_SEND,
    );
    assert!(
        !permission::allows(denied, permission::CHAT_SEND),
        "a grant of the same bit must not undo the deny"
    );
    let granted = permission::resolve(RoomRole::Member, permission::CHAT_PIN, 0);
    assert!(permission::allows(granted, permission::CHAT_PIN));
    assert!(
        permission::allows(granted, permission::CHAT_SEND),
        "a grant adds to the role default rather than replacing it"
    );
    let owner_denied = permission::resolve(RoomRole::Owner, 0, permission::ROOM_MANAGE);
    assert!(
        !permission::allows(owner_denied, permission::ROOM_MANAGE),
        "the mask arithmetic has no exception for the owner; the service does"
    );
}

#[test]
fn an_unknown_grant_bit_grants_nothing() {
    let stray = 1u64 << 40;
    assert_eq!(permission::unknown_bits(stray), stray);
    assert_eq!(permission::unknown_bits(permission::ALL), 0);
    let resolved = permission::resolve(RoomRole::Member, stray, 0);
    assert_eq!(
        resolved & stray,
        0,
        "a bit this build does not define must not survive into an effective mask"
    );
    assert_eq!(resolved, permission::MEMBER_DEFAULT);
}

#[test]
fn an_all_of_check_needs_every_bit_asked_for() {
    let mask = permission::CHAT_SEND | permission::CHAT_PIN;
    assert!(permission::allows(mask, permission::CHAT_SEND));
    assert!(permission::allows(mask, mask));
    assert!(
        permission::allows(mask, 0),
        "an empty mask is the membership-only form and is always satisfied"
    );
    assert!(
        !permission::allows(mask, mask | permission::USER_BAN),
        "holding two of three bits is not holding three"
    );
}

#[test]
fn rank_is_strict_and_the_owner_is_at_the_top() {
    assert!(permission::outranks(RoomRole::Owner, RoomRole::Manager));
    assert!(permission::outranks(RoomRole::Manager, RoomRole::Admin));
    assert!(permission::outranks(RoomRole::Admin, RoomRole::Moderator));
    assert!(permission::outranks(RoomRole::Moderator, RoomRole::Helper));
    assert!(permission::outranks(RoomRole::Helper, RoomRole::Member));
    assert!(permission::outranks(RoomRole::Member, RoomRole::Unknown));

    assert!(
        !permission::outranks(RoomRole::Owner, RoomRole::Owner),
        "nobody outranks themselves, so a peer cannot act on a peer"
    );
    assert!(!permission::outranks(RoomRole::Admin, RoomRole::Manager));
    assert!(
        !permission::outranks(RoomRole::Manager, RoomRole::Owner),
        "there is nothing above Owner to compare against"
    );
}

#[test]
fn a_mute_withholds_speech_and_nothing_a_listener_needs() {
    for bit in [
        permission::CHAT_SEND,
        permission::VOICE_NOTE_SEND,
        permission::CALL_START,
        permission::ROOM_ANNOUNCE,
    ] {
        assert!(
            permission::SILENCED_BY_MUTE & bit != 0,
            "a mute must withhold {bit:#x}"
        );
    }
    for bit in [
        permission::VOICE_NOTE_PLAY,
        permission::CALL_JOIN,
        permission::BOT_USE,
        permission::ROOM_MODERATE,
    ] {
        assert!(
            permission::SILENCED_BY_MUTE & bit == 0,
            "a muted member may still {bit:#x}"
        );
    }
}

// --- authorize -------------------------------------------------------------------

#[tokio::test]
async fn authorize_reports_the_effective_mask_and_the_room_identity() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let granted = harness
        .rooms
        .authorize(&caller(BOB, BOB_LAPTOP, LATER), room, permission::CHAT_SEND)
        .await
        .expect("a member may talk");
    assert_eq!(granted.room_id, room);
    assert_eq!(
        granted.conversation_id,
        harness.room_row(room).await.conversation_id
    );
    assert_eq!(granted.kind, RoomKind::Public);
    assert_eq!(granted.role, RoomRole::Member);
    assert_eq!(granted.permissions, permission::MEMBER_DEFAULT);
    assert_eq!(granted.slow_mode_seconds, 0);
    assert_eq!(harness.authorizations("granted"), 1);
}

#[tokio::test]
async fn authorize_names_which_of_the_four_refusals_happened() {
    // A client that cannot tell "you were banned an hour ago" from "you cannot pin
    // messages" cannot say anything useful to the person holding the phone.
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);

    expect_code(
        harness
            .rooms
            .authorize(
                &caller(STRANGER, STRANGER_PHONE, LATER),
                room,
                permission::CHAT_SEND,
            )
            .await,
        codes::NOT_A_MEMBER,
    );
    expect_code(
        harness
            .rooms
            .authorize(&caller(BOB, BOB_LAPTOP, LATER), room, permission::CHAT_PIN)
            .await,
        codes::PERMISSION_DENIED,
    );

    harness
        .rooms
        .sanction(
            &alice,
            room,
            id(BOB),
            Sanction::Mute {
                duration_ms: 10 * MINUTE,
                reason: None,
            },
        )
        .await
        .expect("the owner may mute");
    expect_code(
        harness
            .rooms
            .authorize(&caller(BOB, BOB_LAPTOP, LATER), room, permission::CHAT_SEND)
            .await,
        codes::MUTED,
    );

    harness
        .rooms
        .sanction(
            &alice,
            room,
            id(CAROL),
            Sanction::Ban {
                duration_ms: None,
                reason: None,
            },
        )
        .await
        .expect("the owner may ban");
    expect_code(
        harness
            .rooms
            .authorize(
                &caller(CAROL, CAROL_PHONE, LATER),
                room,
                permission::CHAT_SEND,
            )
            .await,
        codes::BANNED,
    );

    assert_eq!(harness.authorizations("not_a_member"), 1);
    assert_eq!(harness.authorizations("denied"), 1);
    assert_eq!(harness.authorizations("muted"), 1);
    assert_eq!(harness.authorizations("banned"), 1);
}

#[tokio::test]
async fn a_ban_is_visible_before_a_departure_is() {
    // The store sets `left_at` when it bans, so a version that read departure first
    // would answer `NOT_A_MEMBER` and hide the ban from the person it was applied to
    // and from the operator reading the logs.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Ban {
                duration_ms: None,
                reason: Some("spam".to_string()),
            },
        )
        .await
        .expect("the owner may ban");
    let member = harness.member_row(room, BOB).await;
    assert!(
        !member.is_active(),
        "the ban also marked the row as departed"
    );
    assert!(member.is_banned(ts(LATER)));
    expect_code(
        harness
            .rooms
            .authorize(&caller(BOB, BOB_LAPTOP, LATER), room, 0)
            .await,
        codes::BANNED,
    );
}

#[tokio::test]
async fn a_muted_member_may_still_listen() {
    // A mute withholds speech, not everything.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Mute {
                duration_ms: 10 * MINUTE,
                reason: None,
            },
        )
        .await
        .expect("the owner may mute");
    let bob = caller(BOB, BOB_LAPTOP, LATER);
    harness
        .rooms
        .authorize(&bob, room, permission::VOICE_NOTE_PLAY)
        .await
        .expect("a muted member may still hear voice notes");
    harness
        .rooms
        .authorize(&bob, room, permission::CALL_JOIN)
        .await
        .expect("and may still join a call");
    harness
        .rooms
        .roster(&bob, room, 10, None)
        .await
        .expect("and may still see who is here");
    // And it lapses without anybody lifting it.
    harness
        .rooms
        .authorize(
            &caller(BOB, BOB_LAPTOP, LATER + 20 * MINUTE),
            room,
            permission::CHAT_SEND,
        )
        .await
        .expect("a timed mute expires on its own");
}

#[tokio::test]
async fn a_muted_moderator_is_told_they_are_muted() {
    // Ahead of the permission check, so a muted moderator is not told they lack a bit
    // they plainly hold.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness.promote(room, BOB, RoomRole::Moderator, LATER).await;
    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Mute {
                duration_ms: 10 * MINUTE,
                reason: None,
            },
        )
        .await
        .expect("the owner outranks a moderator");
    expect_code(
        harness
            .rooms
            .authorize(&caller(BOB, BOB_LAPTOP, LATER), room, permission::CHAT_SEND)
            .await,
        codes::MUTED,
    );
}

#[tokio::test]
async fn a_moderator_is_never_slow_moded() {
    // The moderator's instruction to calm down is the message most likely to be needed
    // twice inside the window. Reported as zero rather than enforced-with-an-exception,
    // so the crate that applies the interval needs one rule and not two.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness.promote(room, BOB, RoomRole::Helper, LATER).await;
    harness
        .rooms
        .update(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            Settings {
                slow_mode_seconds: Some(30),
                ..Settings::default()
            },
        )
        .await
        .expect("the owner may slow the room down");

    let member = harness
        .rooms
        .authorize(
            &caller(CAROL, CAROL_PHONE, LATER),
            room,
            permission::CHAT_SEND,
        )
        .await
        .expect("a member may talk");
    assert_eq!(member.slow_mode_seconds, 30);

    let helper = harness
        .rooms
        .authorize(&caller(BOB, BOB_LAPTOP, LATER), room, permission::CHAT_SEND)
        .await
        .expect("so may a helper");
    assert_eq!(
        helper.slow_mode_seconds, 0,
        "a member who can moderate is not slow-moded"
    );
}

#[tokio::test]
async fn authorize_with_an_empty_mask_asks_only_about_membership() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .authorize(&caller(BOB, BOB_LAPTOP, LATER), room, 0)
        .await
        .expect("a member is a member");
    expect_code(
        harness
            .rooms
            .authorize(&caller(STRANGER, STRANGER_PHONE, LATER), room, 0)
            .await,
        codes::NOT_A_MEMBER,
    );
    // A mute does not intersect an empty mask, so a muted member still passes.
    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Mute {
                duration_ms: 10 * MINUTE,
                reason: None,
            },
        )
        .await
        .expect("the owner may mute");
    harness
        .rooms
        .authorize(&caller(BOB, BOB_LAPTOP, LATER), room, 0)
        .await
        .expect("a muted member is still a member");
}

#[tokio::test]
async fn authorize_is_never_charged_for() {
    // It is called from inside an operation that has already paid, and charging again
    // would bill one user action twice and make a room's send budget depend on how many
    // permission checks the implementation happens to do. Sixty reads at five each
    // would be three hundred against a burst of two hundred.
    let harness = Harness::new();
    let room = harness.founded().await;
    let bob = caller(BOB, BOB_LAPTOP, LATER);
    for _ in 0..60 {
        harness
            .rooms
            .authorize(&bob, room, permission::CHAT_SEND)
            .await
            .expect("no call is refused for want of budget");
    }
    harness
        .rooms
        .summary(&bob, room)
        .await
        .expect("and the account's budget is untouched");
}

#[tokio::test]
async fn authorize_refuses_a_room_that_does_not_exist() {
    let harness = Harness::new();
    expect_code(
        harness
            .rooms
            .authorize(&caller(BOB, BOB_LAPTOP, LATER), id(777), 0)
            .await,
        codes::NOT_FOUND,
    );
}

#[tokio::test]
async fn a_per_member_override_reaches_authorize() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    harness
        .rooms
        .set_permissions(
            &alice,
            room,
            id(BOB),
            permission::CHAT_PIN,
            permission::CHAT_SEND,
        )
        .await
        .expect("the owner may override");
    let bob = caller(BOB, BOB_LAPTOP, LATER);
    let granted = harness
        .rooms
        .authorize(&bob, room, permission::CHAT_PIN)
        .await
        .expect("the grant took effect");
    assert_eq!(
        granted.permissions,
        (permission::MEMBER_DEFAULT | permission::CHAT_PIN) & !permission::CHAT_SEND
    );
    expect_code(
        harness
            .rooms
            .authorize(&bob, room, permission::CHAT_SEND)
            .await,
        codes::PERMISSION_DENIED,
    );
}

// --- roles -----------------------------------------------------------------------

#[tokio::test]
async fn a_promotion_is_broadcast_as_a_member_who_is_still_here() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let fanout = harness
        .rooms
        .set_role(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            RoomRole::Moderator,
        )
        .await
        .expect("the owner may promote");
    let (fanout, event) = expect_member(fanout);
    assert_eq!(fanout.exclude_device, Some(id(ALICE_PHONE)));
    assert_eq!(event.user_id, id(BOB));
    assert!(
        event.joined,
        "the field says whether the member is in the room, and they are"
    );
    assert_eq!(event.role, Some(RoomRole::Moderator));
    assert_eq!(
        event.member_count, None,
        "nothing about the count moved, so it is absent"
    );
    assert_eq!(harness.role_changes("applied"), 1);
    assert_eq!(
        harness.member_row(room, BOB).await.role,
        RoomRole::Moderator
    );
}

#[tokio::test]
async fn setting_the_role_already_held_says_nothing() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let fanout = harness
        .rooms
        .set_role(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            RoomRole::Member,
        )
        .await
        .expect("a no-op is not an error");
    assert!(fanout.is_none());
    assert_eq!(harness.role_changes("unchanged"), 1);
    assert_eq!(harness.role_changes("applied"), 0);
}

#[tokio::test]
async fn ownership_is_not_a_role_that_can_be_given() {
    // Granting it here would produce a room with two owners and an `owner_id` column
    // that names one.
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    expect_code(
        harness
            .rooms
            .set_role(&alice, room, id(BOB), RoomRole::Owner)
            .await,
        codes::CONFLICT,
    );
    expect_code(
        harness
            .rooms
            .set_role(&alice, room, id(BOB), RoomRole::Unknown)
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(harness.role_changes("invalid"), 2);
    assert_eq!(harness.member_row(room, BOB).await.role, RoomRole::Member);
}

#[tokio::test]
async fn a_role_change_needs_the_manage_bit() {
    let harness = Harness::new();
    let room = harness.founded().await;
    // An Administrator holds `ROOM_EDIT` and `USER_BAN` and still cannot appoint.
    harness.promote(room, BOB, RoomRole::Admin, LATER).await;
    expect_code(
        harness
            .rooms
            .set_role(
                &caller(BOB, BOB_LAPTOP, LATER),
                room,
                id(CAROL),
                RoomRole::Helper,
            )
            .await,
        codes::PERMISSION_DENIED,
    );
    assert_eq!(harness.role_changes("denied"), 1);
}

#[tokio::test]
async fn an_actor_must_outrank_both_the_current_role_and_the_granted_one() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .join(DAVE, DAVE_TABLET, room, NOW + 3 * SECOND)
        .await;
    harness.promote(room, BOB, RoomRole::Manager, LATER).await;
    harness.promote(room, CAROL, RoomRole::Manager, LATER).await;
    let bob = caller(BOB, BOB_LAPTOP, LATER);

    // Carol is Bob's peer, so Bob cannot touch her at all.
    expect_code(
        harness
            .rooms
            .set_role(&bob, room, id(CAROL), RoomRole::Member)
            .await,
        codes::PERMISSION_DENIED,
    );
    // And Bob cannot mint a peer either: without this check a Moderator could appoint
    // an Administrator and be outranked by their own appointee a second later.
    expect_code(
        harness
            .rooms
            .set_role(&bob, room, id(DAVE), RoomRole::Manager)
            .await,
        codes::PERMISSION_DENIED,
    );
    // One step below is fine.
    harness
        .rooms
        .set_role(&bob, room, id(DAVE), RoomRole::Admin)
        .await
        .expect("a manager may appoint an administrator");
    assert_eq!(harness.member_row(room, DAVE).await.role, RoomRole::Admin);
}

#[tokio::test]
async fn the_owner_is_not_a_rank_a_role_change_can_reach() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness.promote(room, BOB, RoomRole::Manager, LATER).await;
    expect_code(
        harness
            .rooms
            .set_role(
                &caller(BOB, BOB_LAPTOP, LATER),
                room,
                id(ALICE),
                RoomRole::Member,
            )
            .await,
        codes::PERMISSION_DENIED,
    );
    assert_eq!(harness.member_row(room, ALICE).await.role, RoomRole::Owner);
}

#[tokio::test]
async fn a_moderation_action_cannot_be_aimed_at_its_own_actor() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    expect_code(
        harness
            .rooms
            .set_role(&alice, room, id(ALICE), RoomRole::Member)
            .await,
        codes::CONFLICT,
    );
    expect_code(
        harness
            .rooms
            .sanction(&alice, room, id(ALICE), Sanction::Kick)
            .await,
        codes::CONFLICT,
    );
    expect_code(
        harness
            .rooms
            .set_permissions(&alice, room, id(ALICE), 0, permission::CHAT_SEND)
            .await,
        codes::CONFLICT,
    );
}

#[tokio::test]
async fn a_moderation_action_needs_a_real_subject() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    expect_code(
        harness
            .rooms
            .set_role(&alice, room, Id::NIL, RoomRole::Helper)
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness
            .rooms
            .set_role(&alice, room, id(STRANGER), RoomRole::Helper)
            .await,
        codes::NOT_FOUND,
    );
}

#[tokio::test]
async fn a_departed_member_cannot_be_promoted() {
    // A role handed to somebody who is not there would be a role they discover on
    // returning, granted by a moderator who thought they were talking to them.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .leave(
            &caller(BOB, BOB_LAPTOP, LATER),
            RoomLeaveRequest { room_id: room },
        )
        .await
        .expect("a member may walk out");
    expect_code(
        harness
            .rooms
            .set_role(
                &caller(ALICE, ALICE_PHONE, LATER),
                room,
                id(BOB),
                RoomRole::Helper,
            )
            .await,
        codes::NOT_A_MEMBER,
    );
}

// --- per-member overrides --------------------------------------------------------

#[tokio::test]
async fn an_override_is_stored_and_never_broadcast() {
    // `RoomMemberEvent` carries a role and not a permission set, and inventing a frame
    // for it would put a moderation detail about one member on a topic the whole room
    // subscribes to.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .set_permissions(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            permission::CHAT_PIN,
            permission::VOICE_NOTE_SEND,
        )
        .await
        .expect("the owner may override");
    let member = harness.member_row(room, BOB).await;
    assert_eq!(member.permissions_grant, permission::CHAT_PIN);
    assert_eq!(member.permissions_deny, permission::VOICE_NOTE_SEND);
    assert_eq!(harness.overrides("applied"), 1);
}

#[tokio::test]
async fn an_override_that_changed_nothing_is_a_no_op() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    harness
        .rooms
        .set_permissions(&alice, room, id(BOB), permission::CHAT_PIN, 0)
        .await
        .expect("the first override applies");
    harness
        .rooms
        .set_permissions(&alice, room, id(BOB), permission::CHAT_PIN, 0)
        .await
        .expect("the second is a no-op");
    assert_eq!(harness.overrides("applied"), 1);
    assert_eq!(harness.overrides("unchanged"), 1);
}

#[tokio::test]
async fn an_override_refuses_a_bit_this_build_does_not_define() {
    // Keeping it would make the difference invisible until the bit acquired a meaning
    // in a later release and started granting something nobody asked for.
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    expect_code(
        harness
            .rooms
            .set_permissions(&alice, room, id(BOB), 1 << 40, 0)
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness
            .rooms
            .set_permissions(&alice, room, id(BOB), 0, 1 << 63)
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness
            .rooms
            .set_permissions(
                &alice,
                room,
                id(BOB),
                permission::CHAT_SEND,
                permission::CHAT_SEND,
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(harness.overrides("invalid"), 3);
    let member = harness.member_row(room, BOB).await;
    assert_eq!(member.permissions_grant, 0);
    assert_eq!(member.permissions_deny, 0);
}

#[tokio::test]
async fn a_permission_cannot_be_granted_by_somebody_who_does_not_hold_it() {
    // Otherwise the override mask is a privilege-escalation primitive: grant yourself
    // `ROOM_MANAGE`, then grant yourself the rest.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .join(DAVE, DAVE_TABLET, room, NOW + 3 * SECOND)
        .await;
    harness.promote(room, BOB, RoomRole::Manager, LATER).await;
    // A Manager holds every bit, so the only way to make one lack a bit is to take it
    // away from them.
    harness
        .rooms
        .set_permissions(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            0,
            permission::CHAT_PIN,
        )
        .await
        .expect("the owner may deny a manager a bit");

    let bob = caller(BOB, BOB_LAPTOP, LATER);
    expect_code(
        harness
            .rooms
            .set_permissions(&bob, room, id(DAVE), permission::CHAT_PIN, 0)
            .await,
        codes::PERMISSION_DENIED,
    );
    // A deny is unchecked by design: the worst it does is take a permission away,
    // which `ROOM_MANAGE` already allows by demotion, so requiring the bit would block
    // an administrator denied `CHAT_PIN` themselves from moderating pins.
    harness
        .rooms
        .set_permissions(&bob, room, id(DAVE), 0, permission::CHAT_PIN)
        .await
        .expect("a bit may be withheld by somebody who does not hold it");
    assert_eq!(
        harness.member_row(room, DAVE).await.permissions_deny,
        permission::CHAT_PIN
    );
}

#[tokio::test]
async fn an_override_needs_the_manage_bit_and_cannot_reach_the_owner() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness.promote(room, BOB, RoomRole::Admin, LATER).await;
    expect_code(
        harness
            .rooms
            .set_permissions(
                &caller(BOB, BOB_LAPTOP, LATER),
                room,
                id(CAROL),
                0,
                permission::CHAT_SEND,
            )
            .await,
        codes::PERMISSION_DENIED,
    );
    harness.promote(room, BOB, RoomRole::Manager, LATER).await;
    expect_code(
        harness
            .rooms
            .set_permissions(
                &caller(BOB, BOB_LAPTOP, LATER),
                room,
                id(ALICE),
                0,
                permission::CHAT_SEND,
            )
            .await,
        codes::PERMISSION_DENIED,
    );
    assert_eq!(harness.overrides("denied"), 2);
    assert_eq!(harness.member_row(room, ALICE).await.permissions_deny, 0);
}

// --- sanctions -------------------------------------------------------------------

#[tokio::test]
async fn a_mute_is_between_a_moderator_and_one_member() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let fanout = harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Mute {
                duration_ms: 10 * MINUTE,
                reason: Some("tenang dulu".to_string()),
            },
        )
        .await
        .expect("the owner may mute");
    assert!(
        fanout.is_none(),
        "announcing it to the room turns a correction into a punishment"
    );
    let member = harness.member_row(room, BOB).await;
    assert_eq!(member.muted_until, Some(ts(LATER + 10 * MINUTE)));
    assert!(member.is_active(), "a mute does not remove anybody");
    assert_eq!(harness.sanctions("mute"), 1);
    assert_eq!(harness.room_row(room).await.member_count, 3);
}

#[tokio::test]
async fn an_unmute_clears_the_mute_and_nothing_else() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    harness
        .rooms
        .sanction(
            &alice,
            room,
            id(BOB),
            Sanction::Ban {
                duration_ms: Some(DAY),
                reason: Some("spam".to_string()),
            },
        )
        .await
        .expect("the owner may ban");
    harness
        .rooms
        .sanction(
            &alice,
            room,
            id(BOB),
            Sanction::Mute {
                duration_ms: MINUTE,
                reason: None,
            },
        )
        .await
        .expect("a banned account can still be muted");
    // Every arm passes the other sanction's expiry back unchanged, because the store
    // writes both columns: muting somebody must not be a way to clear their ban.
    assert_eq!(
        harness.member_row(room, BOB).await.banned_until,
        Some(ts(LATER + DAY)),
        "the mute must not have lifted the ban"
    );

    let fanout = harness
        .rooms
        .sanction(&alice, room, id(BOB), Sanction::Unmute)
        .await
        .expect("the owner may unmute");
    assert!(fanout.is_none());
    let member = harness.member_row(room, BOB).await;
    assert_eq!(member.muted_until, None);
    assert_eq!(
        member.banned_until,
        Some(ts(LATER + DAY)),
        "and the unmute must not have lifted it either"
    );
    assert_eq!(harness.sanctions("unmute"), 1);
}

#[tokio::test]
async fn a_kick_removes_the_member_and_tells_the_room() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    let fanout = harness
        .rooms
        .sanction(&alice, room, id(BOB), Sanction::Kick)
        .await
        .expect("the owner may kick");
    let (fanout, event) = expect_member(fanout);
    assert_eq!(
        fanout.exclude_device,
        Some(id(ALICE_PHONE)),
        "the acting moderator's own socket already knows"
    );
    assert_eq!(event.user_id, id(BOB));
    assert!(!event.joined);
    assert_eq!(event.member_count, Some(2));
    assert!(!harness.member_row(room, BOB).await.is_active());
    assert_eq!(harness.sanctions("kick"), 1);

    // A kick aimed at somebody already gone writes nothing and announces nothing.
    let again = harness
        .rooms
        .sanction(&alice, room, id(BOB), Sanction::Kick)
        .await
        .expect("kicking twice is not an error");
    assert!(again.is_none());
    assert_eq!(harness.room_row(room).await.member_count, 2);

    // And a kick is not a ban: they may come back.
    harness
        .rooms
        .join(&caller(BOB, BOB_LAPTOP, LATER), join_request(room))
        .await
        .expect("a kick does not bar a return");
}

#[tokio::test]
async fn a_ban_excludes_the_acting_moderator_from_its_own_broadcast() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let fanout = harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Ban {
                duration_ms: Some(DAY),
                reason: Some("spam".to_string()),
            },
        )
        .await
        .expect("the owner may ban");
    let (fanout, event) = expect_member(fanout);
    assert_eq!(
        fanout.exclude_device,
        Some(id(ALICE_PHONE)),
        "the exclusion is a device id, and the subject's account id is not one"
    );
    assert_eq!(
        event.user_id,
        id(BOB),
        "the event is about the banned member"
    );
    assert!(!event.joined);
    assert_eq!(
        event.role, None,
        "the room hears that somebody left, not why"
    );
    assert_eq!(event.member_count, Some(2));

    let member = harness.member_row(room, BOB).await;
    assert_eq!(member.banned_until, Some(ts(LATER + DAY)));
    assert_eq!(member.ban_reason.as_deref(), Some("spam"));
    assert!(!member.is_active());
    assert_eq!(harness.sanctions("ban"), 1);
}

#[tokio::test]
async fn a_permanent_ban_is_a_far_future_timestamp_and_not_a_null() {
    // "No expiry" and "not banned" must never be the same value.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Ban {
                duration_ms: None,
                reason: None,
            },
        )
        .await
        .expect("the owner may ban permanently");
    let member = harness.member_row(room, BOB).await;
    assert_eq!(member.banned_until, Some(ts(PERMANENT_BAN_MS)));
    assert!(member.is_banned(ts(LATER)));
    assert!(
        member.is_banned(ts(PERMANENT_BAN_MS - 1)),
        "and it is still in force the millisecond before the end of the calendar"
    );
}

#[tokio::test]
async fn an_absurd_ban_duration_is_clamped_rather_than_wrapped() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Ban {
                duration_ms: Some(i64::MAX),
                reason: None,
            },
        )
        .await
        .expect("a very large duration is a permanent ban, not an overflow");
    assert_eq!(
        harness.member_row(room, BOB).await.banned_until,
        Some(ts(PERMANENT_BAN_MS))
    );
}

#[tokio::test]
async fn an_unban_clears_the_ban_and_its_reason_but_not_the_mute() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    harness
        .rooms
        .sanction(
            &alice,
            room,
            id(BOB),
            Sanction::Mute {
                duration_ms: 10 * MINUTE,
                reason: None,
            },
        )
        .await
        .expect("the owner may mute");
    harness
        .rooms
        .sanction(
            &alice,
            room,
            id(BOB),
            Sanction::Ban {
                duration_ms: Some(DAY),
                reason: Some("spam".to_string()),
            },
        )
        .await
        .expect("the owner may ban");
    let fanout = harness
        .rooms
        .sanction(&alice, room, id(BOB), Sanction::Unban)
        .await
        .expect("the owner may lift a ban");
    assert!(
        fanout.is_none(),
        "lifting a ban does not put anybody back in the room; they rejoin"
    );
    let member = harness.member_row(room, BOB).await;
    assert_eq!(member.banned_until, None);
    assert_eq!(
        member.ban_reason, None,
        "a reason that outlived its sanction would explain the next one"
    );
    assert_eq!(
        member.muted_until,
        Some(ts(LATER + 10 * MINUTE)),
        "the mute is a separate decision"
    );
    assert!(
        !member.is_active(),
        "an unban is not a readmission; the row still shows the departure"
    );
    assert_eq!(harness.sanctions("unban"), 1);

    harness
        .rooms
        .join(&caller(BOB, BOB_LAPTOP, LATER), join_request(room))
        .await
        .expect("and the lifted ban no longer bars the door");
}

#[tokio::test]
async fn each_sanction_needs_its_own_permission() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .join(DAVE, DAVE_TABLET, room, NOW + 3 * SECOND)
        .await;
    // A Helper has `USER_MUTE` and neither `USER_KICK` nor `USER_BAN`.
    harness.promote(room, BOB, RoomRole::Helper, LATER).await;
    let bob = caller(BOB, BOB_LAPTOP, LATER);

    harness
        .rooms
        .sanction(
            &bob,
            room,
            id(DAVE),
            Sanction::Mute {
                duration_ms: MINUTE,
                reason: None,
            },
        )
        .await
        .expect("a helper may mute");
    expect_code(
        harness
            .rooms
            .sanction(&bob, room, id(DAVE), Sanction::Kick)
            .await,
        codes::PERMISSION_DENIED,
    );
    expect_code(
        harness
            .rooms
            .sanction(
                &bob,
                room,
                id(DAVE),
                Sanction::Ban {
                    duration_ms: None,
                    reason: None,
                },
            )
            .await,
        codes::PERMISSION_DENIED,
    );
    assert_eq!(harness.sanctions("mute"), 1);
    assert_eq!(
        harness.sanctions("kick"),
        0,
        "a refused action is not a sanction applied"
    );
    assert_eq!(harness.sanctions("ban"), 0);
}

#[tokio::test]
async fn leaving_is_not_a_way_to_avoid_a_ban() {
    // Somebody leaving ahead of the consequence is the ordinary case, so the subject of
    // a sanction is not required to be active.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .leave(
            &caller(BOB, BOB_LAPTOP, LATER),
            RoomLeaveRequest { room_id: room },
        )
        .await
        .expect("a member may walk out");
    harness
        .rooms
        .sanction(
            &caller(ALICE, ALICE_PHONE, LATER),
            room,
            id(BOB),
            Sanction::Ban {
                duration_ms: None,
                reason: None,
            },
        )
        .await
        .expect("an account that just left can still be banned");
    assert!(harness.member_row(room, BOB).await.is_banned(ts(LATER)));
}

#[tokio::test]
async fn the_owner_cannot_be_sanctioned_by_anybody() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness.promote(room, BOB, RoomRole::Manager, LATER).await;
    let bob = caller(BOB, BOB_LAPTOP, LATER);
    for sanction in [
        Sanction::Mute {
            duration_ms: MINUTE,
            reason: None,
        },
        Sanction::Kick,
        Sanction::Ban {
            duration_ms: None,
            reason: None,
        },
        Sanction::Unban,
    ] {
        expect_code(
            harness
                .rooms
                .sanction(&bob, room, id(ALICE), sanction)
                .await,
            codes::PERMISSION_DENIED,
        );
    }
    let owner = harness.member_row(room, ALICE).await;
    assert!(owner.is_active());
    assert_eq!(owner.banned_until, None);
    assert_eq!(owner.muted_until, None);
}

#[tokio::test]
async fn a_sanction_refuses_a_duration_or_a_reason_it_cannot_store() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    for sanction in [
        Sanction::Mute {
            duration_ms: 0,
            reason: None,
        },
        Sanction::Mute {
            duration_ms: MAX_MUTE_MS + 1,
            reason: None,
        },
        Sanction::Ban {
            duration_ms: Some(0),
            reason: None,
        },
        Sanction::Ban {
            duration_ms: Some(-1),
            reason: None,
        },
    ] {
        expect_code(
            harness
                .rooms
                .sanction(&alice, room, id(BOB), sanction)
                .await,
            codes::VALIDATION_FAILED,
        );
    }
    expect_code(
        harness
            .rooms
            .sanction(
                &alice,
                room,
                id(BOB),
                Sanction::Mute {
                    duration_ms: MINUTE,
                    reason: Some(long(MAX_REASON_LEN + 1)),
                },
            )
            .await,
        codes::FIELD_TOO_LONG,
    );
    // Thirty days exactly is a mute; longer is a ban.
    harness
        .rooms
        .sanction(
            &alice,
            room,
            id(BOB),
            Sanction::Mute {
                duration_ms: MAX_MUTE_MS,
                reason: Some(long(MAX_REASON_LEN)),
            },
        )
        .await
        .expect("exactly at the ceiling is allowed");
    let member = harness.member_row(room, BOB).await;
    assert_eq!(member.muted_until, Some(ts(LATER + MAX_MUTE_MS)));
}

#[tokio::test]
async fn a_sanction_refuses_a_room_that_does_not_exist() {
    let harness = Harness::new();
    expect_code(
        harness
            .rooms
            .sanction(
                &caller(ALICE, ALICE_PHONE, LATER),
                id(777),
                id(BOB),
                Sanction::Kick,
            )
            .await,
        codes::NOT_FOUND,
    );
}

// --- ownership -------------------------------------------------------------------

#[tokio::test]
async fn a_transfer_demotes_the_outgoing_owner_to_manager() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let fanout = harness
        .rooms
        .transfer_ownership(&proven(ALICE, ALICE_PHONE, LATER), room, id(BOB))
        .await
        .expect("the owner may give the room away");
    let (fanout, event) = expect_member(fanout);
    assert_eq!(fanout.exclude_device, Some(id(ALICE_PHONE)));
    assert_eq!(
        event.user_id,
        id(BOB),
        "one event, about the incoming owner: two would arrive in an order the gateway \
         does not promise"
    );
    assert!(event.joined);
    assert_eq!(event.role, Some(RoomRole::Owner));

    assert_eq!(harness.room_row(room).await.owner_id, id(BOB));
    assert_eq!(harness.member_row(room, BOB).await.role, RoomRole::Owner);
    assert_eq!(
        harness.member_row(room, ALICE).await.role,
        RoomRole::Manager,
        "demoted, not removed, so they can still undo a transfer they regret"
    );
    assert_eq!(harness.transfers(), 1);

    // And the new owner is the one who can no longer walk out.
    expect_code(
        harness
            .rooms
            .leave(
                &caller(BOB, BOB_LAPTOP, LATER),
                RoomLeaveRequest { room_id: room },
            )
            .await,
        codes::CONFLICT,
    );
    harness
        .rooms
        .leave(
            &caller(ALICE, ALICE_PHONE, LATER),
            RoomLeaveRequest { room_id: room },
        )
        .await
        .expect("and the outgoing owner may now leave");
}

#[tokio::test]
async fn a_transfer_needs_a_recently_proved_factor_and_does_not_charge_for_the_refusal() {
    // Brief section 85, and the check is ahead of the rate limiter on purpose: a
    // refusal that had already spent budget would let an attacker drain the real
    // owner's allowance while being turned away.
    let harness = Harness::new();
    let room = harness.founded().await;
    let alice = caller(ALICE, ALICE_PHONE, LATER);
    for _ in 0..40 {
        expect_code(
            harness
                .rooms
                .transfer_ownership(&alice, room, id(BOB))
                .await,
            codes::REAUTHENTICATION_REQUIRED,
        );
    }
    assert_eq!(harness.transfers(), 0);
    harness
        .rooms
        .transfer_ownership(&proven(ALICE, ALICE_PHONE, LATER), room, id(BOB))
        .await
        .expect("forty refusals cost nothing, so the real transfer still fits");
    assert_eq!(harness.transfers(), 1);
}

#[tokio::test]
async fn only_the_owner_may_transfer_and_only_to_an_active_member() {
    let harness = Harness::new();
    let room = harness.founded().await;
    harness.promote(room, BOB, RoomRole::Manager, LATER).await;
    // Not a permission bit: there is no room-transfer bit, and there should not be,
    // because a bit for it would be a bit somebody could be granted.
    expect_code(
        harness
            .rooms
            .transfer_ownership(&proven(BOB, BOB_LAPTOP, LATER), room, id(CAROL))
            .await,
        codes::PERMISSION_DENIED,
    );
    let alice = proven(ALICE, ALICE_PHONE, LATER);
    expect_code(
        harness
            .rooms
            .transfer_ownership(&alice, room, Id::NIL)
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        harness
            .rooms
            .transfer_ownership(&alice, room, id(STRANGER))
            .await,
        codes::NOT_FOUND,
    );
    assert_eq!(harness.transfers(), 0);
    assert_eq!(harness.room_row(room).await.owner_id, id(ALICE));
}

#[tokio::test]
async fn transferring_to_yourself_changes_nothing() {
    let harness = Harness::new();
    let room = harness.founded().await;
    let fanout = harness
        .rooms
        .transfer_ownership(&proven(ALICE, ALICE_PHONE, LATER), room, id(ALICE))
        .await
        .expect("already the owner");
    assert!(fanout.is_none());
    assert_eq!(
        harness.transfers(),
        0,
        "the counter stays honest about how many transfers happened"
    );
    assert_eq!(harness.member_row(room, ALICE).await.role, RoomRole::Owner);
}

#[tokio::test]
async fn a_transfer_refuses_a_room_that_does_not_exist() {
    let harness = Harness::new();
    expect_code(
        harness
            .rooms
            .transfer_ownership(&proven(ALICE, ALICE_PHONE, LATER), id(777), id(BOB))
            .await,
        codes::NOT_FOUND,
    );
}

// --- metrics ---------------------------------------------------------------------

#[tokio::test]
async fn every_series_exists_before_anything_happens() {
    // A dashboard built on a series that only appears after the first failure shows a
    // gap where it should show a zero, and an alert on it never fires.
    let harness = Harness::new();
    for outcome in ["accepted", "invalid", "taken", "rate_limited"] {
        assert_eq!(harness.creations(outcome), 0, "creations {outcome}");
    }
    for outcome in [
        "accepted",
        "already",
        "rejoined",
        "not_found",
        "archived",
        "full",
        "banned",
        "not_admitted",
        "rate_limited",
    ] {
        assert_eq!(harness.joins(outcome), 0, "joins {outcome}");
    }
    for outcome in ["applied", "unchanged", "invalid", "denied"] {
        assert_eq!(harness.leaves(outcome), 0, "leaves {outcome}");
        assert_eq!(harness.settings_changes(outcome), 0, "settings {outcome}");
        assert_eq!(harness.role_changes(outcome), 0, "roles {outcome}");
        assert_eq!(harness.overrides(outcome), 0, "overrides {outcome}");
    }
    for outcome in ["granted", "not_a_member", "banned", "muted", "denied"] {
        assert_eq!(
            harness.authorizations(outcome),
            0,
            "authorizations {outcome}"
        );
    }
    for action in ["mute", "unmute", "kick", "ban", "unban"] {
        assert_eq!(harness.sanctions(action), 0, "sanctions {action}");
    }
    assert_eq!(harness.archives(), 0);
    assert_eq!(harness.transfers(), 0);
    assert_eq!(harness.listings(), 0);
}

#[tokio::test]
async fn no_metric_is_labelled_by_an_account_or_a_room() {
    // Brief section 174. A label whose value comes from a request is an unbounded
    // series count, and one keyed by an account id is a record of who did what.
    let harness = Harness::new();
    let room = harness.founded().await;
    harness
        .rooms
        .list(&caller(ALICE, ALICE_PHONE, LATER), list_request(10))
        .await
        .expect("a listing is served");
    let text = harness.registry.render();
    for forbidden in [
        &id(ALICE).to_text(),
        &id(ALICE_PHONE).to_text(),
        &room.to_text(),
        &room.public_id(PublicId::Room),
        LOBBY,
    ] {
        assert!(
            !text.contains(forbidden),
            "the metric text must not name {forbidden}"
        );
    }
    assert!(
        text.contains("migo_rooms_listings_total"),
        "and it must still name the series"
    );
}
