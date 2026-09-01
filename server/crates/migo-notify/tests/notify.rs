//! A push is a wake-up, not a message. These tests are about the difference.
//!
//! Most of this crate is a fan-out with a rate limiter in front of it, and a fan-out
//! that tells the wrong person is caught by the wrong person. The parts that are not
//! caught that way are the ones this suite exists for:
//!
//! **The payload cannot carry prose.** Brief section 44 forbids message text, a voice
//! note, or signalling in a push, and section 77 says the payload must be minimum and
//! generic. [`Wakeup`] has no string field, so the guarantee is structural — but a fake
//! transport that records every payload lets a test prove that a distinctive sentence a
//! sender wrote reaches the wire nowhere, and that the only text a phone sees is the
//! fixed generic alert.
//!
//! **The token is a secret in transit and a hash at rest.** Section 77 stores the token
//! hashed and section 174 forbids it from every log, metric, and error. The stored form
//! differs from the raw token, the raw token is not recoverable from anything the crate
//! hands back, and it appears in no rendered metric.
//!
//! **A caller with no identity reaches no method, and pays no bucket.** The identity
//! check runs before the rate-limit charge, so an unauthenticated caller cannot even
//! spend a token bucket, let alone read somebody's inbox.
//!
//! **A withheld wake-up is a success.** Connected, coalesced, budgeted, and stale are
//! decisions, not errors; the inbox row is written and the badge is right regardless.
//!
//! The rate limiter is the real one over a real cache, so the arithmetic is part of the
//! test: an account's burst is two hundred and a badge costs one, which is why the
//! budget test counts to two hundred.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use migo_cache::traits::RoutingCache;
use migo_cache::{MemoryCache, SessionRoute, Ttl};
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Random, Result, Secret, SeededRandom, Timestamp};
use migo_protocol::{codes, NotificationKind, Platform};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::model::{
    notification_kind, DeviceStatus, NewAccount, NewDevice, Notification, PushProvider,
};
use migo_store::traits::{AccountStore, DeviceStore, NotifyStore};
use migo_store::MemoryStore;

use migo_notify::{
    Caller, Delivery, Event, Inbox, Item, Notifications, Notifier, NotifyConfig, PushSender,
    RawToken, Sent, Target, TokenKeeper, Wakeup, COALESCE_WINDOW_MS, MAX_INBOX_PAGE, MAX_TOKEN_LEN,
    REGISTRATION_TTL_MS,
};

const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const DAY: i64 = 24 * 60 * MINUTE;
const NOW: i64 = 1_700_000_000 * SECOND;

const ALICE: u128 = 1;
const BOB: u128 = 2;
const CAROL: u128 = 3;

const ALICE_PHONE: u128 = 101;
const ALICE_TABLET: u128 = 102;
const ALICE_LAPTOP: u128 = 103;
const BOB_PHONE: u128 = 201;
const CAROL_PHONE: u128 = 301;

/// The deployment secret the service derives its token keys from. A test that wants to
/// open a sealed token derives a [`TokenKeeper`] from the same bytes.
const ROOT_SECRET: &[u8] = b"migo-notify integration test root secret v1";

/// A raw push token with a shape no hash or ciphertext of it could accidentally contain.
const RAW_TOKEN: &str = "fcm-RAW-TOKEN-marker-9f8e7d6c5b4a3210-do-not-log";

/// A room and a subject id, so a wake-up has something to point at.
const ROOM: u128 = 7_000;
const SUBJECT: u128 = 8_000;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

fn caller(account: u128, device: u128) -> Caller {
    Caller::new(id(account), id(device), TrustTier::Established, ts(NOW))
}

/// What the fake transport should say when handed a wake-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reply {
    /// Accepted for delivery.
    Delivered,
    /// The token is dead; the caller must retire the registration.
    Unregistered,
    /// The provider is refusing traffic; the token is fine.
    Throttled,
    /// The attempt failed in a way the provider did not describe.
    Error,
}

/// One wake-up as the transport received it, with the token in the clear.
///
/// This is the whole point of the fake: it keeps every byte the crate handed a provider,
/// so a test can assert what is — and is not — in it.
#[derive(Clone, Debug)]
struct Handed {
    device_id: Id,
    platform: Platform,
    provider: i16,
    token: String,
    wakeup: Wakeup,
}

/// A push provider that records instead of sends.
struct FakeSender {
    handed: Mutex<Vec<Handed>>,
    reply: Mutex<Reply>,
    /// When `Some`, the sender handles only this provider number; when `None`, all.
    only: Mutex<Option<i16>>,
}

impl FakeSender {
    fn new() -> Self {
        Self {
            handed: Mutex::new(Vec::new()),
            reply: Mutex::new(Reply::Delivered),
            only: Mutex::new(None),
        }
    }

    fn set_reply(&self, reply: Reply) {
        *self.reply.lock() = reply;
    }

    fn handle_only(&self, provider: i16) {
        *self.only.lock() = Some(provider);
    }

    fn calls(&self) -> usize {
        self.handed.lock().len()
    }

    fn last(&self) -> Handed {
        self.handed
            .lock()
            .last()
            .cloned()
            .expect("a wake-up was sent")
    }

    fn all(&self) -> Vec<Handed> {
        self.handed.lock().clone()
    }

    /// Every string the transport saw, concatenated: tokens and the debug of every
    /// wake-up and target. What a leak would have to pass through.
    fn transcript(&self) -> String {
        let mut out = String::new();
        for handed in self.handed.lock().iter() {
            out.push_str(&handed.token);
            out.push('\n');
            out.push_str(&format!("{handed:?}"));
            out.push('\n');
            out.push_str(handed.wakeup.alert());
            out.push('\n');
        }
        out
    }
}

#[async_trait]
impl PushSender for FakeSender {
    async fn send(&self, target: Target<'_>, wakeup: &Wakeup) -> Result<Sent> {
        self.handed.lock().push(Handed {
            device_id: target.device_id,
            platform: target.platform,
            provider: target.provider,
            token: target.token.to_string(),
            wakeup: *wakeup,
        });
        let reply = *self.reply.lock();
        match reply {
            Reply::Delivered => Ok(Sent::Delivered),
            Reply::Unregistered => Ok(Sent::Unregistered),
            Reply::Throttled => Ok(Sent::Throttled),
            Reply::Error => Err(migo_protocol::fault::internal("push provider unreachable")),
        }
    }

    fn handles(&self, provider: i16) -> bool {
        match *self.only.lock() {
            Some(only) => only == provider,
            None => true,
        }
    }
}

type TestNotify =
    Notifications<MemoryStore, MemoryCache, CacheRateLimiter<MemoryCache>, FakeSender>;

/// Everything a test needs, with the real limiter and cache and a recording transport.
struct Harness {
    notify: TestNotify,
    store: Arc<MemoryStore>,
    cache: Arc<MemoryCache>,
    sender: Arc<FakeSender>,
    registry: Registry,
}

impl Harness {
    fn new() -> Self {
        Self::configured(NotifyConfig::default())
    }

    fn configured(config: NotifyConfig) -> Self {
        let settings = Config::default();
        let store = Arc::new(MemoryStore::new());
        let cache = Arc::new(MemoryCache::new());
        let sender = Arc::new(FakeSender::new());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(
            Arc::new(MemoryCache::new()),
            policies,
            &registry,
        ));
        let notify = Notifications::new(
            Arc::clone(&store),
            Arc::clone(&cache),
            limiter,
            Arc::clone(&sender),
            Box::new(SeededRandom::new(42)) as Box<dyn Random>,
            ROOT_SECRET,
            config,
            &registry,
        );
        Self {
            notify,
            store,
            cache,
            sender,
            registry,
        }
    }

    async fn account(&self, account: u128, username: &str) {
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
    }

    async fn device(&self, account: u128, device: u128, platform: Platform) {
        self.store
            .register_device(NewDevice {
                device_id: id(device),
                account_id: id(account),
                platform,
                display_name: "Test device".to_string(),
                app_version: "1.0.0".to_string(),
                os_version: None,
                device_model: None,
                status: DeviceStatus::Active,
                public_credential: None,
                created_at: ts(SECOND),
            })
            .await
            .expect("a fresh device id is free");
    }

    async fn revoke(&self, device: u128) {
        self.store
            .revoke_device(id(device), ts(NOW))
            .await
            .expect("a device can be revoked");
    }

    /// Marks a device connected, so the wake path finds a live socket for it.
    async fn connect(&self, account: u128, device: u128) {
        self.cache
            .bind_session(
                SessionRoute {
                    account_id: id(account),
                    device_id: id(device),
                    node_id: "node-a".to_string(),
                    connected_at: ts(NOW),
                    expires_at: ts(NOW + DAY),
                },
                Ttl::from_seconds(300),
                ts(NOW),
            )
            .await
            .expect("the routing cache takes a binding");
    }

    /// Registers a push token for one device through the service, as a client would.
    async fn register(&self, account: u128, device: u128, token: &str, platform: Platform) {
        let provider = match platform {
            Platform::Ios => PushProvider::Apns.to_i16(),
            Platform::Web => PushProvider::WebPush.to_i16(),
            _ => PushProvider::Fcm.to_i16(),
        };
        self.notify
            .register(
                &caller(account, device),
                RawToken::new(token, provider, platform),
            )
            .await
            .expect("a well-formed registration is accepted");
    }

    fn counter(&self, name: &'static str, labels: &[(&str, &str)]) -> u64 {
        self.registry.counter(name, "", labels).get()
    }

    fn plain(&self, name: &'static str) -> u64 {
        self.counter(name, &[])
    }

    fn events(&self, kind: &str) -> u64 {
        self.counter("migo_notify_events_total", &[("kind", kind)])
    }

    fn stored_count(&self, kind: &str) -> u64 {
        self.counter("migo_notify_stored_total", &[("kind", kind)])
    }

    fn woken(&self, kind: &str) -> u64 {
        self.counter("migo_notify_wakeups_sent_total", &[("kind", kind)])
    }

    fn withheld(&self, reason: &str) -> u64 {
        self.counter("migo_notify_wakeups_withheld_total", &[("reason", reason)])
    }

    fn failed(&self, reason: &str) -> u64 {
        self.counter("migo_notify_wakeups_failed_total", &[("reason", reason)])
    }

    fn regs(&self, outcome: &str) -> u64 {
        self.counter("migo_notify_registrations_total", &[("outcome", outcome)])
    }

    fn rl_checks(&self) -> u64 {
        self.plain("migo_ratelimit_checks_total")
    }

    fn rl_rejections(&self, scope: &str) -> u64 {
        self.counter("migo_ratelimit_rejections_total", &[("scope", scope)])
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

/// A storable event (a gift) from `actor` to `account`, pointing at a room and subject.
fn gift(account: u128, actor: u128) -> Event {
    Event::new(id(account), NotificationKind::Gift, ts(NOW))
        .by(id(actor))
        .in_room(id(ROOM))
        .about(id(SUBJECT))
}

/// A non-storable event (a message) from `actor` to `account`.
fn message(account: u128, actor: u128) -> Event {
    Event::new(id(account), NotificationKind::Message, ts(NOW))
        .by(id(actor))
        .in_room(id(ROOM))
        .about(id(SUBJECT))
}

// ---------------------------------------------------------------------------
// Who is asking
//
// A notification inbox is a record of who was told what, and a push registration is a
// credential. An anonymous reader of the first or writer of the second is the whole
// risk, so every caller-facing method is checked one by one: the check with no
// interesting failure mode is the one most likely to be left out of a method added
// later. The check runs before the rate-limit charge, because a bucket an
// unauthenticated caller can spend is a bucket it can exhaust for whichever account id
// it borrowed.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inbox_refuses_a_caller_with_no_account() {
    let harness = Harness::new();
    let nobody = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));
    expect_code(
        harness.notify.inbox(&nobody, 20).await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn badge_refuses_a_caller_with_no_account() {
    let harness = Harness::new();
    let nobody = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));
    expect_code(harness.notify.badge(&nobody).await, codes::UNAUTHENTICATED);
}

#[tokio::test]
async fn acknowledge_refuses_a_caller_with_no_account() {
    let harness = Harness::new();
    let nobody = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));
    expect_code(
        harness.notify.acknowledge(&nobody, ts(NOW)).await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn register_refuses_a_caller_with_no_account() {
    let harness = Harness::new();
    let nobody = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));
    let token = RawToken::new(RAW_TOKEN, PushProvider::Fcm.to_i16(), Platform::Android);
    expect_code(
        harness.notify.register(&nobody, token).await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn unregister_refuses_a_caller_with_no_account() {
    let harness = Harness::new();
    let nobody = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));
    expect_code(
        harness.notify.unregister(&nobody).await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn a_read_refuses_a_caller_with_an_account_but_no_device() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    let headless = Caller::new(id(ALICE), Id::NIL, TrustTier::Established, ts(NOW));
    expect_code(
        harness.notify.inbox(&headless, 20).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.notify.badge(&headless).await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn registration_refuses_a_caller_with_an_account_but_no_device() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    let headless = Caller::new(id(ALICE), Id::NIL, TrustTier::Established, ts(NOW));
    let token = RawToken::new(RAW_TOKEN, PushProvider::Fcm.to_i16(), Platform::Android);
    expect_code(
        harness.notify.register(&headless, token).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.notify.unregister(&headless).await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn an_unauthenticated_read_is_refused_before_any_rate_limit_charge() {
    let harness = Harness::new();
    let nobody = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));
    let _ = harness.notify.inbox(&nobody, 20).await;
    let _ = harness.notify.badge(&nobody).await;
    let _ = harness.notify.acknowledge(&nobody, ts(NOW)).await;
    assert_eq!(
        harness.rl_checks(),
        0,
        "the limiter must not have been consulted for an unidentified caller"
    );
    assert_eq!(harness.rl_rejections("account"), 0);
}

#[tokio::test]
async fn an_unauthenticated_registration_moves_no_registration_metric() {
    let harness = Harness::new();
    let nobody = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));
    let token = RawToken::new(RAW_TOKEN, PushProvider::Fcm.to_i16(), Platform::Android);
    let _ = harness.notify.register(&nobody, token).await;
    assert_eq!(harness.rl_checks(), 0);
    for outcome in ["registered", "rejected", "retired", "unregistered"] {
        assert_eq!(harness.regs(outcome), 0, "outcome {outcome} moved");
    }
}

// ---------------------------------------------------------------------------
// A wake-up is not a sentence
//
// Brief section 44: a push payload wakes a device, it does not tell it anything. The
// provider is somebody else's server and the payload passes through it in the clear, so
// every word of prose in it is a word of somebody's conversation read by a third party
// and, on a locked screen, by whoever is holding the phone. The guarantee here is
// structural rather than a matter of care: `Event` has nowhere to put text, so there is
// no field a careless caller could fill. These tests hold that shape in place.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_wake_up_carries_no_prose_from_the_event() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.account(BOB, "bob").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    harness
        .notify
        .notify(message(ALICE, BOB))
        .await
        .expect("a message event is delivered");

    // Whatever the transport saw, it did not see any of these. The words are the ones a
    // chat notification is usually built out of; none of them can reach a payload
    // because no method on this crate accepts them.
    let transcript = harness.sender.transcript();
    for forbidden in ["alice", "bob", "Hello", "hello", "@example.test"] {
        assert!(
            !transcript.contains(forbidden),
            "the transport saw {forbidden:?} in: {transcript}"
        );
    }
}

#[tokio::test]
async fn a_wake_up_names_no_person() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift event is delivered");

    // The actor is who caused it. It is deliberately absent from the wake-up: the client
    // learns who from the inbox row, over its own authenticated connection, after the
    // wake-up got it to ask. A provider that logs payloads learns nothing about who is
    // talking to whom.
    let handed = harness.sender.last();
    let debug = format!("{:?}", handed.wakeup);
    assert!(
        !debug.contains(&id(BOB).to_text()),
        "the actor reached the payload: {debug}"
    );
    assert!(
        !debug.contains(&id(ALICE).to_text()),
        "the recipient reached the payload: {debug}"
    );
}

#[tokio::test]
async fn every_alert_is_a_fixed_phrase_chosen_by_kind_alone() {
    // The alert text is the one string in the payload, and it is picked from a closed set
    // by the kind of the event. It cannot be derived from content because no content is
    // in scope where it is chosen. Fifteen kinds, fifteen constants, no formatting.
    let kinds = [
        NotificationKind::Message,
        NotificationKind::VoiceNote,
        NotificationKind::Mention,
        NotificationKind::Reply,
        NotificationKind::IncomingCall,
        NotificationKind::MissedCall,
        NotificationKind::FriendRequest,
        NotificationKind::Gift,
        NotificationKind::LevelUp,
        NotificationKind::Achievement,
        NotificationKind::RoomInvite,
        NotificationKind::RoomAnnouncement,
        NotificationKind::Event,
        NotificationKind::GameChallenge,
        NotificationKind::Unknown,
    ];
    for kind in kinds {
        let wakeup = Wakeup {
            kind,
            room_id: Some(id(ROOM)),
            subject_id: Some(id(SUBJECT)),
            badge: 3,
            at: ts(NOW),
        };
        let alert = wakeup.alert();
        assert!(!alert.is_empty(), "{kind:?} has no alert");
        // No id, no name, no punctuation that would suggest interpolation.
        assert!(!alert.contains(':'), "{kind:?} alert looks interpolated");
        assert!(!alert.contains('{'), "{kind:?} alert looks interpolated");
    }
}

#[tokio::test]
async fn a_wake_up_carries_the_badge_so_the_client_need_not_ask() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Ios).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Ios)
        .await;

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("the first gift is delivered");

    // A count is not prose. It is the one number a badge needs, and sending it with the
    // wake-up is what lets a device show the right number before it has managed to
    // reconnect -- which on a flaky connection is the difference between a correct badge
    // and a stale one.
    assert_eq!(harness.sender.last().wakeup.badge, 1);
}

// ---------------------------------------------------------------------------
// A push token is a credential
//
// Brief sections 77 and 145: a push token is stored hashed and is never logged. It is
// also the only thing that can reach a device, so unlike a password it has to be
// recoverable -- you cannot send a push without it. Both facts are true at once here: the
// row keeps a sealed copy, which the server can open, and a hash, which is the handle
// everything else uses. Retiring a token the provider rejected, matching a registration
// across devices, and counting registrations all work off the hash, so the raw token is
// read exactly once per wake-up and never written anywhere else.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_registration_stores_a_hash_and_never_the_raw_token() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    let targets = harness
        .store
        .push_targets(id(ALICE))
        .await
        .expect("the registration is stored");
    assert_eq!(targets.len(), 1);
    let stored = &targets[0].registration;

    // The hash is a hash: fixed width, hex, and nothing of the token survives in it.
    assert_eq!(stored.hash.len(), 64);
    assert!(stored.hash.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(!stored.hash.contains(RAW_TOKEN));
    // The sealed copy is ciphertext, so it does not contain the token either, even though
    // it can be turned back into it with the root secret.
    assert!(!stored.sealed.contains(RAW_TOKEN));
    assert!(!stored.sealed.contains("RAW-TOKEN-marker"));
}

#[tokio::test]
async fn the_sealed_token_opens_back_to_exactly_what_was_registered() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("the event is delivered");

    // A token that opens to something else is a token that reaches nobody. This is the
    // one place the raw value is allowed to exist: in memory, on the way to the provider.
    assert_eq!(harness.sender.last().token, RAW_TOKEN);
}

#[tokio::test]
async fn the_same_token_hashes_the_same_way_and_a_different_one_does_not() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.account(BOB, "bob").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(ALICE, ALICE_TABLET, Platform::Android).await;
    harness.device(BOB, BOB_PHONE, Platform::Android).await;

    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness
        .register(
            ALICE,
            ALICE_TABLET,
            "fcm-a-different-token",
            Platform::Android,
        )
        .await;
    let alice = harness
        .store
        .push_targets(id(ALICE))
        .await
        .expect("alice's targets");
    let phone = alice
        .iter()
        .find(|t| t.device_id == id(ALICE_PHONE))
        .expect("the phone is registered")
        .registration
        .clone();
    let tablet = alice
        .iter()
        .find(|t| t.device_id == id(ALICE_TABLET))
        .expect("the tablet is registered")
        .registration
        .clone();

    // Distinct, so retiring one token does not silence an unrelated device.
    assert_ne!(phone.hash, tablet.hash);

    // Stable across accounts and across seals, which is what makes the hash usable as the
    // handle: a token the provider rejects can be retired wherever it was registered
    // without the server needing to open anything.
    harness
        .register(BOB, BOB_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    let bob = harness
        .store
        .push_targets(id(BOB))
        .await
        .expect("bob's targets");
    assert_eq!(phone.hash, bob[0].registration.hash);
    // The sealed copies differ even for the same token: each seal uses a fresh nonce, so
    // two rows holding the same token are not visibly the same row.
    assert_ne!(phone.sealed, bob[0].registration.sealed);
}

#[tokio::test]
async fn a_reissued_token_belongs_to_whoever_registered_it_last() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.account(BOB, "bob").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(BOB, BOB_PHONE, Platform::Android).await;

    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness
        .register(BOB, BOB_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    // A token is an address for one install, and providers hand a retired token to the
    // next install on the same hardware. Two rows holding it would mean Bob's phone buzzes
    // for Alice's messages -- somebody else's activity on hardware they since sold. The
    // last registration is the live one and every earlier holder loses it.
    assert!(harness
        .store
        .push_targets(id(ALICE))
        .await
        .expect("alice's targets")
        .is_empty());
    let bob = harness
        .store
        .push_targets(id(BOB))
        .await
        .expect("bob's targets");
    assert_eq!(bob.len(), 1);
    assert_eq!(bob[0].device_id, id(BOB_PHONE));

    // And the fan-out agrees: an event for Alice reaches nobody, because the address that
    // used to be hers is not hers.
    harness
        .notify
        .notify(gift(ALICE, CAROL))
        .await
        .expect("delivered");
    assert_eq!(harness.sender.calls(), 0);
    harness
        .notify
        .notify(gift(BOB, CAROL))
        .await
        .expect("delivered");
    assert_eq!(harness.sender.calls(), 1);
    assert_eq!(harness.sender.last().device_id, id(BOB_PHONE));
}

#[tokio::test]
async fn an_empty_token_is_refused() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;

    expect_code(
        harness
            .notify
            .register(
                &caller(ALICE, ALICE_PHONE),
                RawToken::new("", PushProvider::Fcm.to_i16(), Platform::Android),
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    // Nothing was written, so a device that sent nonsense is not left holding a row that
    // will be woken forever with a token that cannot arrive.
    assert!(harness
        .store
        .push_targets(id(ALICE))
        .await
        .expect("no targets")
        .is_empty());
    assert_eq!(harness.regs("rejected"), 1);
    assert_eq!(harness.regs("registered"), 0);
}

#[tokio::test]
async fn a_token_at_the_length_ceiling_is_kept_and_one_byte_over_is_refused() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;

    let at_limit = "t".repeat(MAX_TOKEN_LEN);
    harness
        .notify
        .register(
            &caller(ALICE, ALICE_PHONE),
            RawToken::new(&at_limit, PushProvider::Fcm.to_i16(), Platform::Android),
        )
        .await
        .expect("a token at the ceiling is a legal token");

    let over = "t".repeat(MAX_TOKEN_LEN + 1);
    expect_code(
        harness
            .notify
            .register(
                &caller(ALICE, ALICE_PHONE),
                RawToken::new(&over, PushProvider::Fcm.to_i16(), Platform::Android),
            )
            .await,
        codes::VALIDATION_FAILED,
    );

    // The refusal did not replace the good registration. A ceiling that clobbers what was
    // already there would let a device lose its own push by sending one bad request.
    let targets = harness
        .store
        .push_targets(id(ALICE))
        .await
        .expect("targets");
    assert_eq!(targets.len(), 1);
    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("the event is delivered");
    assert_eq!(harness.sender.last().token, at_limit);
}

#[tokio::test]
async fn registering_again_replaces_the_devices_token_rather_than_adding_one() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness
        .register(ALICE, ALICE_PHONE, "fcm-rotated-token", Platform::Android)
        .await;

    // Providers rotate tokens without asking. One device is one row, so a rotation is an
    // update: two rows would mean one wake-up sent twice, once to an address that no
    // longer resolves.
    let targets = harness
        .store
        .push_targets(id(ALICE))
        .await
        .expect("targets");
    assert_eq!(targets.len(), 1);

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("the event is delivered");
    assert_eq!(harness.sender.calls(), 1);
    assert_eq!(harness.sender.last().token, "fcm-rotated-token");
}

#[tokio::test]
async fn unregistering_stops_the_wake_ups_for_that_device_only() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(ALICE, ALICE_TABLET, Platform::Ios).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness
        .register(ALICE, ALICE_TABLET, "apns-tablet-token", Platform::Ios)
        .await;

    harness
        .notify
        .unregister(&caller(ALICE, ALICE_PHONE))
        .await
        .expect("a device may stop being woken");

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("the event is delivered");

    // Signing out on the phone is not signing out on the tablet.
    assert_eq!(harness.sender.calls(), 1);
    assert_eq!(harness.sender.last().device_id, id(ALICE_TABLET));
    assert_eq!(harness.regs("unregistered"), 1);
}

#[tokio::test]
async fn unregistering_a_device_that_was_never_registered_is_not_an_error() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;

    // A client that signs out twice, or signs out after the row was already retired by a
    // provider rejection, is not doing anything wrong. The end state is what was asked
    // for, so saying "no" would only teach the client to ignore the answer.
    harness
        .notify
        .unregister(&caller(ALICE, ALICE_PHONE))
        .await
        .expect("unregistering nothing succeeds");
}

#[tokio::test]
async fn a_raw_token_never_reaches_the_metrics() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("the event is delivered");

    // Section 174: a metric is labelled by shape, never by identity. The scrape endpoint
    // is usually the least-guarded thing a deployment exposes, so this is the cheapest
    // place for a credential to leak and the one worth asserting on directly.
    let rendered = harness.registry.render();
    assert!(!rendered.contains(RAW_TOKEN));
    assert!(!rendered.contains("RAW-TOKEN-marker"));
    assert!(!rendered.contains(&id(ALICE).to_text()));
    assert!(!rendered.contains(&id(ALICE_PHONE).to_text()));
}

// ---------------------------------------------------------------------------
// Withholding a wake-up
//
// Every wake-up costs the recipient's battery and, on a locked screen, their attention.
// The four reasons a wake-up is withheld are cheapest-first by design: an already
// connected device needs nothing, a device woken a moment ago for the same kind needs
// nothing, a device being hammered needs protecting, and a device whose registration
// aged out cannot be reached at all. Each reason is counted separately, because
// "we did not push" for four different reasons is four different operational stories.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_connected_device_is_not_woken() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness.connect(ALICE, ALICE_PHONE).await;

    harness
        .notify
        .notify(message(ALICE, BOB))
        .await
        .expect("the event is delivered");

    // The socket already carried the message. A push on top of it is a duplicate buzz for
    // something the user is already looking at.
    assert_eq!(harness.sender.calls(), 0);
    assert_eq!(harness.withheld("connected"), 1);
}

#[tokio::test]
async fn a_connected_device_is_woken_when_the_deployment_asks_for_it() {
    let harness = Harness::configured(NotifyConfig {
        skip_connected: false,
        ..NotifyConfig::default()
    });
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness.connect(ALICE, ALICE_PHONE).await;

    harness
        .notify
        .notify(message(ALICE, BOB))
        .await
        .expect("the event is delivered");

    // A route mark can outlive the socket it describes, and on some networks it usually
    // does. A deployment that would rather double-buzz than drop a notification can say
    // so, and then the connected check stops applying.
    assert_eq!(harness.sender.calls(), 1);
    assert_eq!(harness.withheld("connected"), 0);
}

#[tokio::test]
async fn a_second_wake_up_of_the_same_kind_inside_the_window_is_coalesced() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    harness
        .notify
        .notify(message(ALICE, BOB))
        .await
        .expect("first");
    harness
        .notify
        .notify(message(ALICE, BOB))
        .await
        .expect("second");
    harness
        .notify
        .notify(message(ALICE, CAROL))
        .await
        .expect("third");

    // Three messages in one burst is one buzz. The client learns the rest when it wakes:
    // the wake-up is a hint that there is something to fetch, and one hint is enough.
    assert_eq!(harness.sender.calls(), 1);
    assert_eq!(harness.withheld("coalesced"), 2);
}

#[tokio::test]
async fn a_different_kind_is_not_coalesced_against_the_first() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    harness
        .notify
        .notify(message(ALICE, BOB))
        .await
        .expect("a message");
    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift");

    // The window is per kind, because the whole point of the alert text is that a gift and
    // a message read differently. Collapsing them would show the wrong one.
    assert_eq!(harness.sender.calls(), 2);
    assert_eq!(harness.withheld("coalesced"), 0);
}

#[tokio::test]
async fn the_window_expires() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    harness
        .notify
        .notify(message(ALICE, BOB))
        .await
        .expect("the first message");
    let later = Event::new(id(ALICE), NotificationKind::Message, ts(NOW + 31 * SECOND))
        .by(id(BOB))
        .in_room(id(ROOM));
    harness.notify.notify(later).await.expect("a later message");

    // Half a minute later is a new conversation, not the same burst.
    assert_eq!(harness.sender.calls(), 2);
}

#[tokio::test]
async fn an_urgent_kind_skips_the_window() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    let call = |at: i64| {
        Event::new(id(ALICE), NotificationKind::IncomingCall, ts(at))
            .by(id(BOB))
            .in_room(id(ROOM))
    };
    harness
        .notify
        .notify(call(NOW))
        .await
        .expect("the first ring");
    harness
        .notify
        .notify(call(NOW + SECOND))
        .await
        .expect("the second ring");

    // A ringing phone is the one case where the window is wrong: a call that arrives
    // during a coalescing window and is silently dropped is a missed call the recipient
    // never had the chance to answer. Section 44 wants one buzz per burst; a call is not
    // a burst.
    assert_eq!(harness.sender.calls(), 2);
    assert_eq!(harness.withheld("coalesced"), 0);
}

#[tokio::test]
async fn a_stale_registration_is_not_woken() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    let long_after = Event::new(
        id(ALICE),
        NotificationKind::Gift,
        ts(NOW + REGISTRATION_TTL_MS + SECOND),
    )
    .by(id(BOB));
    harness
        .notify
        .notify(long_after)
        .await
        .expect("the event is delivered");

    // A token nobody has refreshed for two months belongs to an app that was uninstalled
    // or a device that was replaced. Providers charge for pushing to those, and some of
    // them start refusing the whole sender.
    assert_eq!(harness.sender.calls(), 0);
    assert_eq!(harness.withheld("stale"), 1);
}

#[tokio::test]
async fn a_provider_the_deployment_cannot_reach_counts_as_stale() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(ALICE, ALICE_TABLET, Platform::Ios).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness
        .register(ALICE, ALICE_TABLET, "apns-tablet-token", Platform::Ios)
        .await;
    // The deployment configured FCM and not APNs.
    harness.sender.handle_only(PushProvider::Fcm.to_i16());

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("the event is delivered");

    // The row is unreachable for a reason the sender knows and the store does not, so it
    // is withheld rather than attempted. Attempting it would spend a token unseal and
    // produce a failure metric for a deployment decision, which reads as an outage.
    assert_eq!(harness.sender.calls(), 1);
    assert_eq!(harness.sender.last().device_id, id(ALICE_PHONE));
    assert_eq!(harness.withheld("stale"), 1);
}

#[tokio::test]
async fn the_wake_up_budget_stops_a_flood() {
    let harness = Harness::configured(NotifyConfig {
        coalesce_window_ms: 0,
        ..NotifyConfig::default()
    });
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    // With coalescing off, the budget is the only thing between a device and a thousand
    // buzzes. Section 70: the limit belongs to the person being woken, not to whoever is
    // doing the waking, so no amount of accounts can add up to more buzzes than one
    // device is willing to take.
    for _ in 0..400 {
        harness
            .notify
            .notify(message(ALICE, BOB))
            .await
            .expect("the event is always recorded");
    }
    assert!(
        harness.withheld("budget") > 0,
        "the budget never refused anything in 400 wake-ups"
    );
    assert!(
        harness.sender.calls() < 400,
        "the budget did not stop anything"
    );
    // Nothing was lost: every event that could not buzz is still an event.
    assert_eq!(harness.events("message"), 400);
}

#[tokio::test]
async fn nobody_is_woken_by_their_own_doing() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    let own = Event::new(id(ALICE), NotificationKind::Message, ts(NOW))
        .by(id(ALICE))
        .in_room(id(ROOM));
    assert!(own.is_self_inflicted());
    harness.notify.notify(own).await.expect("the call succeeds");

    // Sending a message from the tablet must not buzz the phone. This is checked before
    // anything is stored, so the inbox does not fill up with the user's own activity
    // either.
    assert_eq!(harness.sender.calls(), 0);
    assert_eq!(harness.stored_count("message"), 0);
}

#[tokio::test]
async fn a_revoked_device_is_not_woken() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(ALICE, ALICE_TABLET, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness
        .register(ALICE, ALICE_TABLET, "fcm-tablet-token", Platform::Android)
        .await;
    harness.revoke(ALICE_PHONE).await;

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("the event is delivered");

    // A revoked device is a device somebody took away, or one the user pressed "sign out
    // everywhere" about. Waking it would tell whoever holds it that there is new activity
    // on an account it no longer has any business with.
    assert_eq!(harness.sender.calls(), 1);
    assert_eq!(harness.sender.last().device_id, id(ALICE_TABLET));
}

#[tokio::test]
async fn every_device_of_an_account_is_woken() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(ALICE, ALICE_TABLET, Platform::Ios).await;
    harness.device(ALICE, ALICE_LAPTOP, Platform::Web).await;
    harness
        .register(ALICE, ALICE_PHONE, "fcm-phone", Platform::Android)
        .await;
    harness
        .register(ALICE, ALICE_TABLET, "apns-tablet", Platform::Ios)
        .await;
    harness
        .register(ALICE, ALICE_LAPTOP, "webpush-laptop", Platform::Web)
        .await;

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("the event is delivered");

    let mut woken: Vec<Id> = harness.sender.all().iter().map(|h| h.device_id).collect();
    woken.sort();
    let mut expected = vec![id(ALICE_PHONE), id(ALICE_TABLET), id(ALICE_LAPTOP)];
    expected.sort();
    assert_eq!(woken, expected);
    assert_eq!(harness.woken("gift"), 3);
}

#[tokio::test]
async fn one_announcement_reaches_every_recipient_once() {
    let harness = Harness::new();
    for (account, device, name) in [
        (ALICE, ALICE_PHONE, "alice"),
        (BOB, BOB_PHONE, "bob"),
        (CAROL, CAROL_PHONE, "carol"),
    ] {
        harness.account(account, name).await;
        harness.device(account, device, Platform::Android).await;
        harness
            .register(account, device, name, Platform::Android)
            .await;
    }

    // A room announcement to four thousand members is the shape the trait doc names, and
    // three is enough to prove the fan-out is per recipient rather than per call.
    let announcement = Event::new(Id::NIL, NotificationKind::RoomAnnouncement, ts(NOW))
        .by(id(BOB))
        .in_room(id(ROOM));
    harness
        .notify
        .notify_many(&[id(ALICE), id(BOB), id(CAROL)], announcement)
        .await
        .expect("a batch is delivered");

    let mut woken: Vec<Id> = harness.sender.all().iter().map(|h| h.device_id).collect();
    woken.sort();
    // Bob announced it, so Bob is not woken by it: the self-inflicted rule survives the
    // batch path rather than being a property of the single-recipient one.
    let mut expected = vec![id(ALICE_PHONE), id(CAROL_PHONE)];
    expected.sort();
    assert_eq!(woken, expected);
    assert_eq!(harness.events("room_announcement"), 3);
}

#[tokio::test]
async fn an_account_with_no_registered_device_is_still_an_event() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("the event is delivered");

    // Push is a convenience, not the record. Somebody who never granted the notification
    // permission still has an inbox, and it still has to be right when they open the app.
    assert_eq!(harness.sender.calls(), 0);
    assert_eq!(harness.stored_count("gift"), 1);
    let inbox = harness
        .notify
        .inbox(&caller(ALICE, ALICE_PHONE), 10)
        .await
        .expect("the inbox reads");
    assert_eq!(inbox.items.len(), 1);
}

// ---------------------------------------------------------------------------
// The inbox is the record; the push is not
//
// A push can be dropped by the provider, by the OS, or by a user who denied the
// permission, and none of that may cost anybody a notification. So the durable row is
// written first and the wake-up is best-effort on top of it. Not every kind gets a row:
// a message already lives in the conversation and a mention already lives in the message,
// so storing them again would give the client two sources of truth that drift.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_storable_kind_becomes_a_row_and_a_conversational_one_does_not() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;

    let gift_delivery = harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift");
    let message_delivery = harness
        .notify
        .notify(message(ALICE, BOB))
        .await
        .expect("a message");

    assert!(gift_delivery.stored, "a gift has nowhere else to live");
    assert!(
        !message_delivery.stored,
        "a message already lives in the conversation"
    );
    assert!(notification_kind::is_storable(notification_kind::GIFT));
    assert_eq!(harness.stored_count("gift"), 1);
    assert_eq!(harness.stored_count("message"), 0);

    let inbox = harness
        .notify
        .inbox(&caller(ALICE, ALICE_PHONE), 10)
        .await
        .expect("the inbox reads");
    assert_eq!(inbox.items.len(), 1);
    assert_eq!(inbox.items[0].kind, NotificationKind::Gift);
}

#[tokio::test]
async fn an_inbox_row_carries_the_ids_needed_to_fetch_the_thing_itself() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift");

    let inbox = harness
        .notify
        .inbox(&caller(ALICE, ALICE_PHONE), 10)
        .await
        .expect("the inbox reads");
    let item = &inbox.items[0];

    // The row is a pointer, not a copy. Whatever the gift was worth, whatever the room is
    // called, and whoever Bob is are all fetched over the authenticated connection by id,
    // so a stale row cannot show stale content and a revoked permission cannot be read
    // around.
    assert_eq!(item.actor_id, Some(id(BOB)));
    assert_eq!(item.room_id, Some(id(ROOM)));
    assert_eq!(item.subject_id, Some(id(SUBJECT)));
    assert_eq!(item.at, ts(NOW));
    assert!(!item.read);
    assert!(!item.notification_id.is_nil());
}

#[tokio::test]
async fn the_badge_counts_unread_rows_and_acknowledging_clears_it() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;

    for at in [NOW, NOW + SECOND, NOW + 2 * SECOND] {
        let event = Event::new(id(ALICE), NotificationKind::Gift, ts(at))
            .by(id(BOB))
            .in_room(id(ROOM))
            .about(id(SUBJECT));
        harness.notify.notify(event).await.expect("a gift");
    }

    let who = caller(ALICE, ALICE_PHONE);
    assert_eq!(
        harness.notify.badge(&who).await.expect("the badge reads"),
        3
    );

    // Acknowledged through the second one: the third is still unread, because a client
    // that scrolled halfway has read halfway.
    let cleared = harness
        .notify
        .acknowledge(&who, ts(NOW + SECOND))
        .await
        .expect("the acknowledgement lands");
    assert_eq!(cleared, 2);
    assert_eq!(
        harness.notify.badge(&who).await.expect("the badge reads"),
        1
    );

    let inbox = harness
        .notify
        .inbox(&who, 10)
        .await
        .expect("the inbox reads");
    assert_eq!(inbox.unread, 1);
    assert_eq!(inbox.items.iter().filter(|i| i.read).count(), 2);
}

#[tokio::test]
async fn acknowledging_twice_clears_nothing_the_second_time() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift");

    let who = caller(ALICE, ALICE_PHONE);
    assert_eq!(
        harness
            .notify
            .acknowledge(&who, ts(NOW))
            .await
            .expect("the first acknowledgement"),
        1
    );
    // A client that retried because it never saw the answer is not asking for anything
    // different, and the count it gets back is how many it actually changed. Reporting 1
    // again would make a client that sums them think it read two things.
    assert_eq!(
        harness
            .notify
            .acknowledge(&who, ts(NOW))
            .await
            .expect("the retry"),
        0
    );
    assert_eq!(harness.notify.badge(&who).await.expect("the badge"), 0);
}

#[tokio::test]
async fn an_inbox_never_shows_somebody_elses_rows() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.account(BOB, "bob").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(BOB, BOB_PHONE, Platform::Android).await;

    harness
        .notify
        .notify(gift(ALICE, CAROL))
        .await
        .expect("alice's gift");
    harness
        .notify
        .notify(gift(BOB, CAROL))
        .await
        .expect("bob's gift");

    // The account comes from the authenticated caller and there is no parameter for it, so
    // there is nothing to tamper with -- but the isolation is worth asserting rather than
    // arguing, because a future overload taking an account id would break it silently.
    let alice = harness
        .notify
        .inbox(&caller(ALICE, ALICE_PHONE), 50)
        .await
        .expect("alice's inbox");
    assert_eq!(alice.items.len(), 1);
    assert_eq!(alice.unread, 1);

    // And acknowledging on one account does not clear the other's badge.
    harness
        .notify
        .acknowledge(&caller(ALICE, ALICE_PHONE), ts(NOW))
        .await
        .expect("alice acknowledges");
    assert_eq!(
        harness
            .notify
            .badge(&caller(BOB, BOB_PHONE))
            .await
            .expect("bob's badge"),
        1
    );
}

#[tokio::test]
async fn an_inbox_page_is_capped_however_much_is_asked_for() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    for n in 0..(MAX_INBOX_PAGE as i64 + 10) {
        let event = Event::new(id(ALICE), NotificationKind::Gift, ts(NOW + n * SECOND))
            .by(id(BOB))
            .about(id(SUBJECT));
        harness.notify.notify(event).await.expect("a gift");
    }

    // A client asking for everything gets a page. The ceiling is the server's, because a
    // client that asks for a million rows is either broken or hostile and either way the
    // answer has to fit in one response.
    let inbox = harness
        .notify
        .inbox(&caller(ALICE, ALICE_PHONE), u16::MAX)
        .await
        .expect("the inbox reads");
    assert_eq!(inbox.items.len(), MAX_INBOX_PAGE as usize);
    // The unread count is not capped: a badge that stops counting at fifty is a badge
    // that lies about how far behind somebody is.
    assert_eq!(inbox.unread, MAX_INBOX_PAGE as u32 + 10);
}

#[tokio::test]
async fn the_newest_rows_come_first() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    for n in 0..5 {
        let event = Event::new(id(ALICE), NotificationKind::Gift, ts(NOW + n * MINUTE))
            .by(id(BOB))
            .about(id(SUBJECT));
        harness.notify.notify(event).await.expect("a gift");
    }

    let inbox = harness
        .notify
        .inbox(&caller(ALICE, ALICE_PHONE), 3)
        .await
        .expect("the inbox reads");
    assert_eq!(inbox.items.len(), 3);
    // A capped page has to be capped at the useful end. Newest first also means the page a
    // client gets after a wake-up contains the thing it was woken for.
    assert_eq!(inbox.items[0].at, ts(NOW + 4 * MINUTE));
    assert_eq!(inbox.items[2].at, ts(NOW + 2 * MINUTE));
}

// ---------------------------------------------------------------------------
// A provider that says no
//
// The three transport outcomes mean three different things and only one of them changes
// stored state. A dead token has to be retired or the deployment pays to rediscover it on
// every notification for the rest of the account's life; a throttled provider has to be
// left alone; an error has to be counted and forgotten. None of them is allowed to lose
// the durable row, because the transport is the part that was always allowed to fail.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_dead_token_is_retired_so_it_is_never_tried_again() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness.sender.set_reply(Reply::Unregistered);

    let delivery = harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("delivered");

    assert_eq!(delivery.failed, 1);
    assert_eq!(delivery.woken, 0);
    assert!(delivery.stored, "the row survives a dead token");
    assert_eq!(harness.failed("unregistered"), 1);
    assert_eq!(harness.regs("retired"), 1);

    // The row is gone, so the second notification does not even reach the provider.
    assert!(harness
        .store
        .push_targets(id(ALICE))
        .await
        .expect("targets")
        .is_empty());
    harness.sender.set_reply(Reply::Delivered);
    let second = Event::new(id(ALICE), NotificationKind::Gift, ts(NOW + MINUTE)).by(id(BOB));
    harness.notify.notify(second).await.expect("delivered");
    assert_eq!(harness.sender.calls(), 1, "the dead token was tried twice");
}

#[tokio::test]
async fn a_throttled_provider_keeps_the_registration() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness.sender.set_reply(Reply::Throttled);

    let delivery = harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("delivered");
    assert_eq!(delivery.failed, 1);
    assert!(delivery.stored);
    assert_eq!(harness.failed("throttled"), 1);
    assert_eq!(harness.regs("retired"), 0);

    // The token was fine; the provider was busy. Retiring it here would silence a live
    // device for good because somebody else's traffic spiked.
    harness.sender.set_reply(Reply::Delivered);
    let second = Event::new(id(ALICE), NotificationKind::Gift, ts(NOW + MINUTE)).by(id(BOB));
    harness.notify.notify(second).await.expect("delivered");
    assert_eq!(harness.sender.calls(), 2);
    assert_eq!(harness.woken("gift"), 1);
}

#[tokio::test]
async fn a_transport_error_is_counted_and_the_notification_still_happened() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness.sender.set_reply(Reply::Error);

    // A provider outage is not a client error. The call succeeds, the row is written, the
    // badge is right, and the client finds out the moment it next connects.
    let delivery = harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("delivered");
    assert_eq!(delivery.failed, 1);
    assert!(delivery.stored);
    assert_eq!(harness.failed("error"), 1);
    assert_eq!(harness.regs("retired"), 0);
    assert_eq!(
        harness
            .notify
            .badge(&caller(ALICE, ALICE_PHONE))
            .await
            .expect("the badge"),
        1
    );
}

#[tokio::test]
async fn one_dead_token_does_not_stop_the_other_devices() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(ALICE, ALICE_TABLET, Platform::Ios).await;
    harness
        .register(ALICE, ALICE_PHONE, "fcm-phone", Platform::Android)
        .await;
    harness
        .register(ALICE, ALICE_TABLET, "apns-tablet", Platform::Ios)
        .await;
    // The FCM leg is dead, the APNs leg is fine.
    harness.sender.handle_only(PushProvider::Apns.to_i16());

    let delivery = harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("delivered");

    // A fan-out that aborts on the first bad leg wakes a prefix of somebody's devices and
    // gives the caller no way to tell which prefix.
    assert_eq!(delivery.woken, 1);
    assert_eq!(harness.sender.last().device_id, id(ALICE_TABLET));
}

// ---------------------------------------------------------------------------
// Maintenance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_sweep_deletes_read_rows_and_leaves_unread_ones() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    for n in 0..6 {
        let event = Event::new(id(ALICE), NotificationKind::Gift, ts(NOW + n * DAY))
            .by(id(BOB))
            .about(id(SUBJECT));
        harness.notify.notify(event).await.expect("a gift");
    }
    let who = caller(ALICE, ALICE_PHONE);
    harness
        .notify
        .acknowledge(&who, ts(NOW + 2 * DAY))
        .await
        .expect("the first three are read");

    let gone = harness
        .notify
        .sweep(ts(NOW + 5 * DAY), 100)
        .await
        .expect("the sweep runs");

    // Read and old is the only combination that is safe to delete: an unread row is
    // somebody's badge, and deleting it would silently decrement a count they never saw.
    assert_eq!(gone, 3);
    let inbox = harness
        .notify
        .inbox(&who, 50)
        .await
        .expect("the inbox reads");
    assert_eq!(inbox.items.len(), 3);
    assert_eq!(inbox.unread, 3);
    assert!(inbox.items.iter().all(|item| !item.read));
}

#[tokio::test]
async fn the_sweep_honours_its_limit_and_returns_zero_when_it_is_done() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    for n in 0..5 {
        let event = Event::new(id(ALICE), NotificationKind::Gift, ts(NOW + n * SECOND))
            .by(id(BOB))
            .about(id(SUBJECT));
        harness.notify.notify(event).await.expect("a gift");
    }
    let who = caller(ALICE, ALICE_PHONE);
    harness
        .notify
        .acknowledge(&who, ts(NOW + 10 * SECOND))
        .await
        .expect("all read");

    // The limit is what keeps a maintenance job from taking a lock over a hundred million
    // rows. The caller loops, so "returns zero" is the termination condition and has to be
    // reachable.
    let cutoff = ts(NOW + MINUTE);
    let mut total = 0;
    loop {
        let gone = harness
            .notify
            .sweep(cutoff, 2)
            .await
            .expect("the sweep runs");
        assert!(gone <= 2, "the sweep took {gone} rows for a limit of 2");
        if gone == 0 {
            break;
        }
        total += gone;
    }
    assert_eq!(total, 5);
    assert_eq!(harness.notify.badge(&who).await.expect("the badge"), 0);
}

// ---------------------------------------------------------------------------
// What a caller may spend
//
// Reading an inbox and registering a token are both cheap and both unlimited-looking to a
// client, so both are charged. The charge happens after the identity check, so an
// unauthenticated caller cannot spend a bucket it has no business reaching, and before the
// store, so a client in a loop cannot turn a badge poll into a database load test.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reading_an_inbox_is_charged() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;

    let before = harness.rl_checks();
    harness
        .notify
        .inbox(&caller(ALICE, ALICE_PHONE), 10)
        .await
        .expect("the inbox reads");
    harness
        .notify
        .badge(&caller(ALICE, ALICE_PHONE))
        .await
        .expect("the badge reads");
    assert!(
        harness.rl_checks() > before,
        "neither read reached the limiter"
    );
}

#[tokio::test]
async fn a_client_polling_its_badge_forever_is_eventually_refused() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;

    let who = caller(ALICE, ALICE_PHONE);
    let mut refused = None;
    for attempt in 0..2_000 {
        if let Err(error) = harness.notify.badge(&who).await {
            refused = Some((attempt, error.code()));
            break;
        }
    }
    let (attempt, code) = refused.expect("two thousand badge reads were all allowed");
    assert!(attempt > 0, "the very first badge read was refused");
    assert_eq!(code, codes::RATE_LIMITED);
    assert!(harness.rl_rejections("device") > 0 || harness.rl_rejections("account") > 0);
}

#[tokio::test]
async fn an_unauthenticated_caller_never_reaches_the_limiter() {
    let harness = Harness::new();
    let nobody = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));

    for _ in 0..5 {
        expect_code(harness.notify.badge(&nobody).await, codes::UNAUTHENTICATED);
    }
    // Section 161 and the ordering that goes with it: an anonymous caller must not be able
    // to spend anybody's bucket. If the charge came first, a stranger sending nil ids in a
    // loop would exhaust whatever bucket "nobody" resolves to, and every other caller who
    // resolves to the same bucket would be refused for it.
    assert_eq!(harness.rl_checks(), 0);
}

// ---------------------------------------------------------------------------
// What the scrape endpoint may say
//
// Section 174: no metric is labelled by an account, a device, or a conversation. A
// notification metric is the easiest place in the system to get this wrong, because the
// interesting question really is "who is being buzzed" -- and that is exactly the question
// a scrape endpoint must not be able to answer.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_series_is_labelled_by_shape_and_never_by_identity() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.account(BOB, "bob").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(BOB, BOB_PHONE, Platform::Ios).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    harness
        .register(BOB, BOB_PHONE, "apns-bob", Platform::Ios)
        .await;
    harness.connect(BOB, BOB_PHONE).await;

    // Exercise every counter the crate has: an event, a stored row, a wake-up, a
    // withholding, a failure, a registration, an inbox read, a badge read, and an
    // acknowledgement.
    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift");
    harness
        .notify
        .notify(message(BOB, ALICE))
        .await
        .expect("a message");
    harness.sender.set_reply(Reply::Unregistered);
    let later = Event::new(id(ALICE), NotificationKind::LevelUp, ts(NOW + MINUTE));
    harness.notify.notify(later).await.expect("a level-up");
    let who = caller(ALICE, ALICE_PHONE);
    harness
        .notify
        .inbox(&who, 10)
        .await
        .expect("the inbox reads");
    harness.notify.badge(&who).await.expect("the badge reads");
    harness
        .notify
        .acknowledge(&who, ts(NOW))
        .await
        .expect("acknowledged");
    harness.notify.unregister(&who).await.expect("unregistered");

    let rendered = harness.registry.render();
    for identity in [
        id(ALICE).to_text(),
        id(BOB).to_text(),
        id(ALICE_PHONE).to_text(),
        id(BOB_PHONE).to_text(),
        id(ROOM).to_text(),
        id(SUBJECT).to_text(),
    ] {
        assert!(
            !rendered.contains(&identity),
            "an id reached the scrape endpoint: {identity}"
        );
    }
    for secret in ["alice", "bob", RAW_TOKEN, "apns-bob"] {
        assert!(
            !rendered.contains(secret),
            "a name or a token reached the scrape endpoint: {secret}"
        );
    }
    // And the series that should be there, are.
    for series in [
        "migo_notify_events_total",
        "migo_notify_stored_total",
        "migo_notify_wakeups_sent_total",
        "migo_notify_wakeups_withheld_total",
        "migo_notify_wakeups_failed_total",
        "migo_notify_registrations_total",
    ] {
        assert!(rendered.contains(series), "{series} is not exported");
    }
}

#[tokio::test]
async fn a_deployment_that_notified_nobody_still_exports_its_series() {
    let harness = Harness::new();

    // A counter that springs into existence on its first increment is a counter that reads
    // as a gap in a dashboard rather than a zero, and an alert on a rate cannot fire on a
    // series that is not there. Registering them up front is what makes "no wake-ups were
    // withheld" distinguishable from "the withholding path is not wired up".
    let rendered = harness.registry.render();
    assert!(rendered.contains("migo_notify_wakeups_withheld_total"));
    assert_eq!(harness.withheld("connected"), 0);
    assert_eq!(harness.withheld("coalesced"), 0);
    assert_eq!(harness.withheld("budget"), 0);
    assert_eq!(harness.withheld("stale"), 0);
    assert_eq!(harness.failed("unregistered"), 0);
    assert_eq!(harness.failed("throttled"), 0);
    assert_eq!(harness.failed("error"), 0);
    assert_eq!(harness.regs("registered"), 0);
    assert_eq!(harness.regs("rejected"), 0);
    assert_eq!(harness.regs("retired"), 0);
    assert_eq!(harness.regs("unregistered"), 0);
}

#[tokio::test]
async fn the_coalescing_window_is_the_documented_half_minute() {
    // The window is the one constant a deployment tunes without reading the code, and the
    // brief names it. A silent change here would turn a burst of six messages into six
    // buzzes without any test going red, so the number itself is the assertion.
    assert_eq!(i64::from(COALESCE_WINDOW_MS), 30 * SECOND);
    assert_eq!(REGISTRATION_TTL_MS, 60 * DAY);
    assert_eq!(MAX_INBOX_PAGE, 50);
    assert_eq!(MAX_TOKEN_LEN, 512);
    let defaults = NotifyConfig::default();
    assert!(defaults.push_enabled);
    assert!(defaults.skip_connected);
    assert_eq!(defaults.coalesce_window_ms, COALESCE_WINDOW_MS);
    assert_eq!(defaults.registration_ttl_ms, REGISTRATION_TTL_MS);
}

#[tokio::test]
async fn a_deployment_with_push_switched_off_still_keeps_the_record() {
    let harness = Harness::configured(NotifyConfig {
        push_enabled: false,
        ..NotifyConfig::default()
    });
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    let delivery = harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift");

    // A deployment with no provider credentials -- a self-hosted instance, a test
    // environment, a region where the provider is blocked -- is a deployment where push is
    // off, not one where notifications are lost.
    assert_eq!(harness.sender.calls(), 0);
    assert_eq!(delivery.woken, 0);
    assert!(delivery.stored);
    assert_eq!(
        harness
            .notify
            .badge(&caller(ALICE, ALICE_PHONE))
            .await
            .expect("the badge"),
        1
    );
}

// ---------------------------------------------------------------------------
// The stored row, read as a row rather than through the API
//
// Section 44 says a notification is a pointer, and the place that has to be true is the
// column list: an inbox row with a text column would be a copy of a private message in a
// table nothing encrypts, sitting next to the account it belongs to.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stored_notification_is_ids_and_timestamps_and_nothing_else() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.account(BOB, "bob").await;

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift is stored");

    let rows: Vec<Notification> = harness
        .store
        .notifications(id(ALICE), 10)
        .await
        .expect("the inbox can be read");
    assert_eq!(rows.len(), 1);

    // Destructured exhaustively on purpose. Adding a field to the row makes this stop
    // compiling, which is the only way a rule about what a table may not hold survives
    // somebody adding a convenient `body` column two years from now.
    let Notification {
        notification_id,
        account_id,
        kind,
        room_id,
        actor_id,
        subject_id,
        created_at,
        read_at,
    } = rows[0].clone();
    assert!(!notification_id.is_nil());
    assert_eq!(account_id, id(ALICE));
    assert_eq!(kind, notification_kind::GIFT);
    assert_eq!(room_id, Some(id(ROOM)));
    assert_eq!(actor_id, Some(id(BOB)));
    assert_eq!(subject_id, Some(id(SUBJECT)));
    assert_eq!(created_at, ts(NOW));
    assert_eq!(read_at, None);
}

#[tokio::test]
async fn an_inbox_item_carries_the_pointer_the_row_carries() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.account(BOB, "bob").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift is stored");

    let inbox: Inbox = harness
        .notify
        .inbox(&caller(ALICE, ALICE_PHONE), 10)
        .await
        .expect("an account may read its own inbox");
    assert_eq!(inbox.unread, 1);
    let item: &Item = &inbox.items[0];

    // The API hands back the same three pointers the row holds, and the client resolves
    // them against the tables that own the content. What it does not hand back is any
    // prose, because there is none to hand back.
    let rows = harness
        .store
        .notifications(id(ALICE), 10)
        .await
        .expect("the inbox can be read");
    assert_eq!(item.notification_id, rows[0].notification_id);
    assert_eq!(item.actor_id, rows[0].actor_id);
    assert_eq!(item.subject_id, rows[0].subject_id);
    assert_eq!(item.room_id, rows[0].room_id);
    assert_eq!(item.at, rows[0].created_at);
    assert!(!item.read);
}

// ---------------------------------------------------------------------------
// The token at rest, opened with the key the deployment holds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stored_token_opens_with_the_deployment_key_and_nothing_else() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;

    let targets = harness
        .store
        .push_targets(id(ALICE))
        .await
        .expect("alice's targets");
    assert_eq!(targets.len(), 1);
    let stored = &targets[0].registration;

    // Section 77: what is in the column is a ciphertext and a hash, never the token. The
    // marker in RAW_TOKEN exists so this assertion cannot pass by accident.
    assert!(!stored.sealed.contains(RAW_TOKEN));
    assert!(!stored.hash.contains(RAW_TOKEN));

    let keeper = TokenKeeper::derive(ROOT_SECRET);
    assert_eq!(keeper.hash(RAW_TOKEN), stored.hash);
    assert_eq!(
        keeper
            .open(id(ALICE_PHONE), stored)
            .expect("the deployment key opens its own registration"),
        RAW_TOKEN
    );

    // A different deployment secret is a different key, so the same bytes are unreadable
    // and the same token hashes to something else. That is what makes the hash safe to
    // put in a log line: it is a handle inside one deployment and meaningless outside it.
    let stranger = TokenKeeper::derive(b"a different deployment's root secret");
    assert_ne!(stranger.hash(RAW_TOKEN), stored.hash);
    let refused = stranger
        .open(id(ALICE_PHONE), stored)
        .expect_err("another deployment's key opens nothing");
    assert_eq!(refused.code(), codes::INTERNAL_ERROR);
    assert!(!refused.public_message().contains(RAW_TOKEN));
}

#[tokio::test]
async fn a_token_sealed_for_one_device_does_not_open_for_another() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness
        .register(ALICE, ALICE_PHONE, RAW_TOKEN, Platform::Android)
        .await;
    let targets = harness
        .store
        .push_targets(id(ALICE))
        .await
        .expect("alice's targets");
    let stored = &targets[0].registration;

    // The device id is the sealing context, so a row moved to another device's key -- by a
    // bad migration, a restore, or somebody editing the table -- fails to open rather than
    // quietly sending this phone's notifications to that one.
    let keeper = TokenKeeper::derive(ROOT_SECRET);
    assert!(keeper.open(id(ALICE_TABLET), stored).is_err());
    assert!(keeper.open(id(ALICE_PHONE), stored).is_ok());
}

// ---------------------------------------------------------------------------
// One account, three platforms, three providers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn each_platform_is_handed_to_the_transport_with_its_own_provider() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.account(BOB, "bob").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(ALICE, ALICE_TABLET, Platform::Ios).await;
    harness.device(ALICE, ALICE_LAPTOP, Platform::Web).await;
    harness
        .register(
            ALICE,
            ALICE_PHONE,
            "fcm-token-for-the-phone",
            Platform::Android,
        )
        .await;
    harness
        .register(
            ALICE,
            ALICE_TABLET,
            "apns-token-for-the-tablet",
            Platform::Ios,
        )
        .await;
    harness
        .register(
            ALICE,
            ALICE_LAPTOP,
            "webpush-token-for-the-laptop",
            Platform::Web,
        )
        .await;

    harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift wakes every registered device");

    let handed: Vec<Handed> = harness.sender.all();
    assert_eq!(handed.len(), 3);
    // The platform comes from the device row and the provider from the registration, and
    // a transport picks work off `handles(provider)`. Handing FCM a token minted for APNS
    // is a delivery that fails for a reason no log line would explain, so the pairing is
    // asserted per device rather than in aggregate.
    for one in &handed {
        let expected = match one.platform {
            Platform::Android => PushProvider::Fcm.to_i16(),
            Platform::Ios => PushProvider::Apns.to_i16(),
            Platform::Web => PushProvider::WebPush.to_i16(),
            other => panic!("no device was registered on {other:?}"),
        };
        assert_eq!(one.provider, expected, "{one:?}");
    }
    let platforms: Vec<Platform> = handed.iter().map(|one| one.platform).collect();
    assert!(platforms.contains(&Platform::Android));
    assert!(platforms.contains(&Platform::Ios));
    assert!(platforms.contains(&Platform::Web));

    // Every wake-up carried the same badge, because the badge is a property of the inbox
    // rather than of the screen it lands on.
    let badges: Vec<u32> = handed.iter().map(|one| one.wakeup.badge).collect();
    assert_eq!(badges, vec![1, 1, 1]);
}

#[tokio::test]
async fn a_delivery_counts_every_device_it_reached() {
    let harness = Harness::new();
    harness.account(ALICE, "alice").await;
    harness.account(BOB, "bob").await;
    harness.device(ALICE, ALICE_PHONE, Platform::Android).await;
    harness.device(ALICE, ALICE_TABLET, Platform::Ios).await;
    harness.device(ALICE, ALICE_LAPTOP, Platform::Web).await;
    harness.connect(ALICE, ALICE_LAPTOP).await;
    harness
        .register(
            ALICE,
            ALICE_PHONE,
            "fcm-token-for-the-phone",
            Platform::Android,
        )
        .await;
    harness
        .register(
            ALICE,
            ALICE_TABLET,
            "apns-token-for-the-tablet",
            Platform::Ios,
        )
        .await;

    let delivery: Delivery = harness
        .notify
        .notify(gift(ALICE, BOB))
        .await
        .expect("a gift is delivered");

    // A laptop with a live socket already knows; only the two asleep phones are woken. The
    // count exists so an operator reading it can tell "nobody was told" from "everybody
    // was already looking", which are the same zero in a naive counter.
    assert!(delivery.stored);
    assert_eq!(delivery.woken, 2);
    assert_eq!(delivery.withheld, 0);
    assert_eq!(delivery.failed, 0);
    assert!(delivery.reached());
    assert_eq!(harness.sender.calls(), 2);
}
