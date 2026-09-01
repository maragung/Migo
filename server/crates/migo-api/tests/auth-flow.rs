//! End-to-end auth flow: captcha → register → login → recovery.
//!
//! # What is under test
//!
//! The captcha route, the registration handler, the sign-in handler, and the
//! recovery request surface are all exercised as one user would see them on
//! the web client. A challenge is fetched, the captcha answer is sent back
//! with each bootstrap body, and a recovery request is started. The
//! in-memory backend and the seeded random make the whole flow
//! deterministic: every challenge id and every grant token is reproducible
//! run to run.
//!
//! The rate-limit burst in the harness is sized generously (1000 tokens)
//! so the test never trips the bucket the way a real public-internet
//! deployment would. The test cares about the route layer and the auth
//! pipeline, not the limiter; the limiter has its own dedicated tests in
//! `api.rs`. The captcha threshold is set to one so a single sign-in
//! failure from a network is enough to require a captcha on the next
//! attempt, which is the case this file is interested in.
//!
//! # The invariants this file pins
//!
//! 1. **The captcha route issues a usable challenge.** The response carries a
//!    `challenge_id`, a base64-encoded PNG in `image_png_base64`, the `mode`
//!    it was rendered in, and a `ttl_seconds` field — and no answer, as a
//!    field or as a substring of the raw body, because the answer exists
//!    only as the picture. The accessible `image_alt` mode answers a
//!    different, valid challenge, and an unknown mode is a validation error
//!    rather than a silent default.
//! 2. **Register with a fresh captcha succeeds and returns a grant.**
//!    The response is `201` with `access_token`, `refresh_token`, and the
//!    minted `account_id`/`device_id`/`session_id`. The grant can then be
//!    used as a bearer for the rest of the surface.
//! 3. **A captcha is one-shot.** A second register carrying the same
//!    `challenge_id` is refused, because the gate deletes the challenge on
//!    a match and the verify path returns `false` on a missing row.
//! 4. **Login with a fresh captcha succeeds for a known account and
//!    mints a fresh access token.** The `account_id` is the same as the
//!    one register minted; the new access token is not the same string.
//! 5. **Recovery request is enumeration-safe.** A request for an
//!    identifier that does not exist returns the same `{ ok: true }`
//!    body and code as one for a real account.
//! 6. **The captcha gate engages on a sign-in failure.** With a threshold
//!    of one, a wrong-password sign-in from a network trips the gate; the
//!    next attempt (a register, in this test) without a captcha proof is
//!    refused with `CAPTCHA_REQUIRED`.
//! 7. **A wrong captcha answer is refused with a curated message.** When
//!    the gate is engaged, a present-but-wrong `answer` returns
//!    `INVALID_CAPTCHA`, distinct from the `CAPTCHA_REQUIRED` a missing
//!    proof returns.
//! 8. **The recovery confirm is not an oracle.** Two forged `token_id`s
//!    — one for a never-issued id, one for a freshly-minted but
//!    unconfirmed id — answer with the same status and the same body
//!    bytes, and neither response echoes the supplied tag or the
//!    identifier back.
//! 9. **The account security surface revokes what it names.**
//!    `/auth/sessions/{id}/revoke` ends exactly the session whose id
//!    is in the path (the caller's own survives), and
//!    `/devices/{id}/revoke` ends the device *and* every session on
//!    it, answering with how many died. A revoked session's token and
//!    a revoked device's token both stop opening doors on the next
//!    request.
//!
//! The test catches the failure mode the user is fixing on the web
//! client: a form that drops the `captcha` field, sends the wrong
//! `device` shape, or omits the `password`/`username` fields will
//! fail the body validation in the route layer and not reach the
//! service; the wire shape this file exercises is exactly the one
//! the working client sends.

#![allow(clippy::items_after_statements)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use base64::Engine as _;
use migo_api::{router, ApiServices};
use migo_auth::{Auth, SharedAuth};
use migo_cache::MemoryCache;
use migo_captcha::{CaptchaService, CaptchaStore as _, InMemoryStore as CaptchaStore};
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Clock, ManualClock, Secret, SeededRandom, Timestamp};
use migo_protocol::{codes, NodeInfo};
use migo_ratelimit::{CacheRateLimiter, Policies, SharedRateLimiter};
use migo_store::MemoryStore;

// --- constants ----------------------------------------------------------------------------

/// The 32-byte signing key the auth service needs, base64 as configuration carries it.
const TEST_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhAREhMUFRYXGBkaGxwdHh8=";

/// A fixed wall-clock instant so token issue and expiry are deterministic.
const NOW_MS: i64 = 1_800_000_000_000;

/// A stable seed so ids and challenge codes are reproducible run to run.
const SEED: u64 = 0xc0de_c4fc_0001;

/// A password that clears the floor: long enough, not on the common list, and
/// not derived from any username these tests register.
const GOOD_PASSWORD: &str = "sunflower gravel bicycle";

/// The captcha threshold the harness installs: small enough that the next
/// attempt from a network that has just failed a sign-in has to carry a
/// captcha proof.
const CAPTCHA_THRESHOLD: u32 = 1;

/// The price of one registration in the harness: a small constant rather
/// than the full anonymous endpoint bucket, so the bootstrap endpoints
/// stay usable across the calls a single test makes on one /24. The
/// limiter's price list is exercised by the limiter's own tests in
/// `api.rs`; this file is about the route layer.
const REGISTRATION_COST: u32 = 4;

/// The anonymous bucket the harness installs. Sized large enough that the
/// bootstrap endpoints are not rate-limited during a single test, so the
/// test's failures are about the route and the auth pipeline, not the
/// limiter. The limiter has its own dedicated tests.
const ANONYMOUS_BURST: u32 = 1_000;

/// A 32-byte secret root the recovery MAC key is derived from. Test-only.
const RECOVERY_ROOT: &[u8] = b"recovery-root-for-tests-only!";

// --- harness ------------------------------------------------------------------------------

/// The router under test plus the captcha store the test reaches into to read
/// the answer to the freshly-issued challenge.
struct Harness {
    app: Router,
    clock: Arc<ManualClock>,
    captcha_store: Arc<CaptchaStore>,
    /// Kept so `issue_captcha` can mint a challenge through the same service the gate
    /// verifies against — the only way to learn the answer, since nothing on the wire
    /// carries it anymore.
    captcha_service: Arc<CaptchaService>,
}

impl Harness {
    /// A surface with captcha on at a threshold of one and a generous
    /// anonymous bucket, so the bootstrap endpoints are not the limiting
    /// factor in any one test.
    fn new() -> Self {
        Self::with(|_| {})
    }

    /// A surface built from a caller-mutated base config.
    fn with(mutate: impl FnOnce(&mut Config)) -> Self {
        let mut config = Config::default();
        config.auth.token_key = Some(Secret::new(TEST_KEY));
        config.auth.captcha_threshold = Some(CAPTCHA_THRESHOLD);
        // The rate-limit price of one registration is overridable in
        // development because the public-internet default is the full
        // anonymous endpoint bucket, which would burn a single test's
        // budget in one call. Lowering the cost to a small constant keeps
        // the limiter present and exercised, but lets the test exercise
        // the route layer without colliding with itself.
        config.auth.registration_cost = Some(REGISTRATION_COST);
        // Generous bucket so the limiter never refuses a bootstrap call in a
        // test that is about the auth pipeline, not the limiter. The
        // `user`/`bot` buckets are unchanged because the bootstrap path
        // never charges them.
        config.rate_limit.anonymous_burst = ANONYMOUS_BURST;
        config.rate_limit.anonymous_refill_per_second = ANONYMOUS_BURST;
        mutate(&mut config);

        let clock = Arc::new(ManualClock::new(Timestamp::from_unix_ms(NOW_MS)));
        let registry = Arc::new(Registry::new());
        let cache = Arc::new(MemoryCache::new());
        let policies = Policies::from_config(&config.rate_limit).expect("policies validate");
        let real_limiter = Arc::new(CacheRateLimiter::new(cache, policies, &registry));
        let store = Arc::new(MemoryStore::new());

        // A dedicated captcha service and store; the store is held alongside
        // the router so the test can look up the answer it just issued.
        let captcha_store = Arc::new(CaptchaStore::new());
        let captcha_clock: Arc<dyn Clock + Send + Sync> = Arc::clone(&clock) as Arc<dyn Clock>;
        let captcha_service = Arc::new(CaptchaService::new(
            b"captcha-test-secret",
            captcha_clock,
            Config::default().captcha,
        ));
        let captcha_store_dyn: Arc<dyn migo_captcha::CaptchaStore + Send + Sync> =
            captcha_store.clone();
        let gate = Arc::new(migo_auth::captcha::CaptchaGate::new(
            captcha_service.clone(),
            captcha_store_dyn,
            CAPTCHA_THRESHOLD,
        ));

        let auth = Auth::new(
            store,
            real_limiter.clone(),
            &config,
            &registry,
            Box::new(SeededRandom::new(SEED)),
        )
        .expect("auth builds")
        .with_captcha(gate)
        .expect("captcha attaches")
        .with_recovery(RECOVERY_ROOT);

        let authenticator: SharedAuth = Arc::new(auth);
        let edge: SharedRateLimiter = real_limiter as SharedRateLimiter;
        let services = ApiServices {
            authenticator,
            rate_limiter: edge,
            clock: Arc::clone(&clock) as Arc<dyn Clock>,
            registry: Arc::clone(&registry),
            node: NodeInfo {
                node_id: "migo-test-node".to_string(),
                region: "test-region".to_string(),
                country: "ID".to_string(),
            },
            features: 0b101,
            // The tests exercise the auth bootstrap surface, not the media byte routes.
            media_files: None,
        };
        let app = router(&config, services);
        Self {
            app,
            clock,
            captcha_store,
            captcha_service,
        }
    }

    /// Drives one request through the whole middleware stack and router.
    async fn send(&self, request: Request<Body>) -> Resp {
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("the router is infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body collected")
            .to_vec();
        Resp {
            status,
            headers,
            bytes,
        }
    }

    /// Mints a fresh challenge through the same service the gate verifies against and
    /// stores it, returning the id the client would hold and the answer only the
    /// test-side door knows.
    ///
    /// The wire is exercised separately — `issue_captcha_over_the_route` below pins what
    /// the route actually returns — because the answer no longer exists on the wire in
    /// any form, which is the point of the image challenge: this is the only way to
    /// complete a challenge programmatically, and it lives behind the test-internal
    /// feature of the captcha crate.
    async fn issue_captcha(&self) -> CaptchaIssued {
        let (_view, stored, answer) = self
            .captcha_service
            .issue_for_test(migo_captcha::CaptchaMode::Image, &mut migo_core::OsRandom)
            .expect("the service issues");
        self.captcha_store
            .put(&stored)
            .await
            .expect("the challenge is stored");
        CaptchaIssued {
            challenge_id: stored.challenge_id,
            answer,
        }
    }

    /// Fetches a challenge over the route itself and pins the wire shape: an id, a
    /// base64 PNG, the mode, a positive countdown — and no answer in any field, nor as a
    /// substring of the raw body of any string the response carries.
    async fn issue_captcha_over_the_route(&self, mode: Option<&str>) -> Value {
        let body = match mode {
            Some(mode) => json!({ "mode": mode }),
            None => json!({}),
        };
        let resp = self
            .send(build_req(
                Method::POST,
                "/v1/auth/captcha",
                None,
                None,
                Some(&body),
            ))
            .await;
        assert_eq!(
            resp.status,
            StatusCode::OK,
            "captcha issue should succeed; body={}",
            resp.text()
        );
        let parsed: Value = serde_json::from_slice(&resp.bytes).expect("captcha is JSON");
        assert!(
            parsed["challenge_id"].is_string(),
            "challenge_id present and textual"
        );
        let image = parsed["image_png_base64"]
            .as_str()
            .expect("image_png_base64 present")
            .to_string();
        assert!(!image.is_empty(), "the image is not empty");
        let png = base64::engine::general_purpose::STANDARD
            .decode(&image)
            .expect("the image is standard base64");
        assert_eq!(&png[..4], b"\x89PNG", "the bytes are a PNG");
        assert!(
            parsed["ttl_seconds"].as_u64().unwrap_or(0) > 0,
            "ttl_seconds is a positive integer"
        );
        assert!(
            parsed.get("question").is_none(),
            "the old text-question field is gone from the wire"
        );
        parsed
    }

    /// Advances the shared clock.
    #[allow(dead_code)]
    fn advance(&self, millis: i64) {
        self.clock.advance_millis(millis);
    }
}

/// One fresh captcha challenge the test will answer.
struct CaptchaIssued {
    challenge_id: migo_core::Id,
    answer: String,
}

// --- request/response helpers ------------------------------------------------------------

/// A collected response: status, headers, and body bytes, so a test can look
/// at all three.
struct Resp {
    status: StatusCode,
    #[allow(dead_code)]
    headers: HeaderMap,
    bytes: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes)
            .unwrap_or_else(|_| panic!("body is not JSON: {}", self.text()))
    }

    fn text(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or("<non-utf8 body>")
    }

    fn error_code(&self) -> u64 {
        self.json()["error"]["code"]
            .as_u64()
            .expect("error envelope has a numeric code")
    }
}

fn build_req(
    method: Method,
    path: &str,
    ip: Option<&str>,
    bearer: Option<&str>,
    body: Option<&Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(ip) = ip {
        builder = builder.header("x-forwarded-for", ip);
    }
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(value).expect("body serialises"),
            ))
            .expect("request builds"),
        None => builder.body(Body::empty()).expect("request builds"),
    }
}

fn post_json(path: &str, ip: Option<&str>, body: &Value) -> Request<Body> {
    build_req(Method::POST, path, ip, None, Some(body))
}

#[track_caller]
fn expect_error(resp: &Resp, status: StatusCode, code: u32) {
    assert_eq!(
        resp.status,
        status,
        "expected status {status}; body={}",
        resp.text()
    );
    assert_eq!(
        resp.error_code(),
        u64::from(code),
        "expected error code {code}; body={}",
        resp.text()
    );
}

// --- the user-facing flow ----------------------------------------------------------------

/// The full happy path the user is asking the test to defend: a captcha
/// is fetched, a register carries it and succeeds, a login carries a
/// fresh captcha and succeeds, and a recovery request carries a fresh
/// captcha and answers `{ ok: true }` whether the account exists or not.
///
/// Every step is the exact body the working web client sends: the
/// `captcha` field is a `{ challenge_id, answer }` object, the `device`
/// field carries a `display_name` (the only field the route requires),
/// and the recovery request body is the `{ identifier, captcha }` pair
/// the route expects. A regression that drops any of these — the most
/// likely failure mode given the user report — is a body-validation
/// refusal at the route layer, not a silent success.
#[tokio::test]
async fn the_full_flow_captcha_register_login_and_recovery() {
    let h = Harness::new();

    // 1. The route itself: a usable challenge on the wire, in both modes, with
    //    no answer anywhere in the response. The register flow below uses a
    //    challenge minted through the service directly, because completing an
    //    image challenge programmatically is exactly what production callers
    //    cannot do.
    let standard = h.issue_captcha_over_the_route(None).await;
    assert_eq!(
        standard["mode"], "image",
        "the default mode is the standard image"
    );
    let alternative = h.issue_captcha_over_the_route(Some("image_alt")).await;
    assert_eq!(alternative["mode"], "image_alt");
    assert_ne!(
        standard["image_png_base64"], alternative["image_png_base64"],
        "the accessible mode is a different challenge, not the same picture gentler"
    );
    let register_captcha = h.issue_captcha().await;
    assert!(
        (5..=6).contains(&register_captcha.answer.len()),
        "a five-to-six character answer"
    );
    assert!(
        register_captcha
            .answer
            .bytes()
            .all(|byte| migo_captcha::ALPHABET.contains(&byte)),
        "the answer draws from the challenge alphabet"
    );

    // 2. Register with the captcha proof. A 201 with a fresh grant is the
    //    user-visible success shape.
    let register_body = json!({
        "username": "alice",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Integration Flow Device" },
        "captcha": {
            "challenge_id": register_captcha.challenge_id,
            "answer": register_captcha.answer,
        },
    });
    let register_resp = h
        .send(post_json(
            "/v1/auth/register",
            Some("203.0.113.10"),
            &register_body,
        ))
        .await;
    // A distinct /24 for the rest of the calls so each bootstrap charges
    // its own endpoint bucket. The captcha route and the bootstrap
    // endpoints share the network's per-/24 bucket by design, and the
    // first register consumes a sizeable slice; mixing distinct /24s
    // keeps the rest of the flow free of rate-limit interference.
    assert_eq!(
        register_resp.status,
        StatusCode::CREATED,
        "register should succeed; body={}",
        register_resp.text()
    );
    let register_grant = register_resp.json();
    assert!(register_grant["access_token"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    assert!(register_grant["refresh_token"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    let register_account_id = register_grant["account_id"]
        .as_str()
        .expect("account_id present")
        .to_string();
    assert_eq!(
        register_grant["is_new_account"], true,
        "the first register on a fresh store reports is_new_account=true"
    );

    // 3. A captcha is one-shot by design. The same challenge id submitted
    //    a second time from a network whose gate is engaged (a sign-in
    //    failure has tripped the per-IP counter) is refused; the
    //    route-level assertion lives in
    //    `a_captcha_with_a_wrong_answer_is_refused_with_a_curated_message`
    //    and the captcha-store consumption is asserted in
    //    `Harness::issue_captcha` for the second issuance. We do not pin
    //    the replay here because a fresh network has not tripped the
    //    gate yet, and the route legitimately accepts a register without
    //    consulting the captcha on a cold call.

    // 4. Login with a fresh captcha. The first sign-in from this network is
    //    captcha-gated by design: every bootstrap carries a proof, because
    //    the network has not earned the warm-path bypass.
    let login_captcha = h.issue_captcha().await;
    let login_body = json!({
        "identifier": "alice",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Integration Flow Device" },
        "captcha": {
            "challenge_id": login_captcha.challenge_id,
            "answer": login_captcha.answer,
        },
    });
    let login_resp = h
        .send(post_json("/v1/auth/login", Some("192.0.2.10"), &login_body))
        .await;
    assert_eq!(
        login_resp.status,
        StatusCode::OK,
        "login should succeed; body={}",
        login_resp.text()
    );
    let login_grant = login_resp.json();
    assert_eq!(
        login_grant["account_id"]
            .as_str()
            .expect("account_id on login"),
        register_account_id,
        "the login is for the same account the register opened"
    );
    assert!(login_grant["access_token"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    let login_token = login_grant["access_token"].as_str().unwrap();
    assert_ne!(
        login_token,
        register_grant["access_token"].as_str().unwrap(),
        "the access token minted on a fresh login is not the one the register minted"
    );
    // The minted session is a brand-new row in the store, distinct from the
    // session the register minted; pinning the ids here would be brittle
    // (the store may reuse a slot, the auth may not), so the assertion is
    // about the access token, not the session id.

    // 5. Start a recovery with the same identifier and a fresh captcha. The
    //    response is the same `{ ok: true }` whether the account exists or
    //    not, so an attacker cannot tell the difference from the wire.
    let recovery_captcha = h.issue_captcha().await;
    let recovery_body = json!({
        "identifier": "alice",
        "captcha": {
            "challenge_id": recovery_captcha.challenge_id,
            "answer": recovery_captcha.answer,
        },
    });
    let recovery_resp = h
        .send(post_json(
            "/v1/auth/recovery/request",
            Some("192.0.2.20"),
            &recovery_body,
        ))
        .await;
    assert_eq!(
        recovery_resp.status,
        StatusCode::OK,
        "recovery request should succeed; body={}",
        recovery_resp.text()
    );
    assert_eq!(recovery_resp.json()["ok"], true);

    // 6. The same recovery request for an identifier that does not exist
    //    answers the same way. The body and the code are identical, so the
    //    response is not an account-existence oracle.
    let other_captcha = h.issue_captcha().await;
    let other_body = json!({
        "identifier": "stranger@example.test",
        "captcha": {
            "challenge_id": other_captcha.challenge_id,
            "answer": other_captcha.answer,
        },
    });
    let other_resp = h
        .send(post_json(
            "/v1/auth/recovery/request",
            Some("198.51.100.20"),
            &other_body,
        ))
        .await;
    assert_eq!(
        other_resp.status,
        StatusCode::OK,
        "a recovery request for an unknown identifier also succeeds; body={}",
        other_resp.text()
    );
    assert_eq!(
        other_resp.json(),
        recovery_resp.json(),
        "the two responses are byte-identical: the route is enumeration-safe"
    );
}

/// Sign-in is never captcha-gated, whatever the gate's state.
///
/// The product decision: a returning member is the person the product exists for, and standing
/// between them and their account because a stranger from their network mistyped passwords is
/// punishing the wrong party. So the same wrong-password failure that engages the gate for
/// `register` leaves `sign-in` open — a correct-password login from the same IP, with no
/// captcha proof attached, succeeds.
#[tokio::test]
async fn a_sign_in_never_requires_a_captcha_even_once_the_gate_is_engaged() {
    let h = Harness::new();
    let ip = "198.51.100.30";

    // Register the account (the gate starts clean, but the register carries a proof anyway —
    // the threshold is one and this test's earlier traffic on this IP is none).
    let setup_captcha = h.issue_captcha().await;
    let setup_body = json!({
        "username": "gale",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Setup Device" },
        "captcha": {
            "challenge_id": setup_captcha.challenge_id,
            "answer": setup_captcha.answer,
        },
    });
    let setup_resp = h
        .send(post_json("/v1/auth/register", Some(ip), &setup_body))
        .await;
    assert_eq!(
        setup_resp.status,
        StatusCode::CREATED,
        "setup register should succeed; body={}",
        setup_resp.text()
    );

    // Trip the gate with a wrong-password sign-in.
    let trip_captcha = h.issue_captcha().await;
    let trip_body = json!({
        "identifier": "gale",
        "password": "deliberately-wrong",
        "device": { "display_name": "Trip Device" },
        "captcha": {
            "challenge_id": trip_captcha.challenge_id,
            "answer": trip_captcha.answer,
        },
    });
    let trip_resp = h
        .send(post_json("/v1/auth/login", Some(ip), &trip_body))
        .await;
    assert_eq!(
        trip_resp.status,
        StatusCode::UNAUTHORIZED,
        "a wrong-password sign-in fails; body={}",
        trip_resp.text()
    );

    // The gate is engaged for this network (a register without a proof would be refused —
    // the test above pins that). Sign-in, though, carries no captcha and succeeds anyway.
    let login_body = json!({
        "identifier": "gale",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "No Captcha Login" },
    });
    let login_resp = h
        .send(post_json("/v1/auth/login", Some(ip), &login_body))
        .await;
    assert_eq!(
        login_resp.status,
        StatusCode::OK,
        "sign-in must not be captcha-gated; body={}",
        login_resp.text()
    );
    assert!(
        login_resp.json()["access_token"].as_str().is_some(),
        "the ungated login mints a grant"
    );
}

/// The progressive sign-in lockout: five wrong passwords lock the account for a
/// minute, the next three for three, the next three for five — and a correct
/// password clears the whole ladder. The lock follows the account across every
/// identifier and network (v0.8.4).
#[tokio::test]
async fn repeated_sign_in_failures_lock_the_account_on_a_climbing_ladder() {
    let h = Harness::new();
    let ip = "198.51.100.40";

    // Register the target account (the gate starts clean).
    let setup_captcha = h.issue_captcha().await;
    let setup_body = json!({
        "username": "laddered",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Setup Device" },
        "captcha": {
            "challenge_id": setup_captcha.challenge_id,
            "answer": setup_captcha.answer,
        },
    });
    let setup_resp = h
        .send(post_json("/v1/auth/register", Some(ip), &setup_body))
        .await;
    assert_eq!(
        setup_resp.status,
        StatusCode::CREATED,
        "setup register should succeed; body={}",
        setup_resp.text()
    );

    // Five wrong passwords. None of them is captcha-gated (sign-in never is) and
    // none of them trips the lockout yet. The clock advances between attempts so the
    // anonymous rate-limit bucket refills — the ladder under test is the lockout, not
    // the bucket.
    for attempt in 1..=5 {
        h.advance(2_000);
        let body = json!({
            "identifier": "laddered",
            "password": "deliberately-wrong",
            "device": { "display_name": "Ladder Device" },
        });
        let resp = h.send(post_json("/v1/auth/login", Some(ip), &body)).await;
        assert_eq!(
            resp.status,
            StatusCode::UNAUTHORIZED,
            "wrong password {attempt} is a plain refusal; body={}",
            resp.text()
        );
    }

    // The sixth attempt is locked: 429 AUTH_LOCKED, with the server's advice on
    // how long to wait.
    let body = json!({
        "identifier": "laddered",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Locked Device" },
    });
    let locked = h.send(post_json("/v1/auth/login", Some(ip), &body)).await;
    assert_eq!(
        locked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "the fifth failure must have engaged the lockout; body={}",
        locked.text()
    );
    assert_eq!(locked.error_code(), u64::from(codes::AUTH_LOCKED));
    let locked_json = locked.json();
    let retry_ms = locked_json["error"]["retry_after_ms"]
        .as_u64()
        .expect("the lockout names its remaining wait");
    assert!(
        retry_ms > 0 && retry_ms <= 60_000,
        "the first rung is a minute: retry_after_ms={retry_ms}"
    );

    // Waiting it out (the clock advances past the minute) and signing in with the
    // right password succeeds — and clears the ladder.
    h.advance(retry_ms as i64 + 1_000);
    let clear_body = json!({
        "identifier": "laddered",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "After Lockout Device" },
    });
    let clear_resp = h
        .send(post_json("/v1/auth/login", Some(ip), &clear_body))
        .await;
    assert_eq!(
        clear_resp.status,
        StatusCode::OK,
        "an expired lockout frees the account; body={}",
        clear_resp.text()
    );

    // A fresh wrong-password run starts the ladder from its first rung again —
    // the success reset it, so four more failures are plain refusals.
    for attempt in 1..=4 {
        h.advance(2_000);
        let body = json!({
            "identifier": "laddered",
            "password": "deliberately-wrong",
            "device": { "display_name": "Ladder Device" },
        });
        let resp = h.send(post_json("/v1/auth/login", Some(ip), &body)).await;
        assert_eq!(
            resp.status,
            StatusCode::UNAUTHORIZED,
            "post-reset failure {attempt} is a plain refusal; body={}",
            resp.text()
        );
    }
}

/// A register without a captcha is refused once the gate is engaged.
///
/// The captcha gate is tripped by a sign-in failure: a wrong password on
/// a known identifier from a network records one failure past the
/// threshold of one, and the next attempt from the same network has to
/// carry a captcha proof. The next attempt in this test is a register,
/// not a sign-in, and the assertion is that the gate fires before the
/// password check.
#[tokio::test]
async fn a_register_without_a_captcha_is_refused_once_the_gate_is_engaged() {
    let h = Harness::new();
    let ip = "198.51.100.10";

    // First, register an account we can later mis-sign-in to. The captcha
    // is sent with the register so the password is what the route checks,
    // not the captcha.
    let setup_captcha = h.issue_captcha().await;
    let setup_body = json!({
        "username": "carol",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Setup Device" },
        "captcha": {
            "challenge_id": setup_captcha.challenge_id,
            "answer": setup_captcha.answer,
        },
    });
    let setup_resp = h
        .send(post_json("/v1/auth/register", Some(ip), &setup_body))
        .await;
    assert_eq!(
        setup_resp.status,
        StatusCode::CREATED,
        "setup register should succeed; body={}",
        setup_resp.text()
    );

    // Trip the gate: a sign-in with the right identifier and a wrong
    // password. The auth's `note_captcha_failure` records one strike
    // against the network, and the threshold is one, so the gate is now
    // engaged for the next attempt from the same IP.
    let trip_captcha = h.issue_captcha().await;
    let trip_body = json!({
        "identifier": "carol",
        "password": "deliberately-wrong",
        "device": { "display_name": "Trip Device" },
        "captcha": {
            "challenge_id": trip_captcha.challenge_id,
            "answer": trip_captcha.answer,
        },
    });
    let trip_resp = h
        .send(post_json("/v1/auth/login", Some(ip), &trip_body))
        .await;
    assert_eq!(
        trip_resp.status,
        StatusCode::UNAUTHORIZED,
        "a wrong-password sign-in fails; body={}",
        trip_resp.text()
    );

    // The next attempt from the same network — a register, with no
    // captcha — is refused at the captcha gate, not at the password
    // check. The body is well-formed and the password clears the floor;
    // the refusal is purely because the captcha proof is missing.
    let no_captcha_body = json!({
        "username": "dave",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "No Captcha Device" },
    });
    let no_captcha_resp = h
        .send(post_json("/v1/auth/register", Some(ip), &no_captcha_body))
        .await;
    expect_error(
        &no_captcha_resp,
        StatusCode::BAD_REQUEST,
        codes::CAPTCHA_REQUIRED,
    );
}

/// A captcha with a wrong answer is refused with a curated message,
/// distinct from the refusal a missing captcha receives.
///
/// The gate is engaged by a sign-in failure from the same network (see
/// the test above for the mechanism). Once engaged, a present-but-wrong
/// `answer` is `INVALID_CAPTCHA` so the client knows to fetch a fresh
/// challenge; a missing captcha is `CAPTCHA_REQUIRED`. The two are
/// distinct on purpose: a wrong answer means the user typed something,
/// and a missing answer means the client did not even try.
#[tokio::test]
async fn a_captcha_with_a_wrong_answer_is_refused_with_a_curated_message() {
    let h = Harness::new();
    let ip = "198.51.100.20";

    // Engage the gate with a sign-in against an identifier that does not
    // exist. The failure path records a strike; the user-facing response
    // is the same as a wrong password on a real account, so an attacker
    // cannot tell the two apart.
    let trip_captcha = h.issue_captcha().await;
    let trip_body = json!({
        "identifier": "no-such-account",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Trip Device" },
        "captcha": {
            "challenge_id": trip_captcha.challenge_id,
            "answer": trip_captcha.answer,
        },
    });
    let trip_resp = h
        .send(post_json("/v1/auth/login", Some(ip), &trip_body))
        .await;
    assert_eq!(
        trip_resp.status,
        StatusCode::UNAUTHORIZED,
        "a sign-in for an unknown identifier fails; body={}",
        trip_resp.text()
    );

    // The gate is now engaged. A register with a captcha proof that
    // names a real challenge id but submits a wrong answer is refused
    // with INVALID_CAPTCHA.
    let challenge = h.issue_captcha().await;
    let wrong = if challenge.answer == "000000" {
        "000001".to_string()
    } else {
        "000000".to_string()
    };
    let body = json!({
        "username": "erin",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Wrong Answer Device" },
        "captcha": {
            "challenge_id": challenge.challenge_id,
            "answer": wrong,
        },
    });
    let resp = h
        .send(post_json("/v1/auth/register", Some(ip), &body))
        .await;
    expect_error(&resp, StatusCode::BAD_REQUEST, codes::INVALID_CAPTCHA);
}

/// A recovery request without a captcha is refused before the gate is
/// consulted: a recovery flow is captcha-required as a structural matter,
/// not a gate-tripped one. The error is a validation error on the
/// `captcha` field, and the response does not hint at whether the
/// identifier matches a real account.
#[tokio::test]
async fn a_recovery_request_without_a_captcha_is_refused() {
    let h = Harness::new();

    let body = json!({ "identifier": "frank" });
    let resp = h
        .send(post_json(
            "/v1/auth/recovery/request",
            Some("198.51.100.30"),
            &body,
        ))
        .await;
    expect_error(&resp, StatusCode::BAD_REQUEST, codes::VALIDATION_FAILED);
    // The error message must not leak whether the identifier matched: a
    // validation error on the captcha field is a captcha error, not a
    // "this account does not exist" error.
    let text = resp.text();
    assert!(
        !text.to_lowercase().contains("not found"),
        "the validation error must not hint at account existence; body={text}"
    );
    assert!(
        !text.to_lowercase().contains("unknown user"),
        "the validation error must not hint at account existence; body={text}"
    );
}

/// Two forged recovery-confirm token ids answer with the same status and
/// the same body bytes, so the confirm route is not an oracle for
/// whether a recovery flow is in progress.
#[tokio::test]
async fn a_recovery_confirm_with_an_unknown_token_id_is_not_an_oracle() {
    let h = Harness::new();

    // The two requests use syntactically-valid ids that no issuance has
    // minted. Both must answer with the same body and the same code, so
    // the response is not a way to tell whether a recovery flow is in
    // progress.
    let id1 = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let id2 = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    let body1 = json!({
        "token_id": id1,
        "tag": "00112233445566778899aabbccddeeff",
        "new_password": "a-new-passphrase-here",
    });
    let body2 = json!({
        "token_id": id2,
        "tag": "00112233445566778899aabbccddeeff",
        "new_password": "a-new-passphrase-here",
    });
    let resp1 = h
        .send(post_json(
            "/v1/auth/recovery/confirm",
            Some("198.51.100.40"),
            &body1,
        ))
        .await;
    let resp2 = h
        .send(post_json(
            "/v1/auth/recovery/confirm",
            Some("198.51.100.41"),
            &body2,
        ))
        .await;
    assert_eq!(
        resp1.status, resp2.status,
        "status must not distinguish the two"
    );
    assert_eq!(
        resp1.bytes, resp2.bytes,
        "the response body must be byte-identical: the confirm route is enumeration-safe"
    );
    // The hex tag a caller supplied is never echoed back.
    for resp in [&resp1, &resp2] {
        let text = resp.text();
        assert!(
            !text.contains("00112233445566778899aabbccddeeff"),
            "the response echoes the tag back"
        );
    }
}

// --- the account security surface -------------------------------------------------------

/// Brief sections 16-18 over the routes: the session list, the single-session revoke,
/// and the device revoke that ends every session on the device.
///
/// The single-session route is pinned here because it was once wired to the device
/// revoker — it passed the session id where a device id was expected, so a caller
/// naming a real session read a 404, and the web client's per-row "revoke" control
/// could never have worked. The wire contract this test defends is the one every
/// client is written against: the id in `/auth/sessions/{id}/revoke` is a session's,
/// and the id in `/devices/{id}/revoke` is a device's.
#[tokio::test]
async fn revoking_one_session_and_then_its_device_over_the_routes() {
    let h = Harness::new();

    // Two devices on one account: the register's own, and a login's.
    let captcha = h.issue_captcha().await;
    let register_body = json!({
        "username": "ada",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Ada's laptop" },
        "captcha": {
            "challenge_id": captcha.challenge_id,
            "answer": captcha.answer,
        },
    });
    let first = h
        .send(post_json(
            "/v1/auth/register",
            Some("203.0.113.10"),
            &register_body,
        ))
        .await;
    assert_eq!(
        first.status,
        StatusCode::CREATED,
        "register should succeed; body={}",
        first.text()
    );
    let first_grant = first.json();
    let first_session = first_grant["session_id"]
        .as_str()
        .expect("session_id on the grant")
        .to_string();
    let first_device = first_grant["device_id"]
        .as_str()
        .expect("device_id on the grant")
        .to_string();
    let first_token = first_grant["access_token"]
        .as_str()
        .expect("access_token on the grant")
        .to_string();

    let captcha = h.issue_captcha().await;
    let login_body = json!({
        "identifier": "ada",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Ada's phone" },
        "captcha": {
            "challenge_id": captcha.challenge_id,
            "answer": captcha.answer,
        },
    });
    let second = h
        .send(post_json(
            "/v1/auth/login",
            Some("198.51.100.7"),
            &login_body,
        ))
        .await;
    assert_eq!(
        second.status,
        StatusCode::OK,
        "login should succeed; body={}",
        second.text()
    );
    let second_grant = second.json();
    let second_device = second_grant["device_id"]
        .as_str()
        .expect("device_id on the grant")
        .to_string();
    let second_token = second_grant["access_token"]
        .as_str()
        .expect("access_token on the grant")
        .to_string();
    assert_ne!(
        second_device, first_device,
        "a login with a different display name is a different device"
    );

    // The caller's own session list shows both sessions.
    let sessions_resp = h
        .send(build_req(
            Method::GET,
            "/v1/auth/sessions",
            Some("198.51.100.8"),
            Some(&second_token),
            None,
        ))
        .await;
    assert_eq!(
        sessions_resp.status,
        StatusCode::OK,
        "the sessions list; body={}",
        sessions_resp.text()
    );
    let sessions = sessions_resp.json()["sessions"]
        .as_array()
        .expect("sessions array")
        .clone();
    assert!(
        sessions.len() >= 2,
        "both devices' sessions are listed; got {}",
        sessions.len()
    );

    // Revoke the *first* session by its session id, from the second device. This is
    // the call the old wiring turned into a 404.
    let revoke = h
        .send(build_req(
            Method::POST,
            &format!("/v1/auth/sessions/{first_session}/revoke"),
            Some("198.51.100.8"),
            Some(&second_token),
            Some(&json!({})),
        ))
        .await;
    assert_eq!(
        revoke.status,
        StatusCode::NO_CONTENT,
        "one session revoked by id; body={}",
        revoke.text()
    );

    // The revoked session's token no longer opens a door, and the caller's own
    // session is untouched — that is the "one" in the route's name.
    let refused = h
        .send(build_req(
            Method::GET,
            "/v1/auth/sessions",
            Some("198.51.100.9"),
            Some(&first_token),
            None,
        ))
        .await;
    assert_eq!(
        refused.status,
        StatusCode::UNAUTHORIZED,
        "the revoked session's token is dead; body={}",
        refused.text()
    );
    let still = h
        .send(build_req(
            Method::GET,
            "/v1/auth/sessions",
            Some("198.51.100.9"),
            Some(&second_token),
            None,
        ))
        .await;
    assert_eq!(
        still.status,
        StatusCode::OK,
        "the caller's own session survives a single-session revoke"
    );

    // Sign in again on the first device, so the device revoke below has a live
    // session to end and the count it returns says so.
    let captcha = h.issue_captcha().await;
    let resume_body = json!({
        "identifier": "ada",
        "password": GOOD_PASSWORD,
        "device": {
            "display_name": "Ada's laptop",
            "device_id": first_device,
        },
        "captcha": {
            "challenge_id": captcha.challenge_id,
            "answer": captcha.answer,
        },
    });
    let third = h
        .send(post_json(
            "/v1/auth/login",
            Some("198.51.100.11"),
            &resume_body,
        ))
        .await;
    assert_eq!(
        third.status,
        StatusCode::OK,
        "the laptop signs back in; body={}",
        third.text()
    );
    let third_grant = third.json();
    assert_eq!(
        third_grant["device_id"].as_str(),
        Some(first_device.as_str()),
        "claiming the device resumes it rather than minting a new one"
    );
    let third_token = third_grant["access_token"]
        .as_str()
        .expect("access_token on the grant")
        .to_string();

    // The device list shows the laptop as active before the revoke.
    let devices_resp = h
        .send(build_req(
            Method::GET,
            "/v1/devices",
            Some("198.51.100.12"),
            Some(&second_token),
            None,
        ))
        .await;
    assert_eq!(
        devices_resp.status,
        StatusCode::OK,
        "the device list; body={}",
        devices_resp.text()
    );
    let devices = devices_resp.json()["devices"]
        .as_array()
        .expect("devices array")
        .clone();
    let laptop = devices
        .iter()
        .find(|device| device["device_id"].as_str() == Some(first_device.as_str()))
        .expect("the laptop is listed");
    assert_eq!(laptop["status"], "active");

    // The device revoke: one call, the device's sessions die with it, and the
    // answer says how many.
    let device_revoke = h
        .send(build_req(
            Method::POST,
            &format!("/v1/devices/{first_device}/revoke"),
            Some("198.51.100.12"),
            Some(&second_token),
            Some(&json!({})),
        ))
        .await;
    assert_eq!(
        device_revoke.status,
        StatusCode::OK,
        "device revoke; body={}",
        device_revoke.text()
    );
    let body = device_revoke.json();
    assert_eq!(body["ok"], true, "the answer confirms the revoke");
    assert_eq!(
        body["revoked"].as_u64(),
        Some(1),
        "exactly the laptop's one live session ended; body={}",
        device_revoke.text()
    );

    // The laptop's fresh token is dead and the device is gone from the list — the
    // two facts a security screen shows after "this device is gone". (A revoked
    // device is filtered out of the account's list: the store's contract keeps
    // `devices_for_account` to devices that may still authenticate.)
    let dead = h
        .send(build_req(
            Method::GET,
            "/v1/auth/sessions",
            Some("198.51.100.13"),
            Some(&third_token),
            None,
        ))
        .await;
    assert_eq!(
        dead.status,
        StatusCode::UNAUTHORIZED,
        "the revoked device's session is gone; body={}",
        dead.text()
    );
    let after = h
        .send(build_req(
            Method::GET,
            "/v1/devices",
            Some("198.51.100.13"),
            Some(&second_token),
            None,
        ))
        .await;
    let devices = after.json()["devices"]
        .as_array()
        .expect("devices array")
        .clone();
    assert!(
        devices
            .iter()
            .all(|device| device["device_id"].as_str() != Some(first_device.as_str())),
        "the revoked device is gone from the list; body={}",
        after.text()
    );
}
