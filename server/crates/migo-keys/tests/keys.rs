//! Integration tests for the key service.
//!
//! Everything runs against `MemoryStore` and a recording rate limiter, with hand-written
//! timestamps and identities derived from fixed seeds. No clock is read and no socket is
//! opened, so a failure here is a failure in the code rather than in the machine.
//!
//! The limiter is a fake and the store is real, and that split is the point. The store is
//! where the load-bearing guarantee lives -- a one-time prekey is consumed inside the same
//! call that returns the bundle -- so faking it would fake the thing most worth testing.
//! The limiter, by contrast, is interesting here only for *what* was charged and *when*:
//! the service charges a publication after the write and a fetch before the read, on
//! purpose and for opposite reasons, and a recording fake is the only way to prove either.
//!
//! The properties under test are the ones section 163 makes expensive to get wrong:
//!
//! - a signature the server cannot verify never reaches storage, and the refusal is
//!   `INVALID_KEY_MATERIAL` rather than a validation error, because the two want different
//!   bug reports;
//! - a prekey that is already dead on arrival is refused now rather than failing later;
//! - a one-time prekey is handed out at most once, ever;
//! - a publication replaces rather than merges, so the server never serves a key whose
//!   private half the device has thrown away;
//! - key material is filed under the calling device and no request field can move it;
//! - a device that has run out still gets a usable bundle, and the caller is told;
//! - no refusal carries the bytes it refused.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Result, Secret, Timestamp};
use migo_crypto::{
    IdentityPublic, IdentitySecret, KeyPair, SignedPrekey, IDENTITY_PUBLIC_LEN, PUBLIC_KEY_LEN,
    SIGNATURE_LEN,
};
use migo_keys::model::{
    Caller, KeysConfig, PublishRequest, MAX_BUNDLES_PER_FETCH, MAX_ONE_TIME_PREKEYS,
    ONE_TIME_PREKEY_LOW_WATER, SIGNED_PREKEY_LIFETIME_MS,
};
use migo_keys::service::Keys;
use migo_keys::traits::Keyring;
use migo_keys::Bundle;
use migo_protocol::{codes, Opcode, Platform};
use migo_ratelimit::{BucketKey, Policies, RateLimiter, Scope, TrustTier, Verdict};
use migo_store::model::{NewAccount, NewDevice};
use migo_store::traits::{AccountStore, DeviceStore, KeyStore};
use migo_store::MemoryStore;

// --- fixtures -------------------------------------------------------------

/// One second in milliseconds.
const SECOND: i64 = 1_000;

/// The instant every test calls "now". Far enough from zero that a test can subtract.
const NOW: i64 = 1_700_000_000 * SECOND;

/// Alice, who publishes most of the keys.
const ALICE: u128 = 1;
/// Bob, who fetches them.
const BOB: u128 = 2;

/// Alice's phone.
const ALICE_PHONE: u128 = 101;
/// Alice's laptop. A second device of the same account, for the fanout tests.
const ALICE_LAPTOP: u128 = 102;
/// Bob's phone, which does the fetching.
const BOB_PHONE: u128 = 103;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

/// A caller at `millis`, on the tier an ordinary signed-in user has.
fn caller(account: u128, device: u128, millis: i64) -> Caller {
    Caller::new(id(account), id(device), TrustTier::Established, ts(millis))
}

/// A 32-byte seed from a tag and an index, so every key in a test is distinct and every
/// run derives the same ones.
fn seed(tag: u8, index: u32) -> [u8; 32] {
    let mut out = [tag; 32];
    out[28..].copy_from_slice(&index.to_be_bytes());
    out
}

/// Lower-case hex, matching what the service returns for a fingerprint.
fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A device's own key material, both halves, as the device itself would hold it.
///
/// The private halves live here and never cross into a request: there is no field on
/// [`PublishRequest`] that could carry one, which is section 163 enforced by the shape of
/// the type rather than by a check.
struct Identity {
    secret: IdentitySecret,
    public: IdentityPublic,
}

impl Identity {
    fn new(tag: u8) -> Self {
        let secret = IdentitySecret::from_seeds(seed(tag, 0), seed(tag.wrapping_add(0x80), 0));
        let public = secret.public();
        Self { secret, public }
    }

    /// A well-formed publication: `prekeys` one-time keys, a signed prekey signed by this
    /// identity, and the lifetime the composition root would fill in.
    fn publication(&self, tag: u8, signed_prekey_id: u32, prekeys: u32) -> PublishRequest {
        let signed = SignedPrekey::create(
            &self.secret,
            signed_prekey_id,
            &KeyPair::from_seed(seed(tag, signed_prekey_id)),
        );
        PublishRequest {
            identity_key: self.public.to_bytes().to_vec(),
            signed_prekey_id,
            signed_prekey: signed.public_key.to_vec(),
            signed_prekey_signature: signed.signature.to_vec(),
            signed_prekey_expires_at: ts(NOW).saturating_add_millis(SIGNED_PREKEY_LIFETIME_MS),
            one_time_prekeys: (0..prekeys)
                .map(|index| {
                    let key_id = 1_000 + index;
                    (
                        key_id,
                        KeyPair::from_seed(seed(tag ^ 0x5a, key_id))
                            .public()
                            .to_vec(),
                    )
                })
                .collect(),
        }
    }
}

// --- the recording limiter ------------------------------------------------

/// One call to the limiter, exactly as the service made it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Charge {
    keys: Vec<BucketKey>,
    cost: u32,
    tier: TrustTier,
    now: Timestamp,
}

/// A limiter that records every charge and can be told to refuse the next one.
///
/// Not a token bucket. A real one would answer "allowed" for everything these tests do,
/// which is exactly the answer that makes the charge invisible; the question here is
/// whether a charge happened at all, against which surfaces, and on which side of the
/// side effect.
struct RecordingLimiter {
    policies: Policies,
    charges: Mutex<Vec<Charge>>,
    refuse: AtomicBool,
}

impl RecordingLimiter {
    fn new() -> Self {
        let config = Config::default();
        Self {
            policies: Policies::from_config(&config.rate_limit)
                .expect("the default policies are valid"),
            charges: Mutex::new(Vec::new()),
            refuse: AtomicBool::new(false),
        }
    }

    fn charges(&self) -> Vec<Charge> {
        self.charges
            .lock()
            .expect("no test panics under the lock")
            .clone()
    }

    fn refuse_everything(&self) {
        self.refuse.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl RateLimiter for RecordingLimiter {
    async fn charge(
        &self,
        keys: &[BucketKey],
        cost: u32,
        tier: TrustTier,
        now: Timestamp,
    ) -> Result<Verdict> {
        self.charges
            .lock()
            .expect("no test panics under the lock")
            .push(Charge {
                keys: keys.to_vec(),
                cost,
                tier,
                now,
            });
        if self.refuse.load(Ordering::SeqCst) {
            return Ok(Verdict::Rejected {
                scope: Scope::Account,
                retry_after_ms: 1_500,
            });
        }
        Ok(Verdict::Allowed { remaining: 1 })
    }

    async fn peek(&self, _key: &BucketKey, _tier: TrustTier, _now: Timestamp) -> Result<u32> {
        Ok(1)
    }

    async fn clear(&self, _key: &BucketKey) -> Result<()> {
        Ok(())
    }

    fn policies(&self) -> &Policies {
        &self.policies
    }
}

// --- the harness ----------------------------------------------------------

/// Everything a test needs, built the way `migod` builds it.
struct Harness {
    keys: Keys<MemoryStore, RecordingLimiter>,
    store: Arc<MemoryStore>,
    limiter: Arc<RecordingLimiter>,
    registry: Registry,
}

impl Harness {
    /// Two accounts and three devices, with the default policy.
    async fn new() -> Self {
        Self::configured(KeysConfig::default()).await
    }

    async fn configured(config: KeysConfig) -> Self {
        let registry = Registry::new();
        let store = Arc::new(MemoryStore::new());
        let limiter = Arc::new(RecordingLimiter::new());
        let keys = Keys::new(Arc::clone(&store), Arc::clone(&limiter), &registry, config);
        let harness = Self {
            keys,
            store,
            limiter,
            registry,
        };
        harness.seed_account(ALICE, "alice").await;
        harness.seed_account(BOB, "bob").await;
        harness.seed_device(ALICE, ALICE_PHONE).await;
        harness.seed_device(ALICE, ALICE_LAPTOP).await;
        harness.seed_device(BOB, BOB_PHONE).await;
        harness
    }

    async fn seed_account(&self, value: u128, username: &str) {
        self.store
            .create_account(NewAccount {
                account_id: id(value),
                username: username.to_string(),
                email: Some(format!("{username}@example.test")),
                phone: None,
                password_hash: Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
                locale: "id-ID".to_string(),
                country: Some("ID".to_string()),
                created_at: ts(NOW - SECOND),
            })
            .await
            .expect("a fresh username is free");
    }

    async fn seed_device(&self, account: u128, device: u128) {
        self.store
            .register_device(NewDevice {
                device_id: id(device),
                account_id: id(account),
                platform: Platform::Android,
                display_name: "Pixel".to_string(),
                app_version: "0.1.0".to_string(),
                os_version: Some("14".to_string()),
                device_model: Some("Pixel 8".to_string()),
                created_at: ts(NOW - SECOND),
            })
            .await
            .expect("a fresh device id is free");
    }

    /// How many one-time prekeys the store still holds for a device.
    async fn remaining(&self, account: u128, device: u128) -> u32 {
        self.store
            .one_time_prekey_count(id(account), id(device))
            .await
            .expect("the memory store does not fail a count")
    }

    /// One counter's value, by name and labels.
    fn counter(&self, name: &'static str, labels: &[(&str, &str)]) -> u64 {
        self.registry.counter(name, "", labels).get()
    }

    fn rejections(&self, reason: &str) -> u64 {
        self.counter("migo_keys_publish_rejected_total", &[("reason", reason)])
    }
}

/// Fails unless `result` is an error carrying exactly `code`.
#[track_caller]
fn expect_code<T>(result: Result<T>, code: u32) {
    match result {
        Ok(_) => panic!("expected error {code}, got success"),
        Err(error) => assert_eq!(error.code(), code, "wrong failure class: {error}"),
    }
}

/// Fails unless the refusal says nothing about the bytes it refused.
///
/// Section 174: key material never reaches a log line, and every one of these errors is
/// produced while processing attacker-supplied input. The internal message is the one that
/// gets logged, so it is the one that has to be clean.
#[track_caller]
fn expect_no_key_material<T>(result: Result<T>, material: &[u8]) {
    let error = result.err().expect("expected a refusal");
    assert_clean(&error, material);
}

/// The same check against an error already in hand.
#[track_caller]
fn assert_clean(error: &migo_core::Error, material: &[u8]) {
    let rendered = format!("{error} {:?}", error.public_message());
    assert!(
        !rendered.contains(&hex_of(material)),
        "a refusal carried the material it refused: {rendered}"
    );
    let printable: String = material
        .iter()
        .map(|byte| char::from(*byte))
        .filter(|c| c.is_ascii_graphic())
        .collect();
    if printable.len() > 8 {
        assert!(
            !rendered.contains(&printable),
            "a refusal carried the material it refused: {rendered}"
        );
    }
}

/// The single bundle a device-scoped fetch must return.
#[track_caller]
fn only(bundles: Vec<Bundle>) -> Bundle {
    assert_eq!(bundles.len(), 1, "expected exactly one bundle");
    bundles.into_iter().next().expect("length was just checked")
}

// --- publication: structure ----------------------------------------------

#[tokio::test]
async fn an_unidentified_caller_cannot_publish_or_fetch() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    for (account, device) in [(0, ALICE_PHONE), (ALICE, 0), (0, 0)] {
        let who = Caller::new(id(account), id(device), TrustTier::Established, ts(NOW));
        expect_code(
            h.keys.publish(&who, alice.publication(0x21, 1, 2)).await,
            codes::UNAUTHENTICATED,
        );
        expect_code(
            h.keys.bundles(&who, id(ALICE), None).await,
            codes::UNAUTHENTICATED,
        );
    }

    assert!(
        h.limiter.charges().is_empty(),
        "a caller the server cannot identify is refused before anything is charged"
    );
}

#[tokio::test]
async fn a_wrong_length_signed_prekey_is_a_validation_failure_naming_the_field() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    for length in [0usize, PUBLIC_KEY_LEN - 1, PUBLIC_KEY_LEN + 1] {
        let mut request = alice.publication(0x21, 1, 2);
        request.signed_prekey = vec![7u8; length];
        let error = h
            .keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
            .await
            .expect_err("a prekey that is not 32 bytes is refused");
        assert_eq!(
            error.code(),
            codes::VALIDATION_FAILED,
            "a wrong length is a malformed frame, not broken cryptography"
        );
        assert!(
            format!("{error}").contains("signed_prekey"),
            "the refusal has to name the field so the report says which: {error}"
        );
    }
}

#[tokio::test]
async fn a_wrong_length_signature_is_a_validation_failure() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    for length in [0usize, SIGNATURE_LEN - 1, SIGNATURE_LEN + 1] {
        let mut request = alice.publication(0x21, 1, 2);
        request.signed_prekey_signature = vec![9u8; length];
        expect_code(
            h.keys
                .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
                .await,
            codes::VALIDATION_FAILED,
        );
    }
}

#[tokio::test]
async fn an_identity_key_that_is_not_sixty_four_bytes_is_invalid_key_material() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    // Section 163's "exactly 64 bytes", and it lands as `INVALID_KEY_MATERIAL` rather than
    // as a validation failure because the length is checked by the parse that also rejects
    // an off-curve point. Pinned because it is the one length in the request that does not
    // follow the rule the other two do.
    for length in [0usize, IDENTITY_PUBLIC_LEN - 1, IDENTITY_PUBLIC_LEN + 1] {
        let mut request = alice.publication(0x21, 1, 2);
        request.identity_key = vec![3u8; length];
        expect_code(
            h.keys
                .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
                .await,
            codes::INVALID_KEY_MATERIAL,
        );
    }
    assert_eq!(h.rejections("bad_identity"), 3);
}

#[tokio::test]
async fn more_one_time_prekeys_than_the_ceiling_is_refused_and_exactly_the_ceiling_is_not() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    let over = u32::try_from(MAX_ONE_TIME_PREKEYS).expect("the ceiling fits a u32") + 1;
    expect_code(
        h.keys
            .publish(
                &caller(ALICE, ALICE_PHONE, NOW),
                alice.publication(0x21, 1, over),
            )
            .await,
        codes::VALIDATION_FAILED,
    );

    let at_ceiling = u32::try_from(MAX_ONE_TIME_PREKEYS).expect("the ceiling fits a u32");
    let outcome = h
        .keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 1, at_ceiling),
        )
        .await
        .expect("exactly the ceiling is allowed");
    assert_eq!(outcome.accepted_prekeys, at_ceiling);
}

// --- publication: cryptography -------------------------------------------

#[tokio::test]
async fn an_identity_key_with_a_small_order_exchange_half_is_refused() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    // An all-zero X25519 public key is the identity element: every Diffie-Hellman against
    // it yields an all-zero shared secret, so two peers would derive the same key from
    // nothing and neither would notice.
    let mut identity_key = alice.public.to_bytes().to_vec();
    identity_key[PUBLIC_KEY_LEN..].fill(0);
    let mut request = alice.publication(0x21, 1, 2);
    request.identity_key = identity_key;

    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
            .await,
        codes::INVALID_KEY_MATERIAL,
    );
    assert_eq!(h.rejections("bad_identity"), 1);
}

#[tokio::test]
async fn a_signed_prekey_signed_by_another_identity_is_refused() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);
    let impostor = Identity::new(0x22);

    // The publication carries Alice's identity key and a prekey signed by somebody else.
    // This is the substitution the check exists for: without it the server would store a
    // prekey whose private half Alice does not hold, and every session opened from the
    // bundle would be unreadable by her.
    let mut request = alice.publication(0x21, 1, 2);
    let forged = impostor.publication(0x21, 1, 0);
    request.signed_prekey = forged.signed_prekey;
    request.signed_prekey_signature = forged.signed_prekey_signature;

    let signature = request.signed_prekey_signature.clone();
    let error = h
        .keys
        .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
        .await
        .expect_err("a prekey signed by somebody else is refused");
    assert_eq!(error.code(), codes::INVALID_KEY_MATERIAL);
    assert_clean(&error, &signature);
    assert_eq!(h.rejections("bad_signature"), 1);
    assert_eq!(
        h.remaining(ALICE, ALICE_PHONE).await,
        0,
        "nothing reached storage"
    );
}

#[tokio::test]
async fn a_signature_moved_onto_a_different_key_id_is_refused() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    // The prekey signature covers the key id, so a valid signature cannot be replayed
    // under a second id. Without that binding the two sides could disagree about which
    // prekey a session used while both believed the bundle was authentic.
    let mut request = alice.publication(0x21, 7, 2);
    request.signed_prekey_id = 8;

    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
            .await,
        codes::INVALID_KEY_MATERIAL,
    );
    assert_eq!(h.rejections("bad_signature"), 1);
}

#[tokio::test]
async fn a_one_time_prekey_that_is_not_a_usable_public_key_is_refused() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    let mut request = alice.publication(0x21, 1, 3);
    request.one_time_prekeys[1].1 = vec![0u8; PUBLIC_KEY_LEN];

    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
            .await,
        codes::INVALID_KEY_MATERIAL,
    );
    assert_eq!(h.rejections("bad_prekey"), 1);
    assert_eq!(
        h.remaining(ALICE, ALICE_PHONE).await,
        0,
        "one bad prekey refuses the whole publication rather than storing the good ones"
    );
}

#[tokio::test]
async fn a_one_time_prekey_of_the_wrong_length_is_a_validation_failure() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    let mut request = alice.publication(0x21, 1, 3);
    request.one_time_prekeys[2].1 = vec![1u8; PUBLIC_KEY_LEN + 1];

    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(h.rejections("malformed"), 1);
}

// --- publication: the clock ----------------------------------------------

#[tokio::test]
async fn a_signed_prekey_that_is_already_expired_is_refused_at_publication() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    // Refused as `INVALID_KEY_MATERIAL` and not as a validation failure: the store would
    // give the latter for the same condition, and section 163 asks for the former. The
    // point of the check is that "later" is a moment nobody will connect back to this one.
    for expires_at in [ts(0), ts(NOW - SECOND), ts(NOW)] {
        let mut request = alice.publication(0x21, 1, 2);
        request.signed_prekey_expires_at = expires_at;
        expect_code(
            h.keys
                .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
                .await,
            codes::INVALID_KEY_MATERIAL,
        );
    }
    assert_eq!(h.rejections("expired"), 3);

    // One millisecond of life is life.
    let mut request = alice.publication(0x21, 1, 2);
    request.signed_prekey_expires_at = ts(NOW + 1);
    h.keys
        .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
        .await
        .expect("a prekey that expires next millisecond is still valid this millisecond");
}

#[tokio::test]
async fn the_default_lifetime_is_a_month_and_publishes_cleanly() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    assert_eq!(
        SIGNED_PREKEY_LIFETIME_MS,
        30 * 24 * 60 * 60 * 1_000,
        "the signed prekey lifetime section 163 names"
    );
    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 1, 2),
        )
        .await
        .expect("the lifetime the composition root fills in is accepted");
}

// --- publication: key ids ------------------------------------------------

#[tokio::test]
async fn a_key_id_beyond_the_positive_range_is_refused_rather_than_wrapped() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    // The wire says `u32` and the column says `i32`. Wrapping would store the prekey under
    // a number the client never used, which surfaces much later as a session that will not
    // open, so the top half of the range is refused outright.
    let too_big = u32::try_from(i32::MAX).expect("i32::MAX fits a u32") + 1;

    let signed = alice.publication(0x21, too_big, 0);
    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), signed)
            .await,
        codes::VALIDATION_FAILED,
    );

    let mut one_time = alice.publication(0x21, 1, 2);
    one_time.one_time_prekeys[1].0 = too_big;
    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), one_time)
            .await,
        codes::VALIDATION_FAILED,
    );

    // The largest id that does fit is accepted, so the boundary is where it claims to be.
    let mut edge = alice.publication(0x21, 1, 2);
    edge.one_time_prekeys[0].0 = too_big - 1;
    h.keys
        .publish(&caller(ALICE, ALICE_PHONE, NOW), edge)
        .await
        .expect("i32::MAX is a usable key id");
}

#[tokio::test]
async fn a_duplicate_one_time_prekey_id_is_skipped_rather_than_refused() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    // A client with one repeated id is still publishing a coherent batch, and refusing the
    // whole thing would leave it unable to publish at all. The duplicate is dropped and
    // counted.
    let mut request = alice.publication(0x21, 1, 4);
    let first = request.one_time_prekeys[0].clone();
    request.one_time_prekeys[3] = first;

    let outcome = h
        .keys
        .publish(&caller(ALICE, ALICE_PHONE, NOW), request)
        .await
        .expect("a duplicate id is not a refusal");
    assert_eq!(outcome.accepted_prekeys, 3, "the duplicate was dropped");
    assert_eq!(outcome.one_time_prekeys_remaining, 3);
    assert_eq!(h.remaining(ALICE, ALICE_PHONE).await, 3);
    assert_eq!(
        h.counter("migo_keys_one_time_prekeys_skipped_total", &[]),
        1
    );
    assert_eq!(
        h.counter("migo_keys_one_time_prekeys_accepted_total", &[]),
        3
    );
}

// --- publication: replace, not merge -------------------------------------

#[tokio::test]
async fn publishing_replaces_the_stored_material_rather_than_merging_with_it() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 1, 5),
        )
        .await
        .expect("the first publication is accepted");
    assert_eq!(h.remaining(ALICE, ALICE_PHONE).await, 5);

    // A reinstalled client has lost every old private key. A merge would leave the server
    // serving those for weeks, and every session formed from one would be undecryptable.
    let second = alice.publication(0x99, 2, 2);
    let expected_signed_prekey = second.signed_prekey.clone();
    let outcome = h
        .keys
        .publish(&caller(ALICE, ALICE_PHONE, NOW), second)
        .await
        .expect("a device may republish");
    assert_eq!(outcome.accepted_prekeys, 2);
    assert_eq!(
        outcome.one_time_prekeys_remaining, 2,
        "the five from the first batch are gone, not added to"
    );

    let fetched = only(
        h.keys
            .bundles(
                &caller(BOB, BOB_PHONE, NOW),
                id(ALICE),
                Some(id(ALICE_PHONE)),
            )
            .await
            .expect("a fetch of a device that has published succeeds")
            .bundles,
    );
    assert_eq!(fetched.signed_prekey_id, 2);
    assert_eq!(fetched.signed_prekey, expected_signed_prekey);
}

#[tokio::test]
async fn key_material_is_filed_under_the_calling_device() {
    let h = Harness::new().await;
    let phone = Identity::new(0x11);
    let laptop = Identity::new(0x22);

    // Nothing in a `PublishRequest` names a device, so the only thing that can decide
    // where a publication lands is the caller. Two devices of one account publishing
    // different identities is the test that shows it: if the request could steer the
    // filing, one of these would have overwritten the other.
    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            phone.publication(0x31, 1, 1),
        )
        .await
        .expect("the phone publishes");
    h.keys
        .publish(
            &caller(ALICE, ALICE_LAPTOP, NOW),
            laptop.publication(0x32, 1, 1),
        )
        .await
        .expect("the laptop publishes");

    let from_phone = only(
        h.keys
            .bundles(
                &caller(BOB, BOB_PHONE, NOW),
                id(ALICE),
                Some(id(ALICE_PHONE)),
            )
            .await
            .expect("the phone's bundle")
            .bundles,
    );
    let from_laptop = only(
        h.keys
            .bundles(
                &caller(BOB, BOB_PHONE, NOW),
                id(ALICE),
                Some(id(ALICE_LAPTOP)),
            )
            .await
            .expect("the laptop's bundle")
            .bundles,
    );

    assert_eq!(from_phone.device_id, id(ALICE_PHONE));
    assert_eq!(from_laptop.device_id, id(ALICE_LAPTOP));
    assert_eq!(from_phone.identity_key, phone.public.to_bytes().to_vec());
    assert_eq!(from_laptop.identity_key, laptop.public.to_bytes().to_vec());
    assert_ne!(from_phone.identity_key, from_laptop.identity_key);
}

#[tokio::test]
async fn a_publication_for_an_unknown_device_is_refused_and_never_charged() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    // Publishing is charged after the write, on purpose: a publication the store refused
    // should not spend a budget twenty times the price of a fetch. Nothing about the order
    // is exploitable, because the store refuses the same request every time.
    expect_code(
        h.keys
            .publish(&caller(ALICE, 9_999, NOW), alice.publication(0x21, 1, 2))
            .await,
        codes::NOT_FOUND,
    );
    assert!(
        h.limiter.charges().is_empty(),
        "a publication the store refused costs the caller nothing"
    );
}

// --- publication: what the caller is told --------------------------------

#[tokio::test]
async fn the_fingerprint_is_the_lower_case_hex_of_the_stored_identity() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);
    let bob = Identity::new(0x22);

    let outcome = h
        .keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 1, 2),
        )
        .await
        .expect("the publication is accepted");

    // Derived from the bytes that were just verified, so a client rendering it is
    // rendering a fingerprint of the key the server actually stored.
    assert_eq!(
        outcome.identity_fingerprint,
        hex_of(&alice.public.fingerprint())
    );
    assert_eq!(outcome.identity_fingerprint.len(), 64);
    assert!(
        outcome
            .identity_fingerprint
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "a safety number a user reads aloud is lower-case hex: {}",
        outcome.identity_fingerprint
    );

    let republished = h
        .keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 2, 2),
        )
        .await
        .expect("republishing the same identity is allowed");
    assert_eq!(
        republished.identity_fingerprint, outcome.identity_fingerprint,
        "the fingerprint follows the identity, not the publication"
    );

    let other = h
        .keys
        .publish(
            &caller(ALICE, ALICE_LAPTOP, NOW),
            bob.publication(0x22, 1, 2),
        )
        .await
        .expect("a second device publishes its own identity");
    assert_ne!(other.identity_fingerprint, outcome.identity_fingerprint);
}

#[tokio::test]
async fn a_publication_is_charged_to_the_endpoint_and_the_account_but_never_the_device() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 1, 2),
        )
        .await
        .expect("the publication is accepted");

    let charges = h.limiter.charges();
    assert_eq!(charges.len(), 1, "one publication, one charge");
    let charge = &charges[0];
    assert_eq!(
        charge.keys,
        vec![
            BucketKey::endpoint_write_of_account(id(ALICE), Opcode::KeyPublish),
            BucketKey::account(id(ALICE)),
        ],
        "the service's write endpoint first because it is the tightest surface, then the \
         account; the edge's endpoint bucket is a separate one, so the two layers never \
         empty each other's budget"
    );
    assert!(
        !charge
            .keys
            .iter()
            .any(|key| *key == BucketKey::device(id(ALICE_PHONE))),
        "a per-device budget would let an account with forty devices churn keys forty \
         times as fast, and key churn is an account-level concern"
    );
    assert_eq!(
        charge.cost,
        Opcode::KeyPublish.cost(),
        "priced from the IDL"
    );
    assert_eq!(charge.tier, TrustTier::Established);
    assert_eq!(charge.now, ts(NOW));
    assert!(
        Opcode::KeyPublish.cost() > Opcode::KeyBundleFetch.cost(),
        "publishing writes, and churning it in a loop costs a transaction each time"
    );
}

#[tokio::test]
async fn a_publication_the_limiter_refuses_is_reported_as_rate_limited() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);
    h.limiter.refuse_everything();

    expect_code(
        h.keys
            .publish(
                &caller(ALICE, ALICE_PHONE, NOW),
                alice.publication(0x21, 1, 2),
            )
            .await,
        codes::RATE_LIMITED,
    );
}

// --- fetch: structure ----------------------------------------------------

#[tokio::test]
async fn a_nil_subject_is_refused_before_anything_is_charged() {
    let h = Harness::new().await;
    let who = caller(BOB, BOB_PHONE, NOW);

    expect_code(
        h.keys.bundles(&who, Id::NIL, None).await,
        codes::FIELD_REQUIRED,
    );
    expect_code(
        h.keys.bundles(&who, id(ALICE), Some(Id::NIL)).await,
        codes::FIELD_REQUIRED,
    );
    assert!(
        h.limiter.charges().is_empty(),
        "a request that names nobody is refused before it is priced"
    );
}

#[tokio::test]
async fn a_fetch_of_a_device_that_has_published_nothing_is_empty_and_still_charged() {
    let h = Harness::new().await;

    // Empty is a valid answer: whether an account with no keys is `NOT_FOUND` or an empty
    // list is a promise the opcode makes, not a fact about key material. And the charge
    // lands anyway, because a fetch is charged before the read -- otherwise a caller could
    // probe every device on the server for free.
    let fetched = h
        .keys
        .bundles(
            &caller(BOB, BOB_PHONE, NOW),
            id(ALICE),
            Some(id(ALICE_PHONE)),
        )
        .await
        .expect("an account with no published keys is not an error");
    assert!(fetched.bundles.is_empty());
    assert!(!fetched.any_exhausted);
    assert_eq!(h.limiter.charges().len(), 1);
}

#[tokio::test]
async fn a_fetch_is_charged_to_the_endpoint_and_the_account_before_the_read() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);
    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 1, 3),
        )
        .await
        .expect("the publication is accepted");

    h.limiter.refuse_everything();
    expect_code(
        h.keys
            .bundles(
                &caller(BOB, BOB_PHONE, NOW + SECOND),
                id(ALICE),
                Some(id(ALICE_PHONE)),
            )
            .await,
        codes::RATE_LIMITED,
    );
    assert_eq!(
        h.remaining(ALICE, ALICE_PHONE).await,
        3,
        "a refused fetch must not consume a prekey: that is what charging first buys"
    );

    let charges = h.limiter.charges();
    let fetch = charges.last().expect("the fetch was charged");
    assert_eq!(
        fetch.keys,
        vec![
            BucketKey::endpoint_write_of_account(id(BOB), Opcode::KeyBundleFetch),
            BucketKey::account(id(BOB)),
        ],
        "the fetcher pays, not the subject"
    );
    assert_eq!(fetch.cost, Opcode::KeyBundleFetch.cost());
}

// --- fetch: one prekey, once --------------------------------------------

#[tokio::test]
async fn every_fetch_consumes_one_prekey_and_never_the_same_one_twice() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);
    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 1, 3),
        )
        .await
        .expect("the publication is accepted");

    let mut handed_out = Vec::new();
    for round in 0..3 {
        let bundle = only(
            h.keys
                .bundles(
                    &caller(BOB, BOB_PHONE, NOW + round * SECOND),
                    id(ALICE),
                    Some(id(ALICE_PHONE)),
                )
                .await
                .expect("a device with prekeys left serves a bundle")
                .bundles,
        );
        let (key_id, key) = bundle
            .one_time_prekey
            .expect("three prekeys cover three fetches");
        assert_eq!(key.len(), PUBLIC_KEY_LEN);
        assert_eq!(
            h.remaining(ALICE, ALICE_PHONE).await,
            2 - u32::try_from(round).expect("a small loop counter fits"),
        );
        handed_out.push(key_id);
    }

    handed_out.sort_unstable();
    let mut unique = handed_out.clone();
    unique.dedup();
    assert_eq!(
        handed_out, unique,
        "handing the same one-time prekey to two peers silently reduces the guarantee to \
         the signed prekey alone"
    );
}

#[tokio::test]
async fn a_device_that_has_run_out_still_serves_a_usable_bundle_and_says_so() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);
    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 1, 1),
        )
        .await
        .expect("the publication is accepted");

    let first = only(
        h.keys
            .bundles(
                &caller(BOB, BOB_PHONE, NOW),
                id(ALICE),
                Some(id(ALICE_PHONE)),
            )
            .await
            .expect("the only prekey is served")
            .bundles,
    );
    assert!(first.one_time_prekey.is_some());
    assert!(!first.is_exhausted());

    let fetched = h
        .keys
        .bundles(
            &caller(BOB, BOB_PHONE, NOW + SECOND),
            id(ALICE),
            Some(id(ALICE_PHONE)),
        )
        .await
        .expect("running out weakens the first message, it does not fail the conversation");
    let second = only(fetched.bundles.clone());
    assert!(second.is_exhausted());
    assert!(
        fetched.any_exhausted,
        "the flag section 163 asks the server to set, so the owner gets a top-up nudge"
    );
    assert_eq!(
        second.signed_prekey_id, 1,
        "the signed prekey is still there"
    );
    assert_eq!(second.signed_prekey.len(), PUBLIC_KEY_LEN);
    assert_eq!(
        h.counter("migo_keys_bundles_without_one_time_prekey_total", &[]),
        1
    );
    assert_eq!(h.counter("migo_keys_bundles_served_total", &[]), 2);
}

#[tokio::test]
async fn policy_can_refuse_the_weaker_session_instead_of_serving_it() {
    let alice = Identity::new(0x11);

    for refuse_when_exhausted in [false, true] {
        let h = Harness::configured(KeysConfig {
            refuse_when_exhausted,
        })
        .await;
        h.keys
            .publish(
                &caller(ALICE, ALICE_PHONE, NOW),
                alice.publication(0x21, 1, 0),
            )
            .await
            .expect("a publication with no one-time prekeys is legal");

        let result = h
            .keys
            .bundles(
                &caller(BOB, BOB_PHONE, NOW),
                id(ALICE),
                Some(id(ALICE_PHONE)),
            )
            .await;

        if refuse_when_exhausted {
            expect_code(result, codes::PREKEYS_EXHAUSTED);
            assert_eq!(
                h.counter("migo_keys_fetches_refused_exhausted_total", &[]),
                1,
                "the metric records what the policy cost: the prekeys are already spent"
            );
        } else {
            let fetched = result.expect("the default is to serve the weaker session");
            assert!(fetched.any_exhausted);
            assert_eq!(fetched.bundles.len(), 1);
            assert_eq!(
                h.counter("migo_keys_fetches_refused_exhausted_total", &[]),
                0
            );
        }
    }
}

#[tokio::test]
async fn the_low_water_mark_is_where_a_client_is_told_to_top_up() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    assert!(
        u64::from(ONE_TIME_PREKEY_LOW_WATER)
            < u64::try_from(MAX_ONE_TIME_PREKEYS).expect("the ceiling fits"),
        "a threshold at or above the ceiling would ask for a top-up that can never land"
    );

    let ceiling = u32::try_from(MAX_ONE_TIME_PREKEYS).expect("the ceiling fits a u32");
    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 1, ceiling),
        )
        .await
        .expect("a full batch is accepted");

    for round in 0..(ceiling - ONE_TIME_PREKEY_LOW_WATER) {
        h.keys
            .bundles(
                &caller(BOB, BOB_PHONE, NOW + i64::from(round)),
                id(ALICE),
                Some(id(ALICE_PHONE)),
            )
            .await
            .expect("a fetch while prekeys remain succeeds");
    }
    assert_eq!(
        h.remaining(ALICE, ALICE_PHONE).await,
        ONE_TIME_PREKEY_LOW_WATER,
        "the count a client watches to decide when to publish a fresh batch"
    );
}

// --- fetch: whole accounts ----------------------------------------------

#[tokio::test]
async fn a_fetch_without_a_device_returns_one_bundle_per_live_device() {
    let h = Harness::new().await;
    let phone = Identity::new(0x11);
    let laptop = Identity::new(0x22);
    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            phone.publication(0x31, 1, 2),
        )
        .await
        .expect("the phone publishes");
    h.keys
        .publish(
            &caller(ALICE, ALICE_LAPTOP, NOW),
            laptop.publication(0x32, 1, 2),
        )
        .await
        .expect("the laptop publishes");

    let fetched = h
        .keys
        .bundles(&caller(BOB, BOB_PHONE, NOW), id(ALICE), None)
        .await
        .expect("a fanout fetch succeeds");
    assert_eq!(fetched.bundles.len(), 2);
    assert!(!fetched.any_exhausted);

    let mut devices: Vec<Id> = fetched.bundles.iter().map(|b| b.device_id).collect();
    devices.sort_unstable();
    assert_eq!(devices, vec![id(ALICE_PHONE), id(ALICE_LAPTOP)]);
    for bundle in &fetched.bundles {
        assert_eq!(bundle.account_id, id(ALICE));
        assert!(bundle.one_time_prekey.is_some());
    }

    // One prekey each, not two from one device.
    assert_eq!(h.remaining(ALICE, ALICE_PHONE).await, 1);
    assert_eq!(h.remaining(ALICE, ALICE_LAPTOP).await, 1);
    assert_eq!(
        h.limiter.charges().len(),
        3,
        "two publications and one fetch, whatever the fanout width"
    );
}

#[tokio::test]
async fn a_revoked_devices_key_material_is_never_served() {
    let h = Harness::new().await;
    let phone = Identity::new(0x11);
    let laptop = Identity::new(0x22);
    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            phone.publication(0x31, 1, 2),
        )
        .await
        .expect("the phone publishes");
    h.keys
        .publish(
            &caller(ALICE, ALICE_LAPTOP, NOW),
            laptop.publication(0x32, 1, 2),
        )
        .await
        .expect("the laptop publishes");

    // Revocation is not this crate's operation -- whoever removes a device calls the store
    // -- and the filter is applied in the query, which is why nothing here has to remember
    // to skip it.
    h.store
        .revoke_device_keys(id(ALICE), id(ALICE_PHONE), ts(NOW + SECOND))
        .await
        .expect("revocation succeeds");

    let by_device = h
        .keys
        .bundles(
            &caller(BOB, BOB_PHONE, NOW + SECOND),
            id(ALICE),
            Some(id(ALICE_PHONE)),
        )
        .await
        .expect("a revoked device is absent, not an error");
    assert!(
        by_device.bundles.is_empty(),
        "key material for a device that no longer exists can only produce unreadable sessions"
    );

    let by_account = h
        .keys
        .bundles(&caller(BOB, BOB_PHONE, NOW + SECOND), id(ALICE), None)
        .await
        .expect("the fanout still works");
    let survivor = only(by_account.bundles);
    assert_eq!(survivor.device_id, id(ALICE_LAPTOP));
}

#[tokio::test]
async fn more_devices_than_the_ceiling_are_served_rather_than_refused() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    // The bundles have already been taken and their prekeys already spent by the time the
    // count is known, so throwing them away would consume key material and deliver
    // nothing. It is a fact worth a log line, not a refusal.
    let extra = u32::try_from(MAX_BUNDLES_PER_FETCH).expect("the ceiling fits a u32") + 1;
    for index in 0..extra {
        let device = 200 + u128::from(index);
        h.seed_device(ALICE, device).await;
        h.keys
            .publish(&caller(ALICE, device, NOW), alice.publication(0x41, 1, 1))
            .await
            .expect("each device publishes");
    }

    let fetched = h
        .keys
        .bundles(&caller(BOB, BOB_PHONE, NOW), id(ALICE), None)
        .await
        .expect("a wide account is served, not refused");
    assert_eq!(
        u32::try_from(fetched.bundles.len()).expect("the width fits a u32"),
        extra,
        "every device that published is served"
    );
    assert!(fetched.bundles.len() > MAX_BUNDLES_PER_FETCH);
}

// --- fetch: what a sender can check for itself ---------------------------

#[tokio::test]
async fn a_fetched_bundle_verifies_without_trusting_the_server() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);
    let published = alice.publication(0x21, 4, 2);
    let expected_signature = published.signed_prekey_signature.clone();
    h.keys
        .publish(&caller(ALICE, ALICE_PHONE, NOW), published)
        .await
        .expect("the publication is accepted");

    let bundle = only(
        h.keys
            .bundles(
                &caller(BOB, BOB_PHONE, NOW),
                id(ALICE),
                Some(id(ALICE_PHONE)),
            )
            .await
            .expect("the bundle is served")
            .bundles,
    );

    // This is exactly what a sender does with a bundle before composing anything: parse
    // the identity, then verify the prekey against it. The server chose which bundle to
    // serve, so without this check it could substitute a prekey it controls.
    assert_eq!(bundle.identity_key.len(), IDENTITY_PUBLIC_LEN);
    let identity = IdentityPublic::parse(&bundle.identity_key)
        .expect("a served identity key parses on the sender's side");
    assert_eq!(identity.to_bytes(), alice.public.to_bytes());

    let mut public_key = [0u8; PUBLIC_KEY_LEN];
    public_key.copy_from_slice(&bundle.signed_prekey);
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(&bundle.signed_prekey_signature);
    SignedPrekey {
        key_id: bundle.signed_prekey_id,
        public_key,
        signature,
    }
    .verify(&identity)
    .expect("the signature the publisher made survives the round trip intact");
    assert_eq!(bundle.signed_prekey_signature, expected_signature);
    assert_eq!(
        bundle.signed_prekey_expires_at,
        ts(NOW).saturating_add_millis(SIGNED_PREKEY_LIFETIME_MS),
        "the expiry travels with the bundle so a nearly dead prekey becomes a nudge"
    );

    let (_, one_time) = bundle
        .one_time_prekey
        .expect("a device with prekeys serves one");
    let mut probe = Vec::with_capacity(IDENTITY_PUBLIC_LEN);
    probe.extend_from_slice(&identity.signing);
    probe.extend_from_slice(&one_time);
    IdentityPublic::parse(&probe).expect("a served one-time prekey is a usable public key");
}

// --- metrics -------------------------------------------------------------

#[tokio::test]
async fn every_rejection_reason_has_its_own_series_and_starts_at_zero() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);
    let impostor = Identity::new(0x22);

    // A panel reading "no data" for "publications refused for a bad signature" cannot be
    // told apart from a panel whose query is wrong, so every series exists from startup.
    for reason in [
        "malformed",
        "bad_identity",
        "bad_signature",
        "bad_prekey",
        "expired",
    ] {
        assert_eq!(
            h.rejections(reason),
            0,
            "{reason} starts registered at zero"
        );
    }
    assert_eq!(h.counter("migo_keys_published_total", &[]), 0);

    let mut malformed = alice.publication(0x21, 1, 1);
    malformed.signed_prekey = vec![0u8; 1];
    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), malformed)
            .await,
        codes::VALIDATION_FAILED,
    );

    let mut bad_identity = alice.publication(0x21, 1, 1);
    bad_identity.identity_key = vec![0u8; IDENTITY_PUBLIC_LEN];
    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), bad_identity)
            .await,
        codes::INVALID_KEY_MATERIAL,
    );

    let mut bad_signature = alice.publication(0x21, 1, 1);
    bad_signature.signed_prekey_signature =
        impostor.publication(0x21, 1, 0).signed_prekey_signature;
    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), bad_signature)
            .await,
        codes::INVALID_KEY_MATERIAL,
    );

    let mut bad_prekey = alice.publication(0x21, 1, 1);
    bad_prekey.one_time_prekeys[0].1 = vec![0u8; PUBLIC_KEY_LEN];
    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), bad_prekey)
            .await,
        codes::INVALID_KEY_MATERIAL,
    );

    let mut expired = alice.publication(0x21, 1, 1);
    expired.signed_prekey_expires_at = ts(NOW - SECOND);
    expect_code(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), expired)
            .await,
        codes::INVALID_KEY_MATERIAL,
    );

    for reason in [
        "malformed",
        "bad_identity",
        "bad_signature",
        "bad_prekey",
        "expired",
    ] {
        assert_eq!(h.rejections(reason), 1, "{reason} counted exactly once");
    }
    assert_eq!(
        h.counter("migo_keys_published_total", &[]),
        0,
        "not one of those reached storage"
    );
    assert_eq!(h.remaining(ALICE, ALICE_PHONE).await, 0);
}

#[tokio::test]
async fn an_accepted_publication_moves_the_publication_counters() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);

    h.keys
        .publish(
            &caller(ALICE, ALICE_PHONE, NOW),
            alice.publication(0x21, 1, 4),
        )
        .await
        .expect("the publication is accepted");
    h.keys
        .publish(
            &caller(ALICE, ALICE_LAPTOP, NOW),
            alice.publication(0x22, 1, 2),
        )
        .await
        .expect("a second publication is accepted");

    assert_eq!(h.counter("migo_keys_published_total", &[]), 2);
    assert_eq!(
        h.counter("migo_keys_one_time_prekeys_accepted_total", &[]),
        6
    );
    assert_eq!(
        h.counter("migo_keys_one_time_prekeys_skipped_total", &[]),
        0
    );

    // Section 174: not one series here is labelled by account, device, or conversation. A
    // counter keyed by device id would be the social graph written in cardinality.
    let rendered = h.registry.render();
    for forbidden in [
        &id(ALICE).to_text(),
        &id(ALICE_PHONE).to_text(),
        &id(ALICE_LAPTOP).to_text(),
    ] {
        assert!(
            !rendered.contains(forbidden.as_str()),
            "a metric named a subject: {forbidden}"
        );
    }
}

#[tokio::test]
async fn no_refusal_carries_the_key_material_it_refused() {
    let h = Harness::new().await;
    let alice = Identity::new(0x11);
    let impostor = Identity::new(0x22);

    let identity_key = vec![0xabu8; IDENTITY_PUBLIC_LEN];
    let mut bad_identity = alice.publication(0x21, 1, 1);
    bad_identity.identity_key = identity_key.clone();
    expect_no_key_material(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), bad_identity)
            .await,
        &identity_key,
    );

    let forged = impostor.publication(0x21, 1, 0).signed_prekey_signature;
    let mut bad_signature = alice.publication(0x21, 1, 1);
    bad_signature.signed_prekey_signature = forged.clone();
    expect_no_key_material(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), bad_signature)
            .await,
        &forged,
    );

    let mut bad_prekey = alice.publication(0x21, 1, 1);
    let prekey = vec![0u8; PUBLIC_KEY_LEN];
    bad_prekey.one_time_prekeys[0].1 = prekey.clone();
    expect_no_key_material(
        h.keys
            .publish(&caller(ALICE, ALICE_PHONE, NOW), bad_prekey)
            .await,
        &prekey,
    );

    // A length is not key material, and it is the one number that turns "the server
    // refused my key" into a fixable bug report, so it is allowed to be in the message.
    let mut short = alice.publication(0x21, 1, 1);
    short.signed_prekey = vec![1u8; 7];
    let error = h
        .keys
        .publish(&caller(ALICE, ALICE_PHONE, NOW), short)
        .await
        .expect_err("a short prekey is refused");
    assert!(
        format!("{error}").contains('7') && format!("{error}").contains("32"),
        "the refusal should say what was expected and what arrived: {error}"
    );
}
