//! The authentication bootstrap: the four endpoints a client uses before it can open a socket.
//!
//! Register, sign in, and refresh mint a session; sign out ends one. These are the reason the
//! REST surface exists at all — a client cannot open the realtime transport without an access
//! token, and it cannot get an access token over a transport it has not opened yet (brief
//! section 118 permits exactly this bootstrap over REST). Everything a session then does happens
//! on the socket, not here.
//!
//! Each handler is thin on purpose. It maps its REST-native JSON body into the authenticator's
//! own input type, walks the section 119 pipeline — an edge rate-limit charge on the three
//! unauthenticated endpoints, then the domain call that authenticates, validates, executes, and
//! audits — and maps the returned [`Grant`] into a JSON response. The refresh and access tokens
//! do cross the wire here: they are the caller's own credentials, returned to the caller that
//! just proved its identity. They must never reach a log (section 145), which is why nothing
//! here traces them.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use migo_auth::{DeviceClaim, Grant, Refresh, Registration, SignIn};
use migo_core::{Id, Secret};
use migo_protocol::Platform;

use crate::extract::{Authenticated, RequestFacts};
use crate::ratelimit::charge_ip;
use crate::ApiState;

/// One unit charged against the caller's network bucket per bootstrap attempt.
const BOOTSTRAP_COST: u32 = 1;

/// The default locale assumed when a client discloses none.
fn default_locale() -> String {
    "en".to_string()
}

/// The auth routes, nested under `/auth`.
pub(crate) fn routes() -> Router<ApiState> {
    Router::new().nest(
        "/auth",
        Router::new()
            .route("/register", post(register))
            .route("/login", post(login))
            .route("/refresh", post(refresh))
            .route("/logout", post(logout)),
    )
}

/// What a client claims about the device a session runs on. Every field is a claim; none of it
/// grants anything (see `migo_auth`'s device model).
#[derive(Deserialize)]
struct DeviceRequest {
    #[serde(default)]
    device_id: Option<Id>,
    #[serde(default)]
    platform: Option<String>,
    display_name: String,
    #[serde(default)]
    app_version: Option<String>,
    #[serde(default)]
    os_version: Option<String>,
    #[serde(default)]
    device_model: Option<String>,
}

impl DeviceRequest {
    /// Turns the claim into the authenticator's device type.
    fn into_claim(self) -> DeviceClaim {
        let platform = self
            .platform
            .as_deref()
            .map_or(Platform::Unknown, parse_platform);
        let mut claim = DeviceClaim::new(platform, self.display_name);
        if let Some(device_id) = self.device_id {
            claim = claim.on_device(device_id);
        }
        if let Some(app_version) = self.app_version {
            claim = claim.with_app_version(app_version);
        }
        claim.os_version = self.os_version;
        claim.device_model = self.device_model;
        claim
    }
}

/// Maps a platform name to the claimed platform, defaulting to `Unknown` for anything else.
fn parse_platform(name: &str) -> Platform {
    match name.to_ascii_lowercase().as_str() {
        "web" => Platform::Web,
        "android" => Platform::Android,
        "ios" => Platform::Ios,
        "desktop" => Platform::Desktop,
        "bot" => Platform::Bot,
        _ => Platform::Unknown,
    }
}

/// A new-account request.
#[derive(Deserialize)]
struct RegisterRequest {
    username: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    phone: Option<String>,
    password: String,
    #[serde(default = "default_locale")]
    locale: String,
    #[serde(default)]
    country: Option<String>,
    device: DeviceRequest,
}

/// A sign-in request. One identifier field because a user does not think of a username and an
/// email as different kinds of thing.
#[derive(Deserialize)]
struct LoginRequest {
    identifier: String,
    password: String,
    device: DeviceRequest,
}

/// A refresh-token exchange. The device id is checked against the session the token was minted
/// for, so a token replayed from another device is refused.
#[derive(Deserialize)]
struct RefreshRequest {
    refresh_token: String,
    device_id: Id,
}

/// A sign-out request, naming the session to end.
#[derive(Deserialize)]
struct LogoutRequest {
    session_id: Id,
}

/// The session a successful bootstrap yields. The token fields are the caller's own credentials.
#[derive(Serialize)]
struct GrantResponse {
    account_id: Id,
    device_id: Id,
    session_id: Id,
    access_token: String,
    refresh_token: String,
    access_expires_at_ms: i64,
    refresh_expires_at_ms: i64,
    capabilities: u64,
    is_new_account: bool,
}

impl From<Grant> for GrantResponse {
    fn from(grant: Grant) -> Self {
        Self {
            account_id: grant.account_id,
            device_id: grant.device_id,
            session_id: grant.session_id,
            access_token: grant.access_token,
            refresh_token: grant.refresh_token.expose().to_string(),
            access_expires_at_ms: grant.access_expires_at.as_unix_ms(),
            refresh_expires_at_ms: grant.refresh_expires_at.as_unix_ms(),
            capabilities: grant.capabilities.bits(),
            is_new_account: grant.is_new_account,
        }
    }
}

/// `POST /v1/auth/register` — create an account and open its first session.
async fn register(
    State(state): State<ApiState>,
    facts: RequestFacts,
    Json(body): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<GrantResponse>), crate::ApiError> {
    charge_ip(&state, facts.ip, BOOTSTRAP_COST).await?;
    let now = state.now();
    let registration = Registration {
        username: body.username,
        email: body.email,
        phone: body.phone,
        password: Secret::new(body.password),
        locale: body.locale,
        country: body.country,
        device: body.device.into_claim(),
    };
    let context = facts.context(now);
    let grant = state
        .authenticator()
        .register(registration, &context)
        .await?;
    Ok((StatusCode::CREATED, Json(grant.into())))
}

/// `POST /v1/auth/login` — open a session for an existing account.
async fn login(
    State(state): State<ApiState>,
    facts: RequestFacts,
    Json(body): Json<LoginRequest>,
) -> Result<Json<GrantResponse>, crate::ApiError> {
    charge_ip(&state, facts.ip, BOOTSTRAP_COST).await?;
    let now = state.now();
    let sign_in = SignIn {
        identifier: body.identifier,
        password: Secret::new(body.password),
        device: body.device.into_claim(),
    };
    let context = facts.context(now);
    let grant = state.authenticator().sign_in(sign_in, &context).await?;
    Ok(Json(grant.into()))
}

/// `POST /v1/auth/refresh` — exchange a refresh token for a fresh session.
async fn refresh(
    State(state): State<ApiState>,
    facts: RequestFacts,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<GrantResponse>, crate::ApiError> {
    charge_ip(&state, facts.ip, BOOTSTRAP_COST).await?;
    let now = state.now();
    let exchange = Refresh {
        refresh_token: Secret::new(body.refresh_token),
        device_id: body.device_id,
    };
    let context = facts.context(now);
    let grant = state.authenticator().refresh(exchange, &context).await?;
    Ok(Json(grant.into()))
}

/// `POST /v1/auth/logout` — end the named session. Requires the caller to be authenticated.
async fn logout(
    State(state): State<ApiState>,
    auth: Authenticated,
    Json(body): Json<LogoutRequest>,
) -> Result<StatusCode, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    state
        .authenticator()
        .sign_out(&auth.identity, body.session_id, &context)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
