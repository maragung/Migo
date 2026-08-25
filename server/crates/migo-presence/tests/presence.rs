//! Integration tests for the presence service.
//!
//! Everything runs against `MemoryStore`, `MemoryCache`, and a `CacheRateLimiter` over
//! the same cache, with hand-written timestamps. No clock is read and no socket is
//! opened, so an expiry test asserts what the code does rather than how fast the
//! machine ran it — which is the property that makes a presence test worth keeping
//! when it fails at three in the morning.
//!
//! The tests are written against the properties that would be expensive to get wrong
//! in production rather than against the shape of the code: that a user who asked to
//! be hidden is hidden on every path including the arithmetic ones, that a client
//! obeying the heartbeat it was given never blinks offline, that a change nobody can
//! observe costs nobody a frame, and that a privacy setting is consulted before a
//! timestamp is disclosed rather than after.

use std::sync::Arc;

use migo_cache::{MemoryCache, PresenceEntry};
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Result, Secret, Timestamp};
use migo_presence::model::{
    Caller, MAX_LAST_SEEN_LOOKUPS, MAX_SNAPSHOT_SUBJECTS, MISSED_HEARTBEATS,
};
use migo_presence::service::Presences;
use migo_presence::traits::Presence;
use migo_presence::{cadence_for, Detail, Fanout, PresenceConfig, PresenceScope};
use migo_protocol::{
    codes, BandwidthMode, Platform, PresenceEvent, PresenceState, PresenceUpdate, RelationshipKind,
};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::model::{NewAccount, NewDevice, Profile, Relationship, Visibility};
use migo_store::traits::{AccountStore, DeviceStore};
use migo_store::MemoryStore;

/// One second in milliseconds.
const SECOND: i64 = 1_000;
/// One minute.
const MINUTE: i64 = 60 * SECOND;

/// The heartbeat `PresenceConfig::default()` advertises.
const HEARTBEAT: i64 = 30 * SECOND;
/// How long an entry from a `Normal` session lives.
const LIFETIME: i64 = HEARTBEAT * MISSED_HEARTBEATS as i64;

/// Alice, who owns two devices.
const ALICE: u128 = 1;
/// Bob, whom Alice watches.
const BOB: u128 = 2;
/// Carol, who blocks Alice.
const CAROL: u128 = 3;
/// Dave, who shows last-seen to friends only.
const DAVE: u128 = 4;

/// Alice's phone.
const ALICE_PHONE: u128 = 101;
/// Alice's laptop, the second device that makes projection interesting.
const ALICE_LAPTOP: u128 = 102;
/// Bob's laptop.
const BOB_LAPTOP: u128 = 103;
/// Carol's phone.
const CAROL_PHONE: u128 = 104;
/// Dave's phone.
const DAVE_PHONE: u128 = 105;

type TestPresence = Presences<MemoryStore, MemoryCache, CacheRateLimiter<MemoryCache>>;

/// Everything a test needs, built the way `migod` builds it.
struct Harness {
    presence: TestPresence,
    store: Arc<MemoryStore>,
    registry: Registry,
}

impl Harness {
    fn new() -> Self {
        let config = Config::default();
        let store = Arc::new(MemoryStore::new());
        let cache = Arc::new(MemoryCache::new());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&config.rate_limit).expect("default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(
            Arc::clone(&cache),
            policies,
            &registry,
        ));
        let presence = Presences::new(
            Arc::clone(&store),
            Arc::clone(&cache),
            limiter,
            &registry,
            PresenceConfig::default(),
        );
        Self {
            presence,
            store,
            registry,
        }
    }

    /// Seeds an account, its profile, and one device.
    async fn person(&self, account: u128, username: &str, device: u128, last_seen: Visibility) {
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
                display_name: username.to_string(),
                bio: None,
                avatar_media_id: None,
                birth_year: None,
                show_last_seen: last_seen,
                who_can_message: Visibility::Everyone,
                who_can_add: Visibility::Everyone,
                searchable: true,
                updated_at: ts(SECOND),
            })
            .await
            .expect("a new account has no profile yet");
        self.device(account, device).await;
    }

    /// Registers one more device on an existing account.
    async fn device(&self, account: u128, device: u128) {
        self.store
            .register_device(NewDevice {
                device_id: id(device),
                account_id: id(account),
                platform: Platform::Android,
                display_name: "Pixel".to_string(),
                app_version: "0.1.0".to_string(),
                os_version: Some("14".to_string()),
                device_model: Some("Pixel 8".to_string()),
                created_at: ts(SECOND),
            })
            .await
            .expect("a fresh device id is free");
    }

    /// Records a relationship edge directly, without going through the social crate.
    async fn edge(&self, a: u128, b: u128, kind: RelationshipKind, accepted: bool) {
        use migo_store::traits::SocialStore;
        self.store
            .put_relationship(Relationship {
                account_id: id(a),
                other_id: id(b),
                kind,
                created_at: ts(SECOND),
                accepted_at: if accepted { Some(ts(SECOND)) } else { None },
            })
            .await
            .expect("an edge can always be recorded");
    }

    /// The value of a single metric series, or `None` if it was never registered.
    fn metric(&self, series: &str) -> Option<f64> {
        self.registry
            .render()
            .lines()
            .find(|line| line.starts_with(series))
            .and_then(|line| line.rsplit(' ').next().map(str::to_string))
            .and_then(|value| value.parse().ok())
    }

    /// The last-seen stamp on a device row, which only a disconnect moves.
    async fn device_last_seen(&self, device: u128) -> Timestamp {
        self.store
            .device_by_id(id(device))
            .await
            .expect("the store answers")
            .expect("the device was registered")
            .last_seen_at
    }
}

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

/// A caller at `millis`, on the ordinary trust tier and a full-bandwidth session.
///
/// `Established` and not `Trusted`: the tier a real user has is the tier the tests
/// should meet the limits on, or the limits are only ever exercised by people who do
/// not have them.
fn caller(account: u128, device: u128, millis: i64) -> Caller {
    Caller::new(
        id(account),
        id(device),
        TrustTier::Established,
        BandwidthMode::Normal,
        ts(millis),
    )
}

/// The same, on a named bandwidth mode.
fn caller_on(account: u128, device: u128, mode: BandwidthMode, millis: i64) -> Caller {
    Caller::new(
        id(account),
        id(device),
        TrustTier::Established,
        mode,
        ts(millis),
    )
}

/// Asserts that a call failed with one specific protocol code.
///
/// The code and not the message, so that rewording an internal string does not break
/// a test while a change of failure *class* does.
#[track_caller]
fn expect_code<T: std::fmt::Debug>(result: Result<T>, code: u32) {
    match result {
        Ok(value) => panic!("expected error {code}, got success: {value:?}"),
        Err(error) => assert_eq!(error.code(), code, "wrong failure class: {error}"),
    }
}

/// The fanout a call was expected to produce, or a panic naming the silence.
#[track_caller]
fn spoken(fanout: Option<Fanout>) -> Fanout {
    fanout.expect("expected a presence frame, got silence")
}

/// Asserts that nothing was published.
#[track_caller]
fn silent(fanout: Option<Fanout>) {
    assert!(fanout.is_none(), "expected silence, got {fanout:?}");
}

/// The state a fanout carries.
#[track_caller]
fn published(fanout: Option<Fanout>) -> PresenceState {
    spoken(fanout).event.state
}

/// One subject's row in a snapshot, or a panic naming what came back instead.
#[track_caller]
fn row(events: &[PresenceEvent], subject: u128) -> &PresenceEvent {
    events
        .iter()
        .find(|event| event.user_id == id(subject))
        .unwrap_or_else(|| panic!("no row for {subject} in {events:?}"))
}

/// The entry one device holds, or a panic.
#[track_caller]
fn entry(entries: &[PresenceEntry], device: u128) -> &PresenceEntry {
    entries
        .iter()
        .find(|entry| entry.device_id == id(device))
        .unwrap_or_else(|| panic!("no entry for device {device} in {entries:?}"))
}

// --- arriving and leaving --------------------------------------------------------

#[tokio::test]
async fn a_connecting_device_comes_online_and_the_second_one_says_nothing() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness.device(ALICE, ALICE_LAPTOP).await;

    let first = spoken(
        harness
            .presence
            .connected(&caller(ALICE, ALICE_PHONE, MINUTE))
            .await
            .expect("a registered device may connect"),
    );
    assert_eq!(first.event.state, PresenceState::Online);
    assert_eq!(
        first.subject_id,
        id(ALICE),
        "presence is published to the subject's own topic, not a conversation's"
    );
    assert_eq!(
        first.exclude_device,
        Some(id(ALICE_PHONE)),
        "the socket that reported the state already knows it"
    );
    assert!(
        first.event.last_seen.is_none(),
        "last seen is per viewer and a broadcast is encoded once"
    );

    // Brief section 156: the account was already Online, so the laptop arriving
    // changes nothing anybody can observe.
    silent(
        harness
            .presence
            .connected(&caller(ALICE, ALICE_LAPTOP, MINUTE + SECOND))
            .await
            .expect("a second device may connect"),
    );

    assert_eq!(
        harness.metric("migo_presence_sessions_total{event=\"connected\"}"),
        Some(2.0)
    );
    assert_eq!(
        harness.metric("migo_presence_broadcasts_total{state=\"Online\"}"),
        Some(1.0)
    );
}

#[tokio::test]
async fn the_last_device_leaving_goes_offline_and_stamps_the_row() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness.device(ALICE, ALICE_LAPTOP).await;
    let phone = caller(ALICE, ALICE_PHONE, MINUTE);
    let laptop = caller(ALICE, ALICE_LAPTOP, MINUTE);
    harness.presence.connected(&phone).await.unwrap();
    harness.presence.connected(&laptop).await.unwrap();

    // One of two leaving is not an event: the account is still online elsewhere.
    silent(
        harness
            .presence
            .disconnected(&caller(ALICE, ALICE_LAPTOP, MINUTE + 10 * SECOND))
            .await
            .expect("a device may always disconnect"),
    );
    assert_eq!(
        published(
            harness
                .presence
                .disconnected(&caller(ALICE, ALICE_PHONE, MINUTE + 20 * SECOND))
                .await
                .unwrap()
        ),
        PresenceState::Offline,
        "the last device leaving is the one that takes the account offline"
    );

    assert_eq!(
        harness.device_last_seen(ALICE_LAPTOP).await,
        ts(MINUTE + 10 * SECOND),
        "a clean disconnect is the moment last-seen is recorded"
    );
    assert_eq!(
        harness.device_last_seen(ALICE_PHONE).await,
        ts(MINUTE + 20 * SECOND)
    );
    assert_eq!(
        harness.metric("migo_presence_sessions_total{event=\"disconnected\"}"),
        Some(2.0)
    );
}

#[tokio::test]
async fn a_disconnect_is_immediate_rather_than_a_lifetime_away() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .presence
        .connected(&caller(ALICE, ALICE_PHONE, MINUTE))
        .await
        .unwrap();
    harness
        .presence
        .disconnected(&caller(ALICE, ALICE_PHONE, MINUTE + SECOND))
        .await
        .unwrap();

    assert!(
        harness
            .presence
            .devices(&caller(ALICE, ALICE_PHONE, MINUTE + 2 * SECOND))
            .await
            .unwrap()
            .is_empty(),
        "the entry is cleared, not left to expire"
    );
}

// --- heartbeats -----------------------------------------------------------------

#[tokio::test]
async fn a_heartbeat_refreshes_the_entry_without_moving_since_or_speaking() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .presence
        .connected(&caller(ALICE, ALICE_PHONE, MINUTE))
        .await
        .unwrap();

    silent(
        harness
            .presence
            .heartbeat(&caller(ALICE, ALICE_PHONE, MINUTE + 10 * SECOND))
            .await
            .expect("a live device may heartbeat"),
    );

    let entries = harness
        .presence
        .devices(&caller(ALICE, ALICE_PHONE, MINUTE + 10 * SECOND))
        .await
        .unwrap();
    let refreshed = entry(&entries, ALICE_PHONE);
    assert_eq!(
        refreshed.since,
        ts(MINUTE),
        "online since 09:12 must not reset every heartbeat"
    );
    assert_eq!(
        refreshed.expires_at,
        ts(MINUTE + 10 * SECOND + LIFETIME),
        "the deadline moves even though the state did not"
    );
    assert_eq!(
        harness.device_last_seen(ALICE_PHONE).await,
        ts(SECOND),
        "a heartbeat must not write a row: that is a write per device per interval"
    );
    assert_eq!(harness.metric("migo_presence_heartbeats_total"), Some(1.0));
    assert_eq!(harness.metric("migo_presence_revivals_total"), Some(0.0));
}

#[tokio::test]
async fn an_entry_that_expired_under_a_live_socket_is_revived_and_announced() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .presence
        .connected(&caller(ALICE, ALICE_PHONE, MINUTE))
        .await
        .unwrap();

    // A suspended laptop: the socket survived, the entry did not.
    let late = MINUTE + LIFETIME;
    assert_eq!(
        published(
            harness
                .presence
                .heartbeat(&caller(ALICE, ALICE_PHONE, late))
                .await
                .unwrap()
        ),
        PresenceState::Online,
        "watchers were told Offline by the expiry, so they have to be told again"
    );
    let entries = harness
        .presence
        .devices(&caller(ALICE, ALICE_PHONE, late))
        .await
        .unwrap();
    assert_eq!(
        entry(&entries, ALICE_PHONE).since,
        ts(late),
        "a revived device entered its state now, not before the gap"
    );
    assert_eq!(harness.metric("migo_presence_revivals_total"), Some(1.0));
}

#[tokio::test]
async fn a_slower_session_gets_a_proportionally_longer_lifetime() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;

    harness
        .presence
        .connected(&caller_on(
            ALICE,
            ALICE_PHONE,
            BandwidthMode::UltraLowData,
            MINUTE,
        ))
        .await
        .unwrap();

    let entries = harness
        .presence
        .devices(&caller(ALICE, ALICE_PHONE, MINUTE))
        .await
        .unwrap();
    assert_eq!(
        entry(&entries, ALICE_PHONE).expires_at,
        ts(MINUTE + 4 * LIFETIME),
        "a client told to heartbeat four times more slowly must not blink offline \
         between two punctual heartbeats"
    );
}

// --- invisibility ---------------------------------------------------------------

#[tokio::test]
async fn invisible_is_published_as_offline_and_never_as_itself() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .person(BOB, "bob", BOB_LAPTOP, Visibility::Everyone)
        .await;
    harness
        .presence
        .connected(&caller(ALICE, ALICE_PHONE, MINUTE))
        .await
        .unwrap();

    assert_eq!(
        published(
            harness
                .presence
                .set(
                    &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
                    PresenceUpdate {
                        state: PresenceState::Invisible,
                        custom_status: None,
                    },
                )
                .await
                .unwrap()
        ),
        PresenceState::Offline,
        "there is no code path that publishes Invisible"
    );

    // What Bob sees, and what Alice sees about herself, are deliberately different.
    let seen_by_bob = harness
        .presence
        .snapshot(
            &caller(BOB, BOB_LAPTOP, 2 * MINUTE),
            &[id(ALICE)],
            Detail::StateOnly,
        )
        .await
        .unwrap();
    assert_eq!(row(&seen_by_bob, ALICE).state, PresenceState::Offline);

    let seen_by_alice = harness
        .presence
        .snapshot(
            &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
            &[id(ALICE)],
            Detail::StateOnly,
        )
        .await
        .unwrap();
    assert_eq!(
        row(&seen_by_alice, ALICE).state,
        PresenceState::Invisible,
        "a user can see that they are hidden; that is the whole point of hiding"
    );
    assert_eq!(
        harness.metric("migo_presence_broadcasts_total{state=\"Invisible\"}"),
        None
    );
}

#[tokio::test]
async fn a_reconnecting_device_inherits_invisibility_instead_of_flashing_online() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness.device(ALICE, ALICE_LAPTOP).await;
    harness
        .presence
        .connected(&caller(ALICE, ALICE_PHONE, MINUTE))
        .await
        .unwrap();
    harness
        .presence
        .set(
            &caller(ALICE, ALICE_PHONE, MINUTE),
            PresenceUpdate {
                state: PresenceState::Invisible,
                custom_status: None,
            },
        )
        .await
        .unwrap();

    silent(
        harness
            .presence
            .connected(&caller(ALICE, ALICE_LAPTOP, 2 * MINUTE))
            .await
            .expect("a second device may connect"),
    );
    let entries = harness
        .presence
        .devices(&caller(ALICE, ALICE_PHONE, 2 * MINUTE))
        .await
        .unwrap();
    assert_eq!(
        entry(&entries, ALICE_LAPTOP).state,
        PresenceState::Invisible,
        "a user hiding on their phone must not be exposed by their laptop reconnecting"
    );
}

#[tokio::test]
async fn coming_back_from_invisible_is_the_users_own_decision() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .presence
        .connected(&caller(ALICE, ALICE_PHONE, MINUTE))
        .await
        .unwrap();
    harness
        .presence
        .set(
            &caller(ALICE, ALICE_PHONE, MINUTE),
            PresenceUpdate {
                state: PresenceState::Invisible,
                custom_status: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        published(
            harness
                .presence
                .set(
                    &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
                    PresenceUpdate {
                        state: PresenceState::Online,
                        custom_status: None,
                    },
                )
                .await
                .unwrap()
        ),
        PresenceState::Online,
        "inheriting invisibility can only fail in the direction the user can fix"
    );
}

// --- projection across devices --------------------------------------------------

#[tokio::test]
async fn the_strongest_state_across_devices_wins_and_survives_its_owner_leaving() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness.device(ALICE, ALICE_LAPTOP).await;
    harness
        .presence
        .connected(&caller(ALICE, ALICE_PHONE, MINUTE))
        .await
        .unwrap();
    harness
        .presence
        .connected(&caller(ALICE, ALICE_LAPTOP, MINUTE))
        .await
        .unwrap();

    // Busy outranks Online because Busy is only ever set on purpose.
    assert_eq!(
        published(
            harness
                .presence
                .set(
                    &caller(ALICE, ALICE_LAPTOP, MINUTE + 10 * SECOND),
                    PresenceUpdate {
                        state: PresenceState::Busy,
                        custom_status: None,
                    },
                )
                .await
                .unwrap()
        ),
        PresenceState::Busy
    );
    // And the phone dropping to Away does not undo it, because the laptop still says Busy.
    silent(
        harness
            .presence
            .set(
                &caller(ALICE, ALICE_PHONE, MINUTE + 20 * SECOND),
                PresenceUpdate {
                    state: PresenceState::Away,
                    custom_status: None,
                },
            )
            .await
            .unwrap(),
    );
    // The laptop leaving hands the account back to whatever the phone said.
    assert_eq!(
        published(
            harness
                .presence
                .disconnected(&caller(ALICE, ALICE_LAPTOP, MINUTE + 30 * SECOND))
                .await
                .unwrap()
        ),
        PresenceState::Away
    );
}

#[tokio::test]
async fn declaring_the_same_state_twice_costs_one_frame() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .presence
        .connected(&caller(ALICE, ALICE_PHONE, MINUTE))
        .await
        .unwrap();
    let away = PresenceUpdate {
        state: PresenceState::Away,
        custom_status: None,
    };

    assert_eq!(
        published(
            harness
                .presence
                .set(&caller(ALICE, ALICE_PHONE, 2 * MINUTE), away.clone())
                .await
                .unwrap()
        ),
        PresenceState::Away
    );
    silent(
        harness
            .presence
            .set(&caller(ALICE, ALICE_PHONE, 3 * MINUTE), away)
            .await
            .unwrap(),
    );

    let entries = harness
        .presence
        .devices(&caller(ALICE, ALICE_PHONE, 3 * MINUTE))
        .await
        .unwrap();
    assert_eq!(
        entry(&entries, ALICE_PHONE).since,
        ts(2 * MINUTE),
        "since moves when the state moves, and a re-declaration is not a move"
    );
    assert_eq!(
        harness.metric("migo_presence_updates_total{outcome=\"accepted\"}"),
        Some(1.0)
    );
    assert_eq!(
        harness.metric("migo_presence_updates_total{outcome=\"unchanged\"}"),
        Some(1.0)
    );
}

// --- refusals -------------------------------------------------------------------

#[tokio::test]
async fn a_custom_status_is_refused_rather_than_stored_where_it_would_evaporate() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;

    expect_code(
        harness
            .presence
            .set(
                &caller(ALICE, ALICE_PHONE, MINUTE),
                PresenceUpdate {
                    state: PresenceState::Online,
                    custom_status: Some("in a meeting".to_string()),
                },
            )
            .await,
        codes::FEATURE_DISABLED,
    );
    assert_eq!(
        harness.metric("migo_presence_updates_total{outcome=\"unsupported\"}"),
        Some(1.0)
    );
}

#[tokio::test]
async fn a_state_this_build_does_not_know_is_refused() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;

    expect_code(
        harness
            .presence
            .set(
                &caller(ALICE, ALICE_PHONE, MINUTE),
                PresenceUpdate {
                    state: PresenceState::Unknown,
                    custom_status: None,
                },
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(
        harness.metric("migo_presence_updates_total{outcome=\"invalid\"}"),
        Some(1.0)
    );
}

#[tokio::test]
async fn a_caller_without_a_device_is_refused_on_every_path() {
    let harness = Harness::new();
    let nameless = Caller::new(
        id(ALICE),
        id(0),
        TrustTier::Established,
        BandwidthMode::Normal,
        ts(MINUTE),
    );

    // A nil device id is the same cache field for every session of an account, so
    // accepting one would let a single caller overwrite the presence of every device
    // the account really has.
    expect_code(
        harness.presence.connected(&nameless).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.presence.heartbeat(&nameless).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.presence.disconnected(&nameless).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.presence.devices(&nameless).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness
            .presence
            .snapshot(&nameless, &[id(BOB)], Detail::StateOnly)
            .await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness
            .presence
            .set(
                &nameless,
                PresenceUpdate {
                    state: PresenceState::Online,
                    custom_status: None,
                },
            )
            .await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn a_flood_of_updates_is_refused_without_losing_the_state_that_got_through() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    let flooding = Caller::new(
        id(ALICE),
        id(ALICE_PHONE),
        TrustTier::New,
        BandwidthMode::Normal,
        ts(MINUTE),
    );

    let mut accepted = 0u32;
    let mut refusal = None;
    for turn in 0..400u32 {
        let state = if turn % 2 == 0 {
            PresenceState::Away
        } else {
            PresenceState::Online
        };
        match harness
            .presence
            .set(
                &flooding,
                PresenceUpdate {
                    state,
                    custom_status: None,
                },
            )
            .await
        {
            Ok(_) => accepted += 1,
            Err(error) => {
                refusal = Some(error);
                break;
            }
        }
    }
    let error = refusal.expect("an unbounded loop of presence updates must eventually be refused");
    assert_eq!(error.code(), codes::RATE_LIMITED);
    assert!(
        accepted > 0,
        "a limit that refuses the first call is a limit nobody can use"
    );
    assert_eq!(
        harness.metric("migo_presence_updates_total{outcome=\"rate_limited\"}"),
        Some(1.0)
    );

    // The refusal must not have eaten the last state that was accepted.
    let entries = harness
        .presence
        .devices(&flooding)
        .await
        .expect("reading your own devices is not rate limited");
    assert_eq!(entries.len(), 1);

    // And the bucket refills, so the refusal is a delay rather than a ban.
    harness
        .presence
        .set(
            &Caller::new(
                id(ALICE),
                id(ALICE_PHONE),
                TrustTier::New,
                BandwidthMode::Normal,
                ts(MINUTE + 10 * SECOND),
            ),
            PresenceUpdate {
                state: PresenceState::Busy,
                custom_status: None,
            },
        )
        .await
        .expect("ten seconds of refill is enough for one more update");
}

// --- snapshots ------------------------------------------------------------------

#[tokio::test]
async fn a_snapshot_answers_once_per_subject_in_the_order_it_was_asked() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .person(BOB, "bob", BOB_LAPTOP, Visibility::Everyone)
        .await;
    harness
        .person(CAROL, "carol", CAROL_PHONE, Visibility::Everyone)
        .await;
    harness
        .presence
        .connected(&caller(BOB, BOB_LAPTOP, MINUTE))
        .await
        .unwrap();

    let events = harness
        .presence
        .snapshot(
            &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
            &[id(BOB), id(CAROL), id(BOB), id(0), id(ALICE)],
            Detail::StateOnly,
        )
        .await
        .unwrap();

    assert_eq!(
        events.iter().map(|event| event.user_id).collect::<Vec<_>>(),
        vec![id(BOB), id(CAROL), id(ALICE)],
        "a repeated id would render as two contacts, and a nil id as a ghost"
    );
    assert_eq!(row(&events, BOB).state, PresenceState::Online);
    assert_eq!(row(&events, CAROL).state, PresenceState::Offline);
    assert_eq!(harness.metric("migo_presence_snapshots_total"), Some(1.0));
    assert_eq!(
        harness.metric("migo_presence_snapshot_subjects_count"),
        Some(1.0)
    );
}

#[tokio::test]
async fn a_snapshot_larger_than_the_bound_is_clamped_rather_than_refused() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;

    let subjects: Vec<Id> = (0..MAX_SNAPSHOT_SUBJECTS + 8)
        .map(|n| id(1_000 + n as u128))
        .collect();
    let events = harness
        .presence
        .snapshot(
            &caller(ALICE, ALICE_PHONE, MINUTE),
            &subjects,
            Detail::StateOnly,
        )
        .await
        .expect("a contact list that is too long renders truncated, it does not fail");
    assert_eq!(events.len(), MAX_SNAPSHOT_SUBJECTS);

    // And an empty ask is an empty answer rather than an error.
    assert!(harness
        .presence
        .snapshot(&caller(ALICE, ALICE_PHONE, MINUTE), &[], Detail::StateOnly)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_block_hides_presence_in_both_directions() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .person(BOB, "bob", BOB_LAPTOP, Visibility::Everyone)
        .await;
    harness
        .person(CAROL, "carol", CAROL_PHONE, Visibility::Everyone)
        .await;
    harness
        .presence
        .connected(&caller(BOB, BOB_LAPTOP, MINUTE))
        .await
        .unwrap();
    harness
        .presence
        .connected(&caller(CAROL, CAROL_PHONE, MINUTE))
        .await
        .unwrap();
    harness
        .edge(ALICE, BOB, RelationshipKind::Block, false)
        .await;
    harness
        .edge(CAROL, ALICE, RelationshipKind::Block, false)
        .await;

    let events = harness
        .presence
        .snapshot(
            &caller(ALICE, ALICE_PHONE, 2 * MINUTE),
            &[id(BOB), id(CAROL)],
            Detail::WithLastSeen,
        )
        .await
        .unwrap();
    assert_eq!(
        row(&events, BOB).state,
        PresenceState::Offline,
        "somebody Alice blocked is somebody Alice does not watch"
    );
    assert_eq!(
        row(&events, CAROL).state,
        PresenceState::Offline,
        "and somebody who blocked Alice is somebody Alice may not watch"
    );
    assert!(row(&events, BOB).last_seen.is_none());
    assert!(row(&events, CAROL).last_seen.is_none());
}

// --- last seen ------------------------------------------------------------------

#[tokio::test]
async fn last_seen_is_disclosed_only_to_the_audience_the_subject_chose() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .person(BOB, "bob", BOB_LAPTOP, Visibility::Everyone)
        .await;
    harness
        .person(CAROL, "carol", CAROL_PHONE, Visibility::Nobody)
        .await;
    harness
        .person(DAVE, "dave", DAVE_PHONE, Visibility::Friends)
        .await;
    for (account, device) in [(BOB, BOB_LAPTOP), (CAROL, CAROL_PHONE), (DAVE, DAVE_PHONE)] {
        harness
            .presence
            .connected(&caller(account, device, MINUTE))
            .await
            .unwrap();
        harness
            .presence
            .disconnected(&caller(account, device, 2 * MINUTE))
            .await
            .unwrap();
    }

    let viewer = caller(ALICE, ALICE_PHONE, 3 * MINUTE);
    let events = harness
        .presence
        .snapshot(
            &viewer,
            &[id(BOB), id(CAROL), id(DAVE)],
            Detail::WithLastSeen,
        )
        .await
        .unwrap();
    assert_eq!(row(&events, BOB).last_seen, Some(ts(2 * MINUTE)));
    assert_eq!(
        row(&events, CAROL).last_seen,
        None,
        "Nobody means nobody, including the people who can see the account exists"
    );
    assert_eq!(
        row(&events, DAVE).last_seen,
        None,
        "a Friends-only field is not readable by a stranger"
    );

    // A pending request is not a friendship, or asking to be a friend would be a way
    // of reading a Friends-only field.
    harness
        .edge(ALICE, DAVE, RelationshipKind::Friend, false)
        .await;
    let pending = harness
        .presence
        .snapshot(&viewer, &[id(DAVE)], Detail::WithLastSeen)
        .await
        .unwrap();
    assert_eq!(row(&pending, DAVE).last_seen, None);

    harness
        .edge(ALICE, DAVE, RelationshipKind::Friend, true)
        .await;
    let accepted = harness
        .presence
        .snapshot(&viewer, &[id(DAVE)], Detail::WithLastSeen)
        .await
        .unwrap();
    assert_eq!(row(&accepted, DAVE).last_seen, Some(ts(2 * MINUTE)));
}

#[tokio::test]
async fn last_seen_is_never_disclosed_about_someone_who_is_hidden_but_connected() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .person(BOB, "bob", BOB_LAPTOP, Visibility::Everyone)
        .await;
    harness
        .presence
        .connected(&caller(BOB, BOB_LAPTOP, MINUTE))
        .await
        .unwrap();
    harness
        .presence
        .set(
            &caller(BOB, BOB_LAPTOP, MINUTE),
            PresenceUpdate {
                state: PresenceState::Invisible,
                custom_status: None,
            },
        )
        .await
        .unwrap();

    let events = harness
        .presence
        .snapshot(
            &caller(ALICE, ALICE_PHONE, MINUTE + SECOND),
            &[id(BOB)],
            Detail::WithLastSeen,
        )
        .await
        .unwrap();
    assert_eq!(row(&events, BOB).state, PresenceState::Offline);
    assert_eq!(
        row(&events, BOB).last_seen,
        None,
        "last seen four seconds ago would undo the hiding with arithmetic"
    );
}

#[tokio::test]
async fn last_seen_is_not_resolved_for_a_caller_that_did_not_ask() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;
    harness
        .person(BOB, "bob", BOB_LAPTOP, Visibility::Everyone)
        .await;

    let events = harness
        .presence
        .snapshot(
            &caller(ALICE, ALICE_PHONE, MINUTE),
            &[id(BOB)],
            Detail::StateOnly,
        )
        .await
        .unwrap();
    assert_eq!(row(&events, BOB).last_seen, None);
    assert_eq!(
        harness.metric("migo_presence_last_seen_total{outcome=\"skipped\"}"),
        Some(0.0),
        "a field nobody asked for is not skipped work, it is work that was never scheduled"
    );
}

#[tokio::test]
async fn last_seen_work_stops_at_the_bound_and_says_so() {
    let harness = Harness::new();
    harness
        .person(ALICE, "alice", ALICE_PHONE, Visibility::Everyone)
        .await;

    let over = 6usize;
    let subjects: Vec<Id> = (0..MAX_LAST_SEEN_LOOKUPS + over)
        .map(|n| id(2_000 + n as u128))
        .collect();
    let events = harness
        .presence
        .snapshot(
            &caller(ALICE, ALICE_PHONE, MINUTE),
            &subjects,
            Detail::WithLastSeen,
        )
        .await
        .unwrap();

    assert_eq!(events.len(), MAX_LAST_SEEN_LOOKUPS + over);
    assert!(events.iter().all(|event| event.last_seen.is_none()));
    assert_eq!(
        harness.metric("migo_presence_last_seen_total{outcome=\"withheld\"}"),
        Some(MAX_LAST_SEEN_LOOKUPS as f64),
        "a subject with no profile is withheld, not disclosed"
    );
    assert_eq!(
        harness.metric("migo_presence_last_seen_total{outcome=\"skipped\"}"),
        Some(over as f64),
        "a room subscribe must not turn into two hundred round trips"
    );
}

// --- cadence --------------------------------------------------------------------

#[tokio::test]
async fn the_cadence_table_follows_the_bandwidth_mode() {
    let harness = Harness::new();

    let normal = harness.presence.cadence(BandwidthMode::Normal);
    assert_eq!(normal.heartbeat_ms, 30_000);
    assert_eq!(normal.min_interval_ms, 5_000);
    assert!(normal.typing);
    assert_eq!(normal.scope, PresenceScope::Everything);

    let low = harness.presence.cadence(BandwidthMode::LowData);
    assert_eq!(low.heartbeat_ms, 60_000);
    assert_eq!(low.min_interval_ms, 20_000);
    assert!(
        low.typing,
        "LowData throttles presence, it does not remove typing"
    );
    assert_eq!(low.scope, PresenceScope::OpenOnly);

    let ultra = harness.presence.cadence(BandwidthMode::UltraLowData);
    assert_eq!(ultra.heartbeat_ms, 120_000);
    assert_eq!(ultra.min_interval_ms, 30_000);
    assert!(!ultra.typing);
    assert_eq!(ultra.scope, PresenceScope::OpenOnly);

    // A client that asked the server to decide, and a peer whose enum we do not know,
    // both get the cadence that renders correctly rather than the cheapest one.
    assert_eq!(harness.presence.cadence(BandwidthMode::Auto), normal);
    assert_eq!(harness.presence.cadence(BandwidthMode::Unknown), normal);
}

#[tokio::test]
async fn the_cadence_floor_holds_at_the_shortest_configurable_heartbeat() {
    // A sixth of one second is not a floor, so the floor is a second.
    let tight = cadence_for(BandwidthMode::Normal, 10);
    assert_eq!(
        tight.heartbeat_ms, 1_000,
        "a small number is clamped, not refused"
    );
    assert_eq!(tight.min_interval_ms, 1_000);

    // And the multiplier cannot push the advertised heartbeat past the ceiling.
    let loose = cadence_for(BandwidthMode::UltraLowData, 300_000);
    assert_eq!(loose.heartbeat_ms, 300_000);
    assert_eq!(loose.presence_ttl().as_millis(), 900_000);
}

#[tokio::test]
async fn the_heartbeat_comes_from_the_gateway_that_advertises_it() {
    let mut config = Config::default();
    config.gateway.heartbeat_ms = 45_000;
    assert_eq!(
        PresenceConfig::from_gateway(&config.gateway).heartbeat_ms,
        45_000
    );

    // A config built in code can be out of range; presence clamps rather than refusing
    // to start, because a typo should not be an outage.
    config.gateway.heartbeat_ms = 10;
    assert_eq!(
        PresenceConfig::from_gateway(&config.gateway).heartbeat_ms,
        1_000
    );
}

// --- observability --------------------------------------------------------------

#[tokio::test]
async fn every_series_exists_before_anything_has_happened() {
    let harness = Harness::new();

    for series in [
        "migo_presence_updates_total{outcome=\"accepted\"}",
        "migo_presence_updates_total{outcome=\"unchanged\"}",
        "migo_presence_updates_total{outcome=\"invalid\"}",
        "migo_presence_updates_total{outcome=\"unsupported\"}",
        "migo_presence_updates_total{outcome=\"rate_limited\"}",
        "migo_presence_broadcasts_total{state=\"Offline\"}",
        "migo_presence_broadcasts_total{state=\"Online\"}",
        "migo_presence_broadcasts_total{state=\"Away\"}",
        "migo_presence_broadcasts_total{state=\"Busy\"}",
        "migo_presence_sessions_total{event=\"connected\"}",
        "migo_presence_sessions_total{event=\"disconnected\"}",
        "migo_presence_last_seen_total{outcome=\"disclosed\"}",
        "migo_presence_last_seen_total{outcome=\"withheld\"}",
        "migo_presence_last_seen_total{outcome=\"skipped\"}",
        "migo_presence_heartbeats_total",
        "migo_presence_revivals_total",
        "migo_presence_snapshots_total",
        "migo_presence_snapshot_subjects_count",
    ] {
        assert_eq!(
            harness.metric(series),
            Some(0.0),
            "an alert cannot be written against {series} until it exists"
        );
    }

    // The two states that are never broadcast have no series at all, so a dashboard
    // cannot conclude from a flat zero that nobody uses invisibility.
    assert_eq!(
        harness.metric("migo_presence_broadcasts_total{state=\"Invisible\"}"),
        None
    );
    assert_eq!(
        harness.metric("migo_presence_broadcasts_total{state=\"Unknown\"}"),
        None
    );
}
