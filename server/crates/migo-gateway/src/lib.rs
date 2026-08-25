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
use crate::hub::Hub;
use crate::metrics::Meters;
use crate::outbound::ResumeBuffer;

pub use crate::dispatch::{ClientContext, Dispatcher, NoopDispatcher};
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
}
