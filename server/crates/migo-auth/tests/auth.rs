//! Integration tests for the authentication service.
//!
//! Everything runs against `MemoryStore` and a `CacheRateLimiter` over `MemoryCache`,
//! with a seeded `SeededRandom` and hand-written timestamps. No clock is read and no
//! address is real, so a failure here is a failure in the code rather than a failure in
//! the machine — which is the property that makes a test worth keeping.
//!
//! The tests are written against the properties that would be expensive to get wrong in
//! production, not against the shape of the code: that a stolen refresh token is worth
//! minutes, that a revoked session stops working on the next request, that an unknown
//! account and a wrong password are indistinguishable, and that a network address never
//! reaches storage in full.

use std::net::IpAddr;
use std::sync::Arc;

use migo_auth::{
    Auth, Authenticator, DeviceClaim, Grant, PasswordChange, Refresh, Registration, RequestContext,
    SignIn,
};
use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, Random, Secret, SeededRandom, Timestamp};
use migo_protocol::{codes, Platform};
use migo_ratelimit::{CacheRateLimiter, Policies};
use migo_store::traits::{AccountStore, DeviceStore, SessionStore};
use migo_store::MemoryStore;

/// One second in milliseconds.
const SECOND: i64 = 1_000;
/// One minute.
const MINUTE: i64 = 60 * SECOND;
/// One hour.
const HOUR: i64 = 60 * MINUTE;
/// One day.
const DAY: i64 = 24 * HOUR;

/// A password long enough to pass the length rule and absent from the common list.
const GOOD_PASSWORD: &str = "sunflower gravel bicycle";

/// Base64 for thirty-two bytes, which is exactly the minimum key length.
///
/// At the boundary on purpose: a key one byte shorter has to be refused, and the only way
/// to know the check is still there is for the tests to sit against it.
const TEST_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// A different key of the same length, for the cross-key rejection test.
const OTHER_KEY: &str = "//79/Pv6+fj39vX08/Lx8O/u7ezr6uno5+bl5OPi4eA=";

type TestAuth = Auth<MemoryStore, CacheRateLimiter<MemoryCache>>;

/// Everything a test needs, built the way `migod` builds it.
struct Harness {
    auth: TestAuth,
    store: Arc<MemoryStore>,
    registry: Registry,
}

impl Harness {
    fn new() -> Self {
        Self::with_config(Config::default())
    }

    fn with_config(mut config: Config) -> Self {
        if config.auth.token_key.is_none() {
            config.auth.token_key = Some(Secret::new(TEST_KEY));
        }
        let store = Arc::new(MemoryStore::new());
        let cache = Arc::new(MemoryCache::new());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&config.rate_limit).expect("default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(cache, policies, &registry));
        let auth = Auth::new(
            Arc::clone(&store),
            limiter,
            &config,
            &registry,
            Box::new(SeededRandom::new(0x5eed_1234)) as Box<dyn Random>,
        )
        .expect("the harness configuration is buildable");
        Self {
            auth,
            store,
            registry,
        }
    }

    /// Registers `username` at `millis`.
    async fn register_at(&self, username: &str, millis: i64) -> Grant {
        self.auth
            .register(registration(username), &context(millis))
            .await
            .expect("registration succeeds")
    }

    /// The same store behind a freshly configured authenticator, for tests
    /// that need a config value naming an account that already exists — the
    /// owner designation cannot be set before the owner has registered, so
    /// the test registers first and rebuilds over the same rows.
    fn reconfigured(&self, mut config: Config) -> Self {
        if config.auth.token_key.is_none() {
            config.auth.token_key = Some(Secret::new(TEST_KEY));
        }
        let cache = Arc::new(MemoryCache::new());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&config.rate_limit).expect("default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(cache, policies, &registry));
        let auth = Auth::new(
            Arc::clone(&self.store),
            limiter,
            &config,
            &registry,
            Box::new(SeededRandom::new(0x5eed_1234)) as Box<dyn Random>,
        )
        .expect("the reconfigured harness is buildable");
        Self {
            auth,
            store: Arc::clone(&self.store),
            registry,
        }
    }

    /// The metric line for `name`, if it has been touched.
    fn metric(&self, name: &str) -> Option<String> {
        self.registry
            .render()
            .lines()
            .find(|line| line.starts_with(name))
            .map(str::to_string)
    }
}

/// A request context at `millis`, from a documentation address.
fn context(millis: i64) -> RequestContext {
    RequestContext::at(Timestamp::from_millis(millis))
        .from_ip("203.0.113.77".parse::<IpAddr>().unwrap())
        .with_user_agent("Mozilla/5.0 (X11; Linux x86_64) migo-test")
}

/// The same, from a different network, so the two do not share a bucket.
fn context_from(millis: i64, ip: &str) -> RequestContext {
    RequestContext::at(Timestamp::from_millis(millis)).from_ip(ip.parse::<IpAddr>().unwrap())
}

/// A registration for `username`, with a good password and a web device.
fn registration(username: &str) -> Registration {
    Registration {
        username: username.to_string(),
        email: None,
        phone: None,
        password: Secret::new(GOOD_PASSWORD),
        locale: "en".to_string(),
        country: Some("ID".to_string()),
        device: DeviceClaim::new(Platform::Web, "Firefox on Linux"),
        captcha: None,
        server: None,
        identity_public_key: None,
    }
}

/// A sign-in for `identifier` with the good password on a new device.
fn sign_in(identifier: &str) -> SignIn {
    SignIn {
        identifier: identifier.to_string(),
        password: Secret::new(GOOD_PASSWORD),
        device: DeviceClaim::new(Platform::Web, "Firefox on Linux"),
        captcha: None,
        server: None,
    }
}

/// The identity behind a grant, at `millis`.
async fn identify(
    auth: &TestAuth,
    grant: &Grant,
    millis: i64,
) -> migo_core::Result<migo_auth::Identity> {
    auth.authenticate(&grant.access_token, grant.device_id, &context(millis))
        .await
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registration_returns_a_usable_session() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    assert!(grant.is_new_account);
    assert!(!grant.access_token.is_empty());
    assert_eq!(
        grant.access_expires_at,
        Timestamp::from_millis(1_000 + 900 * SECOND),
        "the access token lives for the configured fifteen minutes"
    );
    assert_eq!(
        grant.refresh_expires_at,
        Timestamp::from_millis(1_000 + 30 * DAY),
        "the refresh token lives for the configured thirty days"
    );

    let identity = identify(&harness.auth, &grant, 2_000).await.unwrap();
    assert_eq!(identity.account_id(), grant.account_id);
    assert_eq!(identity.username, "ada");
    assert!(identity.is_fresh(Timestamp::from_millis(2_000)));

    let account = harness
        .store
        .account_by_username("ada")
        .await
        .unwrap()
        .expect("the account is stored under its folded name");
    assert_eq!(account.account_id, grant.account_id);
    assert_ne!(
        account.password_hash.expose(),
        GOOD_PASSWORD,
        "the password is never stored as itself"
    );
    assert!(
        account.password_hash.expose().starts_with("$argon2id$"),
        "the stored hash is Argon2id and says so"
    );
}

#[tokio::test]
async fn a_registration_also_creates_a_private_profile() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    let profile = harness
        .store
        .profile(grant.account_id)
        .await
        .unwrap()
        .expect("registration creates a profile");
    assert_eq!(profile.display_name, "ada");
    assert_eq!(
        profile.who_can_message,
        migo_store::model::Visibility::Friends,
        "a new account is not open to messages from strangers by default"
    );
}

#[tokio::test]
async fn a_taken_username_is_refused_by_its_own_code() {
    let harness = Harness::new();
    harness.register_at("ada", 1_000).await;

    let error = harness
        .auth
        .register(registration("Ada"), &context_from(2_000, "198.51.100.4"))
        .await
        .expect_err("the folded name is already taken");
    assert_eq!(error.code(), codes::USERNAME_TAKEN);
}

#[tokio::test]
async fn a_reserved_username_never_reaches_the_store() {
    let harness = Harness::new();
    let error = harness
        .auth
        .register(registration("support"), &context(1_000))
        .await
        .expect_err("reserved names are refused");
    assert_eq!(error.code(), codes::USERNAME_RESERVED);
    assert!(
        harness
            .store
            .account_by_username("support")
            .await
            .unwrap()
            .is_none(),
        "a refused registration leaves nothing behind"
    );
}

#[tokio::test]
async fn a_weak_password_is_refused_before_any_hashing() {
    let harness = Harness::new();
    let mut request = registration("ada");
    request.password = Secret::new("short");
    let error = harness
        .auth
        .register(request, &context(1_000))
        .await
        .expect_err("a short password is refused");
    assert_eq!(error.code(), codes::WEAK_PASSWORD);
}

#[tokio::test]
async fn registration_can_be_switched_off() {
    let mut config = Config::default();
    config.auth.allow_registration = false;
    let harness = Harness::with_config(config);

    let error = harness
        .auth
        .register(registration("ada"), &context(1_000))
        .await
        .expect_err("registration is disabled");
    assert_eq!(error.code(), codes::FEATURE_DISABLED);
}

// ---------------------------------------------------------------------------
// Sign-in
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sign_in_works_by_username_with_or_without_an_at_sign() {
    let harness = Harness::new();
    harness.register_at("ada", 1_000).await;

    for identifier in ["ada", "@ada", "ADA", "  Ada  "] {
        harness
            .auth
            .sign_in(sign_in(identifier), &context_from(2_000, "198.51.100.4"))
            .await
            .unwrap_or_else(|error| panic!("{identifier} should sign in: {error:?}"));
    }
}

#[tokio::test]
async fn sign_in_works_by_email() {
    let harness = Harness::new();
    let mut request = registration("ada");
    request.email = Some("Ada@Example.COM".to_string());
    harness
        .auth
        .register(request, &context(1_000))
        .await
        .unwrap();

    harness
        .auth
        .sign_in(
            sign_in("ada@example.com"),
            &context_from(2_000, "198.51.100.4"),
        )
        .await
        .expect("the stored address is folded on both sides");
}

#[tokio::test]
async fn an_unknown_account_and_a_wrong_password_are_indistinguishable() {
    let harness = Harness::new();
    harness.register_at("ada", 1_000).await;

    let unknown = harness
        .auth
        .sign_in(sign_in("nobody"), &context_from(2_000, "198.51.100.4"))
        .await
        .expect_err("there is no such account");

    let mut wrong = sign_in("ada");
    wrong.password = Secret::new("not the right passphrase");
    let bad = harness
        .auth
        .sign_in(wrong, &context_from(3_000, "198.51.100.9"))
        .await
        .expect_err("the password is wrong");

    assert_eq!(unknown.code(), codes::INVALID_CREDENTIALS);
    assert_eq!(bad.code(), codes::INVALID_CREDENTIALS);
    assert_eq!(
        unknown.public_message(),
        bad.public_message(),
        "the two paths must say exactly the same thing to the caller"
    );
}

#[tokio::test]
async fn a_suspended_account_is_only_told_so_after_its_password_verifies() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;
    harness
        .store
        .set_status(
            grant.account_id,
            migo_store::model::AccountStatus::Suspended,
            None,
            Timestamp::from_millis(1_500),
        )
        .await
        .unwrap();

    let mut wrong = sign_in("ada");
    wrong.password = Secret::new("not the right passphrase");
    let with_wrong_password = harness
        .auth
        .sign_in(wrong, &context_from(2_000, "198.51.100.4"))
        .await
        .expect_err("a wrong password does not get to learn about the suspension");
    assert_eq!(with_wrong_password.code(), codes::INVALID_CREDENTIALS);

    let with_right_password = harness
        .auth
        .sign_in(sign_in("ada"), &context_from(3_000, "198.51.100.9"))
        .await
        .expect_err("the account cannot sign in");
    assert_eq!(with_right_password.code(), codes::ACCOUNT_SUSPENDED);
}

#[tokio::test]
async fn the_device_limit_is_enforced_and_named() {
    let mut config = Config::default();
    config.auth.max_devices_per_user = 2;
    let harness = Harness::with_config(config);
    harness.register_at("ada", 1_000).await;

    harness
        .auth
        .sign_in(sign_in("ada"), &context_from(2_000, "198.51.100.4"))
        .await
        .expect("the second device fits");
    let error = harness
        .auth
        .sign_in(sign_in("ada"), &context_from(3_000, "198.51.100.9"))
        .await
        .expect_err("the third does not");
    assert_eq!(error.code(), codes::TOO_MANY_SESSIONS);
}

#[tokio::test]
async fn signing_in_on_a_known_device_does_not_register_another() {
    let harness = Harness::new();
    let first = harness.register_at("ada", 1_000).await;

    let mut request = sign_in("ada");
    request.device = DeviceClaim::new(Platform::Web, "Firefox on Linux").on_device(first.device_id);
    let second = harness
        .auth
        .sign_in(request, &context_from(2_000, "198.51.100.4"))
        .await
        .unwrap();

    assert_eq!(second.device_id, first.device_id);
    assert_ne!(second.session_id, first.session_id);
    assert_eq!(
        harness
            .store
            .devices_for_account(first.account_id)
            .await
            .unwrap()
            .len(),
        1,
        "one device, two sessions"
    );
}

#[tokio::test]
async fn a_device_belonging_to_another_account_is_refused() {
    let harness = Harness::new();
    let ada = harness.register_at("ada", 1_000).await;
    harness
        .auth
        .register(registration("grace"), &context_from(2_000, "198.51.100.4"))
        .await
        .unwrap();

    let mut request = sign_in("grace");
    request.device = DeviceClaim::new(Platform::Web, "Firefox on Linux").on_device(ada.device_id);
    let error = harness
        .auth
        .sign_in(request, &context_from(3_000, "198.51.100.9"))
        .await
        .expect_err("a device cannot be claimed across accounts");
    assert_eq!(error.code(), codes::DEVICE_MISMATCH);
}

// ---------------------------------------------------------------------------
// Refresh rotation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_refresh_rotates_both_tokens_and_keeps_the_family() {
    let harness = Harness::new();
    let first = harness.register_at("ada", 1_000).await;

    let second = harness
        .auth
        .refresh(
            Refresh {
                refresh_token: first.refresh_token.clone(),
                device_id: first.device_id,
            },
            &context(20 * MINUTE),
        )
        .await
        .expect("the refresh window is still open");

    assert_ne!(second.access_token, first.access_token);
    assert_ne!(
        second.refresh_token.expose(),
        first.refresh_token.expose(),
        "a refresh mints a new refresh token, or a stolen one would be permanent"
    );
    assert_ne!(second.session_id, first.session_id);
    assert_eq!(second.account_id, first.account_id);
    assert_eq!(second.device_id, first.device_id);

    identify(&harness.auth, &second, 20 * MINUTE)
        .await
        .expect("the new access token works");
}

#[tokio::test]
async fn a_refresh_carries_the_authentication_time_forward() {
    let harness = Harness::new();
    let first = harness.register_at("ada", 1_000).await;

    let identity = identify(&harness.auth, &first, 1_000).await.unwrap();
    assert!(identity.is_fresh(Timestamp::from_millis(1_000)));

    let second = harness
        .auth
        .refresh(
            Refresh {
                refresh_token: first.refresh_token.clone(),
                device_id: first.device_id,
            },
            &context(HOUR),
        )
        .await
        .unwrap();

    let refreshed = identify(&harness.auth, &second, HOUR).await.unwrap();
    assert!(
        !refreshed.is_fresh(Timestamp::from_millis(HOUR)),
        "presenting a refresh token proves possession of a token, not the presence of a person"
    );
    let error = refreshed
        .require_fresh(Timestamp::from_millis(HOUR))
        .expect_err("a sensitive operation has to ask again");
    assert_eq!(error.code(), codes::REAUTHENTICATION_REQUIRED);
}

#[tokio::test]
async fn replaying_a_refresh_token_kills_the_whole_family() {
    let harness = Harness::new();
    let first = harness.register_at("ada", 1_000).await;
    let stolen = Refresh {
        refresh_token: first.refresh_token.clone(),
        device_id: first.device_id,
    };

    let second = harness
        .auth
        .refresh(
            Refresh {
                refresh_token: first.refresh_token.clone(),
                device_id: first.device_id,
            },
            &context(MINUTE),
        )
        .await
        .unwrap();

    // The thief presents the copy they took before the rotation.
    let error = harness
        .auth
        .refresh(stolen, &context(2 * MINUTE))
        .await
        .expect_err("the token was already exchanged");
    assert_eq!(error.code(), codes::REFRESH_REUSE_DETECTED);

    // And the legitimate holder is signed out too, because there is no way to tell which
    // of the two parties is which.
    let error = identify(&harness.auth, &second, 3 * MINUTE)
        .await
        .expect_err("the family is gone");
    assert_eq!(error.code(), codes::TOKEN_REVOKED);

    let replayed = harness
        .auth
        .refresh(
            Refresh {
                refresh_token: second.refresh_token.clone(),
                device_id: second.device_id,
            },
            &context(4 * MINUTE),
        )
        .await
        .expect_err("the newer token is revoked as well");
    assert_eq!(replayed.code(), codes::REFRESH_REUSE_DETECTED);
}

#[tokio::test]
async fn a_refresh_from_the_wrong_device_kills_the_family() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    let error = harness
        .auth
        .refresh(
            Refresh {
                refresh_token: grant.refresh_token.clone(),
                device_id: Id::NIL,
            },
            &context(MINUTE),
        )
        .await
        .expect_err("the token was minted for another device");
    assert_eq!(error.code(), codes::DEVICE_MISMATCH);

    let after = identify(&harness.auth, &grant, 2 * MINUTE)
        .await
        .expect_err("the session did not survive");
    assert_eq!(after.code(), codes::TOKEN_REVOKED);
}

#[tokio::test]
async fn an_unknown_refresh_token_is_refused_without_a_hint() {
    let harness = Harness::new();
    harness.register_at("ada", 1_000).await;

    let error = harness
        .auth
        .refresh(
            Refresh {
                refresh_token: Secret::new("this is not a refresh token"),
                device_id: Id::NIL,
            },
            &context(MINUTE),
        )
        .await
        .expect_err("no session matches");
    assert_eq!(error.code(), codes::TOKEN_INVALID);
}

#[tokio::test]
async fn a_refresh_after_the_window_closes_is_refused() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    let error = harness
        .auth
        .refresh(
            Refresh {
                refresh_token: grant.refresh_token.clone(),
                device_id: grant.device_id,
            },
            &context(31 * DAY),
        )
        .await
        .expect_err("the refresh window has closed");
    assert_eq!(error.code(), codes::TOKEN_EXPIRED);
}

// ---------------------------------------------------------------------------
// Access tokens
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_expired_access_token_is_refused_with_a_code_that_says_refresh() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    let error = identify(&harness.auth, &grant, HOUR)
        .await
        .expect_err("fifteen minutes have passed");
    assert_eq!(
        error.code(),
        codes::TOKEN_EXPIRED,
        "the client has to be told to refresh, not to sign in again"
    );
}

#[tokio::test]
async fn a_tampered_access_token_is_refused() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    let mut characters: Vec<char> = grant.access_token.chars().collect();
    // Flip a character inside the claims, well before the tag.
    characters[4] = if characters[4] == 'A' { 'B' } else { 'A' };
    let tampered: String = characters.into_iter().collect();

    let error = harness
        .auth
        .authenticate(&tampered, grant.device_id, &context(2_000))
        .await
        .expect_err("the tag no longer matches");
    assert_eq!(error.code(), codes::TOKEN_INVALID);
    assert!(
        harness
            .metric("migo_auth_token_verify_failures_total")
            .is_some(),
        "a verification failure is counted for the operator"
    );
}

#[tokio::test]
async fn a_token_from_another_key_is_refused() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    let mut other = Config::default();
    other.auth.token_key = Some(Secret::new(OTHER_KEY));
    let stranger = Harness::with_config(other);

    let error = stranger
        .auth
        .authenticate(&grant.access_token, grant.device_id, &context(2_000))
        .await
        .expect_err("a token signed by another key is not a token here");
    assert_eq!(error.code(), codes::TOKEN_INVALID);
}

#[tokio::test]
async fn a_token_presented_from_the_wrong_device_is_refused() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    let error = harness
        .auth
        .authenticate(&grant.access_token, Id::NIL, &context(2_000))
        .await
        .expect_err("the device does not match the claim");
    assert_eq!(error.code(), codes::DEVICE_MISMATCH);
}

#[tokio::test]
async fn a_region_can_be_read_off_a_token_without_verifying_it() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    assert_eq!(
        harness.auth.token_region(&grant.access_token).as_deref(),
        Some("local"),
        "a gateway has to route a token before it can verify one"
    );
}

// ---------------------------------------------------------------------------
// Revocation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_revoked_session_stops_working_on_the_next_request() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;
    let identity = identify(&harness.auth, &grant, 2_000).await.unwrap();

    harness
        .auth
        .sign_out(&identity, grant.session_id, &context(3_000))
        .await
        .unwrap();

    let error = identify(&harness.auth, &grant, 4_000)
        .await
        .expect_err("the session is revoked");
    assert_eq!(
        error.code(),
        codes::TOKEN_REVOKED,
        "the token is still cryptographically valid; the session is what makes it useless"
    );
}

#[tokio::test]
async fn signing_out_of_somebody_elses_session_reads_as_not_found() {
    let harness = Harness::new();
    let ada = harness.register_at("ada", 1_000).await;
    let grace = harness
        .auth
        .register(registration("grace"), &context_from(2_000, "198.51.100.4"))
        .await
        .unwrap();
    let identity = identify(&harness.auth, &ada, 3_000).await.unwrap();

    let error = harness
        .auth
        .sign_out(&identity, grace.session_id, &context(4_000))
        .await
        .expect_err("the session is not ada's");
    assert_eq!(
        error.code(),
        codes::NOT_FOUND,
        "PERMISSION_DENIED would confirm the session exists"
    );

    identify(&harness.auth, &grace, 5_000)
        .await
        .expect("grace is unaffected");
}

#[tokio::test]
async fn signing_out_others_spares_the_caller() {
    let harness = Harness::new();
    let first = harness.register_at("ada", 1_000).await;
    let second = harness
        .auth
        .sign_in(sign_in("ada"), &context_from(2_000, "198.51.100.4"))
        .await
        .unwrap();
    let third = harness
        .auth
        .sign_in(sign_in("ada"), &context_from(3_000, "198.51.100.9"))
        .await
        .unwrap();

    let identity = identify(&harness.auth, &first, 4_000).await.unwrap();
    let revoked = harness
        .auth
        .sign_out_others(&identity, &context(4_000))
        .await
        .unwrap();
    assert_eq!(revoked, 2);

    identify(&harness.auth, &first, 5_000)
        .await
        .expect("the caller stays signed in");
    for other in [&second, &third] {
        let error = identify(&harness.auth, other, 5_000)
            .await
            .expect_err("everything else is signed out");
        assert_eq!(error.code(), codes::TOKEN_REVOKED);
    }
}

#[tokio::test]
async fn signing_out_others_requires_a_recent_password() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;
    let identity = identify(&harness.auth, &grant, 2_000).await.unwrap();

    let error = harness
        .auth
        .sign_out_others(&identity, &context(HOUR))
        .await
        .expect_err("the password was typed an hour ago");
    assert_eq!(error.code(), codes::REAUTHENTICATION_REQUIRED);
}

#[tokio::test]
async fn revoking_a_device_ends_its_sessions_and_does_not_resurrect_it() {
    let harness = Harness::new();
    let first = harness.register_at("ada", 1_000).await;
    let second = harness
        .auth
        .sign_in(sign_in("ada"), &context_from(2_000, "198.51.100.4"))
        .await
        .unwrap();

    let identity = identify(&harness.auth, &second, 3_000).await.unwrap();
    let revoked = harness
        .auth
        .revoke_device(&identity, first.device_id, &context(3_000))
        .await
        .unwrap();
    assert_eq!(revoked, 1);

    let error = identify(&harness.auth, &first, 4_000)
        .await
        .expect_err("the revoked device's session is gone");
    assert_eq!(error.code(), codes::TOKEN_REVOKED);

    // Signing in while claiming the revoked device gets a new one, not the old one back.
    let mut request = sign_in("ada");
    request.device = DeviceClaim::new(Platform::Web, "Firefox on Linux").on_device(first.device_id);
    let third = harness
        .auth
        .sign_in(request, &context_from(5_000, "198.51.100.9"))
        .await
        .unwrap();
    assert_ne!(
        third.device_id, first.device_id,
        "revoking a device must not mean revoking it until the next sign-in"
    );
}

#[tokio::test]
async fn revoking_another_accounts_device_reads_as_not_found() {
    let harness = Harness::new();
    let ada = harness.register_at("ada", 1_000).await;
    let grace = harness
        .auth
        .register(registration("grace"), &context_from(2_000, "198.51.100.4"))
        .await
        .unwrap();

    let identity = identify(&harness.auth, &ada, 3_000).await.unwrap();
    let error = harness
        .auth
        .revoke_device(&identity, grace.device_id, &context(3_000))
        .await
        .expect_err("the device is not ada's");
    assert_eq!(error.code(), codes::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Password change
// ---------------------------------------------------------------------------

#[tokio::test]
async fn changing_a_password_ends_every_session_and_returns_a_replacement() {
    let harness = Harness::new();
    let first = harness.register_at("ada", 1_000).await;
    let second = harness
        .auth
        .sign_in(sign_in("ada"), &context_from(2_000, "198.51.100.4"))
        .await
        .unwrap();

    let identity = identify(&harness.auth, &first, 3_000).await.unwrap();
    let replacement = harness
        .auth
        .change_password(
            &identity,
            PasswordChange {
                current: Secret::new(GOOD_PASSWORD),
                next: Secret::new("pelican trombone lantern"),
            },
            &context(3_000),
        )
        .await
        .unwrap();

    for old in [&first, &second] {
        let error = identify(&harness.auth, old, 4_000)
            .await
            .expect_err("every session ends, including the caller's");
        assert_eq!(error.code(), codes::TOKEN_REVOKED);
    }
    identify(&harness.auth, &replacement, 4_000)
        .await
        .expect("the replacement grant keeps the caller signed in");

    // The new password is the one that works.
    harness
        .auth
        .sign_in(
            SignIn {
                identifier: "ada".to_string(),
                password: Secret::new("pelican trombone lantern"),
                device: DeviceClaim::new(Platform::Web, "Firefox on Linux"),
                captcha: None,
                server: None,
            },
            &context_from(5_000, "198.51.100.20"),
        )
        .await
        .expect("the new password works");
    harness
        .auth
        .sign_in(sign_in("ada"), &context_from(6_000, "198.51.100.30"))
        .await
        .expect_err("the old one does not");
}

#[tokio::test]
async fn changing_a_password_requires_the_current_one() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;
    let identity = identify(&harness.auth, &grant, 2_000).await.unwrap();

    let error = harness
        .auth
        .change_password(
            &identity,
            PasswordChange {
                current: Secret::new("not the current password"),
                next: Secret::new("pelican trombone lantern"),
            },
            &context(2_000),
        )
        .await
        .expect_err("the current password is wrong");
    assert_eq!(error.code(), codes::INVALID_CREDENTIALS);

    identify(&harness.auth, &grant, 3_000)
        .await
        .expect("a failed change leaves the session alone");
}

#[tokio::test]
async fn a_weak_replacement_password_is_refused() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;
    let identity = identify(&harness.auth, &grant, 2_000).await.unwrap();

    let error = harness
        .auth
        .change_password(
            &identity,
            PasswordChange {
                current: Secret::new(GOOD_PASSWORD),
                next: Secret::new("ada"),
            },
            &context(2_000),
        )
        .await
        .expect_err("the replacement is too short and contains the username");
    assert_eq!(error.code(), codes::WEAK_PASSWORD);

    harness
        .auth
        .sign_in(sign_in("ada"), &context_from(3_000, "198.51.100.4"))
        .await
        .expect("the old password still works");
}

// ---------------------------------------------------------------------------
// Session listing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_session_list_marks_the_caller_and_names_the_devices() {
    let harness = Harness::new();
    let first = harness.register_at("ada", 1_000).await;
    let mut request = sign_in("ada");
    request.device = DeviceClaim::new(Platform::Android, "Pixel 8");
    harness
        .auth
        .sign_in(request, &context_from(2_000, "198.51.100.4"))
        .await
        .unwrap();

    let identity = identify(&harness.auth, &first, 3_000).await.unwrap();
    let sessions = harness
        .auth
        .sessions(&identity, &context(3_000))
        .await
        .unwrap();

    assert_eq!(sessions.len(), 2);
    let current = sessions
        .iter()
        .find(|summary| summary.is_current)
        .expect("exactly one session is the caller's");
    assert_eq!(current.session_id, first.session_id);
    assert_eq!(current.device_name, "Firefox on Linux");
    assert!(sessions
        .iter()
        .any(|summary| summary.platform == Platform::Android && summary.device_name == "Pixel 8"));
}

// ---------------------------------------------------------------------------
// Privacy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_session_records_a_network_class_and_never_a_full_address() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    let session = harness
        .store
        .session_by_id(grant.session_id)
        .await
        .unwrap()
        .expect("the session is stored");
    let class = session.ip_class.expect("a class is recorded");
    assert_eq!(class, "203.0.113.0/24");
    assert!(
        !class.contains("113.77"),
        "the host part of an address must never reach storage"
    );
}

#[tokio::test]
async fn a_grant_does_not_carry_the_password_anywhere() {
    let harness = Harness::new();
    let grant = harness.register_at("ada", 1_000).await;

    assert!(!grant.access_token.contains(GOOD_PASSWORD));
    assert!(!grant.refresh_token.expose().contains(GOOD_PASSWORD));
    let debugged = format!("{:?}", grant.refresh_token);
    assert!(
        !debugged.contains(grant.refresh_token.expose()),
        "a refresh token must not appear in a debug rendering, which is what ends up in logs"
    );
}

#[tokio::test]
async fn metrics_carry_no_account_or_address() {
    let harness = Harness::new();
    harness.register_at("ada", 1_000).await;
    let mut wrong = sign_in("ada");
    wrong.password = Secret::new("not the right passphrase");
    let _ = harness
        .auth
        .sign_in(wrong, &context_from(2_000, "198.51.100.4"))
        .await;

    let rendered = harness.registry.render();
    assert!(rendered.contains("migo_auth_signin_total"));
    assert!(
        !rendered.contains("ada"),
        "no username reaches a metric label"
    );
    assert!(
        !rendered.contains("203.0.113"),
        "no address reaches a metric label"
    );
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_wrong_password_costs_the_whole_anonymous_budget() {
    let harness = Harness::new();
    harness.register_at("ada", 1_000).await;

    let mut wrong = sign_in("ada");
    wrong.password = Secret::new("not the right passphrase");
    let first = harness
        .auth
        .sign_in(wrong, &context_from(2_000, "198.51.100.4"))
        .await
        .expect_err("the password is wrong");
    assert_eq!(first.code(), codes::INVALID_CREDENTIALS);

    // The attempt plus the surcharge is the whole bucket, so the next request from the
    // same network within the same millisecond cannot be paid for.
    let second = harness
        .auth
        .sign_in(sign_in("ada"), &context_from(2_000, "198.51.100.4"))
        .await
        .expect_err("the budget is spent");
    assert_eq!(second.code(), codes::RATE_LIMITED);
    assert!(
        second.retry_after().is_some(),
        "a refusal has to tell the client how long to wait"
    );
}

#[tokio::test]
async fn a_correct_password_leaves_room_for_more() {
    let harness = Harness::new();
    harness.register_at("ada", 1_000).await;

    // A fifth of the bucket per success, so several consecutive sign-ins fit where a
    // single failure would not.
    for step in 0..3 {
        harness
            .auth
            .sign_in(sign_in("ada"), &context_from(2_000, "198.51.100.4"))
            .await
            .unwrap_or_else(|error| panic!("sign-in {step} should fit the budget: {error:?}"));
    }
}

#[tokio::test]
async fn pressure_is_per_network_and_not_per_account() {
    let harness = Harness::new();
    harness.register_at("ada", 1_000).await;

    let mut wrong = sign_in("ada");
    wrong.password = Secret::new("not the right passphrase");
    harness
        .auth
        .sign_in(wrong, &context_from(2_000, "198.51.100.4"))
        .await
        .expect_err("spend the attacker's budget");

    // The owner, on a different network, is unaffected. A per-account failure counter
    // would have let a stranger who knows a username lock its owner out.
    harness
        .auth
        .sign_in(sign_in("ada"), &context_from(2_000, "192.0.2.55"))
        .await
        .expect("the owner is not punished for somebody else's guesses");
}

#[tokio::test]
async fn a_request_with_no_address_is_not_rate_limited() {
    let harness = Harness::new();
    let inside = RequestContext::at(Timestamp::from_millis(1_000));
    harness
        .auth
        .register(registration("ada"), &inside)
        .await
        .expect("an addressless caller came from inside the process");

    let account = harness
        .store
        .account_by_username("ada")
        .await
        .unwrap()
        .expect("the account exists");
    assert_eq!(account.username, "ada");
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_token_key_fails_at_construction() {
    let store = Arc::new(MemoryStore::new());
    let cache = Arc::new(MemoryCache::new());
    let registry = Registry::new();
    let config = Config::default();
    let limiter = Arc::new(CacheRateLimiter::new(
        cache,
        Policies::from_config(&config.rate_limit).unwrap(),
        &registry,
    ));

    let error = Auth::new(
        store,
        limiter,
        &config,
        &registry,
        Box::new(SeededRandom::new(1)) as Box<dyn Random>,
    )
    .expect_err("there is no key to sign with");
    assert_eq!(
        error.code(),
        codes::INTERNAL_ERROR,
        "a server that cannot sign a token must not start"
    );
}

#[tokio::test]
async fn a_short_token_key_fails_at_construction() {
    let mut config = Config::default();
    config.auth.token_key = Some(Secret::new("too short"));
    let store = Arc::new(MemoryStore::new());
    let cache = Arc::new(MemoryCache::new());
    let registry = Registry::new();
    let limiter = Arc::new(CacheRateLimiter::new(
        cache,
        Policies::from_config(&config.rate_limit).unwrap(),
        &registry,
    ));

    Auth::new(
        store,
        limiter,
        &config,
        &registry,
        Box::new(SeededRandom::new(1)) as Box<dyn Random>,
    )
    .expect_err("a key below the minimum is refused where somebody is watching");
}

// ---------------------------------------------------------------------------
// The ML-DSA identity ceremonies (brief section 182)
// ---------------------------------------------------------------------------

/// A root secret and the two keys a client derives from it: the account's
/// identity key and one device's credential.
///
/// The roots are fixed bytes rather than generated, so a signature that stops
/// verifying is a change in the verification path and not a change in the
/// inputs.
struct RootSet {
    identity: migo_account::identity::IdentityKey,
    credential: migo_account::identity::DeviceCredential,
}

impl RootSet {
    /// The root whose bytes are `seed`, with its device credential drawn from
    /// its own `DEVICE` domain rather than the raw seed.
    fn new(seed: u8, credential_seed: u8) -> Self {
        let root = migo_account::MigoRoot::from_bytes(&[seed; 32]).expect("a 32-byte root");
        let mut random = SeededRandom::new(u64::from(credential_seed));
        Self {
            identity: migo_account::identity::IdentityKey::from_root(&root),
            credential: migo_account::identity::DeviceCredential::generate(&mut random),
        }
    }
}

/// A request context from the `n`-th documentation network, so one test's
/// register/challenge/answer sequence does not share an anonymous bucket.
fn ceremony_context(millis: i64, network: u8) -> RequestContext {
    RequestContext::at(Timestamp::from_millis(millis))
        .from_ip(format!("198.51.{network}.9").parse::<IpAddr>().unwrap())
}

/// Registers `username` carrying `set`'s keys: the identity key the account
/// will verify against, and the device credential its first device holds.
async fn register_with_root(harness: &Harness, username: &str, set: &RootSet) -> Grant {
    register_with_root_from(harness, username, set, 1_000, 4).await
}

/// The same registration, from a chosen time and documentation network, so a
/// retry does not share its first attempt's anonymous bucket.
async fn register_with_root_from(
    harness: &Harness,
    username: &str,
    set: &RootSet,
    millis: i64,
    network: u8,
) -> Grant {
    let mut request = registration(username);
    request.identity_public_key = Some(set.identity.public_key().to_vec());
    request.device.credential_public_key = Some(set.credential.public_key().to_vec());
    harness
        .auth
        .register(request, &ceremony_context(millis, network))
        .await
        .expect("a root-secret registration succeeds")
}

/// Asks for a login challenge for `identifier` on `device_id`.
async fn login_challenge(
    harness: &Harness,
    identifier: &str,
    device_id: Id,
    millis: i64,
    network: u8,
) -> migo_auth::ChallengeView {
    harness
        .auth
        .issue_identity_challenge(
            migo_auth::IdentityChallengeRequest {
                scope: migo_auth::IdentityChallengeScope::Login {
                    identifier: identifier.to_string(),
                    device_id,
                },
                device: DeviceClaim::new(Platform::Web, "unused"),
            },
            &ceremony_context(millis, network),
        )
        .await
        .expect("a login challenge is issued")
}

#[tokio::test]
async fn a_registration_retry_folds_into_the_account_it_already_made() {
    let harness = Harness::new();
    let set = RootSet::new(0x55, 0x66);
    let first = register_with_root(&harness, "wren", &set).await;

    // The network eats the response; the client, still holding the same root,
    // retries the identical registration. The account must not duplicate —
    // the retry is answered with a grant for the one account that exists.
    let second = register_with_root_from(&harness, "wren", &set, 2_000, 10).await;
    assert_eq!(
        second.account_id, first.account_id,
        "the retry reconciles into the existing account"
    );
    assert!(
        !second.is_new_account,
        "the reconciled grant is not a new account"
    );
    identify(&harness.auth, &second, 3_000)
        .await
        .expect("the reconciled session is real");

    // Exactly one account exists under the name.
    let dup = harness
        .auth
        .register(registration("wren"), &ceremony_context(4_000, 7))
        .await;
    assert!(
        dup.is_err(),
        "a password-only registration of the taken name is still refused"
    );
}

#[tokio::test]
async fn a_retry_with_someone_elses_keys_is_refused() {
    let harness = Harness::new();
    let owner = RootSet::new(0x77, 0x88);
    register_with_root(&harness, "linnet", &owner).await;

    // A different client generated a different root and picked the same
    // username. Nothing about its retry may fold into the owner's account —
    // the verdict stays USERNAME_TAKEN.
    let impostor = RootSet::new(0x99, 0xaa);
    let mut request = registration("linnet");
    request.identity_public_key = Some(impostor.identity.public_key().to_vec());
    request.device.credential_public_key = Some(impostor.credential.public_key().to_vec());
    let collision = harness
        .auth
        .register(request, &ceremony_context(2_000, 8))
        .await
        .expect_err("a foreign key cannot take an existing name");
    assert_eq!(
        collision.code(),
        migo_protocol::codes::USERNAME_TAKEN,
        "the refusal is the ordinary name collision; got {:?}",
        collision
    );
}

#[tokio::test]
async fn a_retry_with_the_right_key_but_a_wrong_password_is_refused() {
    let harness = Harness::new();
    let set = RootSet::new(0xbb, 0xcc);
    register_with_root(&harness, "robin", &set).await;

    // The key matches but the password does not: reconcile never becomes a
    // password oracle, and the answer is the generic invalid-credentials
    // fault rather than anything name-specific.
    let mut request = registration("robin");
    request.password = Secret::new("wrong horse wrong staple");
    request.identity_public_key = Some(set.identity.public_key().to_vec());
    let refused = harness
        .auth
        .register(request, &ceremony_context(2_000, 9))
        .await
        .expect_err("a wrong password never reconciles");
    assert_eq!(
        refused.code(),
        migo_protocol::codes::INVALID_CREDENTIALS,
        "the refusal is invalid credentials; got {:?}",
        refused
    );
}

#[tokio::test]
async fn an_identity_login_opens_a_usable_session() {
    let harness = Harness::new();
    let set = RootSet::new(0x11, 0x22);
    let grant = register_with_root(&harness, "vera", &set).await;

    let view = login_challenge(&harness, "vera", grant.device_id, 2_000, 5).await;
    assert!(!view.payload.is_empty(), "there are bytes to sign");
    assert!(
        view.expires_at.as_unix_ms() > 2_000,
        "the challenge is answerable for some time after it is issued"
    );
    let answer = migo_auth::ChallengeAnswer {
        challenge_id: view.challenge_id,
        identity_signature: set.identity.sign_login(&view.payload).unwrap().to_vec(),
        device_signature: set.credential.sign_login(&view.payload).unwrap().to_vec(),
    };
    let signed_in = harness
        .auth
        .answer_identity_challenge(answer, &ceremony_context(3_000, 6))
        .await
        .expect("both signatures verify");

    identify(&harness.auth, &signed_in, 4_000)
        .await
        .expect("the session the ceremony opened is real");
    assert_eq!(signed_in.account_id, grant.account_id);
}

#[tokio::test]
async fn an_unknown_identifier_gets_the_same_challenge_and_the_same_refusal() {
    let harness = Harness::new();
    let stranger = RootSet::new(0x33, 0x44);

    // No account "nobody" exists. The issue must still hand back a
    // well-formed challenge, and the answer must fail with the same code a
    // real-but-expired challenge would — never INVALID_CREDENTIALS, which is
    // reserved for a signature that did not verify and would tell the prober
    // the account was never there.
    let view = login_challenge(
        &harness,
        "nobody",
        Id::from_bytes([0xde, 0xad, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        2_000,
        5,
    )
    .await;
    assert!(!view.payload.is_empty());
    let answer = migo_auth::ChallengeAnswer {
        challenge_id: view.challenge_id,
        identity_signature: stranger
            .identity
            .sign_login(&view.payload)
            .unwrap()
            .to_vec(),
        device_signature: stranger
            .credential
            .sign_login(&view.payload)
            .unwrap()
            .to_vec(),
    };
    let error = harness
        .auth
        .answer_identity_challenge(answer, &ceremony_context(3_000, 6))
        .await
        .expect_err("a fake challenge is never answerable");
    assert_eq!(error.code(), codes::CHALLENGE_INVALID);
}

#[tokio::test]
async fn a_signature_by_the_wrong_key_is_invalid_credentials() {
    let harness = Harness::new();
    let set = RootSet::new(0x55, 0x66);
    let other = RootSet::new(0x77, 0x88);
    let grant = register_with_root(&harness, "vera", &set).await;

    let view = login_challenge(&harness, "vera", grant.device_id, 2_000, 5).await;
    let answer = migo_auth::ChallengeAnswer {
        challenge_id: view.challenge_id,
        identity_signature: other.identity.sign_login(&view.payload).unwrap().to_vec(),
        device_signature: set.credential.sign_login(&view.payload).unwrap().to_vec(),
    };
    let error = harness
        .auth
        .answer_identity_challenge(answer, &ceremony_context(3_000, 6))
        .await
        .expect_err("the identity key is not the account's");
    assert_eq!(error.code(), codes::INVALID_CREDENTIALS);
}

#[tokio::test]
async fn a_login_challenge_answers_exactly_once() {
    let harness = Harness::new();
    let set = RootSet::new(0x99, 0xaa);
    let grant = register_with_root(&harness, "vera", &set).await;

    let view = login_challenge(&harness, "vera", grant.device_id, 2_000, 5).await;
    let build_answer = || migo_auth::ChallengeAnswer {
        challenge_id: view.challenge_id,
        identity_signature: set.identity.sign_login(&view.payload).unwrap().to_vec(),
        device_signature: set.credential.sign_login(&view.payload).unwrap().to_vec(),
    };
    harness
        .auth
        .answer_identity_challenge(build_answer(), &ceremony_context(3_000, 6))
        .await
        .expect("the first answer is accepted");
    let replay = harness
        .auth
        .answer_identity_challenge(build_answer(), &ceremony_context(4_000, 7))
        .await
        .expect_err("a captured answer is worth nothing the second time");
    assert_eq!(replay.code(), codes::CHALLENGE_INVALID);
}

#[tokio::test]
async fn a_rotated_identity_is_retired_and_its_sessions_survive() {
    let harness = Harness::new();
    let set = RootSet::new(0xbb, 0xcc);
    let successor = RootSet::new(0xdd, 0xee);
    let grant = register_with_root(&harness, "vera", &set).await;
    let identity = identify(&harness.auth, &grant, 2_000)
        .await
        .expect("signed in");

    let view = harness
        .auth
        .issue_rotation_challenge(&identity, &ceremony_context(3_000, 5))
        .await
        .expect("an authenticated device may rotate");
    harness
        .auth
        .rotate_identity(
            &identity,
            migo_auth::RotationAnswer {
                challenge_id: view.challenge_id,
                signature: set.identity.sign_rotate(&view.payload).unwrap().to_vec(),
                new_public_key: successor.identity.public_key().to_vec(),
            },
            &ceremony_context(4_000, 6),
        )
        .await
        .expect("the current key authorises its successor");

    // Rotation is not a logout: the session opened under the old key keeps
    // working, but only the successor can open the next one.
    identify(&harness.auth, &grant, 5_000)
        .await
        .expect("existing sessions survive a rotation");
    let next = login_challenge(&harness, "vera", grant.device_id, 6_000, 7).await;
    let answer = migo_auth::ChallengeAnswer {
        challenge_id: next.challenge_id,
        identity_signature: set.identity.sign_login(&next.payload).unwrap().to_vec(),
        device_signature: set.credential.sign_login(&next.payload).unwrap().to_vec(),
    };
    let error = harness
        .auth
        .answer_identity_challenge(answer, &ceremony_context(7_000, 8))
        .await
        .expect_err("the retired key no longer speaks for the account");
    assert_eq!(error.code(), codes::INVALID_CREDENTIALS);

    let again = login_challenge(&harness, "vera", grant.device_id, 8_000, 9).await;
    let answer = migo_auth::ChallengeAnswer {
        challenge_id: again.challenge_id,
        identity_signature: successor
            .identity
            .sign_login(&again.payload)
            .unwrap()
            .to_vec(),
        device_signature: set.credential.sign_login(&again.payload).unwrap().to_vec(),
    };
    harness
        .auth
        .answer_identity_challenge(answer, &ceremony_context(9_000, 10))
        .await
        .expect("the successor key is the account's identity now");

    // Rotating to the key already in force is a conflict, not a no-op: a
    // client that thinks it has a new key when it does not is a bug to surface.
    let view = harness
        .auth
        .issue_rotation_challenge(&identity, &ceremony_context(10_000, 11))
        .await
        .expect("the device still rotates");
    let error = harness
        .auth
        .rotate_identity(
            &identity,
            migo_auth::RotationAnswer {
                challenge_id: view.challenge_id,
                signature: successor
                    .identity
                    .sign_rotate(&view.payload)
                    .unwrap()
                    .to_vec(),
                new_public_key: successor.identity.public_key().to_vec(),
            },
            &ceremony_context(11_000, 12),
        )
        .await
        .expect_err("the same key twice is not a rotation");
    assert_eq!(error.code(), codes::CONFLICT);
}

#[tokio::test]
async fn an_add_device_challenge_restores_the_account_onto_a_new_device() {
    let harness = Harness::new();
    let set = RootSet::new(0x01, 0x02);
    let new_device = RootSet::new(0x03, 0x04);
    let grant = register_with_root(&harness, "vera", &set).await;

    // The .migo container carries the account id; the new device describes
    // itself and brings a credential it has not yet registered anywhere.
    let view = harness
        .auth
        .issue_identity_challenge(
            migo_auth::IdentityChallengeRequest {
                scope: migo_auth::IdentityChallengeScope::AddDevice {
                    account_id: grant.account_id,
                },
                device: DeviceClaim::new(Platform::Android, "Pixel 8"),
            },
            &ceremony_context(2_000, 5),
        )
        .await
        .expect("the account id names a restorable account");
    assert_ne!(
        view.device_id, grant.device_id,
        "the challenge binds a new device row"
    );

    let restored = harness
        .auth
        .answer_add_device(
            migo_auth::AddDeviceAnswer {
                challenge_id: view.challenge_id,
                identity_signature: set.identity.sign_login(&view.payload).unwrap().to_vec(),
                device_public_key: new_device.credential.public_key().to_vec(),
                device_signature: new_device
                    .credential
                    .sign_login(&view.payload)
                    .unwrap()
                    .to_vec(),
            },
            &ceremony_context(3_000, 6),
        )
        .await
        .expect("the root secret restores the account");
    identify(&harness.auth, &restored, 4_000)
        .await
        .expect("the restored device holds a live session");

    let identity = identify(&harness.auth, &restored, 4_000)
        .await
        .expect("signed in");
    let list = harness
        .auth
        .devices(&identity, &ceremony_context(5_000, 7))
        .await
        .expect("the device list reads");
    assert_eq!(list.len(), 2, "the pending row became a real device");
    let added = list
        .iter()
        .find(|device| device.device_id == restored.device_id)
        .expect("the new device is listed");
    assert_eq!(added.status, "active");
    assert!(
        added.has_credential,
        "the credential that signed is registered"
    );
    assert!(added.is_current, "the list is told which device is asking");
}

#[tokio::test]
async fn a_password_account_can_adopt_its_root_secret_once() {
    let harness = Harness::new();
    let set = RootSet::new(0x05, 0x06);
    let grant = harness.register_at("vera", 1_000).await;
    let identity = identify(&harness.auth, &grant, 2_000)
        .await
        .expect("signed in");
    let publish =
        |identity_key: Vec<u8>, device_key: Option<Vec<u8>>| migo_auth::IdentityPublication {
            identity_public_key: identity_key,
            device_public_key: device_key,
        };

    // The first publication installs the key; the identical retry is fine;
    // a different key is a rotation done through the wrong door.
    harness
        .auth
        .publish_identity_key(
            &identity,
            publish(
                set.identity.public_key().to_vec(),
                Some(set.credential.public_key().to_vec()),
            ),
            &ceremony_context(3_000, 5),
        )
        .await
        .expect("the legacy account adopts its key");
    harness
        .auth
        .publish_identity_key(
            &identity,
            publish(
                set.identity.public_key().to_vec(),
                Some(set.credential.public_key().to_vec()),
            ),
            &ceremony_context(4_000, 6),
        )
        .await
        .expect("an identical retry is the idempotent case");
    let other = RootSet::new(0x07, 0x08);
    let error = harness
        .auth
        .publish_identity_key(
            &identity,
            publish(other.identity.public_key().to_vec(), None),
            &ceremony_context(5_000, 7),
        )
        .await
        .expect_err("a different key is a conflict");
    assert_eq!(error.code(), codes::CONFLICT);

    // With both halves published, the ceremony works on the old account.
    let view = login_challenge(&harness, "vera", grant.device_id, 6_000, 8).await;
    let answer = migo_auth::ChallengeAnswer {
        challenge_id: view.challenge_id,
        identity_signature: set.identity.sign_login(&view.payload).unwrap().to_vec(),
        device_signature: set.credential.sign_login(&view.payload).unwrap().to_vec(),
    };
    harness
        .auth
        .answer_identity_challenge(answer, &ceremony_context(7_000, 9))
        .await
        .expect("the adopted key is the account's identity");
}

#[tokio::test]
async fn wallets_register_idempotently_and_archive_quietly() {
    let harness = Harness::new();
    let grant = harness.register_at("vera", 1_000).await;
    let identity = identify(&harness.auth, &grant, 2_000)
        .await
        .expect("signed in");
    let register = |label: &str| migo_auth::WalletRegistration {
        address: "0xAb8482F84dD2dDec36209d36B85D89Aa3D5f14c8".to_string(),
        chain_type: "evm".to_string(),
        label: Some(label.to_string()),
        derivation_index: 0,
    };

    let first = harness
        .auth
        .register_wallet(&identity, register("main"), &ceremony_context(3_000, 5))
        .await
        .expect("an address registers");
    assert_eq!(first.address, "ab8482f84dd2ddec36209d36b85d89aa3d5f14c8");
    assert_eq!(first.status, "active");

    // The restore case: the same address on the same chain is the same
    // wallet, whatever id was generated along the way.
    let again = harness
        .auth
        .register_wallet(&identity, register("renamed"), &ceremony_context(4_000, 6))
        .await
        .expect("a re-registration is idempotent");
    assert_eq!(again.wallet_id, first.wallet_id);
    assert_eq!(again.label.as_deref(), Some("renamed"));

    let list = harness
        .auth
        .wallets(&identity, &ceremony_context(5_000, 7))
        .await
        .expect("the wallet list reads");
    assert_eq!(list.len(), 1, "one address, one wallet");

    harness
        .auth
        .archive_wallet(&identity, first.wallet_id, &ceremony_context(6_000, 8))
        .await
        .expect("archiving your own wallet works");
    harness
        .auth
        .archive_wallet(&identity, first.wallet_id, &ceremony_context(7_000, 9))
        .await
        .expect("archiving twice is quiet");
    harness
        .auth
        .archive_wallet(
            &identity,
            Id::from_bytes([0xbe, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            &ceremony_context(8_000, 10),
        )
        .await
        .expect("an unknown id is quiet too");

    let list = harness
        .auth
        .wallets(&identity, &ceremony_context(9_000, 11))
        .await
        .expect("the wallet list still reads");
    assert_eq!(list.len(), 1, "the archived wallet stays in the history");
    assert_eq!(list[0].status, "archived", "but it is marked as such");
    assert!(list[0].archived_at.is_some(), "with the time it happened");
}

// ---------------------------------------------------------------------------
// Global admins
// ---------------------------------------------------------------------------

/// The owner's harness, with the owner designation naming an account that
/// has already registered, and two other accounts to appoint and to be
/// refused.
async fn owner_harness() -> (Harness, Grant, Grant, Grant) {
    let plain = Harness::new();
    // One network per account: the limiter buckets by address class, so
    // three registrations from one /24 inside two seconds is exactly the
    // anonymous burst it exists to stop, and the test is about the admin
    // surface, not about the bucket.
    let owner = plain
        .auth
        .register(registration("chief"), &context_from(1_000, "203.0.113.1"))
        .await
        .expect("registration succeeds");
    let first = plain
        .auth
        .register(registration("warden"), &context_from(2_000, "203.0.114.1"))
        .await
        .expect("registration succeeds");
    let second = plain
        .auth
        .register(
            registration("stranger"),
            &context_from(3_000, "203.0.115.1"),
        )
        .await
        .expect("registration succeeds");
    let mut config = Config::default();
    config.auth.owner_account_id = Some(owner.account_id);
    let harness = plain.reconfigured(config);
    (harness, owner, first, second)
}

#[tokio::test]
async fn a_deployment_without_an_owner_has_a_closed_admin_surface() {
    let harness = Harness::new();
    let grant = harness.register_at("nobody", 1_000).await;
    let identity = identify(&harness.auth, &grant, 2_000).await.unwrap();

    let standing = harness
        .auth
        .admin_standing(&identity, &context(3_000))
        .await
        .expect("standing always answers");
    assert!(!standing.owner, "no owner is named, so nobody is the owner");
    assert!(!standing.admin, "and nobody has been appointed");

    let error = harness
        .auth
        .grant_global_admin(&identity, "somebody", &context(4_000))
        .await
        .expect_err("a deployment that names no owner grants nobody");
    assert_eq!(error.code(), codes::PERMISSION_DENIED);
}

#[tokio::test]
async fn the_owner_grants_and_revokes_global_admins() {
    let (harness, owner, first, second) = owner_harness().await;
    let owner_identity = identify(&harness.auth, &owner, 4_000).await.unwrap();

    let view = harness
        .auth
        .grant_global_admin(&owner_identity, "@Warden", &context(5_000))
        .await
        .expect("the owner appoints by username, @ and case and all");
    assert_eq!(view.account_id, first.account_id);
    assert_eq!(view.username, "warden");
    assert_eq!(view.granted_by, owner.account_id);

    let list = harness
        .auth
        .global_admins(&owner_identity, &context(6_000))
        .await
        .expect("the owner reads the list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].account_id, first.account_id);

    let warden_identity = identify(&harness.auth, &first, 7_000).await.unwrap();
    let standing = harness
        .auth
        .admin_standing(&warden_identity, &context(8_000))
        .await
        .expect("standing always answers");
    assert!(!standing.owner, "an appointee is not the owner");
    assert!(standing.admin, "but the appointment shows");

    let second_identity = identify(&harness.auth, &second, 9_000).await.unwrap();
    let standing = harness
        .auth
        .admin_standing(&second_identity, &context(10_000))
        .await
        .expect("standing always answers");
    assert!(
        !standing.owner && !standing.admin,
        "a stranger holds neither"
    );

    harness
        .auth
        .revoke_global_admin(&owner_identity, first.account_id, &context(11_000))
        .await
        .expect("the owner revokes");
    harness
        .auth
        .revoke_global_admin(&owner_identity, first.account_id, &context(12_000))
        .await
        .expect("revoking twice is quiet");

    let standing = harness
        .auth
        .admin_standing(&warden_identity, &context(13_000))
        .await
        .expect("standing always answers");
    assert!(!standing.admin, "the revocation took");
}

#[tokio::test]
async fn only_the_owner_may_touch_the_admin_surface() {
    let (harness, _owner, first, second) = owner_harness().await;
    let owner_identity = identify(&harness.auth, &_owner, 4_000).await.unwrap();
    harness
        .auth
        .grant_global_admin(&owner_identity, "warden", &context(5_000))
        .await
        .expect("the owner appoints");

    // The appointee, and a stranger, are both refused every write and the
    // read alike: the surface is the owner's, not the admin's.
    for identity in [
        identify(&harness.auth, &first, 6_000).await.unwrap(),
        identify(&harness.auth, &second, 7_000).await.unwrap(),
    ] {
        let error = harness
            .auth
            .global_admins(&identity, &context(8_000))
            .await
            .expect_err("only the owner reads the list");
        assert_eq!(error.code(), codes::PERMISSION_DENIED);
        let error = harness
            .auth
            .grant_global_admin(&identity, "stranger", &context(9_000))
            .await
            .expect_err("an admin cannot appoint admins");
        assert_eq!(error.code(), codes::PERMISSION_DENIED);
        let error = harness
            .auth
            .revoke_global_admin(&identity, second.account_id, &context(10_000))
            .await
            .expect_err("an admin cannot revoke admins");
        assert_eq!(error.code(), codes::PERMISSION_DENIED);
    }
}

#[tokio::test]
async fn a_repeated_grant_keeps_the_original() {
    let (harness, owner, _first, _second) = owner_harness().await;
    let owner_identity = identify(&harness.auth, &owner, 4_000).await.unwrap();

    let first_view = harness
        .auth
        .grant_global_admin(&owner_identity, "warden", &context(5_000))
        .await
        .expect("the first grant");
    // A second appointment by a different hand — the owner again — must not
    // rewrite history: the trail answers who put them there and when.
    let again = harness
        .auth
        .grant_global_admin(&owner_identity, "warden", &context(50_000))
        .await
        .expect("a repeated grant is a read, not a rewrite");
    assert_eq!(again.granted_at, first_view.granted_at);
    assert_eq!(again.granted_by, first_view.granted_by);

    let list = harness
        .auth
        .global_admins(&owner_identity, &context(60_000))
        .await
        .expect("the list reads");
    assert_eq!(list.len(), 1, "one appointment, one row");
}

#[tokio::test]
async fn a_grant_names_an_account_that_must_exist() {
    let (harness, owner, _first, _second) = owner_harness().await;
    let owner_identity = identify(&harness.auth, &owner, 4_000).await.unwrap();

    let error = harness
        .auth
        .grant_global_admin(&owner_identity, "ghost", &context(5_000))
        .await
        .expect_err("there is no such account");
    assert_eq!(error.code(), codes::NOT_FOUND);
}

#[tokio::test]
async fn the_grant_and_revoke_leave_an_audit_trail() {
    let (harness, owner, first, _second) = owner_harness().await;
    let owner_identity = identify(&harness.auth, &owner, 4_000).await.unwrap();
    harness
        .auth
        .grant_global_admin(&owner_identity, "warden", &context(5_000))
        .await
        .expect("the grant");
    harness
        .auth
        .revoke_global_admin(&owner_identity, first.account_id, &context(6_000))
        .await
        .expect("the revoke");

    let actions = harness.store.audit_actions();
    assert!(actions.contains(&"global_admin.grant".to_string()));
    assert!(actions.contains(&"global_admin.revoke".to_string()));
}
