//! The route tree, assembled from one small module per resource group.
//!
//! Two tiers: an operational tier mounted at the root — `/health`, `/ready`, `/metrics`, the
//! endpoints a load balancer and a scrape target reach without a version prefix — and the
//! versioned API under `/v1`, where the auth bootstrap and the runtime config document live.
//! New resource groups (users, rooms, media, and the rest of the section 118 public surface)
//! are added by writing a module here and merging it into [`v1`]; nothing else changes.

mod auth;
mod config;
mod health;
mod metrics;

use axum::Router;

use crate::ApiState;

/// The complete route tree, still carrying [`ApiState`] so the caller attaches state and
/// middleware.
pub(crate) fn mount() -> Router<ApiState> {
    Router::new()
        .merge(health::routes())
        .merge(metrics::routes())
        .nest("/v1", v1())
}

/// The versioned API surface.
fn v1() -> Router<ApiState> {
    Router::new().merge(auth::routes()).merge(config::routes())
}
