//! The Prometheus scrape endpoint.
//!
//! `/metrics` renders every series the node's [`Registry`](migo_core::metrics::Registry) holds
//! in the text exposition format a scrape target expects (brief section 118 lists metrics among
//! the REST surfaces). It is a read of already-aggregated counters and gauges — no per-account,
//! per-device, or per-conversation series exists to render, because section 174 forbids minting
//! one, so the output is bounded no matter how many users the node serves.

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use crate::ApiState;

/// The metrics route.
pub(crate) fn routes() -> Router<ApiState> {
    Router::new().route("/metrics", get(metrics))
}

/// Renders the registry in Prometheus text exposition format.
#[allow(clippy::unused_async)]
async fn metrics(State(state): State<ApiState>) -> impl IntoResponse {
    let body = state.registry().render();
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body)
}
