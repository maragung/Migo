//! Liveness and readiness, the two probes an orchestrator polls.
//!
//! Both are unauthenticated and cheap by design (brief section 118 lists them among the REST
//! surfaces). `/health` answers as long as the process can serve a request at all — it is the
//! liveness probe, and a failure means "restart me". `/ready` answers when the node is willing
//! to take traffic; for now that is the same condition, but the two are kept as distinct routes
//! so readiness can later gate on warm dependencies without disturbing liveness.

use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::ApiState;

/// The liveness and readiness routes.
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
}

/// A probe answer: a single machine-readable status word.
#[derive(Serialize)]
struct Probe {
    status: &'static str,
}

/// Liveness: the process is up and can serve a request.
#[allow(clippy::unused_async)]
async fn health() -> Json<Probe> {
    Json(Probe { status: "ok" })
}

/// Readiness: the node is willing to take traffic.
#[allow(clippy::unused_async)]
async fn ready() -> Json<Probe> {
    Json(Probe { status: "ready" })
}
