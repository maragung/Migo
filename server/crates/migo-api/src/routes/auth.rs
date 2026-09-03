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
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use migo_auth::{DeviceClaim, Gender, Grant, Refresh, Registration, ServerEndpoint, SignIn};
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
            .route("/contact", put(set_contact))
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
pub(crate) struct DeviceRequest {
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
    /// The device credential's ML-DSA-65 public key, base64, when the client
    /// registered with a root secret.
    #[serde(default)]
    credential_public_key: Option<String>,
}

impl DeviceRequest {
    /// Turns the claim into the authenticator's device type.
    pub(crate) fn into_claim(self) -> Result<DeviceClaim, crate::ApiError> {
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
        claim.credential_public_key = self
            .credential_public_key
            .as_deref()
            .map(decode_key)
            .transpose()?;
        Ok(claim)
    }
}

/// Decodes a base64 ML-DSA public key from a request body.
///
/// A wrong encoding is a client that is wrong, not an input to repair: the
/// authenticator checks the length, the route checks the encoding, and
/// neither guesses.
fn decode_key(value: &str) -> Result<Vec<u8>, crate::ApiError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| {
            crate::ApiError::from(migo_protocol::fault::validation(
                "public key",
                "must be base64-encoded",
            ))
        })
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
    /// Gender as the user disclosed it on the form: `1` male, `2` female,
    /// `3` other, absent for "not disclosed". A number outside the
    /// numbering is a client that is wrong, not a value to round to the
    /// nearest disclosure.
    #[serde(default)]
    gender: Option<i16>,
    /// The server the client believes it is talking to. Optional on
    /// the wire: a self-hosted client that has not opened the
    /// "Server" disclosure yet sends a body without a `server`
    /// field, and the route layer fills the gap with
    /// [`ServerEndpoint::default_for_host`] before the request
    /// reaches the authenticator.
    #[serde(default)]
    server: Option<ServerEndpointBody>,
    /// The account identity's ML-DSA-65 public key, base64, when the
    /// client is registering with a root secret. Absent on every
    /// legacy client.
    #[serde(default)]
    identity_public_key: Option<String>,
}

/// A sign-in request. One identifier field because a user does not think of a username and an
/// email as different kinds of thing.
#[derive(Deserialize)]
struct LoginRequest {
    identifier: String,
    password: String,
    device: DeviceRequest,
    captcha: Option<CaptchaProofBody>,
    /// The server the client believes it is talking to. Same
    /// defaulting rule as [`RegisterRequest::server`].
    #[serde(default)]
    server: Option<ServerEndpointBody>,
}

/// The wire shape of a [`ServerEndpoint`] on the bootstrap request body.
/// Converted into the auth crate's `ServerEndpoint` at the handler
/// boundary so the rest of the service never sees a
/// `serde::Deserialize` type.
#[derive(Deserialize)]
struct ServerEndpointBody {
    host: String,
    port: u16,
    #[serde(default)]
    gateway_port: Option<u16>,
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    scheme: Option<String>,
    #[serde(default)]
    rest_scheme: Option<String>,
}

impl ServerEndpointBody {
    /// Turns the wire shape into the auth crate's `ServerEndpoint`,
    /// falling back to the standard defaults for any field the client
    /// did not send. A malformed wire value is rejected with a
    /// `VALIDATION_FAILED` envelope, not a panic, so a hand-rolled
    /// client cannot trip the auth service with a bad field name.
    fn into_endpoint(self) -> Result<ServerEndpoint, crate::ApiError> {
        use migo_auth::{RestScheme, Scheme, Transport, WsScheme};

        if self.host.trim().is_empty() {
            return Err(crate::ApiError::from(migo_protocol::fault::validation(
                "server.host",
                "host is required",
            )));
        }
        if self.port == 0 {
            return Err(crate::ApiError::from(migo_protocol::fault::validation(
                "server.port",
                "port is required",
            )));
        }
        let transport = match self.transport.as_deref() {
            None | Some("WebSocket" | "websocket") => Transport::WebSocket,
            Some("Tcp" | "tcp" | "TCP") => Transport::Tcp,
            Some("Quic" | "quic" | "QUIC") => Transport::Quic,
            Some(_other) => {
                return Err(crate::ApiError::from(migo_protocol::fault::validation(
                    "server.transport",
                    "unknown transport; expected WebSocket, Tcp, or Quic",
                )));
            }
        };
        let scheme = match self.scheme.as_deref() {
            None => match transport {
                Transport::WebSocket => Scheme::Ws(WsScheme::Wss),
                Transport::Tcp => Scheme::Tcp(migo_auth::TcpScheme::TcpTls),
                Transport::Quic => Scheme::Quic(migo_auth::QuicScheme::QuicTls),
            },
            Some("Ws" | "ws" | "WS") => Scheme::Ws(WsScheme::Ws),
            Some("Wss" | "wss" | "WSS") => Scheme::Ws(WsScheme::Wss),
            Some("Tcp" | "tcp" | "TCP") => Scheme::Tcp(migo_auth::TcpScheme::Tcp),
            Some("TcpTls" | "tcp-tls" | "TCP-TLS") => Scheme::Tcp(migo_auth::TcpScheme::TcpTls),
            Some("Quic" | "quic") => Scheme::Quic(migo_auth::QuicScheme::Quic),
            Some("QuicTls" | "quic-tls" | "QUIC-TLS") => {
                Scheme::Quic(migo_auth::QuicScheme::QuicTls)
            }
            Some(_) => {
                return Err(crate::ApiError::from(migo_protocol::fault::validation(
                    "server.scheme",
                    "unknown scheme; expected Ws, Wss, Tcp, TcpTls, Quic, or QuicTls",
                )));
            }
        };
        let rest_scheme = match self.rest_scheme.as_deref() {
            None => match scheme {
                Scheme::Ws(WsScheme::Wss)
                | Scheme::Tcp(migo_auth::TcpScheme::TcpTls)
                | Scheme::Quic(migo_auth::QuicScheme::QuicTls) => RestScheme::Https,
                _ => RestScheme::Http,
            },
            Some("Http" | "http") => RestScheme::Http,
            Some("Https" | "https") => RestScheme::Https,
            Some(_) => {
                return Err(crate::ApiError::from(migo_protocol::fault::validation(
                    "server.rest_scheme",
                    "unknown rest scheme; expected Http or Https",
                )));
            }
        };
        let gateway_port = self.gateway_port.unwrap_or(self.port);
        Ok(ServerEndpoint {
            host: self.host.to_ascii_lowercase(),
            port: self.port,
            gateway_port,
            transport,
            scheme,
            rest_scheme,
        })
    }
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
pub(crate) struct GrantResponse {
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
    let server = body
        .server
        .map(ServerEndpointBody::into_endpoint)
        .transpose()?;
    // A number outside the numbering is refused rather than read as "not
    // disclosed": the column's `None` is the user's own silence, and a wrong
    // client's typo must not be recorded as the user's choice.
    let gender = match body.gender {
        None => None,
        Some(raw) => Some(Gender::from_i16(raw).ok_or_else(|| {
            crate::ApiError::from(migo_protocol::fault::validation(
                "gender",
                "must be 1 (male), 2 (female), or 3 (other); omit it to not disclose",
            ))
        })?),
    };
    let registration = Registration {
        username: body.username,
        email: body.email,
        phone: body.phone,
        password: Secret::new(body.password),
        locale: body.locale,
        country: body.country,
        gender,
        device: body.device.into_claim()?,
        identity_public_key: body
            .identity_public_key
            .as_deref()
            .map(decode_key)
            .transpose()?,
        captcha: body.captcha.map(migo_auth::CaptchaProof::from),
        server,
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
    let server = body
        .server
        .map(ServerEndpointBody::into_endpoint)
        .transpose()?;
    let sign_in = SignIn {
        identifier: body.identifier,
        password: Secret::new(body.password),
        device: body.device.into_claim()?,
        captcha: body.captcha.map(migo_auth::CaptchaProof::from),
        server,
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

/// The request body of `POST /v1/auth/captcha`: absent, empty, or carrying a mode.
///
/// The mode is a string on the wire because the JSON is public surface and an unknown
/// value must fail loudly at the route rather than deep inside the renderer.
#[derive(Deserialize, Default)]
struct CaptchaRequest {
    #[serde(default)]
    mode: Option<String>,
}

/// `POST /v1/auth/captcha` — issue a fresh captcha. Anonymous; the rate limiter at the IP
/// tier is the cost gate, the same bucket every other bootstrap endpoint charges.
///
/// The response is the gate's own public view of the issued challenge: an id the client
/// echoes back, the rendered image as base64, the mode it was rendered in, and the
/// seconds the challenge stays valid. The answer exists nowhere in the response, in any
/// form — that is the whole point of rendering it into a picture.
///
/// `{"mode": "image_alt"}` asks for the accessible alternative: a fresh challenge with a
/// different random code and gentler rendering, for the user who could not read the
/// standard one. Refused with `FEATURE_DISABLED` when the deployment turned the
/// alternative off, because silently serving the standard mode to someone who just said
/// they cannot read it is the one wrong answer here.
async fn captcha(
    State(state): State<ApiState>,
    facts: RequestFacts,
    body: Option<Json<CaptchaRequest>>,
) -> Result<Json<migo_captcha::CaptchaChallengeView>, crate::ApiError> {
    charge_ip(&state, facts.ip, BOOTSTRAP_COST).await?;
    let requested = body
        .and_then(|Json(request)| request.mode)
        .unwrap_or_else(|| "image".to_string());
    let mode = match requested.as_str() {
        "image" => migo_captcha::CaptchaMode::Image,
        "image_alt" if state.policy().captcha_accessible_mode => {
            migo_captcha::CaptchaMode::ImageAlt
        }
        "image_alt" => {
            return Err(crate::ApiError::from(
                migo_protocol::fault::feature_disabled("captcha accessible mode"),
            ))
        }
        other => {
            return Err(crate::ApiError::from(migo_protocol::fault::validation(
                "mode",
                &format!("unknown captcha mode {other:?}: expected \"image\" or \"image_alt\""),
            )))
        }
    };
    let now = state.now();
    let challenge = state
        .authenticator()
        .issue_captcha(mode, now)
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

/// The body of `PUT /v1/auth/contact`: the one recoverable contact the caller
/// is recording. Exactly one of the two shapes — an email or a phone — and the
/// service's own validation is the judge of which; the route forwards the
/// string untouched.
#[derive(Deserialize)]
struct ContactBody {
    email_or_phone: String,
}

/// `PUT /v1/auth/contact` — record (or replace, or clear) the caller's
/// recoverable contact.
///
/// The wire contract the SDK's `updateContact` has spoken since the surface
/// was designed (`email_or_phone`, one string); the handler is the last piece
/// of that contract to exist. Idempotent by nature: the column holds one
/// contact, and a second PUT overwrites the first.
async fn set_contact(
    State(state): State<ApiState>,
    auth: Authenticated,
    Json(body): Json<ContactBody>,
) -> Result<StatusCode, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    state
        .authenticator()
        .set_contact(&auth.identity, &body.email_or_phone, &context)
        .await?;
    Ok(StatusCode::NO_CONTENT)
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
        .sign_out(&auth.identity, session_id, &context)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
