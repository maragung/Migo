//! The middleware every REST response passes through, in one place.
//!
//! Four layers, each answering a brief requirement so no handler has to:
//!
//! - A request-body ceiling (section 121): a request larger than the configured maximum is
//!   rejected before a handler allocates for it, so an oversized upload cannot be a memory
//!   amplifier.
//! - CORS (section 118): browser origins are restricted to the configured allow-list, never `*`
//!   in a hardened deployment. An unlisted origin simply gets no CORS grant.
//! - A request id (section 119): one is set if the client did not supply it and propagated back
//!   on the response, so a client, a log line, and a trace share the same correlation id.
//! - An HTTP trace span: every request is observable without a `println` in a handler.

use axum::http::{header, HeaderName, HeaderValue, Method};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use migo_core::config::Config;

/// Wraps a fully-routed, state-attached router in the section 119/121 middleware.
pub(crate) fn apply(router: Router, config: &Config) -> Router {
    router
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(config.http.max_body_bytes))
        .layer(cors(config))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}

/// Builds the CORS layer from the configured origins, methods, and the headers the surface uses.
fn cors(config: &Config) -> CorsLayer {
    let origins: Vec<HeaderValue> = config
        .http
        .cors_origins
        .iter()
        .filter_map(|origin| origin.parse::<HeaderValue>().ok())
        .collect();
    CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static("idempotency-key"),
        ])
        .allow_origin(AllowOrigin::list(origins))
}
