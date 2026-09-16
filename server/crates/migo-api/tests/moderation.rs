//! Integration tests for the moderation operator surface — `/v1/moderation/*`.
//!
//! # What is under test, and why the collaborators are real
//!
//! These tests drive the router the way `migod` mounts it, through `tower`'s `oneshot` rather than
//! a socket, against the real in-memory implementations the workspace ships for exactly this: a
//! `MemoryStore`, a `MemoryCache`, the real `CacheRateLimiter` over it, a fresh `Registry`, and a
//! `ManualClock` so the 5-minute freshness window is a line of test code instead of a sleep. The
//! authenticator and the warden are the real services; nothing about identity or about what
//! moderation does is faked, because every invariant below is about the seam between the HTTP edge
//! and the domain.
//!
//! The one double is the [`Roster`], and it is a double only in where it keeps its answers: it is
//! the thing that says who is staff, and a test needs to say that mid-session — an appointment made
//! after the account already holds a token is exactly the case the tests below need, and it is also
//! the case a real directory has.
//!
//! # The invariants these tests defend (brief sections 48, 49, 118, 145)
//!
//! 1. **The powers never come from the request.** What a caller may do is read from the directory
//!    on every call, so revoking a grant takes effect on the next request rather than at the next
//!    sign-in.
//! 2. **Not staff is refused, and refused as `PERMISSION_DENIED`.** An ordinary account learns it
//!    may not; it does not learn which power it was missing or that a freshness rule exists.
//! 3. **A ruling is an audit entry.** Resolving a case and acting on an account both write a row
//!    the audit route then reads back, with the operator's own words on it.
//! 4. **A stale session cannot rule.** Every act needs a recently proved factor, and the refusal is
//!    `REAUTHENTICATION_REQUIRED` — for staff. For everybody else the answer is the same as it
//!    always was.
//! 5. **The queue is what was filed.** Nothing is invented between the store and the JSON.

#![allow(clippy::items_after_statements)]

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use migo_api::{router, ApiServices};
use migo_auth::{Auth, SharedAuth};
use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Clock, Id, ManualClock, Result, Secret, SeededRandom, Timestamp};
use migo_moderation::{
    open, Caller, Filing, ModerationConfig, Powers, Reason, Roster, SharedRoster, SharedWarden,
    Subject, Warden,
};
use migo_protocol::{codes, ConversationKind, EncryptionMode, MessageKind, NodeInfo};
use migo_ratelimit::{CacheRateLimiter, Policies, SharedRateLimiter, TrustTier};
use migo_store::model::{report_status, Conversation, NewMessage};
use migo_store::traits::MessagingStore;
use migo_store::{MemoryStore, SharedStore};

// --- constants ----------------------------------------------------------------------------

/// The 32-byte signing key the auth service needs, base64 as configuration carries it. Shared with
/// `migo-auth`'s own tests so a token minted here would verify there too.
const TEST_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// A passphrase that clears the floor.
const GOOD_PASSPHRASE: &str = "sunflower gravel bicycle";

/// A fixed wall-clock instant so token issue and expiry are deterministic.
const NOW_MS: i64 = 1_800_000_000_000;

/// The anonymous bucket's price for one registration. The public-internet default is the whole
/// bucket — a registration is the expensive thing a spam operation needs thousands of — and the
/// endpoint surface is half the anonymous budget, so ten is the ceiling a single registration can
/// cost without being refused outright. This suite registers several accounts, so it pays a small
/// constant instead. The limiter is still present and still charged, and every registration below
/// comes from a distinct /24 so no two share a network bucket.
const REGISTRATION_COST: u32 = 4;

/// The node identity the surface reports at `/v1/config`.
const NODE_ID: &str = "migo-test-node";

/// A stable seed so ids are reproducible run to run.
const SEED: u64 = 0x5eed_1234;

/// One minute more than the auth service's freshness window, so advancing by this makes a session
/// that was fresh stale without expiring its access token (which lives fifteen minutes).
const STALE_MS: i64 = migo_auth::REAUTH_WINDOW_MS as i64 + 60_000;

// --- the staff directory double ------------------------------------------------------------

/// A directory whose grants a test writes as it goes.
///
/// The grant is applied to the *next* request, not the next sign-in, which is the whole point of
/// asking the directory every time: the test that appoints an account mid-session is testing the
/// revocability the real roster promises.
#[derive(Default)]
struct TestRoster {
    grants: RwLock<HashMap<Id, Powers>>,
}

impl TestRoster {
    fn new() -> Self {
        Self::default()
    }

    /// Grants one account a power set. A later grant replaces the earlier one.
    fn grant(&self, account_id: Id, powers: Powers) {
        self.grants
            .write()
            .expect("the test roster is never poisoned")
            .insert(account_id, powers);
    }
}

#[async_trait::async_trait]
impl Roster for TestRoster {
    async fn powers(&self, account_id: Id) -> Result<Powers> {
        Ok(self
            .grants
            .read()
            .expect("the test roster is never poisoned")
            .get(&account_id)
            .copied()
            .unwrap_or(Powers::NONE))
    }
}

// --- harness ------------------------------------------------------------------------------

/// The router under test plus the handles a test needs to reach behind it: advance the clock,
/// appoint staff, file a report the way the socket would.
struct Harness {
    app: Router,
    clock: Arc<ManualClock>,
    roster: Arc<TestRoster>,
    warden: SharedWarden,
    store: Arc<MemoryStore>,
}

impl Harness {
    fn new() -> Self {
        let mut config = Config::default();
        config.auth.token_key = Some(Secret::new(TEST_KEY));
        config.auth.registration_cost = Some(REGISTRATION_COST);

        let clock = Arc::new(ManualClock::new(Timestamp::from_unix_ms(NOW_MS)));
        let registry = Arc::new(Registry::new());
        let cache = Arc::new(MemoryCache::new());
        let policies = Policies::from_config(&config.rate_limit).expect("default policies valid");
        let limiter = Arc::new(CacheRateLimiter::new(cache, policies, &registry));
        let store = Arc::new(MemoryStore::new());

        let roster = Arc::new(TestRoster::new());
        // Coerced through a typed binding rather than at the argument: `open`'s first parameter is
        // already `Arc<dyn Store>`, and a bare `Arc::clone(&store)` in that position is checked
        // with the clone's own type parameter fixed to the trait object, which then refuses the
        // concrete `&Arc<MemoryStore>`.
        let shared_store: SharedStore = Arc::clone(&store);
        let warden = open(
            shared_store,
            Arc::clone(&limiter) as SharedRateLimiter,
            Arc::clone(&roster) as SharedRoster,
            Box::new(SeededRandom::new(SEED)),
            ModerationConfig::default(),
            &registry,
        );

        let auth = Auth::new(
            Arc::clone(&store),
            Arc::clone(&limiter),
            &config,
            &registry,
            Box::new(SeededRandom::new(SEED)),
        )
        .expect("auth service builds");
        let authenticator: SharedAuth = Arc::new(auth);

        let services = ApiServices {
            authenticator,
            rate_limiter: Arc::clone(&limiter) as SharedRateLimiter,
            clock: Arc::clone(&clock) as Arc<dyn Clock>,
            registry: Arc::clone(&registry),
            node: NodeInfo {
                node_id: NODE_ID.to_string(),
                region: "test-region".to_string(),
                country: "ID".to_string(),
            },
            features: 0b101,
            // This suite is about moderation, not the media data plane.
            media_files: None,
            warden: Arc::clone(&warden),
            // The route that reports what a caller may do reads the directory itself; it must be
            // the same directory the warden resolves through, or `whoami` would answer for a
            // different world than the one the next request is refused in.
            roster: Arc::clone(&roster) as SharedRoster,
            // The recovery surface is another suite's subject; this one never reaches it.
            recovery_delivery: None,
        };
        let app = router(&config, services);
        Self {
            app,
            clock,
            roster,
            warden,
            store,
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
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body collected")
            .to_vec();
        Resp { status, bytes }
    }

    /// Registers an account and returns its id and access token.
    ///
    /// The address is distinct per call and the registration price is small, so several accounts
    /// can be created in one test without colliding in the anonymous bucket.
    async fn account(&self, ip: &str, username: &str) -> (Id, String) {
        let body = json!({
            "username": username,
            "passphrase": GOOD_PASSPHRASE,
            "device": { "display_name": "Integration Test" },
        });
        let resp = self
            .send(build_req(
                Method::POST,
                "/v1/auth/register",
                Some(ip),
                None,
                Some(&body),
            ))
            .await;
        assert_eq!(
            resp.status,
            StatusCode::CREATED,
            "registration should succeed; body={}",
            resp.text()
        );
        let grant = resp.json();
        let account_id: Id =
            serde_json::from_value(grant["account_id"].clone()).expect("the grant names the id");
        let token = grant["access_token"]
            .as_str()
            .expect("the grant carries a token")
            .to_string();
        (account_id, token)
    }

    /// Appoints an account to a power set.
    fn appoint(&self, account_id: Id, powers: Powers) {
        self.roster.grant(account_id, powers);
    }

    /// Advances the shared clock.
    fn advance(&self, millis: i64) {
        self.clock.advance_millis(millis);
    }

    /// Files a report the way the socket gateway does, and returns the case id.
    ///
    /// The filing comes from the service rather than from an HTTP route because there is no reporting
    /// route: a report is filed by a client inside a conversation, over `REPORT_CREATE`, and this is
    /// that call. The queue these tests read is the queue those reports land in.
    async fn file(&self, reporter: Id, subject: Subject, reason: Reason) -> Id {
        let caller = Caller::new(
            reporter,
            Id::from_bytes([9u8; 16]),
            TrustTier::Established,
            self.clock.now(),
        );
        self.warden
            .file_report(&caller, Filing::new(subject, reason))
            .await
            .expect("a filed report lands")
            .report_id
    }

    /// Seeds a conversation carrying one message, written straight to the store.
    ///
    /// A takedown needs something to take down, and there is no route that sends a message — that is
    /// an opcode, and the gateway is another suite's subject. So the row is written the way the
    /// messaging layer would write it and the takedown then goes through the surface under test.
    async fn message(&self, conversation_id: Id, message_id: Id, sender: Id) {
        self.store
            .create_conversation(
                Conversation {
                    conversation_id,
                    kind: ConversationKind::Group,
                    encryption: EncryptionMode::Transport,
                    room_id: None,
                    // No home node: a conversation seeded for a test never leaves this process.
                    home_region: String::new(),
                    last_seq: 0,
                    created_by: sender,
                    created_at: self.clock.now(),
                    last_message_at: None,
                    archived_at: None,
                    title: None,
                },
                vec![sender],
            )
            .await
            .expect("a fresh conversation id is free");
        self.store
            .append_message(NewMessage {
                message_id,
                conversation_id,
                sender_id: sender,
                sender_device: Some(Id::from_bytes([9u8; 16])),
                kind: MessageKind::Text,
                envelope: vec![1, 2, 3, 4],
                reply_to: None,
                expires_at: None,
                created_at: self.clock.now(),
            })
            .await
            .expect("the first message appends");
    }
}

// --- request builders and response helpers ------------------------------------------------

/// A collected response.
struct Resp {
    status: StatusCode,
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

/// Builds a request, attaching an address, a bearer token, and a JSON body only when given.
fn build_req(
    method: Method,
    path: &str,
    ip: Option<&str>,
    bearer: Option<&str>,
    body: Option<&Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let mut request = match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(value).expect("body serialises"),
            ))
            .expect("request builds"),
        None => builder.body(Body::empty()).expect("request builds"),
    };
    if let Some(ip) = ip {
        let addr: SocketAddr = (ip.parse::<IpAddr>().expect("test ip parses"), 0).into();
        request.extensions_mut().insert(ConnectInfo(addr));
    }
    request
}

fn get(path: &str, bearer: &str) -> Request<Body> {
    build_req(Method::GET, path, None, Some(bearer), None)
}

fn post(path: &str, bearer: &str, body: &Value) -> Request<Body> {
    build_req(Method::POST, path, None, Some(bearer), Some(body))
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

// --- whoami --------------------------------------------------------------------------------

#[tokio::test]
async fn whoami_answers_an_ordinary_account_with_no_powers() {
    // The answer, not an error: a dashboard calls this before it renders, and an account with no
    // powers has to be told so rather than shown a page of buttons that refuse.
    let h = Harness::new();
    let (account_id, token) = h.account("203.0.113.1", "alice").await;
    let resp = h.send(get("/v1/moderation/whoami", &token)).await;
    assert_eq!(resp.status, StatusCode::OK, "body={}", resp.text());
    let body = resp.json();
    assert_eq!(body["bits"], 0);
    assert_eq!(body["staff"], false);
    assert_eq!(body["powers"].as_array().expect("a list").len(), 0);
    assert_eq!(
        serde_json::from_value::<Id>(body["account_id"].clone()).expect("an id"),
        account_id
    );
}

#[tokio::test]
async fn whoami_answers_a_staff_account_with_its_powers() {
    let h = Harness::new();
    let (account_id, token) = h.account("203.0.113.2", "bob").await;
    h.appoint(account_id, Powers::TRIAGE.with(Powers::TAKEDOWN));

    let resp = h.send(get("/v1/moderation/whoami", &token)).await;
    assert_eq!(resp.status, StatusCode::OK, "body={}", resp.text());
    let body = resp.json();
    assert_eq!(body["staff"], true);
    assert_eq!(body["powers"], json!(["triage", "takedown"]));
    assert_eq!(body["bits"], 3);
}

#[tokio::test]
async fn whoami_without_a_token_is_unauthenticated() {
    let h = Harness::new();
    let resp = h
        .send(build_req(
            Method::GET,
            "/v1/moderation/whoami",
            None,
            None,
            None,
        ))
        .await;
    expect_error(&resp, StatusCode::UNAUTHORIZED, codes::UNAUTHENTICATED);
}

// --- the queue -----------------------------------------------------------------------------

#[tokio::test]
async fn the_queue_refuses_an_ordinary_account() {
    let h = Harness::new();
    let (_account_id, token) = h.account("203.0.113.3", "carol").await;
    let resp = h.send(get("/v1/moderation/queue", &token)).await;
    expect_error(&resp, StatusCode::FORBIDDEN, codes::PERMISSION_DENIED);
}

#[tokio::test]
async fn the_queue_lists_what_was_filed_longest_waiting_first() {
    let h = Harness::new();
    let (reporter, token) = h.account("203.0.113.4", "dave").await;
    let (first_subject, _) = h.account("203.0.113.5", "erin").await;
    let (second_subject, _) = h.account("203.0.113.29", "finn").await;
    h.appoint(reporter, Powers::TRIAGE);

    let first = h
        .file(reporter, Subject::User(first_subject), Reason::Spam)
        .await;
    // A second passes, so the two reports are distinguishable by the field the queue is ordered
    // on. Filing both at one instant would leave the order to the ids, which is not what this test
    // is about.
    h.advance(1_000);
    let second = h
        .file(reporter, Subject::User(second_subject), Reason::Harassment)
        .await;
    assert_ne!(first, second, "two subjects are two reports");

    let resp = h.send(get("/v1/moderation/queue", &token)).await;
    assert_eq!(resp.status, StatusCode::OK, "body={}", resp.text());
    let cases = resp.json()["cases"].as_array().expect("a list").clone();
    assert_eq!(2, cases.len(), "both reports are in the queue");
    // Oldest first: the one that has waited longest is the one most likely to matter.
    assert_eq!(
        serde_json::from_value::<Id>(cases[0]["report_id"].clone()).expect("an id"),
        first
    );
    assert_eq!(
        serde_json::from_value::<Id>(cases[1]["report_id"].clone()).expect("an id"),
        second
    );
    assert_eq!(cases[0]["reason_name"], "spam");
    assert_eq!(cases[1]["reason_name"], "harassment");
    assert_eq!(cases[0]["subject_kind_name"], "user");
    assert_eq!(cases[0]["open"], true);
    assert_eq!(cases[0]["status"], report_status::OPEN);
}

#[tokio::test]
async fn a_queue_row_states_the_reason_it_was_filed_under_and_no_words_it_was_not() {
    // The reporter's note travels, and it is the *only* free text on the row: a queue that quoted
    // the message would be rendering content on a screen with no mute, and the reason a report is a
    // pointer is the same reason this is short.
    let h = Harness::new();
    let (reporter, token) = h.account("203.0.113.6", "frank").await;
    let (subject, _subject_token) = h.account("203.0.113.7", "grace").await;
    h.appoint(reporter, Powers::TRIAGE);
    h.file(reporter, Subject::User(subject), Reason::Impersonation)
        .await;

    let resp = h.send(get("/v1/moderation/queue", &token)).await;
    let cases = resp.json()["cases"].as_array().expect("a list").clone();
    assert_eq!(cases[0]["note"], Value::Null);
    assert_eq!(cases[0]["resolution"], Value::Null);
    assert_eq!(cases[0]["evidence_ref"], Value::Null);
    assert_eq!(cases[0]["reason"], 9, "impersonation is code 9");
}

// --- one case ------------------------------------------------------------------------------

#[tokio::test]
async fn one_case_answers_the_report_it_names() {
    let h = Harness::new();
    let (reporter, token) = h.account("203.0.113.8", "heidi").await;
    let (subject, _subject_token) = h.account("203.0.113.9", "ivan").await;
    h.appoint(reporter, Powers::TRIAGE);
    let report_id = h.file(reporter, Subject::User(subject), Reason::Scam).await;

    let resp = h
        .send(get(&format!("/v1/moderation/case/{report_id}"), &token))
        .await;
    assert_eq!(resp.status, StatusCode::OK, "body={}", resp.text());
    let body = resp.json();
    assert_eq!(
        serde_json::from_value::<Id>(body["report_id"].clone()).expect("an id"),
        report_id
    );
    assert_eq!(
        serde_json::from_value::<Id>(body["subject_id"].clone()).expect("an id"),
        subject
    );
    assert_eq!(body["reason_name"], "scam");
    assert_eq!(body["open"], true);
}

#[tokio::test]
async fn an_unknown_report_is_not_found_rather_than_forbidden() {
    // Brief section 48: `NOT_FOUND` wherever the existence of an object is itself the secret. A
    // report about somebody is exactly that, so a caller who may read the queue learns nothing from
    // an id it guessed wrong.
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.10", "judy").await;
    h.appoint(staff, Powers::TRIAGE);

    let unknown = Id::from_bytes([0xAB; 16]);
    let resp = h
        .send(get(&format!("/v1/moderation/case/{unknown}"), &token))
        .await;
    expect_error(&resp, StatusCode::NOT_FOUND, codes::NOT_FOUND);
}

// --- resolving -----------------------------------------------------------------------------

#[tokio::test]
async fn resolving_a_case_closes_it_and_records_who_ruled() {
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.11", "kim").await;
    let (subject, _subject_token) = h.account("203.0.113.12", "liam").await;
    h.appoint(staff, Powers::TRIAGE);
    let report_id = h
        .file(staff, Subject::User(subject), Reason::Harassment)
        .await;

    let resp = h
        .send(post(
            &format!("/v1/moderation/case/{report_id}/resolve"),
            &token,
            &json!({ "resolution": 1, "reason": "warned, first offence" }),
        ))
        .await;
    assert_eq!(resp.status, StatusCode::OK, "body={}", resp.text());
    let body = resp.json();
    assert_eq!(body["open"], false);
    assert_eq!(body["resolution"], 1);
    assert_eq!(body["resolution_name"], "warned");
    assert_eq!(
        serde_json::from_value::<Id>(body["resolved_by"].clone()).expect("an id"),
        staff
    );

    // The ruling is an audit entry, and the audit route is where an operator reads it back.
    h.appoint(staff, Powers::TRIAGE.with(Powers::AUDIT));
    let trail = h
        .send(get(
            &format!("/v1/moderation/audit/report/{report_id}"),
            &token,
        ))
        .await;
    assert_eq!(trail.status, StatusCode::OK, "body={}", trail.text());
    let entries = trail.json()["entries"].as_array().expect("a list").clone();
    assert_eq!(1, entries.len());
    assert_eq!(entries[0]["action"], "moderation.report.resolve");
    assert_eq!(entries[0]["target_kind_name"], "report");
    assert_eq!(entries[0]["actor_kind_name"], "operator");
    assert_eq!(entries[0]["reason"], "warned, first offence");
}

#[tokio::test]
async fn a_second_ruling_on_the_same_case_is_a_conflict() {
    // Two moderators opening the same report is normal; both of them deciding it is not, and the
    // second one is told rather than having their verdict silently overwrite the first.
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.13", "mona").await;
    let (subject, _subject_token) = h.account("203.0.113.30", "nate").await;
    h.appoint(staff, Powers::TRIAGE);
    let report_id = h.file(staff, Subject::User(subject), Reason::Other).await;
    let path = format!("/v1/moderation/case/{report_id}/resolve");
    let body = json!({ "resolution": 0 });

    let first = h.send(post(&path, &token, &body)).await;
    assert_eq!(first.status, StatusCode::OK, "body={}", first.text());
    let second = h.send(post(&path, &token, &body)).await;
    expect_error(&second, StatusCode::CONFLICT, codes::CONFLICT);
}

#[tokio::test]
async fn a_stale_session_cannot_rule() {
    // The freshness window is five minutes and the access token lives fifteen, so a moderator who
    // signed in on Monday morning and left the tab open can read the queue and cannot close a case
    // until they prove a factor again.
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.14", "nina").await;
    let (subject, _subject_token) = h.account("203.0.113.31", "olive").await;
    h.appoint(staff, Powers::TRIAGE);
    let report_id = h.file(staff, Subject::User(subject), Reason::Spam).await;

    h.advance(STALE_MS);
    let resp = h
        .send(post(
            &format!("/v1/moderation/case/{report_id}/resolve"),
            &token,
            &json!({ "resolution": 0 }),
        ))
        .await;
    expect_error(
        &resp,
        StatusCode::UNAUTHORIZED,
        codes::REAUTHENTICATION_REQUIRED,
    );

    // Reading is not acting, and stays open to the same stale session.
    let queue = h.send(get("/v1/moderation/queue", &token)).await;
    assert_eq!(queue.status, StatusCode::OK, "body={}", queue.text());
}

#[tokio::test]
async fn an_ordinary_stale_account_is_refused_for_being_ordinary() {
    // The ordering invariant, and the reason this route asks about freshness instead of demanding
    // it: a caller who is not staff must not learn that a freshness rule exists. The answer is the
    // one they would have got a second after signing in.
    let h = Harness::new();
    let (_account_id, token) = h.account("203.0.113.15", "omar").await;
    h.advance(STALE_MS);
    let resp = h
        .send(post(
            "/v1/moderation/act",
            &token,
            &json!({ "action": "warn", "account_id": Id::from_bytes([1u8; 16]) }),
        ))
        .await;
    expect_error(&resp, StatusCode::FORBIDDEN, codes::PERMISSION_DENIED);
}

// --- acting --------------------------------------------------------------------------------

#[tokio::test]
async fn suspending_an_account_records_it_and_names_the_action_it_took() {
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.16", "pia").await;
    let (target, _target_token) = h.account("203.0.113.17", "quinn").await;
    h.appoint(staff, Powers::ALL);

    let resp = h
        .send(post(
            "/v1/moderation/act",
            &token,
            &json!({
                "action": "suspend",
                "account_id": target,
                "reason": "spam at scale",
            }),
        ))
        .await;
    assert_eq!(resp.status, StatusCode::OK, "body={}", resp.text());
    let body = resp.json();
    // The name the audit row now carries, echoed so a dashboard need not keep its own mapping.
    assert_eq!(body["action"], "moderation.account.suspend");
    assert_eq!(body["notice"]["outcome"], "suspended");
    assert_eq!(body["notice"]["reason"], Value::Null);
    assert_eq!(
        serde_json::from_value::<Id>(body["notice"]["audience"].clone()).expect("an id"),
        target
    );

    let trail = h
        .send(get(
            &format!("/v1/moderation/audit/account/{target}"),
            &token,
        ))
        .await;
    assert_eq!(trail.status, StatusCode::OK, "body={}", trail.text());
    let entries = trail.json()["entries"].as_array().expect("a list").clone();
    assert_eq!(entries[0]["action"], "moderation.account.suspend");
    assert_eq!(entries[0]["reason"], "spam at scale");
    assert_eq!(entries[0]["actor_kind_name"], "operator");
}

#[tokio::test]
async fn a_triage_moderator_may_warn_and_may_not_suspend() {
    // The split the roster makes: reading the queue and warning ride together, closing an account
    // does not. The refusal names no power, which is the same rule the queue's own refusal follows.
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.18", "rosa").await;
    let (target, _target_token) = h.account("203.0.113.19", "sam").await;
    h.appoint(staff, Powers::TRIAGE);

    let warn = h
        .send(post(
            "/v1/moderation/act",
            &token,
            &json!({ "action": "warn", "account_id": target }),
        ))
        .await;
    assert_eq!(warn.status, StatusCode::OK, "body={}", warn.text());
    assert_eq!(warn.json()["action"], "moderation.account.warn");

    let suspend = h
        .send(post(
            "/v1/moderation/act",
            &token,
            &json!({ "action": "suspend", "account_id": target }),
        ))
        .await;
    expect_error(&suspend, StatusCode::FORBIDDEN, codes::PERMISSION_DENIED);
}

#[tokio::test]
async fn a_content_takedown_delivers_no_notice_and_that_is_the_answer() {
    // `None` for every content takedown is a fact about what the service knows — it does not know
    // who wrote the message — rather than a decision about what a user deserves. The route reports
    // it as null rather than inventing an audience, and the takedown itself still goes through.
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.20", "tara").await;
    h.appoint(staff, Powers::ALL);
    let conversation_id = Id::from_bytes([2u8; 16]);
    let message_id = Id::from_bytes([3u8; 16]);
    h.message(conversation_id, message_id, staff).await;

    let resp = h
        .send(post(
            "/v1/moderation/act",
            &token,
            &json!({
                "action": "remove_message",
                "conversation_id": conversation_id,
                "message_id": message_id,
            }),
        ))
        .await;
    assert_eq!(resp.status, StatusCode::OK, "body={}", resp.text());
    let body = resp.json();
    assert_eq!(body["action"], "moderation.message.remove");
    assert_eq!(body["notice"], Value::Null);

    // The trail names the message, which is the target the action itself declared — not the
    // conversation it happened to live in.
    let trail = h
        .send(get(
            &format!("/v1/moderation/audit/message/{message_id}"),
            &token,
        ))
        .await;
    let entries = trail.json()["entries"].as_array().expect("a list").clone();
    assert_eq!(1, entries.len(), "body={}", trail.text());
    assert_eq!(entries[0]["action"], "moderation.message.remove");
}

#[tokio::test]
async fn a_takedown_of_something_that_is_not_there_is_not_found() {
    // The message is gone or was never there, and the answer is the same either way: the action did
    // not happen, so no audit row claims it did.
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.28", "vito").await;
    h.appoint(staff, Powers::ALL);

    let resp = h
        .send(post(
            "/v1/moderation/act",
            &token,
            &json!({
                "action": "remove_message",
                "conversation_id": Id::from_bytes([2u8; 16]),
                "message_id": Id::from_bytes([3u8; 16]),
            }),
        ))
        .await;
    expect_error(&resp, StatusCode::NOT_FOUND, codes::NOT_FOUND);
}

#[tokio::test]
async fn an_unknown_action_word_is_refused() {
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.21", "uma").await;
    h.appoint(staff, Powers::ALL);

    let resp = h
        .send(post(
            "/v1/moderation/act",
            &token,
            &json!({ "action": "banish", "account_id": Id::from_bytes([4u8; 16]) }),
        ))
        .await;
    assert!(
        resp.status.is_client_error(),
        "an unknown action is a client error; status was {}",
        resp.status
    );
    assert_ne!(resp.status, StatusCode::INTERNAL_SERVER_ERROR);
}

// --- the audit route -----------------------------------------------------------------------

#[tokio::test]
async fn the_audit_route_needs_the_audit_power() {
    // AUDIT is separate from the acting powers on purpose: somebody who reviews what moderators did
    // should not need the ability to do it themselves. The converse is the part worth pinning — the
    // power to act is not the power to read the trail.
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.22", "vera").await;
    let (target, _target_token) = h.account("203.0.113.23", "walt").await;
    h.appoint(
        staff,
        Powers::TRIAGE.with(Powers::TAKEDOWN).with(Powers::SUSPEND),
    );

    let resp = h
        .send(get(
            &format!("/v1/moderation/audit/account/{target}"),
            &token,
        ))
        .await;
    expect_error(&resp, StatusCode::FORBIDDEN, codes::PERMISSION_DENIED);
}

#[tokio::test]
async fn an_auditor_who_may_not_act_can_still_read_the_trail() {
    let h = Harness::new();
    let (auditor, token) = h.account("203.0.113.24", "xena").await;
    let (target, _target_token) = h.account("203.0.113.25", "yuri").await;
    h.appoint(auditor, Powers::AUDIT);

    let trail = h
        .send(get(
            &format!("/v1/moderation/audit/account/{target}"),
            &token,
        ))
        .await;
    assert_eq!(trail.status, StatusCode::OK, "body={}", trail.text());
    let entries = trail.json()["entries"].as_array().expect("a list").clone();
    // An account nobody has acted against still has a history: registering is itself an entry, and
    // the trail is the store's own rather than a moderation-only view of it. A dashboard showing
    // this account's warnings shows the whole record and lets the reader decide what matters.
    assert_eq!(1, entries.len(), "body={}", trail.text());
    assert_eq!(entries[0]["action"], "account.register");
    assert_eq!(entries[0]["actor_kind_name"], "user");

    let act = h
        .send(post(
            "/v1/moderation/act",
            &token,
            &json!({ "action": "warn", "account_id": target }),
        ))
        .await;
    expect_error(&act, StatusCode::FORBIDDEN, codes::PERMISSION_DENIED);
}

#[tokio::test]
async fn an_unknown_audit_target_kind_is_a_validation_error() {
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.26", "zoe").await;
    h.appoint(staff, Powers::ALL);

    let resp = h
        .send(get(
            &format!(
                "/v1/moderation/audit/sasquatch/{}",
                Id::from_bytes([5u8; 16])
            ),
            &token,
        ))
        .await;
    expect_error(&resp, StatusCode::BAD_REQUEST, codes::VALIDATION_FAILED);
}

#[tokio::test]
async fn every_audit_kind_the_table_names_is_a_route_this_build_answers() {
    // The path vocabulary and the JSON vocabulary come from one table, so a kind that can be
    // *reported* must be a kind that can be *asked about*. A name that only went one way would
    // leave a dashboard unable to follow up on a row it had just been shown.
    let h = Harness::new();
    let (staff, token) = h.account("203.0.113.27", "ada").await;
    h.appoint(staff, Powers::ALL);
    let target = Id::from_bytes([6u8; 16]);

    for kind in [
        "account",
        "device",
        "session",
        "conversation",
        "message",
        "room",
        "room_member",
        "media",
        "report",
        "ledger_account",
        "transaction",
        "bot",
        "node",
        "identity_key",
        "wallet",
    ] {
        let resp = h
            .send(get(
                &format!("/v1/moderation/audit/{kind}/{target}"),
                &token,
            ))
            .await;
        assert_eq!(
            resp.status,
            StatusCode::OK,
            "kind {kind} should be readable; body={}",
            resp.text()
        );
    }
}
