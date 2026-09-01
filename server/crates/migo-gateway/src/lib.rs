//! The realtime transport for Migo: the crate that turns a stream of bytes into a session and a
//! session into a place the rest of the server can deliver events to.
//!
//! # What lives here, and what deliberately does not
//!
//! This crate owns the *transport*: the connection lifecycle state machine (brief section 149),
//! the handshake and resume protocol (sections 138–140, 150), the three-class backpressure queue
//! (section 151), and the subscription hub that fans one encoded event out to many sockets
//! (section 136). It knows nothing about what any application opcode *means* — sending a message,
//! joining a room, playing a move are all opaque to it.
//!
//! It is also deliberately ignorant of the socket itself. A connection arrives as a
//! [`Transport`]: a trait with `recv`, `send`, and `close` and nothing else. The `migod` binary
//! owns the WebSocket server and adapts each upgraded socket into a `Transport`, so this crate
//! never names `axum`, `tokio-tungstenite`, or any wire library. That keeps section 138's binding
//! rules — one MWP frame per binary message, deflate off, a hard frame ceiling — in one adapter
//! rather than smeared through the driver.
//!
//! # The two seams
//!
//! Everything above the transport reaches it through two traits, both implemented by the
//! composition root:
//!
//! - [`Transport`] adapts a concrete socket *down* to the byte verbs the driver needs.
//! - [`Dispatcher`] adapts application opcodes *up* to the domain crates the gateway must not
//!   depend on (section 177). The gateway calls it only for a `Ready` session, after
//!   the handshake, authentication, phase, and rate checks have already passed.
//!
//! Between them sits [`Gateway`]: constructed once from a [`Registry`], a
//! [`GatewayConfig`], and a bundle of [`GatewayServices`], then
//! asked to [`serve`](Gateway::serve) one transport per connection.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use migo_auth::RequestContext;
//! use migo_core::config::GatewayConfig;
//! use migo_core::metrics::Registry;
//! use migo_core::{Clock, Random, Shutdown, Timestamp};
//! use migo_gateway::{Gateway, GatewayServices, NoopDispatcher, Transport};
//! use migo_protocol::NodeInfo;
//!
//! # fn example<T: Transport>(
//! #     registry: &Registry,
//! #     config: &GatewayConfig,
//! #     authenticator: migo_auth::SharedAuth,
//! #     rate_limiter: migo_ratelimit::SharedRateLimiter,
//! #     clock: Arc<dyn Clock>,
//! #     random: Box<dyn Random>,
//! #     shutdown: Shutdown,
//! #     node: NodeInfo,
//! #     transport: T,
//! # ) {
//! let gateway = Gateway::open(
//!     registry,
//!     config,
//!     GatewayServices {
//!         authenticator,
//!         rate_limiter,
//!         clock,
//!         random,
//!         dispatcher: Arc::new(NoopDispatcher),
//!         shutdown,
//!         node,
//!         features: 0,
//!     },
//! );
//! // Per accepted connection, on its own task:
//! // gateway.serve(transport, RequestContext::at(Timestamp::now())).await;
//! # let _ = (gateway, transport);
//! # }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]

mod codec;
mod config;
mod connection;
mod dispatch;
mod hub;
mod metrics;
mod outbound;
mod session;
mod topic;
mod transport;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::Mutex;

use migo_auth::{RequestContext, SharedAuth};
use migo_core::config::GatewayConfig;
use migo_core::metrics::Registry;
use migo_core::{Clock, Id, Random, Shutdown, Timestamp};
use migo_protocol::NodeInfo;
use migo_ratelimit::SharedRateLimiter;

use crate::config::{Settings, MAX_SUBSCRIPTIONS};
use crate::metrics::Meters;
use crate::outbound::ResumeBuffer;

pub use crate::dispatch::{ClientContext, Dispatcher, NoopDispatcher, TopicRequest};
pub use crate::hub::Hub;
pub use crate::transport::{Transport, TransportError};

/// A running transport node.
///
/// Cheap to clone — it is a handle around shared state — so the accept loop can hold one and hand
/// each connection to [`serve`](Gateway::serve) without further ceremony. All sessions on a node
/// share one [`Gateway`]: one admission counter, one subscription hub, one resume store, one set
/// of metrics.
#[derive(Clone)]
pub struct Gateway {
    inner: Arc<GatewayInner>,
}

/// The shared, per-node state every connection driver borrows.
///
/// Held behind an [`Arc`] inside [`Gateway`]. Everything a driver needs that outlives a single
/// connection lives here: the resolved settings, the two seams ([`authenticator`] and
/// [`dispatcher`]), the clock and randomness, the subscription hub, the resume store, and the
/// live-session counter that admission control turns on.
///
/// [`authenticator`]: GatewayInner::authenticator
/// [`dispatcher`]: GatewayInner::dispatcher
pub(crate) struct GatewayInner {
    /// Runtime knobs resolved once from configuration.
    pub(crate) settings: Settings,
    /// Verifies access tokens on the handshake and on `AUTHENTICATE`.
    pub(crate) authenticator: SharedAuth,
    /// Charges each frame against its buckets before it is handled.
    pub(crate) rate_limiter: SharedRateLimiter,
    /// The single source of the server's notion of now.
    pub(crate) clock: Arc<dyn Clock>,
    /// Randomness for session ids and reconnect jitter, behind a lock because [`Random`] takes
    /// `&mut self` and the node is shared.
    pub(crate) random: Mutex<Box<dyn Random>>,
    /// This node's identity, echoed to every client in `WELCOME`.
    pub(crate) node: NodeInfo,
    /// The feature bits this node supports; a client's requested features are masked against it.
    pub(crate) features: u64,
    /// The cooperative shutdown signal every driver races against.
    pub(crate) shutdown: Shutdown,
    /// Every metric series this crate publishes.
    pub(crate) meters: Arc<Meters>,
    /// The subscription registry that fans events out to sessions.
    pub(crate) hub: Arc<Hub>,
    /// The application-logic seam, called for every application opcode on a ready session.
    pub(crate) dispatcher: Arc<dyn Dispatcher>,
    /// Retained resume state for recently-dropped sessions, keyed by session id (section 150).
    pub(crate) resume_store: DashMap<Id, ResumeBuffer>,
    /// How many sessions are currently admitted, for the section 149 ceiling.
    pub(crate) session_count: AtomicUsize,
}

impl GatewayInner {
    /// The server's notion of now, sampled from the node clock.
    pub(crate) fn now(&self) -> Timestamp {
        self.clock.now()
    }

    /// Mints a fresh session id stamped with the current time.
    pub(crate) fn new_session_id(&self, at: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(at, &mut **random)
    }

    /// A uniformly random delay in `0..bound` milliseconds, for spreading reconnects over a
    /// window. A bound of zero or one yields zero rather than touching the generator.
    pub(crate) fn jitter(&self, bound: u64) -> u64 {
        if bound <= 1 {
            return 0;
        }
        let mut random = self.random.lock();
        random.below(bound)
    }

    /// Tries to take one session slot, returning whether one was free.
    ///
    /// Increments first and rolls back on overflow, so two racing handshakes can never both
    /// squeeze past a ceiling of one: at most one sees a pre-increment value below the ceiling.
    pub(crate) fn try_admit(&self) -> bool {
        let previous = self.session_count.fetch_add(1, Ordering::Relaxed);
        if previous >= self.settings.max_sessions {
            self.session_count.fetch_sub(1, Ordering::Relaxed);
            false
        } else {
            true
        }
    }

    /// Releases a session slot taken by [`try_admit`](GatewayInner::try_admit).
    pub(crate) fn release(&self) {
        self.session_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// Takes the retained resume buffer for a session id, if one is held, removing it from the
    /// store so a resume consumes it exactly once.
    pub(crate) fn take_resume(&self, session_id: Id) -> Option<ResumeBuffer> {
        self.resume_store
            .remove(&session_id)
            .map(|(_, buffer)| buffer)
    }

    /// Retains a session's resume buffer for a possible reconnect.
    ///
    /// The store is bounded by the same ceiling as live sessions; when it is full, expired buffers
    /// are swept before the new one is inserted, so a burst of dropped sessions cannot grow it
    /// without bound.
    pub(crate) fn store_resume(&self, session_id: Id, buffer: ResumeBuffer) {
        if self.resume_store.len() >= self.settings.max_sessions {
            let now = self.now();
            self.resume_store
                .retain(|_, retained| !retained.expired(now));
        }
        self.resume_store.insert(session_id, buffer);
    }
}

/// The collaborators a [`Gateway`] needs, gathered so [`Gateway::open`] takes one bundle rather
/// than a long argument list.
///
/// The composition root builds this: it owns the concrete authenticator, rate limiter, clock,
/// randomness, and — the important one — the [`Dispatcher`] that wires the domain crates in behind
/// the transport.
pub struct GatewayServices {
    /// Verifies access tokens on the handshake and on `AUTHENTICATE`.
    pub authenticator: SharedAuth,
    /// Charges each frame against its rate-limit buckets.
    pub rate_limiter: SharedRateLimiter,
    /// The source of the server's notion of now.
    pub clock: Arc<dyn Clock>,
    /// Randomness for session ids and reconnect jitter.
    pub random: Box<dyn Random>,
    /// The application-logic seam for every application opcode.
    pub dispatcher: Arc<dyn Dispatcher>,
    /// The cooperative shutdown signal shared with the rest of the process.
    pub shutdown: Shutdown,
    /// This node's identity, echoed to clients in `WELCOME`.
    pub node: NodeInfo,
    /// The feature bits this node advertises.
    pub features: u64,
}

impl Gateway {
    /// Opens a gateway, registering its metrics and resolving its settings once.
    ///
    /// Call this a single time per process; then [`serve`](Gateway::serve) each accepted
    /// connection on its own task. The [`Registry`] is borrowed only for the duration of the call,
    /// to register every series at zero up front.
    #[must_use]
    pub fn open(registry: &Registry, config: &GatewayConfig, services: GatewayServices) -> Self {
        let meters = Arc::new(Meters::new(registry));
        let hub = Arc::new(Hub::new(MAX_SUBSCRIPTIONS, Arc::clone(&meters)));
        let inner = GatewayInner {
            settings: Settings::from_config(config),
            authenticator: services.authenticator,
            rate_limiter: services.rate_limiter,
            clock: services.clock,
            random: Mutex::new(services.random),
            node: services.node,
            features: services.features,
            shutdown: services.shutdown,
            meters,
            hub,
            dispatcher: services.dispatcher,
            resume_store: DashMap::new(),
            session_count: AtomicUsize::new(0),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Drives one connection to completion: handshake, ready loop, teardown.
    ///
    /// Returns when the socket has closed for any reason. `context` carries the per-connection
    /// request facts the transport already knows — the peer address, the arrival time — which the
    /// driver stamps onto authentication and rate-limit checks. Spawn one call per accepted
    /// connection; a single gateway serves any number of them concurrently.
    pub async fn serve<T: Transport>(&self, transport: T, context: RequestContext) {
        connection::run(&self.inner, transport, context).await;
    }

    /// Broadcasts one server-originated notification event to a recipient's user topic.
    ///
    /// This is the seam a domain crate calls when it has something to wake a user about — a
    /// mention, a room invite, a friend request — and the recipient's session must learn of it
    /// over the realtime path. The event is encoded once, fanned out by the subscription hub to
    /// every session subscribed to the recipient's `User` topic, and counted in the same
    /// backpressure series as any other frame the gateway pushes.
    ///
    /// The notification is delivered with `DeliveryClass::Droppable` (its wire class) and a
    /// coalescing key keyed on the recipient, so a burst of notifications for the same user
    /// collapses to the latest for any subscriber whose mailbox is backed up.
    ///
    /// The recipient's subscription is the same gate the realtime path uses: only sessions that
    /// successfully passed the dispatcher-authorized `SUBSCRIBE` for that `User` topic receive
    /// the event.
    pub fn emit_notification(
        &self,
        recipient: migo_core::Id,
        event: &migo_protocol::NotificationEvent,
        now: migo_core::Timestamp,
    ) {
        use migo_protocol::{to_frame, Opcode, Topic, TopicKind};
        let topic = Topic {
            kind: TopicKind::User,
            id: recipient,
        };
        let bytes = match to_frame(Opcode::NotificationEvent.to_wire(), 0, event) {
            Ok(frame) => match frame.encode() {
                Ok(bytes) => bytes,
                Err(_) => return,
            },
            Err(_) => return,
        };
        self.inner.hub.broadcast(
            &topic,
            &bytes,
            Opcode::NotificationEvent.class(),
            Some(coalesce_key_for(&recipient)),
            now,
            None,
        );
    }

    /// Publishes a server-originated frame to one topic, for callers outside a session.
    ///
    /// The mesh ingest path is the reason this exists: an event that arrives over the
    /// server-to-server link carries no originating session, but its readers are the same
    /// subscribers the realtime path serves, so it enters the same hub through the same
    /// subscription gate. Nothing here re-checks who may read the topic — that decision was
    /// made once, when the subscription was authorized — and nothing here charges a rate
    /// limit, because the sender is this node, not a client connection.
    pub fn broadcast_to_topic<E: migo_protocol::Encode>(
        &self,
        topic: &migo_protocol::Topic,
        opcode: migo_protocol::Opcode,
        event: &E,
        now: migo_core::Timestamp,
    ) {
        use migo_protocol::to_frame;
        let bytes = match to_frame(opcode.to_wire(), 0, event) {
            Ok(frame) => match frame.encode() {
                Ok(bytes) => bytes,
                Err(_) => return,
            },
            Err(_) => return,
        };
        self.inner
            .hub
            .broadcast(topic, &bytes, opcode.class(), None, now, None);
    }
}

/// A stable per-process key that groups the frames of one Coalescable stream.
fn coalesce_key_for(id: &migo_core::Id) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use migo_auth::{
        Authenticator, Claims, Grant, Identity, PasswordChange, Refresh, Registration,
        SessionSummary, SignIn,
    };
    use migo_cache::MemoryCache;
    use migo_core::config::Config;
    use migo_core::{ManualClock, SeededRandom};
    use migo_protocol::{from_frame, Frame, NotificationEvent, Opcode, Topic, TopicKind};
    use migo_ratelimit::{CacheRateLimiter, Policies};

    use super::*;

    use crate::outbound::Outbound;
    use crate::session::SessionHandle;

    const SECOND: i64 = 1_000;
    /// A fixed, plausible wall clock; nothing under test reads it.
    const NOW: i64 = 1_700_000_000 * SECOND;

    fn ts(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    /// An authenticator whose every method is unreachable: the seam under test speaks to the hub
    /// alone, so a call reaching this double would mean a gateway that authenticates its own
    /// outbound frames, and a test failure rather than a silent anomaly.
    struct UnusedAuth;

    #[async_trait]
    impl Authenticator for UnusedAuth {
        async fn register(
            &self,
            _request: Registration,
            _context: &RequestContext,
        ) -> migo_core::Result<Grant> {
            unimplemented!("the broadcast test never registers an account")
        }

        async fn sign_in(
            &self,
            _request: SignIn,
            _context: &RequestContext,
        ) -> migo_core::Result<Grant> {
            unimplemented!("the broadcast test never signs accounts in")
        }

        async fn refresh(
            &self,
            _request: Refresh,
            _context: &RequestContext,
        ) -> migo_core::Result<Grant> {
            unimplemented!("the broadcast test never refreshes tokens")
        }

        async fn authenticate(
            &self,
            _access_token: &str,
            _device_id: Id,
            _context: &RequestContext,
        ) -> migo_core::Result<Identity> {
            unimplemented!("the broadcast test never verifies a token")
        }

        fn verify_access(&self, _access_token: &str, _now: Timestamp) -> migo_core::Result<Claims> {
            unimplemented!("the broadcast test never verifies a token")
        }

        fn token_region(&self, _access_token: &str) -> Option<String> {
            None
        }

        async fn sign_out(
            &self,
            _identity: &Identity,
            _session_id: Id,
            _context: &RequestContext,
        ) -> migo_core::Result<()> {
            unimplemented!("the broadcast test never signs sessions out")
        }

        async fn sign_out_others(
            &self,
            _identity: &Identity,
            _context: &RequestContext,
        ) -> migo_core::Result<u64> {
            unimplemented!("the broadcast test never revokes sessions")
        }

        async fn sessions(
            &self,
            _identity: &Identity,
            _context: &RequestContext,
        ) -> migo_core::Result<Vec<SessionSummary>> {
            unimplemented!("the broadcast test never lists sessions")
        }

        async fn revoke_device(
            &self,
            _identity: &Identity,
            _device_id: Id,
            _context: &RequestContext,
        ) -> migo_core::Result<u64> {
            unimplemented!("the broadcast test never revokes devices")
        }

        async fn change_password(
            &self,
            _identity: &Identity,
            _change: PasswordChange,
            _context: &RequestContext,
        ) -> migo_core::Result<Grant> {
            unimplemented!("the broadcast test never changes passwords")
        }

        async fn set_contact(
            &self,
            _identity: &Identity,
            _contact: &str,
            _context: &RequestContext,
        ) -> migo_core::Result<()> {
            unimplemented!("the broadcast test never changes a contact record")
        }

        fn issue_captcha<'a>(
            &'a self,
            _mode: migo_captcha::CaptchaMode,
            _now: Timestamp,
        ) -> std::pin::Pin<
            std::boxed::Box<
                dyn std::future::Future<Output = Option<migo_captcha::CaptchaChallengeView>>
                    + Send
                    + 'a,
            >,
        > {
            unimplemented!("the broadcast test never issues captchas")
        }

        async fn request_recovery(
            &self,
            _identifier: &str,
            _captcha: &migo_captcha::CaptchaProof,
            _context: &RequestContext,
        ) -> migo_core::Result<migo_store::traits::RecoveryRow> {
            unimplemented!("the broadcast test never starts a recovery flow")
        }

        async fn confirm_recovery(
            &self,
            _token_id: Id,
            _tag: &[u8],
            _new_password: &migo_core::Secret,
            _context: &RequestContext,
        ) -> migo_core::Result<()> {
            unimplemented!("the broadcast test never confirms a recovery flow")
        }
        // --- the identity ceremonies: never reached from the gateway ----------------

        async fn issue_identity_challenge(
            &self,
            _request: migo_auth::IdentityChallengeRequest,
            _context: &migo_auth::RequestContext,
        ) -> migo_core::Result<migo_auth::ChallengeView> {
            unimplemented!("the broadcast test never issues an identity challenge")
        }

        async fn answer_identity_challenge(
            &self,
            _answer: migo_auth::ChallengeAnswer,
            _context: &migo_auth::RequestContext,
        ) -> migo_core::Result<Grant> {
            unimplemented!("the broadcast test never answers an identity challenge")
        }

        async fn answer_add_device(
            &self,
            _answer: migo_auth::AddDeviceAnswer,
            _context: &migo_auth::RequestContext,
        ) -> migo_core::Result<Grant> {
            unimplemented!("the broadcast test never answers an add-device challenge")
        }

        async fn issue_rotation_challenge(
            &self,
            _identity: &Identity,
            _context: &migo_auth::RequestContext,
        ) -> migo_core::Result<migo_auth::ChallengeView> {
            unimplemented!("the broadcast test never issues a rotation challenge")
        }

        async fn rotate_identity(
            &self,
            _identity: &Identity,
            _answer: migo_auth::RotationAnswer,
            _context: &migo_auth::RequestContext,
        ) -> migo_core::Result<()> {
            unimplemented!("the broadcast test never rotates an identity")
        }

        async fn publish_identity_key(
            &self,
            _identity: &Identity,
            _publication: migo_auth::IdentityPublication,
            _context: &migo_auth::RequestContext,
        ) -> migo_core::Result<()> {
            unimplemented!("the broadcast test never publishes an identity key")
        }

        async fn devices(
            &self,
            _identity: &Identity,
            _context: &migo_auth::RequestContext,
        ) -> migo_core::Result<Vec<migo_auth::DeviceSummary>> {
            unimplemented!("the broadcast test never lists devices")
        }

        async fn register_wallet(
            &self,
            _identity: &Identity,
            _registration: migo_auth::WalletRegistration,
            _context: &migo_auth::RequestContext,
        ) -> migo_core::Result<migo_auth::WalletSummary> {
            unimplemented!("the broadcast test never registers a wallet")
        }

        async fn wallets(
            &self,
            _identity: &Identity,
            _context: &migo_auth::RequestContext,
        ) -> migo_core::Result<Vec<migo_auth::WalletSummary>> {
            unimplemented!("the broadcast test never lists wallets")
        }

        async fn archive_wallet(
            &self,
            _identity: &Identity,
            _wallet_id: Id,
            _context: &migo_auth::RequestContext,
        ) -> migo_core::Result<()> {
            unimplemented!("the broadcast test never archives a wallet")
        }
    }

    /// A gateway over the real limiter and registry and an authenticator nothing reaches, mirroring
    /// the harness in `tests/gateway.rs` minus the socket: the seam under test is called from
    /// outside any session.
    fn gateway() -> Gateway {
        let registry = Registry::new();
        let policies = Policies::from_config(&Config::default().rate_limit)
            .expect("the default rate-limit policies are valid");
        let limiter: SharedRateLimiter = Arc::new(CacheRateLimiter::new(
            Arc::new(MemoryCache::new()),
            policies,
            &registry,
        ));
        Gateway::open(
            &registry,
            &GatewayConfig::default(),
            GatewayServices {
                authenticator: Arc::new(UnusedAuth) as SharedAuth,
                rate_limiter: limiter,
                clock: Arc::new(ManualClock::new(ts(NOW))) as Arc<dyn Clock>,
                random: Box::new(SeededRandom::new(1)),
                dispatcher: Arc::new(NoopDispatcher),
                shutdown: Shutdown::new(),
                node: NodeInfo::default(),
                features: 0,
            },
        )
    }

    #[test]
    fn broadcast_to_topic_reaches_a_subscribed_session() {
        let gateway = gateway();
        let session_id = Id::from(0x00B2);
        // A real mailbox built to the node's resolved settings, registered against the hub
        // directly — the same registry a live session's handle sits in, without the socket.
        let outbound = Arc::new(Outbound::new(
            gateway.inner.settings.queue_capacity,
            gateway.inner.settings.resume_buffer_frames,
            gateway.inner.settings.resume_window_ms,
        ));
        gateway.inner.hub.register(SessionHandle::new(
            session_id,
            Arc::clone(&outbound),
            migo_protocol::BandwidthMode::Normal,
        ));
        let room = Topic {
            kind: TopicKind::Room,
            id: Id::from(0x00C3),
        };
        let event = NotificationEvent::default();

        // Nobody is subscribed yet: the broadcast is a silent no-op that leaves the mailbox
        // untouched and must not panic — the "same gate" property's absence branch.
        gateway.broadcast_to_topic(&room, Opcode::NotificationEvent, &event, ts(NOW));
        assert!(
            outbound.take_ready().is_empty(),
            "a topic with no subscribers delivers nothing"
        );

        // Subscribe through the hub — the gate `broadcast_to_topic` itself does not re-check.
        let subscribed = gateway
            .inner
            .hub
            .subscribe(session_id, std::slice::from_ref(&room));
        assert!(
            subscribed.rejected.is_empty(),
            "a single subscription is far below the per-session cap"
        );

        gateway.broadcast_to_topic(&room, Opcode::NotificationEvent, &event, ts(NOW));

        let ready = outbound.take_ready();
        assert_eq!(
            ready.len(),
            1,
            "the frame reaches the subscribed session's mailbox"
        );
        let frame = Frame::decode(ready[0].clone()).expect("the broadcast frame must decode");
        assert_eq!(
            frame.header.opcode,
            Opcode::NotificationEvent.to_wire(),
            "the frame carries the opcode the caller named"
        );
        assert_eq!(
            frame.header.correlation, 0,
            "a server-originated frame carries correlation 0"
        );
        let decoded: NotificationEvent =
            from_frame(&frame).expect("the broadcast payload must decode");
        assert_eq!(
            decoded, event,
            "the payload arrives exactly as it was encoded"
        );
    }
}
