//! Integration tests for the `migo-api` REST surface.
//!
//! # What is under test, and why in-process
//!
//! `migo-api` builds an [`axum::Router`] and nothing else — it opens no socket. These tests
//! drive that router the way `migod` would, but through `tower`'s `oneshot` rather than a TCP
//! listener, so every assertion is about the router's own behaviour and none about the network.
//! The collaborators are the real in-memory implementations the rest of the workspace ships for
//! exactly this: a `MemoryStore`, a `MemoryCache`, the real `CacheRateLimiter` over that cache,
//! a fresh `Registry`, and a `ManualClock` so an expiry boundary is a line of test code rather
//! than a `sleep`. The authenticator is the real `migo-auth` service; nothing about identity is
//! faked, because the invariants below are precisely about the seams between the HTTP edge and
//! the domain.
//!
//! # The invariants these tests defend (brief sections 118-121, 145, 161, 174)
//!
//! 1. **Authn/authz on every route.** An endpoint that requires a caller refuses one that is
//!    missing, malformed, or expired, and the refusal is the right code (`UNAUTHENTICATED` for
//!    no credential, `TOKEN_INVALID`/`TOKEN_EXPIRED` for a bad or stale one).
//! 2. **Only the public face crosses the wire.** A forced internal fault returns the envelope
//!    with an empty public message and nothing about the cause — no address, no SQL, no path,
//!    no crate name. A login failure is byte-identical for a missing account and a wrong
//!    password, so neither response nor timing is an account-existence oracle.
//! 3. **Input validation at every boundary.** Length limits hold at the edge and one past it;
//!    absurd input — a non-id where an id is required, a body that is not the declared type, a
//!    negative or overflowing count, a body past the ceiling — is refused, not tolerated.
//! 4. **Rate limiting per route.** The unauthenticated bootstrap endpoints are metered, an
//!    exhausted budget is `RATE_LIMITED`, an addressless caller is never charged, and one
//!    caller's spending never touches another's budget.
//! 5. **Content sniffing on uploads** — not applicable: this crate has no upload route. Asserted
//!    by absence below.
//! 6. **Never a byte proxy for media** — this crate serves JSON and one Prometheus text page and
//!    nothing else; asserted by absence below.
//! 7. **Nothing sensitive is logged or metered.** The rendered registry carries no account,
//!    device, or session id, and no response header or non-auth body carries a token or a full
//!    caller IP. (The auth endpoints return the caller's *own* tokens in their body by design —
//!    that is the one place a token legitimately crosses the wire.)
//! 8. **The method/route surface is exactly what is intended.** An unknown path is `404`, a
//!    wrong method on a known path is `405`, and no debug or admin route answers.
//! 9. **Idempotency where the API declares it.** See the `idempotency` section: the extractor
//!    exists but no route consumes it, which is recorded as a finding rather than asserted true.

#![allow(clippy::items_after_statements, clippy::too_many_lines)]

use std::net::IpAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use migo_api::{router, ApiServices, Page, PageParams, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use migo_auth::{Auth, SharedAuth};
use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Clock, ManualClock, Secret, SeededRandom, Timestamp};
use migo_protocol::{codes, NodeInfo};
use migo_ratelimit::{
    BucketKey, CacheRateLimiter, Policies, RateLimiter, SharedRateLimiter, TrustTier, Verdict,
};
use migo_store::MemoryStore;

// --- constants ----------------------------------------------------------------------------

/// The 32-byte signing key the auth service needs, base64 as configuration carries it. Shared
/// with `migo-auth`'s own tests so a token minted here would verify there too.
const TEST_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// A password that clears the floor: long enough, not on the common list, and not derived from
/// any username these tests register.
const GOOD_PASSWORD: &str = "sunflower gravel bicycle";

/// A fixed wall-clock instant so token issue and expiry are deterministic.
const NOW_MS: i64 = 1_800_000_000_000;

/// The access-token lifetime in the default config, in milliseconds. Advancing the clock past it
/// expires an access token without touching a refresh token (thirty days).
const ACCESS_TTL_MS: i64 = 900 * 1_000;

/// The node identity the surface reports at `/v1/config`. Distinct from the token region so a
/// test can tell which value the document actually echoes.
const NODE_ID: &str = "migo-test-node";
const NODE_REGION: &str = "test-region";
const NODE_COUNTRY: &str = "ID";

/// The feature bits the surface advertises.
const FEATURES: u64 = 0b101;

/// A stable seed so ids are reproducible run to run.
const SEED: u64 = 0x5eed_1234;

// --- harness ------------------------------------------------------------------------------

/// The router under test plus the handles a test needs to reach behind it — advance the clock,
/// read the metric registry, inspect a bucket balance.
struct Harness {
    app: Router,
    clock: Arc<ManualClock>,
    registry: Arc<Registry>,
    limiter: SharedRateLimiter,
}

impl Harness {
    /// The default surface: registration allowed, a 1 MiB body ceiling, real limiter shared
    /// between the edge and the domain, as `migod` wires it.
    fn new() -> Self {
        Self::build(base_config(), None, None)
    }

    /// A surface built from a caller-mutated config, for the body-ceiling and feature-flag tests.
    fn with(mutate: impl FnOnce(&mut Config)) -> Self {
        let mut config = base_config();
        mutate(&mut config);
        Self::build(config, None, None)
    }

    /// A surface whose *edge* limiter always faults, so a bootstrap route is forced through the
    /// internal-error funnel. The domain keeps the real limiter; the edge charge fails first.
    fn with_failing_edge(boom: &str) -> Self {
        let edge: SharedRateLimiter = Arc::new(FailingLimiter {
            policies: Policies::default(),
            boom: boom.to_string(),
        });
        Self::build(base_config(), Some(edge), None)
    }

    fn build(
        mut config: Config,
        edge_limiter: Option<SharedRateLimiter>,
        media_files: Option<migo_api::SharedMediaFiles>,
    ) -> Self {
        if config.auth.token_key.is_none() {
            config.auth.token_key = Some(Secret::new(TEST_KEY));
        }
        let clock = Arc::new(ManualClock::new(Timestamp::from_unix_ms(NOW_MS)));
        let registry = Arc::new(Registry::new());
        let cache = Arc::new(MemoryCache::new());
        let policies =
            Policies::from_config(&config.rate_limit).expect("default policies are valid");
        let real_limiter = Arc::new(CacheRateLimiter::new(cache, policies, &registry));
        let store = Arc::new(MemoryStore::new());
        let auth = Auth::new(
            store,
            Arc::clone(&real_limiter),
            &config,
            &registry,
            Box::new(SeededRandom::new(SEED)),
        )
        .expect("auth service builds");
        let authenticator: SharedAuth = Arc::new(auth);
        let edge: SharedRateLimiter =
            edge_limiter.unwrap_or_else(|| Arc::clone(&real_limiter) as SharedRateLimiter);
        let services = ApiServices {
            authenticator,
            rate_limiter: edge,
            clock: Arc::clone(&clock) as Arc<dyn Clock>,
            registry: Arc::clone(&registry),
            node: NodeInfo {
                node_id: NODE_ID.to_string(),
                region: NODE_REGION.to_string(),
                country: NODE_COUNTRY.to_string(),
            },
            features: FEATURES,
            media_files,
        };
        let app = router(&config, services);
        Self {
            app,
            clock,
            registry,
            limiter: Arc::clone(&real_limiter) as SharedRateLimiter,
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

    /// Registers an account and returns the raw response.
    async fn register(&self, ip: Option<&str>, username: &str, password: &str) -> Resp {
        let body = json!({
            "username": username,
            "password": password,
            "device": { "display_name": "Integration Test" },
        });
        self.send(post_json("/v1/auth/register", ip, &body)).await
    }

    /// Registers an account expected to succeed and returns the parsed grant.
    async fn account(&self, ip: Option<&str>, username: &str) -> Value {
        let resp = self.register(ip, username, GOOD_PASSWORD).await;
        assert_eq!(
            resp.status,
            StatusCode::CREATED,
            "registration should succeed; body={}",
            resp.text()
        );
        resp.json()
    }

    /// Advances the shared clock.
    fn advance(&self, millis: i64) {
        self.clock.advance_millis(millis);
    }

    /// A bucket's current balance, read without charging it.
    async fn peek_ip(&self, ip: &str) -> u32 {
        let addr: IpAddr = ip.parse().expect("test ip parses");
        self.limiter
            .peek(&BucketKey::ip(addr), TrustTier::Anonymous, self.clock.now())
            .await
            .expect("peek succeeds")
    }
}

/// The base configuration every harness starts from: `Config::default()` with the signing key
/// filled in. Development defaults already allow registration and use in-memory backends.
fn base_config() -> Config {
    let mut config = Config::default();
    config.auth.token_key = Some(Secret::new(TEST_KEY));
    config
}

// --- a limiter double that always faults --------------------------------------------------

/// A [`RateLimiter`] whose charge always returns an internal error carrying a fake secret, so a
/// bootstrap route can be forced through the error funnel and its output inspected for leaks.
struct FailingLimiter {
    policies: Policies,
    boom: String,
}

#[async_trait::async_trait]
impl RateLimiter for FailingLimiter {
    async fn charge(
        &self,
        _keys: &[BucketKey],
        _cost: u32,
        _tier: TrustTier,
        _now: Timestamp,
    ) -> migo_core::Result<Verdict> {
        Err(migo_protocol::fault::internal(self.boom.clone()))
    }

    async fn peek(
        &self,
        _key: &BucketKey,
        _tier: TrustTier,
        _now: Timestamp,
    ) -> migo_core::Result<u32> {
        Ok(0)
    }

    async fn clear(&self, _key: &BucketKey) -> migo_core::Result<()> {
        Ok(())
    }

    fn policies(&self) -> &Policies {
        &self.policies
    }
}

// --- request builders and response helpers ------------------------------------------------

/// A collected response: status, headers, and body bytes, so a test can look at all three.
struct Resp {
    status: StatusCode,
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

    fn error_symbol(&self) -> String {
        self.json()["error"]["symbol"]
            .as_str()
            .expect("error envelope has a symbol")
            .to_string()
    }

    fn error_message(&self) -> String {
        self.json()["error"]["message"]
            .as_str()
            .expect("error envelope has a message")
            .to_string()
    }
}

/// Builds a request, attaching an address, a bearer token, and a JSON body only when given.
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

fn get(path: &str) -> Request<Body> {
    build_req(Method::GET, path, None, None, None)
}

fn post_json(path: &str, ip: Option<&str>, body: &Value) -> Request<Body> {
    build_req(Method::POST, path, ip, None, Some(body))
}

/// Asserts a response is the error envelope with the given HTTP status and error code.
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

// --- operational surface: health, readiness, metrics, config ------------------------------

#[tokio::test]
async fn health_reports_ok() {
    let h = Harness::new();
    let resp = h.send(get("/health")).await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(resp.json()["status"], "ok");
}

#[tokio::test]
async fn readiness_reports_ready() {
    let h = Harness::new();
    let resp = h.send(get("/ready")).await;
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(resp.json()["status"], "ready");
}

#[tokio::test]
async fn metrics_is_prometheus_text() {
    let h = Harness::new();
    let resp = h.send(get("/metrics")).await;
    assert_eq!(resp.status, StatusCode::OK);
    let content_type = resp
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert_eq!(content_type, "text/plain; version=0.0.4");
}

#[tokio::test]
async fn config_reports_node_identity() {
    let h = Harness::new();
    let resp = h.send(get("/v1/config")).await;
    assert_eq!(resp.status, StatusCode::OK);
    let doc = resp.json();
    assert_eq!(doc["node"]["id"], NODE_ID);
    assert_eq!(doc["node"]["region"], NODE_REGION);
    assert_eq!(doc["node"]["country"], NODE_COUNTRY);
}

#[tokio::test]
async fn config_reports_the_feature_bits() {
    let h = Harness::new();
    let resp = h.send(get("/v1/config")).await;
    assert_eq!(resp.json()["features"].as_u64(), Some(FEATURES));
}

#[tokio::test]
async fn config_reports_policy_limits() {
    let h = Harness::new();
    let resp = h.send(get("/v1/config")).await;
    let limits = &resp.json()["limits"];
    assert_eq!(limits["allow_registration"], true);
    assert_eq!(limits["password_min_length"].as_u64(), Some(10));
    assert_eq!(limits["max_page_size"].as_u64(), Some(200));
}

#[tokio::test]
async fn config_is_unauthenticated() {
    // No bearer token, yet the document is served: it is public by design.
    let h = Harness::new();
    let resp = h.send(get("/v1/config")).await;
    assert_eq!(resp.status, StatusCode::OK);
}

// --- registration happy path --------------------------------------------------------------

#[tokio::test]
async fn register_creates_an_account_and_returns_201() {
    let h = Harness::new();
    let resp = h
        .register(Some("203.0.113.1"), "alice", GOOD_PASSWORD)
        .await;
    assert_eq!(resp.status, StatusCode::CREATED, "body={}", resp.text());
    let grant = resp.json();
    assert_eq!(grant["is_new_account"], true);
    assert!(grant["access_token"]
        .as_str()
        .is_some_and(|t| !t.is_empty()));
    assert!(grant["refresh_token"]
        .as_str()
        .is_some_and(|t| !t.is_empty()));
}

#[tokio::test]
async fn register_then_login_returns_the_same_account() {
    // Registration is charged the whole network endpoint bucket, and buckets are per /24, so
    // this signs in from a different network than it registered on — the same client would
    // already hold a session and not sign in again, and the rate model is exercised in its own
    // tests. Here the concern is only that the account is the same one.
    let h = Harness::new();
    let created = h.account(Some("203.0.113.2"), "bob").await;
    let login = json!({
        "identifier": "bob",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Second Device" },
    });
    let resp = h
        .send(post_json("/v1/auth/login", Some("198.51.100.3"), &login))
        .await;
    assert_eq!(resp.status, StatusCode::OK, "body={}", resp.text());
    assert_eq!(resp.json()["account_id"], created["account_id"]);
    assert_eq!(resp.json()["is_new_account"], false);
}

// --- shared helpers for the sections below ------------------------------------------------

/// A syntactically valid `Id` that no test ever mints, for "names something that does not
/// belong to you" and "names nothing at all" probes.
const A_VALID_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

/// Builds a logout request naming a session, with an optional bearer token.
fn logout_req(bearer: Option<&str>, session_id: &str) -> Request<Body> {
    build_req(
        Method::POST,
        "/v1/auth/logout",
        None,
        bearer,
        Some(&json!({ "session_id": session_id })),
    )
}

/// Joins every response header into one string, so a leak check can scan them all.
fn headers_joined(headers: &HeaderMap) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{name}: {}", value.to_str().unwrap_or("<binary>")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Asserts a blob carries none of the shapes an internal detail leaks as.
#[track_caller]
fn assert_no_internal_shape(blob: &str) {
    for needle in [
        "src/", ".rs:", "panic", "migo_", "Cargo", "SELECT", "postgres", "/home/", "/var/",
        "unwrap",
    ] {
        assert!(
            !blob.contains(needle),
            "internal shape {needle:?} leaked in: {blob}"
        );
    }
}

// --- authn/authz on the logout route (invariant 1) ----------------------------------------

#[tokio::test]
async fn logout_without_a_token_is_unauthenticated() {
    // The `Authenticated` extractor runs before the body is parsed, so a well-formed body does
    // not get the request past a missing credential.
    let h = Harness::new();
    let resp = h.send(logout_req(None, A_VALID_ID)).await;
    expect_error(&resp, StatusCode::UNAUTHORIZED, codes::UNAUTHENTICATED);
}

#[tokio::test]
async fn logout_with_a_garbage_token_is_token_invalid() {
    let h = Harness::new();
    let resp = h
        .send(logout_req(Some("not.a.real.token"), A_VALID_ID))
        .await;
    expect_error(&resp, StatusCode::UNAUTHORIZED, codes::TOKEN_INVALID);
}

#[tokio::test]
async fn logout_with_an_empty_bearer_is_unauthenticated() {
    // "Authorization: Bearer " with nothing after it is treated as no token at all.
    let h = Harness::new();
    let resp = h.send(logout_req(Some(""), A_VALID_ID)).await;
    expect_error(&resp, StatusCode::UNAUTHORIZED, codes::UNAUTHENTICATED);
}

#[tokio::test]
async fn logout_with_a_non_bearer_scheme_is_unauthenticated() {
    let h = Harness::new();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/logout")
        .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({ "session_id": A_VALID_ID })).unwrap(),
        ))
        .unwrap();
    let resp = h.send(request).await;
    expect_error(&resp, StatusCode::UNAUTHORIZED, codes::UNAUTHENTICATED);
}

#[tokio::test]
async fn a_valid_session_can_be_logged_out() {
    let h = Harness::new();
    let grant = h.account(Some("203.0.113.5"), "erin").await;
    let token = grant["access_token"].as_str().unwrap();
    let session = grant["session_id"].as_str().unwrap();
    let resp = h.send(logout_req(Some(token), session)).await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT, "body={}", resp.text());
    assert!(resp.bytes.is_empty(), "204 has no body");
}

#[tokio::test]
async fn an_expired_access_token_cannot_log_out() {
    let h = Harness::new();
    let grant = h.account(Some("203.0.113.6"), "frank").await;
    let token = grant["access_token"].as_str().unwrap().to_string();
    let session = grant["session_id"].as_str().unwrap().to_string();
    h.advance(ACCESS_TTL_MS + 1_000);
    let resp = h.send(logout_req(Some(&token), &session)).await;
    expect_error(&resp, StatusCode::UNAUTHORIZED, codes::TOKEN_EXPIRED);
}

#[tokio::test]
async fn logging_out_a_foreign_session_is_not_found_not_forbidden() {
    // The enumeration guard: a session that belongs to someone else answers NOT_FOUND, not
    // PERMISSION_DENIED, so the response does not confirm the session id names anything real.
    let h = Harness::new();
    let attacker = h.account(Some("203.0.113.7"), "grace").await;
    let victim = h.account(Some("198.51.100.7"), "heidi").await;
    let token = attacker["access_token"].as_str().unwrap();
    let victim_session = victim["session_id"].as_str().unwrap();
    let resp = h.send(logout_req(Some(token), victim_session)).await;
    expect_error(&resp, StatusCode::NOT_FOUND, codes::NOT_FOUND);
}

#[tokio::test]
async fn logging_out_an_unknown_session_is_not_found() {
    let h = Harness::new();
    let grant = h.account(Some("203.0.113.8"), "ivan").await;
    let token = grant["access_token"].as_str().unwrap();
    let resp = h.send(logout_req(Some(token), A_VALID_ID)).await;
    expect_error(&resp, StatusCode::NOT_FOUND, codes::NOT_FOUND);
}

// --- refresh authn (invariant 1) ----------------------------------------------------------

#[tokio::test]
async fn a_refresh_token_rotates_the_session() {
    let h = Harness::new();
    let grant = h.account(Some("203.0.113.9"), "judy").await;
    let refresh = json!({
        "refresh_token": grant["refresh_token"].as_str().unwrap(),
        "device_id": grant["device_id"].as_str().unwrap(),
    });
    let resp = h
        .send(post_json("/v1/auth/refresh", Some("192.0.2.9"), &refresh))
        .await;
    assert_eq!(resp.status, StatusCode::OK, "body={}", resp.text());
    assert_eq!(resp.json()["account_id"], grant["account_id"]);
    assert!(resp.json()["access_token"]
        .as_str()
        .is_some_and(|t| !t.is_empty()));
}

#[tokio::test]
async fn a_refresh_from_another_device_is_a_device_mismatch() {
    let h = Harness::new();
    let grant = h.account(Some("203.0.113.11"), "mallory").await;
    let refresh = json!({
        "refresh_token": grant["refresh_token"].as_str().unwrap(),
        "device_id": A_VALID_ID,
    });
    let resp = h
        .send(post_json("/v1/auth/refresh", Some("192.0.2.11"), &refresh))
        .await;
    expect_error(&resp, StatusCode::UNAUTHORIZED, codes::DEVICE_MISMATCH);
}

#[tokio::test]
async fn a_garbage_refresh_token_is_refused_as_invalid() {
    let h = Harness::new();
    let refresh = json!({
        "refresh_token": "this-is-not-a-real-refresh-token",
        "device_id": A_VALID_ID,
    });
    let resp = h
        .send(post_json("/v1/auth/refresh", Some("192.0.2.12"), &refresh))
        .await;
    expect_error(&resp, StatusCode::UNAUTHORIZED, codes::TOKEN_INVALID);
}

// --- the error funnel: only the public face crosses (invariant 2) -------------------------

/// The fake secret a forced internal fault carries, to prove none of its shapes escape.
const LEAK_BAIT: &str =
    "connection refused to 10.9.8.7:5432; password auth failed for user 'migo_prod'; \
     at /var/lib/migo/store.rs:441 running SELECT * FROM accounts";

#[tokio::test]
async fn an_internal_fault_discloses_nothing_in_the_body() {
    let h = Harness::with_failing_edge(LEAK_BAIT);
    let body = json!({
        "username": "nadia",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Device" },
    });
    let resp = h
        .send(post_json("/v1/auth/register", Some("203.0.113.20"), &body))
        .await;
    assert_eq!(resp.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(resp.error_symbol(), "INTERNAL_ERROR");
    assert_eq!(
        resp.error_message(),
        "",
        "a server fault has no public message"
    );
    let text = resp.text();
    for needle in [
        "10.9.8.7",
        "5432",
        "password",
        "migo_prod",
        "/var/lib",
        "store.rs",
        "SELECT",
        "accounts",
    ] {
        assert!(!text.contains(needle), "leaked {needle:?} in body: {text}");
    }
    assert_no_internal_shape(text);
}

#[tokio::test]
async fn an_internal_fault_discloses_nothing_in_a_header() {
    let h = Harness::with_failing_edge(LEAK_BAIT);
    let body = json!({
        "username": "oscar",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Device" },
    });
    let resp = h
        .send(post_json("/v1/auth/register", Some("203.0.113.21"), &body))
        .await;
    let headers = headers_joined(&resp.headers);
    for needle in ["10.9.8.7", "5432", "migo_prod", "store.rs", "SELECT"] {
        assert!(
            !headers.contains(needle),
            "leaked {needle:?} in headers: {headers}"
        );
    }
}

#[tokio::test]
async fn a_wrong_password_and_an_unknown_user_are_byte_identical() {
    // The credential oracle stays closed: the response is the same, to the byte, whether the
    // account is missing or the password is wrong. Both are sent addressless so neither is
    // masked by a rate-limit refusal.
    let h = Harness::new();
    h.account(Some("203.0.113.30"), "peggy").await;

    let wrong_password = json!({
        "identifier": "peggy",
        "password": "definitely not the password",
        "device": { "display_name": "Device" },
    });
    let unknown_user = json!({
        "identifier": "nobody_here",
        "password": "definitely not the password",
        "device": { "display_name": "Device" },
    });
    let a = h
        .send(post_json("/v1/auth/login", None, &wrong_password))
        .await;
    let b = h
        .send(post_json("/v1/auth/login", None, &unknown_user))
        .await;

    assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    assert_eq!(a.status, b.status, "status must not distinguish the two");
    assert_eq!(a.bytes, b.bytes, "the response body must be byte-identical");
}

#[tokio::test]
async fn an_invalid_credential_carries_only_the_public_sentence() {
    let h = Harness::new();
    h.account(Some("203.0.113.31"), "quinn").await;
    let wrong = json!({
        "identifier": "quinn",
        "password": "wrong",
        "device": { "display_name": "Device" },
    });
    let resp = h.send(post_json("/v1/auth/login", None, &wrong)).await;
    expect_error(&resp, StatusCode::UNAUTHORIZED, codes::INVALID_CREDENTIALS);
    assert_eq!(resp.error_message(), "Username or password is incorrect");
    assert!(
        !resp.error_message().to_lowercase().contains("not found"),
        "the message must not hint that the account is missing"
    );
}

// --- input validation at the boundaries (invariant 3) -------------------------------------

#[tokio::test]
async fn a_reserved_username_is_refused_with_its_own_code() {
    let h = Harness::new();
    let resp = h
        .register(Some("203.0.113.40"), "admin", GOOD_PASSWORD)
        .await;
    expect_error(&resp, StatusCode::BAD_REQUEST, codes::USERNAME_RESERVED);
}

#[tokio::test]
async fn a_username_at_the_length_limit_is_accepted() {
    let h = Harness::new();
    let name = format!("a{}", "b".repeat(31)); // 32 chars, the documented maximum
    let resp = h.register(Some("203.0.113.41"), &name, GOOD_PASSWORD).await;
    assert_eq!(resp.status, StatusCode::CREATED, "body={}", resp.text());
}

#[tokio::test]
async fn a_username_past_the_length_limit_is_refused() {
    let h = Harness::new();
    let name = format!("a{}", "b".repeat(32)); // 33 chars, one past the maximum
    let resp = h.register(Some("203.0.113.42"), &name, GOOD_PASSWORD).await;
    expect_error(&resp, StatusCode::BAD_REQUEST, codes::FIELD_TOO_LONG);
}

#[tokio::test]
async fn a_password_at_the_floor_is_accepted() {
    let h = Harness::new();
    // Ten characters, not on the common list, and not derived from the username.
    let resp = h
        .register(Some("203.0.113.43"), "rupert", "kryptonite")
        .await;
    assert_eq!(resp.status, StatusCode::CREATED, "body={}", resp.text());
}

#[tokio::test]
async fn a_password_below_the_floor_is_refused_as_weak() {
    let h = Harness::new();
    let resp = h.register(Some("203.0.113.44"), "sybil", "kryptonit").await; // nine chars
    expect_error(&resp, StatusCode::BAD_REQUEST, codes::WEAK_PASSWORD);
}

#[tokio::test]
async fn registration_disabled_is_a_feature_disabled_refusal() {
    let h = Harness::with(|config| config.auth.allow_registration = false);
    let resp = h
        .register(Some("203.0.113.45"), "trent", GOOD_PASSWORD)
        .await;
    expect_error(
        &resp,
        StatusCode::SERVICE_UNAVAILABLE,
        codes::FEATURE_DISABLED,
    );
}

#[tokio::test]
async fn config_reports_registration_disabled() {
    let h = Harness::with(|config| config.auth.allow_registration = false);
    let resp = h.send(get("/v1/config")).await;
    assert_eq!(resp.json()["limits"]["allow_registration"], false);
}

// --- absurd input is refused, not tolerated (invariant 3) ---------------------------------

#[tokio::test]
async fn a_non_id_where_an_id_is_required_is_a_client_error_not_a_fault() {
    let h = Harness::new();
    let refresh = json!({ "refresh_token": "x", "device_id": "not-a-valid-id" });
    let resp = h
        .send(post_json(
            "/v1/auth/refresh",
            Some("203.0.113.50"),
            &refresh,
        ))
        .await;
    assert!(resp.status.is_client_error(), "status was {}", resp.status);
    assert_ne!(resp.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_no_internal_shape(resp.text());
}

#[tokio::test]
async fn a_body_of_the_wrong_json_type_is_a_client_error() {
    let h = Harness::new();
    let resp = h
        .send(post_json(
            "/v1/auth/register",
            Some("203.0.113.51"),
            &json!([1, 2, 3]),
        ))
        .await;
    assert!(resp.status.is_client_error(), "status was {}", resp.status);
    assert_ne!(resp.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_no_internal_shape(resp.text());
}

#[tokio::test]
async fn a_body_that_is_not_json_is_a_client_error() {
    let h = Harness::new();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", "203.0.113.52")
        .body(Body::from("this is definitely not json {{{"))
        .unwrap();
    let resp = h.send(request).await;
    assert!(resp.status.is_client_error(), "status was {}", resp.status);
    assert_ne!(resp.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_no_internal_shape(resp.text());
}

#[tokio::test]
async fn a_wrong_content_type_is_unsupported_media_type() {
    let h = Harness::new();
    let body = json!({
        "username": "ursula",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "Device" },
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "text/plain")
        .header("x-forwarded-for", "203.0.113.53")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = h.send(request).await;
    assert_eq!(resp.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn an_oversize_body_is_rejected_before_the_handler() {
    let h = Harness::with(|config| config.http.max_body_bytes = 256);
    let body = json!({
        "username": "victor",
        "password": GOOD_PASSWORD,
        "device": { "display_name": "x".repeat(5_000) },
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(bytes.len() > 256);
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, bytes.len())
        .header("x-forwarded-for", "203.0.113.54")
        .body(Body::from(bytes))
        .unwrap();
    let resp = h.send(request).await;
    assert_eq!(resp.status, StatusCode::PAYLOAD_TOO_LARGE);
}

// --- pagination boundaries, the one paginated type this crate exposes (invariant 3) -------

#[test]
fn a_missing_page_limit_uses_the_default() {
    assert_eq!(PageParams::default().effective_limit(), DEFAULT_PAGE_SIZE);
}

#[test]
fn a_zero_page_limit_clamps_up_to_one() {
    let params = PageParams {
        cursor: None,
        limit: Some(0),
    };
    assert_eq!(params.effective_limit(), 1);
}

#[test]
fn an_absurd_page_limit_clamps_down_to_the_max() {
    let params = PageParams {
        cursor: None,
        limit: Some(1_000_000),
    };
    assert_eq!(params.effective_limit(), MAX_PAGE_SIZE);
}

#[test]
fn a_page_limit_at_the_max_is_kept() {
    let params = PageParams {
        cursor: None,
        limit: Some(MAX_PAGE_SIZE),
    };
    assert_eq!(params.effective_limit(), MAX_PAGE_SIZE);
}

#[test]
fn a_negative_page_limit_fails_to_deserialize() {
    let parsed = serde_json::from_value::<PageParams>(json!({ "limit": -5 }));
    assert!(parsed.is_err(), "a negative count is not a u32");
}

#[test]
fn an_overflowing_page_limit_fails_to_deserialize() {
    let parsed = serde_json::from_value::<PageParams>(json!({ "limit": 5_000_000_000_u64 }));
    assert!(parsed.is_err(), "a count past u32::MAX does not fit");
}

#[test]
fn a_page_omits_the_cursor_at_the_end_of_the_sequence() {
    let full = serde_json::to_value(Page::new(vec![1, 2], Some("more".to_string()))).unwrap();
    assert_eq!(full["next_cursor"], "more");
    let last = serde_json::to_value(Page::new(vec![3], None::<String>)).unwrap();
    assert!(
        last.get("next_cursor").is_none(),
        "an absent cursor is omitted"
    );
}

// --- rate limiting per route (invariant 4) ------------------------------------------------

#[tokio::test]
async fn a_second_registration_from_one_network_is_rate_limited() {
    let h = Harness::new();
    let first = h
        .register(Some("203.0.113.60"), "wendy", GOOD_PASSWORD)
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "body={}", first.text());
    // Same /24, a different username: the refusal is the rate limiter, not a name collision.
    let second = h
        .register(Some("203.0.113.61"), "xander", GOOD_PASSWORD)
        .await;
    expect_error(&second, StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED);
    assert!(
        second.json()["error"]["retry_after_ms"].as_u64().is_some(),
        "a rate-limit refusal advertises a delay"
    );
    assert!(
        second.headers.get(header::RETRY_AFTER).is_some(),
        "a rate-limit refusal sets the Retry-After header"
    );
}

#[tokio::test]
async fn a_registration_from_another_network_is_unaffected() {
    let h = Harness::new();
    h.register(Some("203.0.113.62"), "yolanda", GOOD_PASSWORD)
        .await;
    // A different /24 has its own budget.
    let other = h
        .register(Some("198.51.100.62"), "zack", GOOD_PASSWORD)
        .await;
    assert_eq!(other.status, StatusCode::CREATED, "body={}", other.text());
}

#[tokio::test]
async fn an_addressless_registration_is_never_rate_limited() {
    let h = Harness::new();
    let first = h.register(None, "amy", GOOD_PASSWORD).await;
    let second = h.register(None, "ben", GOOD_PASSWORD).await;
    assert_eq!(first.status, StatusCode::CREATED, "body={}", first.text());
    assert_eq!(second.status, StatusCode::CREATED, "body={}", second.text());
}

#[tokio::test]
async fn an_unauthenticated_logout_does_not_charge_the_limiter() {
    // The authenticate step runs before any charge, and logout has no edge charge at all, so a
    // rejected caller cannot spend a victim network's budget by hammering an authed route.
    let h = Harness::new();
    let request = build_req(
        Method::POST,
        "/v1/auth/logout",
        Some("203.0.113.63"),
        None,
        Some(&json!({ "session_id": A_VALID_ID })),
    );
    let before = h.peek_ip("203.0.113.63").await;
    let resp = h.send(request).await;
    expect_error(&resp, StatusCode::UNAUTHORIZED, codes::UNAUTHENTICATED);
    let after = h.peek_ip("203.0.113.63").await;
    assert_eq!(
        before, after,
        "an unauthenticated logout must not spend budget"
    );
}

#[tokio::test]
async fn one_networks_spending_leaves_another_networks_budget_intact() {
    let h = Harness::new();
    let pristine = h.peek_ip("9.9.9.9").await;
    h.register(Some("203.0.113.64"), "carla", GOOD_PASSWORD)
        .await;
    let victim = h.peek_ip("198.51.100.64").await;
    assert_eq!(
        victim, pristine,
        "a stranger's spend must not touch another network"
    );
}

// --- nothing sensitive is logged or metered (invariant 7) ---------------------------------

#[tokio::test]
async fn the_metrics_render_carries_no_identifiers_or_tokens() {
    let h = Harness::new();
    let one = h.account(Some("203.0.113.70"), "dahlia").await;
    let two = h.account(Some("198.51.100.70"), "ewan").await;
    // Touch a metered path too, so counters exist to render.
    h.send(get("/v1/config")).await;
    let render = h.registry.render();
    for grant in [&one, &two] {
        for field in [
            "account_id",
            "device_id",
            "session_id",
            "access_token",
            "refresh_token",
        ] {
            let value = grant[field].as_str().unwrap();
            assert!(
                !render.contains(value),
                "metrics leaked {field}: {value}\n{render}"
            );
        }
    }
}

#[tokio::test]
async fn no_response_header_carries_the_access_token() {
    let h = Harness::new();
    let grant = h.account(Some("203.0.113.71"), "fiona").await;
    // Re-run register to capture its response headers with a known token in the body.
    let resp = h
        .register(Some("198.51.100.72"), "gregor", GOOD_PASSWORD)
        .await;
    let token = resp.json()["access_token"].as_str().unwrap().to_string();
    let refresh = resp.json()["refresh_token"].as_str().unwrap().to_string();
    let headers = headers_joined(&resp.headers);
    assert!(
        !headers.contains(&token),
        "access token must not be in a header"
    );
    assert!(
        !headers.contains(&refresh),
        "refresh token must not be in a header"
    );
    // And the earlier account's tokens are nowhere in this response either.
    let earlier = grant["access_token"].as_str().unwrap();
    assert!(!headers.contains(earlier));
}

#[tokio::test]
async fn no_response_echoes_the_full_caller_ip() {
    let h = Harness::new();
    let resp = h
        .register(Some("198.51.100.77"), "harriet", GOOD_PASSWORD)
        .await;
    let headers = headers_joined(&resp.headers);
    assert!(
        !headers.contains("198.51.100.77"),
        "the caller IP must not be echoed in a header"
    );
    assert!(
        !resp.text().contains("198.51.100.77"),
        "the caller IP must not be echoed in the body"
    );
}

#[tokio::test]
async fn the_config_document_does_not_echo_the_caller_ip() {
    let h = Harness::new();
    let request = build_req(Method::GET, "/v1/config", Some("198.51.100.78"), None, None);
    let resp = h.send(request).await;
    let headers = headers_joined(&resp.headers);
    assert!(!headers.contains("198.51.100.78"));
    assert!(!resp.text().contains("198.51.100.78"));
}

// --- method and route surface (invariant 8) -----------------------------------------------

#[tokio::test]
async fn an_unknown_path_is_not_found() {
    let h = Harness::new();
    let resp = h.send(get("/no/such/route")).await;
    assert_eq!(resp.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_get_on_the_login_route_is_method_not_allowed() {
    let h = Harness::new();
    let resp = h.send(get("/v1/auth/login")).await;
    assert_eq!(resp.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn a_post_on_the_health_route_is_method_not_allowed() {
    let h = Harness::new();
    let request = build_req(Method::POST, "/health", None, None, Some(&json!({})));
    let resp = h.send(request).await;
    assert_eq!(resp.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn a_post_on_the_config_route_is_method_not_allowed() {
    let h = Harness::new();
    let request = build_req(Method::POST, "/v1/config", None, None, Some(&json!({})));
    let resp = h.send(request).await;
    assert_eq!(resp.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn the_method_not_allowed_response_names_the_allowed_method() {
    let h = Harness::new();
    let resp = h.send(get("/v1/auth/login")).await;
    let allow = resp
        .headers
        .get(header::ALLOW)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(allow.contains("POST"), "Allow header was {allow:?}");
}

#[tokio::test]
async fn there_is_no_admin_route() {
    let h = Harness::new();
    for path in ["/admin", "/v1/admin", "/v1/admin/users"] {
        let resp = h.send(get(path)).await;
        assert_eq!(
            resp.status,
            StatusCode::NOT_FOUND,
            "{path} should not exist"
        );
    }
}

#[tokio::test]
async fn there_is_no_debug_or_reset_route() {
    let h = Harness::new();
    for path in ["/debug", "/metrics/reset", "/v1/debug", "/.env"] {
        let resp = h.send(get(path)).await;
        assert_eq!(
            resp.status,
            StatusCode::NOT_FOUND,
            "{path} should not exist"
        );
    }
}

// --- no media proxy surface (invariant 6) -------------------------------------------------

#[tokio::test]
async fn there_is_no_media_or_file_proxy_route_when_the_data_plane_is_remote() {
    // The default harness wires no byte store, which is the S3 posture: storage serves
    // its own bytes, and this process must not pretend to be an object store it is not.
    let h = Harness::new();
    for path in [
        "/media/x",
        "/v1/media/x",
        "/files/x",
        "/v1/files/x",
        "/blob/x",
    ] {
        let resp = h.send(get(path)).await;
        assert_eq!(
            resp.status,
            StatusCode::NOT_FOUND,
            "{path} should not exist"
        );
    }
}

// ---------------------------------------------------------------------------
// The media data plane
//
// The filesystem backend's URLs point at this very process, so the byte routes are the
// other half of section 168's split: control plane on the socket, bytes on HTTP. An
// in-memory MediaFiles stand-in keeps the test off the disk while exercising the routes'
// own rules — the size ceiling, the traversal guard, the sniff-served content type, and
// the refusal to serve what the scanner refuses.
// ---------------------------------------------------------------------------

/// An in-memory byte store: one key, one blob. The mutex is `parking_lot`'s, like every
/// other lock these tests hold, so `.lock()` is the value and not a `LockResult`.
struct MemoryFiles {
    objects: parking_lot::Mutex<std::collections::HashMap<String, bytes::Bytes>>,
}

#[async_trait::async_trait]
impl migo_api::MediaFiles for MemoryFiles {
    async fn write(&self, key: &str, bytes: bytes::Bytes) -> migo_core::Result<()> {
        self.objects.lock().insert(key.to_string(), bytes);
        Ok(())
    }

    async fn read(&self, key: &str) -> migo_core::Result<bytes::Bytes> {
        self.objects
            .lock()
            .get(key)
            .cloned()
            .ok_or_else(|| migo_protocol::fault::not_found("media object"))
    }
}

/// A harness whose data plane is local, over a recording byte store.
fn media_harness() -> (Harness, Arc<MemoryFiles>) {
    let files = Arc::new(MemoryFiles {
        objects: parking_lot::Mutex::new(std::collections::HashMap::new()),
    });
    (
        Harness::build(base_config(), None, Some(files.clone())),
        files,
    )
}

/// A local-data-plane harness whose upload ceiling is `ceiling` bytes.
fn media_harness_capped(ceiling: u64) -> (Harness, Arc<MemoryFiles>) {
    let mut config = base_config();
    config.media.max_upload_bytes = ceiling;
    let files = Arc::new(MemoryFiles {
        objects: parking_lot::Mutex::new(std::collections::HashMap::new()),
    });
    (Harness::build(config, None, Some(files.clone())), files)
}

/// A minimal PNG: the eight-byte magic is what the sniff serves `image/png` on.
fn png_bytes() -> bytes::Bytes {
    bytes::Bytes::from_static(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
}

#[tokio::test]
async fn a_media_object_round_trips_with_the_type_its_bytes_declare() {
    let (h, _files) = media_harness();

    let put = h
        .send(
            Request::builder()
                .method(Method::PUT)
                .uri("/media/c/2026/ab12.png")
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from(png_bytes()))
                .unwrap(),
        )
        .await;
    assert_eq!(put.status, StatusCode::NO_CONTENT, "the upload lands");

    let got = h.send(get("/media/c/2026/ab12.png")).await;
    assert_eq!(got.status, StatusCode::OK, "the download answers");
    let content_type = got
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(
        content_type, "image/png",
        "the type comes from the bytes, not the upload's claim"
    );
    assert_eq!(got.bytes, png_bytes().to_vec(), "the bytes round-trip");
}

#[tokio::test]
async fn a_key_that_tries_to_climb_out_of_the_media_root_is_refused() {
    let (h, files) = media_harness();
    for key in [
        "/media/../secret",
        "/media/a/../../etc/passwd",
        "/media//double",
        "/media/./dot",
    ] {
        let resp = h
            .send(
                Request::builder()
                    .method(Method::PUT)
                    .uri(key)
                    .body(Body::from(png_bytes()))
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status, StatusCode::NOT_FOUND, "{key} is refused");
    }
    assert!(
        files.objects.lock().is_empty(),
        "nothing was written while probing the guard"
    );
}

#[tokio::test]
async fn an_upload_over_the_ceiling_is_refused_before_it_is_stored() {
    // A one-kilobyte ceiling, and a body twice that.
    let (h, files) = media_harness_capped(1_024);
    let big = vec![0u8; 2_048];
    let resp = h
        .send(
            Request::builder()
                .method(Method::PUT)
                .uri("/media/too-big")
                .body(Body::from(big))
                .unwrap(),
        )
        .await;
    assert_eq!(resp.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        files.objects.lock().is_empty(),
        "the refused bytes were never stored"
    );
}

#[tokio::test]
async fn bytes_the_scanner_refuses_are_never_served() {
    let (h, files) = media_harness();
    files.objects.lock().insert(
        "html-ish".to_string(),
        bytes::Bytes::from_static(b"<html><body>x"),
    );
    let resp = h.send(get("/media/html-ish")).await;
    assert_eq!(
        resp.status,
        StatusCode::NOT_FOUND,
        "polyglot HTML answers as if it were not there"
    );
}

#[tokio::test]
async fn the_config_document_is_json_not_an_opaque_blob() {
    let h = Harness::new();
    let resp = h.send(get("/v1/config")).await;
    let content_type = resp
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("application/json"),
        "content-type was {content_type:?}"
    );
}

// --- CORS is an allow-list, never a wildcard (invariant 8 / middleware) --------------------

#[tokio::test]
async fn a_listed_origin_receives_a_cors_grant() {
    let h = Harness::new();
    let request = Request::builder()
        .method(Method::GET)
        .uri("/health")
        .header(header::ORIGIN, "http://localhost:19991")
        .body(Body::empty())
        .unwrap();
    let resp = h.send(request).await;
    let grant = resp
        .headers
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert_eq!(grant, "http://localhost:19991");
}

#[tokio::test]
async fn an_unlisted_origin_receives_no_cors_grant() {
    let h = Harness::new();
    let request = Request::builder()
        .method(Method::GET)
        .uri("/health")
        .header(header::ORIGIN, "http://evil.example")
        .body(Body::empty())
        .unwrap();
    let resp = h.send(request).await;
    let grant = resp
        .headers
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|v| v.to_str().ok());
    assert!(
        grant.is_none(),
        "an unlisted origin must get no grant, got {grant:?}"
    );
}

#[tokio::test]
async fn cors_never_answers_with_a_wildcard() {
    let h = Harness::new();
    for origin in ["http://localhost:19991", "http://evil.example"] {
        let request = Request::builder()
            .method(Method::GET)
            .uri("/health")
            .header(header::ORIGIN, origin)
            .body(Body::empty())
            .unwrap();
        let resp = h.send(request).await;
        let grant = resp
            .headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert_ne!(grant, "*", "CORS must never grant every origin");
    }
}

#[tokio::test]
async fn a_preflight_for_every_surface_verb_is_granted() {
    // A browser refuses to send a cross-origin PUT or DELETE until the
    // preflight grants that verb, so a method missing from the layer reads as
    // an opaque network failure on an otherwise correct route — exactly what
    // the admin page's appoint and revoke would be without PUT and DELETE.
    let h = Harness::new();
    for method in [Method::GET, Method::POST, Method::PUT, Method::DELETE] {
        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/v1/admins")
            .header(header::ORIGIN, "http://localhost:19991")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, method.as_str())
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization,content-type",
            )
            .body(Body::empty())
            .unwrap();
        let resp = h.send(request).await;
        let granted = resp
            .headers
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            granted.split(',').any(|m| m.trim() == method.as_str()),
            "a preflight for {method} must be granted, allow-methods was {granted:?}"
        );
    }
}

// --- idempotency: declared as an extractor, honoured by no route (invariant 9) ------------

#[tokio::test]
async fn an_idempotency_key_is_not_yet_honoured_on_replay() {
    // FINDING: `IdempotencyKey` is a public extractor and `idempotency-key` is in the CORS
    // allow-list, but no handler consumes it. A replayed register is therefore *not* folded
    // onto the first result — it performs the action again and collides. This test pins that
    // current behaviour; when a route honours the key it should return the first grant instead.
    let h = Harness::new();
    let first = build_req(
        Method::POST,
        "/v1/auth/register",
        Some("203.0.113.80"),
        None,
        Some(&json!({
            "username": "idem",
            "password": GOOD_PASSWORD,
            "device": { "display_name": "Device" },
        })),
    );
    let first = {
        let mut r = first;
        r.headers_mut().insert(
            axum::http::HeaderName::from_static("idempotency-key"),
            axum::http::HeaderValue::from_static("replay-1"),
        );
        h.send(r).await
    };
    assert_eq!(first.status, StatusCode::CREATED, "body={}", first.text());

    // Replay the same key and username from a different network (so the rate limiter does not
    // mask the outcome). A deduplicating implementation would return the first 201 grant.
    let replay = build_req(
        Method::POST,
        "/v1/auth/register",
        Some("198.51.100.80"),
        None,
        Some(&json!({
            "username": "idem",
            "password": GOOD_PASSWORD,
            "device": { "display_name": "Device" },
        })),
    );
    let replay = {
        let mut r = replay;
        r.headers_mut().insert(
            axum::http::HeaderName::from_static("idempotency-key"),
            axum::http::HeaderValue::from_static("replay-1"),
        );
        h.send(r).await
    };
    assert_eq!(
        replay.status,
        StatusCode::CONFLICT,
        "the replay is not deduplicated; it collides on the username. body={}",
        replay.text()
    );
    assert!(
        matches!(
            replay.error_symbol().as_str(),
            "USERNAME_TAKEN" | "ALREADY_EXISTS"
        ),
        "unexpected symbol {}",
        replay.error_symbol()
    );
}
