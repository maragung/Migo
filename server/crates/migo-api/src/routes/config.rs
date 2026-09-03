//! The runtime configuration document a client reads once at startup.
//!
//! `/v1/config` is the public, unauthenticated answer to "what is this node, and what will it
//! let me do?" (brief section 118 lists config among the REST surfaces). A client fetches it
//! before opening a socket to learn the node's identity, the feature bits it advertises, and the
//! handful of policy limits a form needs to validate against — the minimum passphrase length, the
//! largest body the server accepts, the biggest page a listing will return. It is derived
//! entirely from configuration and node identity, so it exposes no secret and touches no
//! service; every value here is one a client is meant to know.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::pagination::MAX_PAGE_SIZE;
use crate::ApiState;

/// The config route.
pub(crate) fn routes() -> Router<ApiState> {
    Router::new().route("/config", get(config))
}

/// The node's identity, as a client needs to display and route to it.
#[derive(Serialize)]
struct Node {
    id: String,
    region: String,
    country: String,
    public_url: String,
}

/// The policy limits a client validates its own forms against, so a request that is bound to be
/// refused can be caught before it is sent.
#[derive(Serialize)]
struct Limits {
    allow_registration: bool,
    passphrase_min_length: usize,
    max_devices_per_user: u32,
    max_body_bytes: usize,
    max_page_size: u32,
}

/// The auth-challenge posture, so a client can shape its forms before it submits them.
#[derive(Serialize)]
struct Captcha {
    /// Whether the node's captcha service is on. `false` means every captcha-free path stays
    /// captcha-free and the client hides its captcha UI.
    enabled: bool,
}

/// The whole document: who this node is, what it can do, and what it will accept.
#[derive(Serialize)]
struct Document {
    node: Node,
    features: u64,
    limits: Limits,
    captcha: Captcha,
}

/// Builds the config document from node identity and the surface's policy.
#[allow(clippy::unused_async)]
async fn config(State(state): State<ApiState>) -> Json<Document> {
    let node = state.node();
    let policy = state.policy();
    Json(Document {
        node: Node {
            id: node.node_id.clone(),
            region: node.region.clone(),
            country: node.country.clone(),
            public_url: policy.public_url.clone(),
        },
        features: state.features(),
        limits: Limits {
            allow_registration: policy.allow_registration,
            passphrase_min_length: policy.passphrase_min_length,
            max_devices_per_user: policy.max_devices_per_user,
            max_body_bytes: policy.max_body_bytes,
            max_page_size: MAX_PAGE_SIZE,
        },
        captcha: Captcha {
            enabled: state.captcha_enabled(),
        },
    })
}
