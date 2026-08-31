//! Binding the assembled [`App`] to a socket and serving until shutdown.
//!
//! [`App::build`] connects the system; this is where it meets the network. The two transports are
//! mounted side by side on one axum [`Router`]: the REST API brings its own routes and middleware,
//! and one WebSocket route at [`GATEWAY_PATH`] upgrades a connection and hands it to the gateway.
//! They share a port and nothing else — neither knows the other exists, which is the whole point of
//! keeping the transports siblings. The optional QUIC listener (see [`crate::quic`]) is the third
//! sibling: it is not part of this socket at all, but bound separately in [`App::build`] when the
//! operator sets `quic.bind`, and it feeds the same gateway the WebSocket route feeds.
//!
//! # One connection's life
//!
//! A GET to [`GATEWAY_PATH`] with the upgrade headers is answered by [`upgrade`]: it samples the
//! server clock once, captures the peer address and user agent the transport happens to know, and
//! on upgrade builds the per-connection [`RequestContext`] the gateway threads through the
//! handshake. The gateway then owns that socket for its lifetime.
//!
//! # Graceful shutdown
//!
//! The axum server stops accepting new connections when the shared [`Shutdown`](migo_core::Shutdown)
//! fires, the same signal the gateway drains its live sessions on. The domain services are held on
//! `self` across the serve so their tasks and pooled connections outlive every request; they drop
//! only once serving has fully stopped.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, State};
use axum::http::header::USER_AGENT;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use axum::Router;

use migo_auth::RequestContext;
use migo_core::Clock;
use migo_gateway::Gateway;

use crate::compose::App;
use crate::transport::WsTransport;

/// The path a client opens its realtime gateway WebSocket against.
pub const GATEWAY_PATH: &str = "/ws";

/// What the WebSocket upgrade route needs to serve a connection: the gateway to hand it to, and the
/// clock to stamp its context with.
#[derive(Clone)]
struct GatewayState {
    gateway: Arc<Gateway>,
    clock: Arc<dyn Clock>,
}

impl App {
    /// Serves both transports on one socket until [`shutdown`](App::shutdown) fires.
    ///
    /// Binds [`bind`](App::bind), merges the gateway's WebSocket route into the REST router, and
    /// serves with graceful shutdown. Returns once the server has stopped accepting and every
    /// in-flight request has drained.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be bound or if the server terminates abnormally.
    pub async fn serve(self) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(self.bind.as_str())
            .await
            .with_context(|| format!("cannot bind to {}", self.bind))?;
        tracing::info!(bind = %self.bind, gateway_path = GATEWAY_PATH, "listening");

        let state = GatewayState {
            gateway: self.gateway,
            clock: self.clock,
        };
        let gateway_route = Router::new()
            .route(GATEWAY_PATH, get(upgrade))
            .with_state(state);
        let app = self.api_router.merge(gateway_route);

        // Move only the shutdown handle into the drain future; the domain services stay owned by
        // `self` for the duration of the serve and drop when it returns.
        let shutdown = self.shutdown;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await
        .context("server stopped abnormally")?;

        tracing::info!("server stopped");
        Ok(())
    }
}

/// Answers the WebSocket upgrade: capture what the transport knows, then hand the socket over.
async fn upgrade(
    State(state): State<GatewayState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: WebSocketUpgrade,
) -> Response {
    // Read the user agent here, while the headers are in hand; it ends up in the user's own session
    // list. The IP scopes the rate-limit buckets and is never stored whole (brief section 162).
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    request.on_upgrade(move |socket| async move {
        let mut context = RequestContext::at(state.clock.now()).from_ip(peer.ip());
        if let Some(user_agent) = user_agent {
            context = context.with_user_agent(user_agent);
        }
        state.gateway.serve(WsTransport::new(socket), context).await;
    })
}
