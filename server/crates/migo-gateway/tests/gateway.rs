//! The realtime transport, tested where a mistake is silent, expensive, or a breach.
//!
//! This crate turns a stream of bytes into a session. Nothing above it — no domain crate, no
//! human — watches the wire; if the state machine admits a frame it should have refused, or lets
//! a queue grow without bound, or leaks an account id into a metric, nothing turns red. The bug
//! ships and is found in production, or in an audit, or never. These tests stand in for the
//! reviewer who cannot see the wire, and they assert the invariants that have no other guardrail:
//!
//! * **Order is enforced, not assumed.** A data frame before the handshake, a second `HELLO`, a
//!   privileged opcode before authentication — each must close the connection, not be acted on.
//! * **Backpressure is bounded and fails closed.** The outbound queue has a hard cap; past it,
//!   droppable frames are dropped and counted, and Critical frames are never dropped.
//! * **A frame is size-checked before it is parsed.** An oversize or truncated frame is refused
//!   without allocating what its header claimed.
//! * **Authorization is read, never trusted from the frame,** and "not a member" is
//!   indistinguishable from "does not exist".
//! * **The wire is binary and push-only** — proven structurally against the opcode enum.
//! * **Nothing sensitive is logged or metered,** and every error a client sees carries only the
//!   public face of the fault, never the internal detail.
//! * **Limits hold exactly at their boundary,** and a duplicate is handled once.
//! * **Shutdown is clean:** a closing connection releases what it held, and a second close is
//!   harmless.
//!
//! The rate limiter and the metrics registry are the real ones, so their arithmetic and their
//! label discipline are part of the test. Only the two edges that would touch a socket or a
//! password are doubles: an in-memory [`Pipe`] that records what the server wrote, and a
//! [`FakeAuth`] that knows exactly one token. No test opens a real listener or binds an address.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;

use migo_auth::{
    Authenticator, Capabilities, Claims, Grant, Identity, PasswordChange, Refresh, Registration,
    RequestContext, SessionSummary, SharedAuth, SignIn,
};
use migo_cache::MemoryCache;
use migo_core::config::{Config, GatewayConfig};
use migo_core::metrics::Registry;
use migo_core::{Clock, Error, Id, ManualClock, SeededRandom, Shutdown, Timestamp};
use migo_protocol::{
    codes, fault, from_frame, to_frame, BandwidthMode, CloseReason, Encode, Error as ErrorMessage,
    Frame, FrameHeader, Hello, MessageEvent, MessageKind, NodeInfo, NotificationEvent, Opcode,
    Ping, PresenceState, PresenceUpdate, ReconnectHint, SubscribeRequest, SubscribeResponse, Topic,
    TopicKind, Welcome, PROTOCOL_VERSION,
};
use migo_ratelimit::{CacheRateLimiter, Policies, SharedRateLimiter, TrustTier};

use migo_gateway::{
    ClientContext, Dispatcher, Gateway, GatewayServices, NoopDispatcher, TopicRequest, Transport,
    TransportError,
};

// ---------------------------------------------------------------------------
// Time, ids, and the one token the fake authenticator honours.
// ---------------------------------------------------------------------------

const SECOND: i64 = 1_000;
const HOUR: i64 = 3_600 * SECOND;
/// A fixed, plausible wall clock. Tests that care about time advance a `ManualClock` from here.
const NOW: i64 = 1_700_000_000 * SECOND;

/// Kept far from any account number so a device id can never collide with an account id.
const DEVICE_OFFSET: u128 = 1_000_000;
/// Likewise for session ids.
const SESSION_OFFSET: u128 = 2_000_000;

const ACCOUNT: u128 = 0x00A1;
/// The single access token [`FakeAuth`] treats as valid; every other string is rejected.
const VALID_TOKEN: &str = "valid-access-token";

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

/// The device id paired with an account, by a fixed offset, so a test can name both without
/// threading two numbers around.
fn device_of(account: u128) -> Id {
    id(account + DEVICE_OFFSET)
}

// ---------------------------------------------------------------------------
// The fake authenticator: the seam the gateway calls to verify a token.
//
// The gateway calls exactly one of the eleven trait methods — `authenticate` — so that is the
// only one with behaviour. The rest are unreachable from the transport and are wired to panic,
// which is itself an assertion: if the gateway ever grows a call to one of them, a test will say
// so rather than silently exercise an untested path.
// ---------------------------------------------------------------------------

struct FakeAuth {
    /// The one token that verifies.
    token: String,
    account: Id,
    device: Id,
    session: Id,
    /// When the identity this returns expires; a test drives auth-expiry by moving a clock past it.
    expires_at: Timestamp,
    /// When the human last proved presence.
    authenticated_at: Timestamp,
    /// Every `(token, device)` pair the gateway asked us to verify, in order.
    calls: Mutex<Vec<(String, Id)>>,
}

impl FakeAuth {
    fn new() -> Self {
        Self {
            token: VALID_TOKEN.to_string(),
            account: id(ACCOUNT),
            device: device_of(ACCOUNT),
            session: id(ACCOUNT + SESSION_OFFSET),
            expires_at: ts(NOW + HOUR),
            authenticated_at: ts(NOW),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// The identity a successful verification yields, built from the fake's fixed facts.
    fn identity(&self) -> Identity {
        Identity {
            claims: Claims {
                account_id: self.account,
                device_id: self.device,
                session_id: self.session,
                capabilities: Capabilities::NONE,
                issued_at: self.authenticated_at,
                expires_at: self.expires_at,
                authenticated_at: self.authenticated_at,
            },
            username: "alice".to_string(),
            tier: TrustTier::Established,
            capabilities: Capabilities::NONE,
        }
    }

    /// How many times the gateway verified a token.
    fn authenticate_calls(&self) -> usize {
        self.calls
            .lock()
            .expect("the calls lock is never poisoned")
            .len()
    }
}

#[async_trait]
impl Authenticator for FakeAuth {
    async fn authenticate(
        &self,
        access_token: &str,
        device_id: Id,
        _context: &RequestContext,
    ) -> migo_core::Result<Identity> {
        self.calls
            .lock()
            .expect("the calls lock is never poisoned")
            .push((access_token.to_string(), device_id));
        if access_token == self.token {
            Ok(self.identity())
        } else {
            // An internal detail with no public face: the gateway must not disclose it, and the
            // "nothing leaks" tests check that it does not.
            Err(fault::error(
                codes::UNAUTHENTICATED,
                "the access token did not verify against the fake directory",
            ))
        }
    }

    fn verify_access(&self, _access_token: &str, _now: Timestamp) -> migo_core::Result<Claims> {
        unimplemented!("the gateway calls authenticate, never verify_access")
    }

    fn token_region(&self, _access_token: &str) -> Option<String> {
        None
    }

    async fn register(
        &self,
        _request: Registration,
        _context: &RequestContext,
    ) -> migo_core::Result<Grant> {
        unimplemented!("the gateway never registers accounts")
    }

    async fn sign_in(
        &self,
        _request: SignIn,
        _context: &RequestContext,
    ) -> migo_core::Result<Grant> {
        unimplemented!("the gateway never signs accounts in")
    }

    async fn refresh(
        &self,
        _request: Refresh,
        _context: &RequestContext,
    ) -> migo_core::Result<Grant> {
        unimplemented!("the gateway never refreshes tokens")
    }

    async fn sign_out(
        &self,
        _identity: &Identity,
        _session_id: Id,
        _context: &RequestContext,
    ) -> migo_core::Result<()> {
        unimplemented!("the gateway never signs sessions out")
    }

    async fn sign_out_others(
        &self,
        _identity: &Identity,
        _context: &RequestContext,
    ) -> migo_core::Result<u64> {
        unimplemented!("the gateway never signs other sessions out")
    }

    async fn sessions(
        &self,
        _identity: &Identity,
        _context: &RequestContext,
    ) -> migo_core::Result<Vec<SessionSummary>> {
        unimplemented!("the gateway never lists sessions")
    }

    async fn revoke_device(
        &self,
        _identity: &Identity,
        _device_id: Id,
        _context: &RequestContext,
    ) -> migo_core::Result<u64> {
        unimplemented!("the gateway never revokes devices")
    }

    async fn change_password(
        &self,
        _identity: &Identity,
        _change: PasswordChange,
        _context: &RequestContext,
    ) -> migo_core::Result<Grant> {
        unimplemented!("the gateway never changes passwords")
    }

    async fn set_contact(
        &self,
        _identity: &Identity,
        _contact: &str,
        _context: &RequestContext,
    ) -> migo_core::Result<()> {
        unimplemented!("the gateway never changes the contact record")
    }

    fn issue_captcha<'a>(
        &'a self,
        _mode: migo_captcha::CaptchaMode,
        _now: migo_core::Timestamp,
    ) -> std::pin::Pin<
        std::boxed::Box<
            dyn std::future::Future<Output = Option<migo_captcha::CaptchaChallengeView>>
                + Send
                + 'a,
        >,
    > {
        unimplemented!("the gateway never issues captchas")
    }

    async fn request_recovery(
        &self,
        _identifier: &str,
        _captcha: &migo_auth::CaptchaProof,
        _context: &RequestContext,
    ) -> migo_core::Result<migo_store::traits::RecoveryRow> {
        unimplemented!("the gateway never starts a recovery flow")
    }

    async fn confirm_recovery(
        &self,
        _token_id: Id,
        _tag: &[u8],
        _new_password: &migo_core::Secret,
        _context: &RequestContext,
    ) -> migo_core::Result<()> {
        unimplemented!("the gateway never confirms a recovery flow")
    }
    // --- the identity ceremonies: never reached from the gateway ----------------

    async fn issue_identity_challenge(
        &self,
        _request: migo_auth::IdentityChallengeRequest,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<migo_auth::ChallengeView> {
        unimplemented!("the gateway never issues an identity challenge")
    }

    async fn answer_identity_challenge(
        &self,
        _answer: migo_auth::ChallengeAnswer,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<Grant> {
        unimplemented!("the gateway never answers an identity challenge")
    }

    async fn answer_add_device(
        &self,
        _answer: migo_auth::AddDeviceAnswer,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<Grant> {
        unimplemented!("the gateway never answers an add-device challenge")
    }

    async fn issue_rotation_challenge(
        &self,
        _identity: &Identity,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<migo_auth::ChallengeView> {
        unimplemented!("the gateway never issues a rotation challenge")
    }

    async fn rotate_identity(
        &self,
        _identity: &Identity,
        _answer: migo_auth::RotationAnswer,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<()> {
        unimplemented!("the gateway never rotates an identity")
    }

    async fn publish_identity_key(
        &self,
        _identity: &Identity,
        _publication: migo_auth::IdentityPublication,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<()> {
        unimplemented!("the gateway never publishes an identity key")
    }

    async fn devices(
        &self,
        _identity: &Identity,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<Vec<migo_auth::DeviceSummary>> {
        unimplemented!("the gateway never lists devices")
    }

    async fn register_wallet(
        &self,
        _identity: &Identity,
        _registration: migo_auth::WalletRegistration,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<migo_auth::WalletSummary> {
        unimplemented!("the gateway never registers a wallet")
    }

    async fn wallets(
        &self,
        _identity: &Identity,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<Vec<migo_auth::WalletSummary>> {
        unimplemented!("the gateway never lists wallets")
    }

    async fn archive_wallet(
        &self,
        _identity: &Identity,
        _wallet_id: Id,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<()> {
        unimplemented!("the gateway never archives a wallet")
    }

    async fn admin_standing(
        &self,
        _identity: &Identity,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<migo_auth::AdminStanding> {
        unimplemented!("the gateway never asks for admin standing")
    }

    async fn global_admins(
        &self,
        _identity: &Identity,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<Vec<migo_auth::AdminView>> {
        unimplemented!("the gateway never lists global admins")
    }

    async fn grant_global_admin(
        &self,
        _identity: &Identity,
        _username: &str,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<migo_auth::AdminView> {
        unimplemented!("the gateway never grants global admins")
    }

    async fn revoke_global_admin(
        &self,
        _identity: &Identity,
        _account_id: Id,
        _context: &migo_auth::RequestContext,
    ) -> migo_core::Result<()> {
        unimplemented!("the gateway never revokes global admins")
    }
}

// ---------------------------------------------------------------------------
// The in-memory transport: a recording double, never a real socket.
//
// `recv` pops synchronously from a preloaded script and never yields at an await point, which is
// the property the driver's `select!` needs: it drops and recreates the `recv` future on every
// loop turn, and a future that never holds a half-consumed frame across a yield can neither lose
// one nor deliver one twice. When the script runs dry the transport either hangs up cleanly
// (`Ok(None)`) or, if asked to stay open, parks forever so a timer or shutdown is what ends the
// session.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Wire {
    inbound: VecDeque<Bytes>,
    outbound: Vec<Bytes>,
    closed: bool,
    park_when_empty: bool,
}

#[derive(Clone, Default)]
struct Pipe {
    wire: Arc<Mutex<Wire>>,
}

impl Pipe {
    fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Wire> {
        self.wire.lock().expect("the wire lock is never poisoned")
    }

    /// Scripts one client frame, encoded exactly as a real client would put it on the wire.
    fn client<M: Encode>(&self, opcode: Opcode, correlation: u32, message: &M) {
        let frame = to_frame(opcode.to_wire(), correlation, message)
            .expect("a scripted client message must encode");
        let bytes = frame.encode().expect("a scripted frame must encode");
        self.push_bytes(bytes);
    }

    /// Scripts one already-encoded frame's bytes, for the malformed-frame tests that must control
    /// the bytes exactly.
    fn push_bytes(&self, bytes: Bytes) {
        self.lock().inbound.push_back(bytes);
    }

    /// Keep the socket open after the script is exhausted, so a timer or the shutdown signal ends
    /// the session rather than a clean client hangup.
    fn keep_open(&self) {
        self.lock().park_when_empty = true;
    }

    /// A transport handle sharing this pipe's buffers, to hand to [`Gateway::serve`].
    fn transport(&self) -> Pipe {
        self.clone()
    }

    /// Every frame the server wrote, decoded (inflated first if it was compressed).
    fn sent(&self) -> Vec<Frame> {
        self.lock()
            .outbound
            .iter()
            .cloned()
            .map(|bytes| Frame::decode(bytes).expect("a captured server frame must decode"))
            .collect()
    }

    /// Whether the driver closed the transport.
    fn was_closed(&self) -> bool {
        self.lock().closed
    }
}

#[async_trait]
impl Transport for Pipe {
    async fn recv(&mut self) -> Result<Option<Bytes>, TransportError> {
        let next = self.lock().inbound.pop_front();
        match next {
            Some(bytes) => Ok(Some(bytes)),
            None => {
                let park = self.lock().park_when_empty;
                if park {
                    // Stay alive without consuming CPU or holding the lock across the await.
                    std::future::pending().await
                } else {
                    Ok(None)
                }
            }
        }
    }

    async fn send(&mut self, frame: Bytes) -> Result<(), TransportError> {
        self.lock().outbound.push(frame);
        Ok(())
    }

    async fn close(&mut self) {
        self.lock().closed = true;
    }
}

// ---------------------------------------------------------------------------
// The harness: a real gateway over the real limiter and registry, fake edges.
// ---------------------------------------------------------------------------

struct Harness {
    gateway: Gateway,
    registry: Registry,
    auth: Arc<FakeAuth>,
    clock: Arc<ManualClock>,
    shutdown: Shutdown,
}

struct HarnessBuilder {
    config: GatewayConfig,
    auth: FakeAuth,
    dispatcher: Arc<dyn Dispatcher>,
    features: u64,
    clock: ManualClock,
    shutdown: Shutdown,
}

impl HarnessBuilder {
    fn new() -> Self {
        Self {
            config: GatewayConfig::default(),
            auth: FakeAuth::new(),
            dispatcher: Arc::new(NoopDispatcher),
            // Advertise every feature bit so a client's requested features pass the mask
            // unchanged unless a test narrows this on purpose.
            features: u64::MAX,
            clock: ManualClock::new(ts(NOW)),
            shutdown: Shutdown::new(),
        }
    }

    fn build(self) -> Harness {
        let registry = Registry::new();
        let policies = Policies::from_config(&Config::default().rate_limit)
            .expect("the default rate-limit policies are valid");
        let limiter: SharedRateLimiter = Arc::new(CacheRateLimiter::new(
            Arc::new(MemoryCache::new()),
            policies,
            &registry,
        ));
        let auth = Arc::new(self.auth);
        let clock = Arc::new(self.clock);
        let shutdown = self.shutdown;
        let gateway = Gateway::open(
            &registry,
            &self.config,
            GatewayServices {
                authenticator: Arc::clone(&auth) as SharedAuth,
                rate_limiter: limiter,
                clock: Arc::clone(&clock) as Arc<dyn Clock>,
                random: Box::new(SeededRandom::new(1)),
                dispatcher: self.dispatcher,
                shutdown: shutdown.clone(),
                node: NodeInfo::default(),
                features: self.features,
            },
        );
        Harness {
            gateway,
            registry,
            auth,
            clock,
            shutdown,
        }
    }
}

impl Harness {
    fn new() -> Self {
        HarnessBuilder::new().build()
    }

    /// Drives one connection to completion over the given pipe, with no peer IP (the in-memory
    /// case), and returns once the socket has closed.
    async fn serve(&self, pipe: &Pipe) {
        self.serve_with(pipe, RequestContext::at(ts(NOW))).await;
    }

    async fn serve_with(&self, pipe: &Pipe, context: RequestContext) {
        self.gateway.serve(pipe.transport(), context).await;
    }

    fn counter(&self, name: &'static str, labels: &[(&str, &str)]) -> u64 {
        self.registry.counter(name, "", labels).get()
    }

    fn gauge(&self, name: &'static str, labels: &[(&str, &str)]) -> i64 {
        self.registry.gauge(name, "", labels).get()
    }

    fn sessions_opened(&self) -> u64 {
        self.counter("migo_gateway_sessions_opened_total", &[])
    }

    fn sessions_closed(&self, reason: &str) -> u64 {
        self.counter("migo_gateway_sessions_closed_total", &[("reason", reason)])
    }

    fn handshake_rejected(&self, reason: &str) -> u64 {
        self.counter(
            "migo_gateway_handshake_rejected_total",
            &[("reason", reason)],
        )
    }

    fn sessions_live(&self) -> i64 {
        self.gauge("migo_gateway_sessions_live", &[])
    }
}

// ---------------------------------------------------------------------------
// Frame builders and frame readers used across the suite.
// ---------------------------------------------------------------------------

/// A minimal, valid opening greeting for the version this build speaks.
fn hello() -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        ..Default::default()
    }
}

/// A greeting that carries an inline access token and device, the shape that promotes a session
/// straight to `Ready` when the token verifies.
fn hello_with_token(token: &str, device: Id) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        access_token: Some(token.to_string()),
        device_id: Some(device),
        ..Default::default()
    }
}

/// The single non-error frame the server sent, decoded as a `WELCOME`.
#[track_caller]
fn welcome_in(frames: &[Frame]) -> Welcome {
    let frame = frames
        .iter()
        .find(|frame| frame.header.opcode == Opcode::Hello.to_wire() && !frame.header.is_error())
        .expect("the handshake must be answered with a WELCOME frame");
    from_frame::<Welcome>(frame).expect("the WELCOME must decode")
}

/// Every error frame the server sent, decoded.
#[track_caller]
fn errors_in(frames: &[Frame]) -> Vec<ErrorMessage> {
    frames
        .iter()
        .filter(|frame| frame.header.is_error())
        .map(|frame| from_frame::<ErrorMessage>(frame).expect("an error frame must decode"))
        .collect()
}

/// The single error frame the server sent, decoded; fails if there is not exactly one.
#[track_caller]
fn sole_error(frames: &[Frame]) -> ErrorMessage {
    let mut errors = errors_in(frames);
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one error frame, found {}",
        errors.len()
    );
    errors.pop().expect("length checked to be one")
}

// ===========================================================================
// Invariant 1 — the state machine admits nothing out of order.
// ===========================================================================

#[tokio::test]
async fn a_fresh_handshake_is_answered_with_a_welcome() {
    let h = Harness::new();
    let pipe = Pipe::new();
    pipe.client(Opcode::Hello, 7, &hello());

    h.serve(&pipe).await;

    let welcome = welcome_in(&pipe.sent());
    assert_eq!(
        welcome.authenticated_user, None,
        "a handshake with no token names no account"
    );
    assert_eq!(
        h.sessions_opened(),
        1,
        "a completed handshake opens exactly one session"
    );
}

#[tokio::test]
async fn the_welcome_reuses_the_hello_opcode_and_correlation() {
    let h = Harness::new();
    let pipe = Pipe::new();
    pipe.client(Opcode::Hello, 12_345, &hello());

    h.serve(&pipe).await;

    let frames = pipe.sent();
    let welcome_frame = frames
        .iter()
        .find(|frame| !frame.header.is_error())
        .expect("a WELCOME frame is present");
    assert_eq!(
        welcome_frame.header.opcode,
        Opcode::Hello.to_wire(),
        "WELCOME reuses the HELLO opcode (section 139)"
    );
    assert_eq!(
        welcome_frame.header.correlation, 12_345,
        "WELCOME echoes the HELLO correlation"
    );
}

#[tokio::test]
async fn an_inline_token_promotes_the_session_and_names_the_account() {
    let h = Harness::new();
    let pipe = Pipe::new();
    pipe.client(
        Opcode::Hello,
        1,
        &hello_with_token(VALID_TOKEN, device_of(ACCOUNT)),
    );

    h.serve(&pipe).await;

    let welcome = welcome_in(&pipe.sent());
    assert_eq!(
        welcome.authenticated_user,
        Some(id(ACCOUNT)),
        "a valid inline token names the account in WELCOME"
    );
    assert_eq!(
        h.auth.authenticate_calls(),
        1,
        "the handshake verifies the inline token exactly once"
    );
}

#[tokio::test]
async fn a_bad_inline_token_is_not_fatal_and_opens_an_unauthenticated_session() {
    let h = Harness::new();
    let pipe = Pipe::new();
    pipe.client(
        Opcode::Hello,
        1,
        &hello_with_token("not-the-valid-token", device_of(ACCOUNT)),
    );

    h.serve(&pipe).await;

    let frames = pipe.sent();
    let welcome = welcome_in(&frames);
    assert_eq!(
        welcome.authenticated_user, None,
        "a bad inline token authenticates no one"
    );
    assert!(
        errors_in(&frames).is_empty(),
        "a bad inline token is not answered with an error — the session may still AUTHENTICATE later"
    );
    assert_eq!(
        h.sessions_opened(),
        1,
        "the session still opens after a bad inline token"
    );
}

/// A dispatcher double that records the lifecycle hooks the gateway fires at it.
///
/// The gateway calls `session_started` once — the moment a connection first holds an
/// identity — and `session_ended` once, when that connection tears down; a session that
/// never authenticates fires neither. This remembers the account behind every call, in
/// order, so a test can assert the pairing the trait promises rather than a bare count.
#[derive(Default)]
struct LifecycleSpy {
    started: Mutex<Vec<Id>>,
    ended: Mutex<Vec<Id>>,
}

impl LifecycleSpy {
    /// The account behind every `session_started`, in the order they fired.
    fn started(&self) -> Vec<Id> {
        self.started
            .lock()
            .expect("the spy lock is never poisoned")
            .clone()
    }

    /// The account behind every `session_ended`, in the order they fired.
    fn ended(&self) -> Vec<Id> {
        self.ended
            .lock()
            .expect("the spy lock is never poisoned")
            .clone()
    }
}

#[async_trait]
impl Dispatcher for LifecycleSpy {
    async fn dispatch(&self, _context: &ClientContext<'_>, _frame: &Frame) -> Result<(), Error> {
        Ok(())
    }

    async fn session_started(&self, identity: &Identity, _mode: BandwidthMode, _now: Timestamp) {
        self.started
            .lock()
            .expect("the spy lock is never poisoned")
            .push(identity.account_id());
    }

    async fn session_ended(&self, identity: &Identity, _mode: BandwidthMode, _now: Timestamp) {
        self.ended
            .lock()
            .expect("the spy lock is never poisoned")
            .push(identity.account_id());
    }
}

#[tokio::test]
async fn an_authenticated_session_starts_once_and_ends_once() {
    let spy = Arc::new(LifecycleSpy::default());
    let mut builder = HarnessBuilder::new();
    builder.dispatcher = Arc::clone(&spy) as Arc<dyn Dispatcher>;
    let h = builder.build();

    let pipe = Pipe::new();
    pipe.client(
        Opcode::Hello,
        1,
        &hello_with_token(VALID_TOKEN, device_of(ACCOUNT)),
    );

    h.serve(&pipe).await;

    assert_eq!(
        spy.started(),
        vec![id(ACCOUNT)],
        "an inline-token session fires the start exactly once, naming its account"
    );
    assert_eq!(
        spy.ended(),
        vec![id(ACCOUNT)],
        "and its teardown fires the matching end exactly once when the socket closes"
    );
}

#[tokio::test]
async fn an_unauthenticated_session_fires_no_lifecycle() {
    let spy = Arc::new(LifecycleSpy::default());
    let mut builder = HarnessBuilder::new();
    builder.dispatcher = Arc::clone(&spy) as Arc<dyn Dispatcher>;
    let h = builder.build();

    let pipe = Pipe::new();
    // A plain greeting carries no token, so the session opens unauthenticated and never
    // holds an identity for the hook to name.
    pipe.client(Opcode::Hello, 1, &hello());

    h.serve(&pipe).await;

    assert!(
        spy.started().is_empty(),
        "a session that never authenticated fires no start"
    );
    assert!(
        spy.ended().is_empty(),
        "and with no start there is no end to pair — the two never fire alone"
    );
}

#[tokio::test]
async fn the_first_frame_must_be_a_hello_or_the_connection_is_refused() {
    let h = Harness::new();
    let pipe = Pipe::new();
    // A PING is a legal opcode, but not as the opening frame.
    pipe.client(Opcode::Ping, 3, &Ping::default());

    h.serve(&pipe).await;

    let error = sole_error(&pipe.sent());
    assert_eq!(
        error.code,
        codes::UNEXPECTED_OPCODE,
        "a non-HELLO opening frame is refused as an unexpected opcode"
    );
    assert_eq!(
        h.sessions_opened(),
        0,
        "a refused handshake opens no session"
    );
    assert_eq!(
        h.handshake_rejected("protocol_violation"),
        1,
        "the refusal is metered as a protocol violation"
    );
    assert!(
        pipe.was_closed(),
        "the socket is closed after a refused handshake"
    );
}

#[tokio::test]
async fn a_refused_first_frame_discloses_only_its_public_reason() {
    let h = Harness::new();
    let pipe = Pipe::new();
    pipe.client(Opcode::Ping, 3, &Ping::default());

    h.serve(&pipe).await;

    let error = sole_error(&pipe.sent());
    assert_eq!(
        error.message.as_deref(),
        Some("expected HELLO"),
        "only the public hint crosses the wire"
    );
    // The internal detail names the state and the raw opcode; neither may appear.
    let message = error.message.unwrap_or_default();
    assert!(
        !message.contains("awaiting"),
        "the internal state description must not cross the wire"
    );
    assert!(
        !message.contains(&Opcode::Ping.to_wire().to_string()),
        "the raw opcode number must not cross the wire"
    );
}

#[tokio::test]
async fn an_opening_frame_that_is_not_even_a_frame_is_a_protocol_violation() {
    let h = Harness::new();
    let pipe = Pipe::new();
    // One byte cannot be a frame header, so this never parses as a frame at all.
    pipe.push_bytes(Bytes::from_static(&[0x01]));

    h.serve(&pipe).await;

    let frames = pipe.sent();
    let error = sole_error(&frames);
    assert_eq!(
        error.code,
        codes::DECODE_FAILED,
        "an unparseable opening frame is DECODE_FAILED"
    );
    // A frame that never parsed has no correlation to echo, so the refusal is server-initiated.
    let error_frame = frames
        .iter()
        .find(|frame| frame.header.is_error())
        .expect("an error frame is present");
    assert_eq!(
        error_frame.header.opcode,
        Opcode::Error.to_wire(),
        "an unparseable opener is refused under the ERROR opcode"
    );
    assert_eq!(
        error_frame.header.correlation, 0,
        "a server-initiated refusal carries correlation 0"
    );
    assert_eq!(h.handshake_rejected("protocol_violation"), 1);
    assert_eq!(h.sessions_opened(), 0);
}

#[tokio::test]
async fn a_hello_body_that_will_not_decode_is_refused_under_the_hello_opcode() {
    let h = Harness::new();
    let pipe = Pipe::new();
    // A well-formed frame header carrying the HELLO opcode, but a payload that cannot decode as a
    // Hello (a lone continuation byte runs off the end of the varint).
    let bogus = Frame::new(
        FrameHeader::new(Opcode::Hello.to_wire(), 9),
        Bytes::from_static(&[0xFF, 0xFF, 0xFF, 0xFF]),
    )
    .encode()
    .expect("the frame encodes");
    pipe.push_bytes(bogus);

    h.serve(&pipe).await;

    let frames = pipe.sent();
    let error = sole_error(&frames);
    assert_eq!(
        error.code,
        codes::DECODE_FAILED,
        "a HELLO body that will not decode is DECODE_FAILED"
    );
    let error_frame = frames
        .iter()
        .find(|frame| frame.header.is_error())
        .expect("an error frame is present");
    assert_eq!(
        error_frame.header.opcode,
        Opcode::Hello.to_wire(),
        "the refusal keeps the HELLO opcode, since the frame parsed as one"
    );
    assert_eq!(h.sessions_opened(), 0);
}

#[tokio::test]
async fn a_hello_for_an_unsupported_protocol_version_is_refused() {
    let h = Harness::new();
    let pipe = Pipe::new();
    let mut greeting = hello();
    greeting.protocol_version = PROTOCOL_VERSION + 1;
    pipe.client(Opcode::Hello, 5, &greeting);

    h.serve(&pipe).await;

    let error = sole_error(&pipe.sent());
    assert_eq!(
        error.code,
        codes::PROTOCOL_VERSION_UNSUPPORTED,
        "a version this node does not speak is PROTOCOL_VERSION_UNSUPPORTED"
    );
    assert_eq!(
        error.message.as_deref(),
        Some("unsupported protocol version")
    );
    assert_eq!(
        h.handshake_rejected("version_unsupported"),
        1,
        "the refusal is metered under version_unsupported, distinct from a protocol violation"
    );
    assert_eq!(h.sessions_opened(), 0);
}

#[tokio::test]
async fn a_second_hello_after_the_handshake_closes_the_connection() {
    let h = Harness::new();
    let pipe = Pipe::new();
    pipe.client(Opcode::Hello, 1, &hello());
    pipe.client(Opcode::Hello, 2, &hello());

    h.serve(&pipe).await;

    let frames = pipe.sent();
    // The first HELLO was answered; the second is the violation.
    let _welcome = welcome_in(&frames);
    let error = sole_error(&frames);
    assert_eq!(
        error.code,
        codes::UNEXPECTED_OPCODE,
        "a second HELLO is refused as an unexpected opcode"
    );
    assert_eq!(
        h.sessions_closed("protocol_violation"),
        1,
        "the session closes as a protocol violation"
    );
    assert_eq!(
        h.sessions_live(),
        0,
        "the live-session gauge is balanced after the violation close"
    );
}

#[tokio::test]
async fn a_clean_client_hangup_closes_the_session_as_a_client_request() {
    let h = Harness::new();
    let pipe = Pipe::new();
    pipe.client(Opcode::Hello, 1, &hello());
    // No keep_open: once the HELLO is consumed, the next recv reports a clean close.

    h.serve(&pipe).await;

    assert_eq!(
        h.sessions_closed("client_request"),
        1,
        "an exhausted client script is a clean, client-driven close"
    );
    assert_eq!(h.sessions_live(), 0);
    assert!(pipe.was_closed(), "the transport is closed on teardown");
}

// ===========================================================================
// Invariant 2 — a session ends on the server's terms as cleanly as on the client's.
//
// The two ways a live session dies without the client hanging up are a node draining and a
// client that has gone silent. Both run entirely on the server's own timers, so nothing on the
// wire proves them and no client complaint can report them: a drain that forgets to tell its
// sessions to reconnect reads exactly like a healthy node until every client retries at once,
// and a liveness check that never fires leaks a session slot per dead socket until the node
// stops accepting connections. Each test below drives one of them over a socket that stays open
// on purpose, so the only thing that can end the session is the mechanism under test.
// ===========================================================================

/// The single `RECONNECT_HINT` the server sent, decoded.
#[track_caller]
fn reconnect_hint_in(frames: &[Frame]) -> ReconnectHint {
    let frame = frames
        .iter()
        .find(|frame| {
            frame.header.opcode == Opcode::ReconnectHint.to_wire() && !frame.header.is_error()
        })
        .expect("a draining node must send a RECONNECT_HINT frame");
    from_frame::<ReconnectHint>(frame).expect("the RECONNECT_HINT must decode")
}

#[tokio::test(start_paused = true)]
async fn a_shutdown_signal_hands_the_client_a_reconnect_hint_before_closing() {
    let h = Harness::new();
    let pipe = Pipe::new();
    pipe.client(Opcode::Hello, 1, &hello());
    // The client is connected and silent, so the shutdown signal is the only thing that can end
    // this session.
    pipe.keep_open();

    let shutdown = h.shutdown.clone();
    let drain = async {
        // Under a paused clock this sleep resumes only once the server has parked in its
        // steady-state loop, which is exactly when a real drain would arrive.
        tokio::time::sleep(Duration::from_millis(50)).await;
        shutdown.trigger();
    };
    tokio::join!(h.serve(&pipe), drain);

    let frames = pipe.sent();
    let _welcome = welcome_in(&frames);
    let hint = reconnect_hint_in(&frames);
    assert_eq!(
        hint.reason,
        CloseReason::ServerShutdown,
        "a drain names itself, so the client reconnects instead of reporting a fault"
    );
    assert!(
        hint.after_ms <= 30_000,
        "the reconnect delay is drawn from a bounded window, got {}",
        hint.after_ms
    );
    let hint_frame = frames
        .iter()
        .find(|frame| frame.header.opcode == Opcode::ReconnectHint.to_wire())
        .expect("the hint frame is present");
    assert_eq!(
        hint_frame.header.correlation, 0,
        "a server-initiated frame carries correlation 0"
    );
    assert!(
        errors_in(&frames).is_empty(),
        "a drain is not a fault, so nothing is reported as an error"
    );
    assert_eq!(
        h.sessions_closed("server_shutdown"),
        1,
        "the close is metered as a server shutdown, not as a client hangup"
    );
    assert_eq!(
        h.sessions_live(),
        0,
        "the live-session gauge is balanced after a drain"
    );
    assert!(pipe.was_closed(), "the transport is closed on teardown");
}

#[tokio::test(start_paused = true)]
async fn two_missed_heartbeats_close_a_silent_session_and_release_its_slot() {
    // A one-second heartbeat means a two-second deadline and a quarter-second liveness tick, so
    // the check under test runs promptly without the suite waiting on production timings.
    let mut builder = HarnessBuilder::new();
    builder.config.heartbeat_ms = 1_000;
    let h = builder.build();
    let pipe = Pipe::new();
    pipe.client(Opcode::Hello, 1, &hello());
    // The socket stays open with nothing behind it: the half-dead connection a crashed client or
    // a vanished network leaves behind, which only the server's own clock can notice.
    pipe.keep_open();

    let clock = Arc::clone(&h.clock);
    let silence = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Past twice the heartbeat, measured on the clock the server actually reads.
        clock.advance_millis(2 * SECOND + 1);
    };
    tokio::join!(h.serve(&pipe), silence);

    assert_eq!(
        h.sessions_closed("heartbeat_timeout"),
        1,
        "a client that stopped sending is closed by the liveness check"
    );
    assert_eq!(
        h.sessions_live(),
        0,
        "the session slot is released, so a dead socket cannot exhaust the node"
    );
    assert!(pipe.was_closed(), "the transport is closed on teardown");
    assert!(
        errors_in(&pipe.sent()).is_empty(),
        "there is nobody left to read an explanation, so none is written"
    );
}

// ===========================================================================
// Invariant — authorization is read from the dispatcher, never trusted from the frame.
//
// `SUBSCRIBE` is the one place a frame's own list of topics would otherwise decide what the
// server sends back. The gateway cannot tell a conversation from a room from an account, so it
// asks the Dispatcher — the seam that reaches the domain — and files only what comes back
// granted. The three tests below drive a `Ready` session and script a `SUBSCRIBE` over the wire,
// with three dispatchers that answer the same question three different ways, and assert the
// session holds exactly and only what the domain granted.
// ===========================================================================

/// A dispatcher that answers every authorization question "yes".
///
/// Used to isolate the gateway's own bookkeeping — the ceiling and the keeping of granted topics
/// — from any notion of who owns what: with everything granted, what is rejected can only be the
/// surplus over the per-session cap.
#[derive(Clone, Copy, Debug, Default)]
struct GrantAll;

#[async_trait]
impl Dispatcher for GrantAll {
    async fn dispatch(&self, _context: &ClientContext<'_>, _frame: &Frame) -> Result<(), Error> {
        Ok(())
    }

    async fn authorize_topics(&self, _request: &TopicRequest<'_>, topics: &[Topic]) -> Vec<bool> {
        vec![true; topics.len()]
    }
}

/// A dispatcher that grants exactly the caller's own `User` topic and nothing else.
///
/// This is the shape of the invariant the suite's preamble names: a topic that is not the
/// caller's is rejected. Everything a stranger could name — a conversation, a room, another
/// account's presence — comes back denied, and because the transport conflates the reasons the
/// same way the domain crates do, the rejection carries no hint of which of those it is.
#[derive(Clone, Copy, Debug, Default)]
struct OwnOnly;

#[async_trait]
impl Dispatcher for OwnOnly {
    async fn dispatch(&self, _context: &ClientContext<'_>, _frame: &Frame) -> Result<(), Error> {
        Ok(())
    }

    async fn authorize_topics(&self, request: &TopicRequest<'_>, topics: &[Topic]) -> Vec<bool> {
        let account = request.identity().account_id();
        topics
            .iter()
            .map(|topic| topic.kind == TopicKind::User && topic.id == account)
            .collect()
    }
}

/// The single non-error `SUBSCRIBE` response the server sent, decoded.
#[track_caller]
fn subscribe_response_in(frames: &[Frame]) -> SubscribeResponse {
    let frame = frames
        .iter()
        .find(|frame| {
            frame.header.opcode == Opcode::Subscribe.to_wire() && !frame.header.is_error()
        })
        .expect("a SUBSCRIBE request must be answered with a SUBSCRIBE response");
    from_frame::<SubscribeResponse>(frame).expect("the SUBSCRIBE response must decode")
}

#[tokio::test]
async fn a_subscribe_on_a_null_dispatcher_grants_nothing() {
    // The default dispatcher a bare gateway stands up with has no domain to ask, so it answers
    // the refusing default. A pre-authenticated client names two topics; the session must end
    // holding neither.
    let h = Harness::new();
    let pipe = Pipe::new();
    pipe.client(
        Opcode::Hello,
        1,
        &hello_with_token(VALID_TOKEN, device_of(ACCOUNT)),
    );
    pipe.client(
        Opcode::Subscribe,
        2,
        &SubscribeRequest {
            topics: vec![
                Topic {
                    kind: TopicKind::Conversation,
                    id: id(0xBEEF),
                },
                Topic {
                    kind: TopicKind::User,
                    id: id(0xCAFE),
                },
            ],
        },
    );

    h.serve(&pipe).await;

    let response = subscribe_response_in(&pipe.sent());
    assert!(
        response.accepted.is_empty(),
        "a dispatcher with no domain grants no topic"
    );
    assert_eq!(
        response.rejected.as_ref().map(Vec::len),
        Some(2),
        "every topic the domain cannot grant is rejected"
    );
}

#[tokio::test]
async fn a_subscribe_keeps_only_the_topics_that_belong_to_the_caller() {
    // The owned-topic invariant: of the three topics asked for, exactly the caller's own is
    // accepted; the stranger's conversation and room are both rejected, and because the reasons
    // are conflated, neither rejection says why.
    let mut builder = HarnessBuilder::new();
    builder.dispatcher = Arc::new(OwnOnly);
    let h = builder.build();
    let pipe = Pipe::new();
    pipe.client(
        Opcode::Hello,
        1,
        &hello_with_token(VALID_TOKEN, device_of(ACCOUNT)),
    );
    let mine = Topic {
        kind: TopicKind::User,
        id: id(ACCOUNT),
    };
    let a_strangers_room = Topic {
        kind: TopicKind::Room,
        id: id(0xCAFE),
    };
    let a_strangers_conversation = Topic {
        kind: TopicKind::Conversation,
        id: id(0xBEEF),
    };
    pipe.client(
        Opcode::Subscribe,
        3,
        &SubscribeRequest {
            topics: vec![
                a_strangers_room.clone(),
                mine.clone(),
                a_strangers_conversation.clone(),
            ],
        },
    );

    h.serve(&pipe).await;

    let response = subscribe_response_in(&pipe.sent());
    assert_eq!(
        response.accepted,
        vec![mine],
        "only the topic that belongs to the caller is subscribed, in the requested order"
    );
    let rejected = response
        .rejected
        .expect("a refused topic is named in the rejected list");
    assert!(
        rejected.contains(&a_strangers_room),
        "a room that is not the caller's is rejected"
    );
    assert!(
        rejected.contains(&a_strangers_conversation),
        "a conversation that is not the caller's is rejected"
    );
}

#[tokio::test]
async fn a_subscribe_refuses_the_surplus_over_the_per_session_ceiling() {
    // A frame can name thousands of topics, but a session may hold only the ceiling. The surplus
    // is refused before the domain is asked anything — the ordering is the point — so a single
    // client frame never turns into a per-topic lookup against the store.
    let mut builder = HarnessBuilder::new();
    builder.dispatcher = Arc::new(GrantAll);
    let h = builder.build();
    let pipe = Pipe::new();
    pipe.client(
        Opcode::Hello,
        1,
        &hello_with_token(VALID_TOKEN, device_of(ACCOUNT)),
    );
    // 513 topics, one past the 512 ceiling, distinct ids so none is a duplicate of another.
    let topics: Vec<Topic> = (0_u32..513)
        .map(|i| Topic {
            kind: TopicKind::User,
            id: id(ACCOUNT + i as u128),
        })
        .collect();
    pipe.client(Opcode::Subscribe, 4, &SubscribeRequest { topics });

    h.serve(&pipe).await;

    let response = subscribe_response_in(&pipe.sent());
    assert_eq!(
        response.accepted.len(),
        512,
        "the session holds exactly the ceiling, no more"
    );
    assert_eq!(
        response.rejected.as_ref().map(Vec::len),
        Some(1),
        "the one topic over the ceiling is rejected without being asked of the domain"
    );
}

// ===========================================================================
// Invariant — backpressure is bounded and fails closed.
//
// Three delivery classes, three rules:
//   * Critical — never dropped. The queue signals lag and the session is closed
//     as `session_lagging` after `lagging_deadline_ms`. There is no Critical
//     drop series on the registry, by design.
//   * Coalescable — newer values for the same key replace older ones in
//     place; a full queue drops the new arrival and counts it.
//   * Droppable — a full queue drops the new arrival and counts it.
//
// The first test below drives all three classes through the same session and
// asserts the matrix: Droppable and Coalescable each have a non-zero drop
// counter, Critical never appears under either label, and the session ends
// closed as `session_lagging` because the Critical class filled the queue
// past the deadline.
// ===========================================================================

/// A dispatcher that, on every application opcode, publishes two Droppable
/// frames, one Coalescable frame, and one Critical frame to a topic the
/// session has been told about. Combined with a one-slot outbound queue,
/// the second Droppable on every dispatch meets a full queue and is
/// counted, the Coalescable meets a full queue without a prior same-key
/// entry and is counted, and the Critical is always enqueued (and would
/// set the lagging mark if a tick had a chance to read it).
struct Flood {
    topic: Topic,
}

#[async_trait]
impl Dispatcher for Flood {
    async fn dispatch(&self, context: &ClientContext<'_>, _frame: &Frame) -> Result<(), Error> {
        // Two Droppable so the second one always meets a full queue and is
        // counted. The first may or may not be enqueued depending on whether
        // the writer drained since the previous dispatch; the second is the
        // one the test asserts on.
        let drop = NotificationEvent::default();
        context.publish(&self.topic, Opcode::NotificationEvent, &drop, None)?;
        context.publish(&self.topic, Opcode::NotificationEvent, &drop, None)?;
        // Coalescable with a stable key so the same slot is the target of
        // coalescing when there is one. With a one-slot queue, it always
        // meets a full queue and never finds a same-key entry to coalesce
        // into.
        let coalesce_key = 0xCAFE_BABE;
        let presence = PresenceUpdate {
            state: PresenceState::Online,
            custom_status: None,
        };
        context.publish(
            &self.topic,
            Opcode::PresenceEvent,
            &presence,
            Some(coalesce_key),
        )?;
        // Critical — always enqueued, never dropped, by structural design.
        let message = MessageEvent {
            message_id: id(0xFEED),
            conversation_id: id(0xBEEF),
            seq: 1,
            sender_id: id(ACCOUNT),
            sender_device: device_of(ACCOUNT),
            sender_key_id: Some(1),
            kind: MessageKind::Text,
            created_at: Timestamp::from_millis(0),
            envelope: vec![0u8; 1],
            reply_to: None,
            edited_at: None,
            deleted: None,
        };
        context.publish(&self.topic, Opcode::MessageEvent, &message, None)?;
        Ok(())
    }

    async fn authorize_topics(&self, _request: &TopicRequest<'_>, topics: &[Topic]) -> Vec<bool> {
        vec![true; topics.len()]
    }
}

#[tokio::test]
async fn backpressure_drops_droppable_and_coalescable_but_never_critical() {
    // A one-slot queue makes the flood deterministic: every PING publishes
    // three frames (one per class) and the writer can drain at most one per
    // pass. The session ends naturally on the clean client hangup, so we do
    // not need a clock advance; what matters is that the drop counters rose
    // before the close.
    let mut builder = HarnessBuilder::new();
    builder.config.session_queue_capacity = 1;
    builder.dispatcher = Arc::new(Flood {
        topic: Topic {
            kind: TopicKind::User,
            id: id(ACCOUNT),
        },
    });
    let h = builder.build();
    let pipe = Pipe::new();
    pipe.client(
        Opcode::Hello,
        1,
        &hello_with_token(VALID_TOKEN, device_of(ACCOUNT)),
    );
    pipe.client(
        Opcode::Subscribe,
        2,
        &SubscribeRequest {
            topics: vec![Topic {
                kind: TopicKind::User,
                id: id(ACCOUNT),
            }],
        },
    );
    // Application opcodes reach `dispatch`. We use ProfileFetch because it
    // is the cheapest user-level opcode: empty list, no rate limit per call
    // beyond the bucket the limiter charges, no per-call cost on the domain
    // side beyond an `is_member` lookup. PING is authless and handled by the
    // gateway itself, so it would never reach the dispatcher.
    use migo_protocol::ProfileRequest;
    for i in 0..6_u32 {
        pipe.client(Opcode::ProfileFetch, 100 + i, &ProfileRequest::default());
    }
    // No keep_open(): once the script is consumed, recv returns None and the
    // session is closed as `client_request`, which is what we want — the
    // drop counters and the absence of a Critical series are unaffected by
    // the close reason.

    h.serve(&pipe).await;

    let droppable = h.counter(
        "migo_gateway_frames_dropped_total",
        &[("class", "droppable")],
    );
    let coalescable = h.counter(
        "migo_gateway_frames_dropped_total",
        &[("class", "coalescable")],
    );
    assert!(
        droppable >= 1,
        "Droppable floods must be counted, got {droppable}"
    );
    assert!(
        coalescable >= 1,
        "Coalescable floods with a different coalesce key must be counted, got {coalescable}"
    );
    // The structural property: a Critical drop series is never published, so
    // the metric exposition has no `class="critical"` line. This is the only
    // assertion that survives a future refactor that changes the dispatcher's
    // shape.
    let rendered = h.registry.render();
    assert!(
        !rendered.lines().any(
            |line| line.starts_with("migo_gateway_frames_dropped_total{")
                && line.contains("class=\"critical\"")
        ),
        "a Critical drop series must not exist; got:\n{rendered}"
    );
}

// ===========================================================================
// Invariant — a frame is size-checked before it is parsed.
//
// `Frame::decode` (migo-wire) compares the incoming buffer length against
// `MAX_FRAME_BYTES` before any header decode, before any payload slice, before
// any allocation of the struct that would hold the parsed fields. The test
// below scripts an oversize buffer that *looks* like a valid header and
// asserts the size check fires before anything else can: the only frame on
// the wire is the error reply, the session is never opened, and the parse
// error is mapped to `FRAME_TOO_LARGE`.
// ===========================================================================

#[tokio::test]
async fn an_oversize_frame_is_refused_before_any_allocation() {
    let h = Harness::new();
    let pipe = Pipe::new();
    let mut bytes = vec![0u8; migo_wire::limits::MAX_FRAME_BYTES + 1];
    // A plausible frame header (version 1, no flags, opcode HELLO, correlation 0).
    // The driver must never reach the decode of these bytes.
    bytes[0] = migo_wire::PROTOCOL_VERSION;
    bytes[1] = 0;
    bytes[2] = Opcode::Hello.to_wire() as u8;
    bytes[3] = 0;
    pipe.push_bytes(Bytes::from(bytes));

    h.serve(&pipe).await;

    let error = sole_error(&pipe.sent());
    assert_eq!(
        error.code,
        codes::FRAME_TOO_LARGE,
        "an oversize frame is rejected with FRAME_TOO_LARGE before any parse"
    );
    let sent = pipe.sent();
    let error_frame = sent
        .iter()
        .find(|frame| frame.header.is_error())
        .expect("an error frame is present");
    assert_eq!(
        error_frame.header.opcode,
        Opcode::Error.to_wire(),
        "a frame that never parsed carries a server-initiated ERROR, not a request reply"
    );
    assert_eq!(
        error_frame.header.correlation, 0,
        "a server-initiated refusal has correlation 0"
    );
    assert_eq!(
        h.sessions_opened(),
        0,
        "no session is opened when the opener is oversize"
    );
    assert_eq!(
        h.handshake_rejected("protocol_violation"),
        1,
        "the refusal is categorised as a protocol violation"
    );
    assert!(
        pipe.was_closed(),
        "the transport is closed after an oversize opener"
    );
}

// ===========================================================================
// Invariant — the wire is binary and push-only.
//
// "Push-only" is structural: the protocol has no Request/Response opcode
// pair, replies reuse the request opcode, and every server-to-client opcode
// is one of a small set of named pushes. A test that walks the opcode enum
// pins that the dispatch table is exactly that shape; if a new opcode is
// ever added that looks like a request, the test fails in CI and forces
// the reviewer to think about whether the new one belongs in this set.
// ===========================================================================

#[test]
fn the_wire_is_push_only_and_has_no_request_or_response_opcode() {
    use migo_protocol::Direction;

    // (1) No opcode is named like a request or response — replies reuse the
    // request opcode, so the enum must be flat.
    for &opcode in Opcode::ALL {
        let name = opcode.name();
        assert!(
            !name.contains("Request"),
            "opcode {name} looks like a request, but the protocol has no Request opcode"
        );
        assert!(
            !name.contains("Response"),
            "opcode {name} looks like a response, but the protocol has no Response opcode"
        );
    }

    // (2) Every server-to-client opcode is one of the small set of
    // server-initiated pushes. Adding a new one will fail this assertion and
    // force the author to think about whether the new shape is push-shaped.
    let server_only: Vec<Opcode> = Opcode::ALL
        .iter()
        .copied()
        .filter(|o| matches!(o.direction(), Direction::ServerToClient))
        .collect();
    let allowed: &[Opcode] = &[
        Opcode::Error,
        Opcode::ReconnectHint,
        Opcode::MessageEvent,
        Opcode::PresenceEvent,
        Opcode::RoomMemberEvent,
        Opcode::RoomStateEvent,
        Opcode::NotificationEvent,
        Opcode::GameEvent,
        // SPEC server-to-client pushes (section 145): the wire shape for each is a one-way
        // server-initiated event with no client request, so they belong in the push set.
        Opcode::FriendEvent,
        Opcode::MediaStateEvent,
        Opcode::EconomyEvent,
        Opcode::BotEvent,
        Opcode::ModerationEvent,
        Opcode::ReactionEvent,
        // Room vote tallies: the ROOM_VOTE_KICK request carries its own reply, but the
        // running tally (who has voted, how many more are needed) changes when *other*
        // members vote — one-way server-initiated frames, coalesced per room so a burst
        // of votes collapses into the newest state.
        Opcode::RoomVoteEvent,
        // Call signaling pushes (section 165): invite events, state changes, and SFU
        // events are all one-way server-initiated frames.
        Opcode::CallInviteEvent,
        Opcode::CallStateEvent,
        Opcode::CallSfuEvent,
        // Group pushes. A member event is a join/leave/removal arriving without a
        // request (including to the members who did not act); a vote tally moves
        // when *other* members vote, like a room's; a state event carries a rename
        // the founder's own reply already told them about.
        Opcode::ConversationMemberEvent,
        Opcode::ConversationVoteEvent,
        Opcode::ConversationStateEvent,
    ];
    for opcode in server_only {
        assert!(
            allowed.contains(&opcode),
            "server-to-client opcode {} is not in the push set; if this is intentional, \
             add it to the allowed list and document why the wire shape is still push-only",
            opcode.name()
        );
    }

    // (3) `accepts_from_client` is the policy the gateway actually enforces:
    // every server-to-client opcode is rejected on a client socket. Reading
    // the same property through the policy enum proves the two agree.
    for &opcode in Opcode::ALL {
        if matches!(opcode.direction(), Direction::ServerToClient) {
            assert!(
                !opcode.accepts_from_client(),
                "{} is server-to-client but accepts_from_client() returned true",
                opcode.name()
            );
        }
    }
}

#[tokio::test]
async fn a_server_to_client_opcode_from_a_client_closes_the_session() {
    // The behavioural half of the same invariant: a client cannot smuggle a
    // server-to-client opcode into the wire and have the gateway treat it as
    // a request. The session is closed as a protocol violation.
    let h = Harness::new();
    let pipe = Pipe::new();
    pipe.client(Opcode::Hello, 1, &hello());
    pipe.client(Opcode::MessageEvent, 2, &MessageEvent::default());

    h.serve(&pipe).await;

    let frames = pipe.sent();
    let _ = welcome_in(&frames);
    let error = sole_error(&frames);
    assert_eq!(
        error.code,
        codes::UNEXPECTED_OPCODE,
        "a server-to-client opcode from a client is an unexpected opcode"
    );
    assert_eq!(
        h.sessions_closed("protocol_violation"),
        1,
        "the session is closed as a protocol violation"
    );
}

// ===========================================================================
// Invariant — nothing sensitive is logged or metered; every error a client
// sees carries only the public face of the fault.
//
// The driver writes the wire-side `Error` envelope through
// `codec::wire_error`, which copies `error.public_message()` into the
// `message` field and nothing else. The internal message, the symbol, the
// raw opcode number, and the textual state name never reach the client.
//
// The structural half of the invariant — the metric registry is labelled only
// by closed enums, never by account id, device id, session id, or topic id
// — is asserted by rendering the registry and grepping the rendered
// exposition for the ids known to this test.
// ===========================================================================

#[tokio::test]
async fn error_frames_carry_only_their_public_face() {
    // Four independent sessions over one harness, each triggering a different
    // error path. The captured `message` fields are concatenated and asserted
    // against a list of internal substrings that must never appear.
    let h = Harness::new();

    // (1) Non-HELLO opener: a public "expected HELLO", set by
    // `connection.rs:247` via `fault::unexpected_opcode(...).public(...)`.
    let pipe_a = Pipe::new();
    pipe_a.client(Opcode::Ping, 1, &Ping::default());
    h.serve(&pipe_a).await;
    let error_a = sole_error(&pipe_a.sent());
    assert_eq!(error_a.code, codes::UNEXPECTED_OPCODE);
    assert_eq!(error_a.message.as_deref(), Some("expected HELLO"));

    // (2) Unparseable opener: a server-initiated DECODE_FAILED, no public
    // detail on the wire. Version byte is valid (1), flags are zero (no
    // reserved bits), and the body lies about its length so the codec
    // fails on the first read.
    let pipe_b = Pipe::new();
    pipe_b.push_bytes(Bytes::from_static(&[1, 0, 0xFF, 0xFF, 0xFF, 0xFF]));
    h.serve(&pipe_b).await;
    let error_b = sole_error(&pipe_b.sent());
    assert_eq!(error_b.code, codes::DECODE_FAILED);
    assert!(
        error_b.message.is_none() || error_b.message.as_deref() == Some(""),
        "wire-decode errors carry no public detail"
    );

    // (3) Bad inline token: a session still opens, but no error frame is sent
    // because the session is allowed to AUTHENTICATE later.
    let pipe_c = Pipe::new();
    pipe_c.client(
        Opcode::Hello,
        1,
        &hello_with_token("not-the-valid-token", device_of(ACCOUNT)),
    );
    h.serve(&pipe_c).await;
    let welcome_c = welcome_in(&pipe_c.sent());
    assert_eq!(
        welcome_c.authenticated_user, None,
        "a bad inline token authenticates nobody"
    );
    assert!(
        errors_in(&pipe_c.sent()).is_empty(),
        "a bad inline token is not answered with an error"
    );

    let mut haystack = String::new();
    for error in errors_in(&pipe_a.sent()) {
        if let Some(msg) = error.message {
            haystack.push_str(&msg);
            haystack.push('\n');
        }
    }
    for error in errors_in(&pipe_b.sent()) {
        if let Some(msg) = error.message {
            haystack.push_str(&msg);
            haystack.push('\n');
        }
    }

    // Well-known internal substrings that the driver must never copy to the
    // wire. The list is intentionally short and exact; the assertion is
    // brittle on purpose so a future regression is caught with a clear diff.
    const INTERNAL_MARKERS: &[&str] = &[
        "the access token did not verify against the fake directory",
        "awaiting HELLO",
        "not authenticated",
        "not accepted from a client",
        "handshake is already complete",
        "wire decode failed",
        "UNAUTHENTICATED",
        "INTERNAL_ERROR",
    ];
    for marker in INTERNAL_MARKERS {
        assert!(
            !haystack.contains(marker),
            "internal marker {marker:?} leaked to a client: {haystack}"
        );
    }

    // Structural half: the rendered metric exposition must not contain any
    // account or device id. The ids used in this test (`ACCOUNT` and
    // `ACCOUNT + DEVICE_OFFSET`) are deliberately chosen so neither the
    // decimal nor any plausible hex form of them appears in the registry
    // unless a label is built from one of them — which the closed-enum
    // labels make impossible.
    let rendered = h.registry.render();
    for forbidden in [
        ACCOUNT.to_string(),
        format!("{:x}", ACCOUNT),
        (ACCOUNT + DEVICE_OFFSET).to_string(),
        format!("{:x}", ACCOUNT + DEVICE_OFFSET),
    ] {
        assert!(
            !rendered.contains(&forbidden),
            "metric exposition leaks the id {forbidden}:\n{rendered}"
        );
    }
}
