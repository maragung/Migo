//! The REST surface for Migo: the JSON API for everything that is deliberately *not* realtime.
//!
//! # Why a second surface exists at all
//!
//! Migo has two front doors, and brief section 118 draws the line between them sharply. The
//! realtime door is the binary MWP transport in `migo-gateway`: every chat message, presence
//! update, typing indicator, reaction, and call-signalling frame goes through it, and JSON is
//! forbidden there. This crate is the other door — REST over JSON — and it carries only the
//! traffic that has no business on a socket: bootstrapping a session before a socket can be
//! opened at all (register, sign in, refresh, sign out), and the operational surface a load
//! balancer and a scrape target need (health, readiness, metrics, the runtime config document).
//!
//! The rule that keeps the two from drifting is that they never re-implement each other. A
//! handler here does not parse a frame or reach into the wire model; it calls the very same
//! domain service the gateway's dispatcher calls, and it maps the domain's own return type into
//! a small local JSON shape at its edge. The single source of truth for the *realtime* model
//! stays the protocol IDL; the REST bodies defined here are REST-native — an authentication
//! request is not a mirror of any wire struct — so there is nothing to keep manually in sync.
//!
//! # What this crate is, mechanically
//!
//! It is a builder for an [`axum::Router`] and nothing more. It opens no socket, binds no port,
//! and spawns no task. [`router`] takes the process configuration and a bundle of already-built
//! shared service handles ([`ApiServices`]) and returns a `Router` with its state and middleware
//! attached. The `migod` binary — the composition root — constructs the services once, hands
//! clones of the same handles to both this crate and the gateway, mounts this router under its
//! HTTP server alongside the `/ws` upgrade route, and runs it. That is the same inversion the
//! gateway uses: the transport layer (this crate, layer 4) depends downward on the domain
//! (layer 3) and never sideways on its sibling transports.
//!
//! # The request pipeline
//!
//! Every mutating endpoint here walks the section 119 pipeline in order: authenticate (the
//! [`Authenticated`] extractor, which verifies the token's signature and expiry with no I/O and
//! then confirms the session has not been revoked), rate limit (a network-scoped charge at the
//! edge on the unauthenticated bootstrap endpoints, layered over the per-account limiting the
//! domain service already applies), validate, execute against the domain service, and let the
//! service audit. Errors leave through one funnel — [`ApiError`] — which maps a
//! [`migo_core::Error`] to its HTTP status from the generated schema (section 118) and puts only
//! the error's public face on the wire (section 161); an internal message never crosses it.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use migo_api::{router, ApiServices};
//! use migo_core::config::Config;
//! use migo_core::metrics::Registry;
//! use migo_core::Clock;
//! use migo_protocol::NodeInfo;
//!
//! # fn example(
//! #     config: &Config,
//! #     authenticator: migo_auth::SharedAuth,
//! #     rate_limiter: migo_ratelimit::SharedRateLimiter,
//! #     clock: Arc<dyn Clock>,
//! #     registry: Arc<Registry>,
//! #     node: NodeInfo,
//! # ) {
//! let app = router(
//!     config,
//!     ApiServices {
//!         authenticator,
//!         rate_limiter,
//!         clock,
//!         registry,
//!         node,
//!         features: 0,
//!         media_files: None,
//!     },
//! );
//! // migod serves `app` on its HTTP listener, e.g. `axum::serve(listener, app)`.
//! # let _ = app;
//! # }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]

mod error;
mod extract;
mod middleware;
mod pagination;
mod ratelimit;
mod routes;

use std::sync::Arc;

use axum::Router;

use migo_auth::SharedAuth;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Clock, Timestamp};
use migo_protocol::NodeInfo;
use migo_ratelimit::SharedRateLimiter;

pub use crate::error::ApiError;
pub use crate::extract::{Authenticated, IdempotencyKey, RequestFacts};
pub use crate::pagination::{Page, PageParams, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
pub use crate::routes::media::{MediaFiles, SharedMediaFiles};

/// The collaborators the REST surface needs, gathered so [`router`] takes one bundle rather than
/// a long argument list.
///
/// The composition root (`migod`) builds this once and hands it in. Every handle is shared — the
/// same [`SharedAuth`] and [`SharedRateLimiter`] the gateway holds — so a token minted on the
/// REST door is honoured on the socket and a rate-limit charge on one door is felt on the other.
pub struct ApiServices {
    /// Verifies access tokens and runs register, sign-in, refresh, and sign-out.
    pub authenticator: SharedAuth,
    /// Charges the network-scoped buckets that defend the unauthenticated bootstrap endpoints.
    pub rate_limiter: SharedRateLimiter,
    /// The single source of the server's notion of now, so an expiry boundary is testable.
    pub clock: Arc<dyn Clock>,
    /// The metric registry this node publishes, rendered by the `/metrics` endpoint.
    pub registry: Arc<Registry>,
    /// This node's identity, reported by the `/v1/config` document.
    pub node: NodeInfo,
    /// The feature bits this node advertises, reported by the `/v1/config` document.
    pub features: u64,
    /// The media byte store behind the data-plane routes. `None` when the deployment's
    /// storage serves its own bytes (the S3 backend): the routes answer `404` rather
    /// than pretending to be an object store they are not.
    pub media_files: Option<SharedMediaFiles>,
}

/// The shared state every handler borrows, behind one [`Arc`] so the router is cheap to clone.
///
/// Holds the service handles from [`ApiServices`] plus the handful of configuration-derived
/// values the surface reports and enforces (the auth policy, the body ceiling, the public URL).
/// Cloning is a reference-count bump; axum clones it per request.
#[derive(Clone)]
pub struct ApiState {
    inner: Arc<Inner>,
}

/// The reference-counted body of [`ApiState`].
struct Inner {
    authenticator: SharedAuth,
    rate_limiter: SharedRateLimiter,
    clock: Arc<dyn Clock>,
    registry: Arc<Registry>,
    node: NodeInfo,
    features: u64,
    policy: Policy,
    media_files: Option<SharedMediaFiles>,
}

/// The configuration-derived values the REST surface reports and enforces.
///
/// Copied out of [`Config`] once at construction so a handler never holds a borrow of the whole
/// configuration tree just to read the registration flag.
#[derive(Clone, Debug)]
struct Policy {
    allow_registration: bool,
    password_min_length: usize,
    max_devices_per_user: u32,
    max_body_bytes: usize,
    public_url: String,
    /// Whether the accessible alternative captcha mode may be requested. Copied from
    /// `captcha.accessible_mode` so the route decides it without borrowing the whole tree.
    captcha_accessible_mode: bool,
    /// Whether the captcha service is on at all. Reported through `/v1/config` so a client can
    /// hide its captcha UI entirely instead of learning it from a refusal.
    captcha_enabled: bool,
    /// The largest object one PUT may carry, from `media.max_upload_bytes`.
    media_max_upload_bytes: u64,
}

impl ApiState {
    /// Assembles the state from the process configuration and the service bundle.
    fn new(config: &Config, services: ApiServices) -> Self {
        let policy = Policy {
            allow_registration: config.auth.allow_registration,
            password_min_length: config.auth.password_min_length,
            max_devices_per_user: config.auth.max_devices_per_user,
            max_body_bytes: config.http.max_body_bytes,
            public_url: config.http.public_url.clone(),
            captcha_accessible_mode: config.captcha.accessible_mode,
            captcha_enabled: config.captcha.enabled,
            media_max_upload_bytes: config.media.max_upload_bytes,
        };
        Self {
            inner: Arc::new(Inner {
                authenticator: services.authenticator,
                rate_limiter: services.rate_limiter,
                clock: services.clock,
                registry: services.registry,
                node: services.node,
                features: services.features,
                policy,
                media_files: services.media_files,
            }),
        }
    }

    /// Whether the captcha service is on, for the config document.
    pub(crate) fn captcha_enabled(&self) -> bool {
        self.inner.policy.captcha_enabled
    }

    /// The authenticator, for the auth handlers and the [`Authenticated`] extractor.
    pub(crate) fn authenticator(&self) -> &SharedAuth {
        &self.inner.authenticator
    }

    /// The rate limiter, for the edge charge on the bootstrap endpoints.
    pub(crate) fn rate_limiter(&self) -> &SharedRateLimiter {
        &self.inner.rate_limiter
    }

    /// The metric registry, for `/metrics`.
    pub(crate) fn registry(&self) -> &Registry {
        &self.inner.registry
    }

    /// This node's identity, for `/v1/config`.
    pub(crate) fn node(&self) -> &NodeInfo {
        &self.inner.node
    }

    /// The advertised feature bits, for `/v1/config`.
    pub(crate) fn features(&self) -> u64 {
        self.inner.features
    }

    /// The configuration-derived policy, for `/v1/config`.
    pub(crate) fn policy(&self) -> &Policy {
        &self.inner.policy
    }

    /// The server's notion of now, sampled from the node clock.
    pub(crate) fn now(&self) -> Timestamp {
        self.inner.clock.now()
    }

    /// The media byte store, when this process serves the data plane.
    ///
    /// The handlers answer `404` rather than `500` when it is absent: a client fetching
    /// an S3 URL from this node is holding a URL minted for another host, which is a
    /// routing mistake the caller can fix, not a fault of this node.
    pub(crate) fn media_files(&self) -> Option<&dyn crate::routes::media::MediaFiles> {
        self.inner.media_files.as_deref()
    }
}

/// Builds the REST router: the route tree, its state, and the section 119/121 middleware.
///
/// Call this once. The returned `Router` is complete and ready for `migod` to serve on its HTTP
/// listener; it carries no state parameter, so it can be nested or served directly. The
/// configuration is borrowed only for the duration of the call — the values the surface needs
/// are copied into [`ApiState`]. The returned `Router` is itself `#[must_use]`, so a caller that
/// drops it on the floor is caught without an attribute here.
pub fn router(config: &Config, services: ApiServices) -> Router {
    let state = ApiState::new(config, services);
    let routed = routes::mount().with_state(state);
    middleware::apply(routed, config)
}
