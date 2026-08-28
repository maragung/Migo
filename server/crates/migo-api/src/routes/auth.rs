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
use axum::routing::{get, post};
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
            .route("/captcha", post(captcha))
            .route("/register", post(register))
            .route("/login", post(login))
            .route("/refresh", post(refresh))
            .route("/logout", post(logout))
            .route("/password", post(change_password))
            .route("/sessions", get(list_sessions))
            .route("/sessions/revoke-others", post(revoke_other_sessions))
            .route("/sessions/{session_id}/revoke", post(revoke_one_session))
            .route("/recovery/request", post(recovery_request))
            .route("/recovery/confirm", post(recovery_confirm)),
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
    /// Captcha proof, present once the gate is engaged and absent on a
    /// first attempt. The handler is allowed to forward `None`; the
    /// `Authenticator` decides whether `None` is acceptable and answers
    /// `CAPTCHA_REQUIRED` when it is not.
    captcha: Option<CaptchaProofBody>,
}

/// A sign-in request. One identifier field because a user does not think of a username and an
/// email as different kinds of thing.
#[derive(Deserialize)]
struct LoginRequest {
    identifier: String,
    password: String,
    device: DeviceRequest,
    captcha: Option<CaptchaProofBody>,
}

/// Wire shape of a captcha proof on a bootstrap request. Converts into the
/// domain `CaptchaProof` at the handler boundary so the rest of the service
/// never sees a `serde::Deserialize` type.
#[derive(Deserialize)]
struct CaptchaProofBody {
    /// The id the user was given when the challenge was issued.
    challenge_id: migo_core::Id,
    /// The six-digit answer the user typed.
    answer: String,
}

impl From<CaptchaProofBody> for migo_auth::CaptchaProof {
    fn from(body: CaptchaProofBody) -> Self {
        Self {
            challenge_id: body.challenge_id,
            answer: body.answer,
        }
    }
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
        captcha: body.captcha.map(migo_auth::CaptchaProof::from),
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
        captcha: body.captcha.map(migo_auth::CaptchaProof::from),
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

// --- captcha and recovery surface -----------------------------------------

/// `POST /v1/auth/captcha` — issue a fresh captcha. Anonymous; the
/// rate limiter at the IP tier is the cost gate. The response is the
/// gate's own public view of the issued challenge: an id the client
/// must echo back, the six digits to type, and the seconds the
/// challenge is still valid.
async fn captcha(
    State(state): State<ApiState>,
    facts: RequestFacts,
) -> Result<Json<migo_captcha::CaptchaChallengeView>, crate::ApiError> {
    charge_ip(&state, facts.ip, BOOTSTRAP_COST).await?;
    let now = state.now();
    let challenge = state
        .authenticator()
        .issue_captcha(now)
        .await
        .ok_or_else(|| crate::ApiError::from(migo_protocol::fault::feature_disabled("captcha")))?;
    Ok(Json(challenge))
}

/// `POST /v1/auth/recovery/request` — start a password-recovery flow.
/// Returns 200 `{ ok: true }` regardless of whether the identifier
/// resolved, so an attacker cannot enumerate accounts.
#[derive(Deserialize)]
struct RecoveryRequestBody {
    identifier: String,
    captcha: Option<CaptchaProofBody>,
}
#[derive(Serialize)]
struct RecoveryRequestResponse {
    ok: bool,
}
async fn recovery_request(
    State(state): State<ApiState>,
    facts: RequestFacts,
    Json(body): Json<RecoveryRequestBody>,
) -> Result<Json<RecoveryRequestResponse>, crate::ApiError> {
    charge_ip(&state, facts.ip, BOOTSTRAP_COST).await?;
    let now = state.now();
    let captcha = body
        .captcha
        .map(migo_auth::CaptchaProof::from)
        .ok_or_else(|| {
            crate::ApiError::from(migo_protocol::fault::validation(
                "captcha",
                "captcha is required for recovery",
            ))
        })?;
    let context = facts.context(now);
    let _ = state
        .authenticator()
        .request_recovery(&body.identifier, &captcha, &context)
        .await?;
    Ok(Json(RecoveryRequestResponse { ok: true }))
}

#[derive(Deserialize)]
struct RecoveryConfirmBody {
    token_id: migo_core::Id,
    /// The hex-encoded HMAC tag the request route issued.
    tag: String,
    new_password: String,
}
async fn recovery_confirm(
    State(state): State<ApiState>,
    facts: RequestFacts,
    Json(body): Json<RecoveryConfirmBody>,
) -> Result<Json<RecoveryRequestResponse>, crate::ApiError> {
    let now = state.now();
    let context = facts.context(now);
    let tag = hex::decode(&body.tag).map_err(|_| {
        crate::ApiError::from(migo_protocol::fault::validation(
            "tag",
            "tag must be hex-encoded",
        ))
    })?;
    state
        .authenticator()
        .confirm_recovery(
            body.token_id,
            &tag,
            &migo_core::Secret::new(body.new_password),
            &context,
        )
        .await?;
    Ok(Json(RecoveryRequestResponse { ok: true }))
}

// --- password, sessions, contact ------------------------------------------

#[derive(Deserialize)]
struct ChangePasswordBody {
    current_password: String,
    new_password: String,
}
async fn change_password(
    State(state): State<ApiState>,
    auth: Authenticated,
    Json(body): Json<ChangePasswordBody>,
) -> Result<Json<GrantResponse>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let change = migo_auth::PasswordChange {
        current: migo_core::Secret::new(body.current_password),
        next: migo_core::Secret::new(body.new_password),
    };
    let grant = state
        .authenticator()
        .change_password(&auth.identity, change, &context)
        .await?;
    Ok(Json(grant.into()))
}

#[derive(Serialize)]
struct SessionsResponse {
    sessions: Vec<migo_auth::SessionSummary>,
}
async fn list_sessions(
    State(state): State<ApiState>,
    auth: Authenticated,
) -> Result<Json<SessionsResponse>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let sessions = state
        .authenticator()
        .sessions(&auth.identity, &context)
        .await?;
    Ok(Json(SessionsResponse { sessions }))
}

#[derive(Serialize)]
struct RevokeOthersResponse {
    ok: bool,
    revoked: u64,
}
async fn revoke_other_sessions(
    State(state): State<ApiState>,
    auth: Authenticated,
) -> Result<Json<RevokeOthersResponse>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let revoked = state
        .authenticator()
        .sign_out_others(&auth.identity, &context)
        .await?;
    Ok(Json(RevokeOthersResponse { ok: true, revoked }))
}

async fn revoke_one_session(
    State(state): State<ApiState>,
    auth: Authenticated,
    axum::extract::Path(session_id): axum::extract::Path<migo_core::Id>,
) -> Result<StatusCode, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    state
        .authenticator()
        .revoke_device(&auth.identity, session_id, &context)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
