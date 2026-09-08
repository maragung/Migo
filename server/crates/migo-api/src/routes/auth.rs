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

use migo_auth::{DeviceClaim, Gender, Grant, Refresh, Registration, ServerEndpoint, SignIn};
use migo_core::{Id, Secret};
use migo_protocol::Platform;

use crate::extract::{Authenticated, RequestFacts};
use crate::ratelimit::charge_ip;
use crate::ApiState;

/// One unit charged against the caller's network bucket per bootstrap attempt.
const BOOTSTRAP_COST: u32 = 1;

/// The delivery channel a recovery row travels to its account's owner.
///
/// The recovery flow's shape: `request_recovery` mints a row holding the
/// token id and the HMAC tag the owner later proves possession of. Neither
/// half may cross the response wire — the caller at that point is an
/// unauthenticated stranger, and handing them the tag hands them the
/// account-reset token itself — so the row leaves through this port instead,
/// to wherever the deployment can actually reach the owner: an email
/// channel, a push provider, an operator console. What the port must NOT do
/// is write the tag into a log or a metric; the doc comment on
/// [`RecoveryDelivery::deliver`] carries the constraint the same way
/// `migo_notify`'s `Wakeup` does.
///
/// `None` in [`ApiServices`](crate::ApiServices) means the deployment has
/// no channel, and the request route refuses with `FEATURE_DISABLED`
/// rather than minting a row nobody can ever confirm — the honest refusal
/// the dead-end audit finding asked for.
#[async_trait::async_trait]
pub trait RecoveryDelivery: Send + Sync {
    /// Carries one freshly-minted row to the account behind it.
    ///
    /// Returns `Err` only when the channel is down — the caller surfaces
    /// that to the requester, because a row that was not delivered is a
    /// flow that cannot complete. Success means handed to the channel,
    /// not read by the human; the row's own one-hour expiry is what bounds
    /// the ambiguity. Implementations must never place the tag in a log,
    /// a metric label, or any store the owner can be probed through.
    async fn deliver(&self, row: &migo_auth::RecoveryRow) -> migo_core::Result<()>;
}

/// A delivery channel that accepts every row and tells nobody.
///
/// The default handle for a deployment that has not configured a channel
/// — the stand-in the composition root uses for local development, where
/// the recovery flow is exercised end to end through a test seam rather
/// than an inbox. Reports success so the route's `ok` reflects the row
/// existing, which is the honest answer for a dev machine; production
/// either wires a real channel or leaves the port `None` and gets the
/// refusal.
#[derive(Clone, Copy, Debug, Default)]
pub struct SinkDelivery;

#[async_trait::async_trait]
impl RecoveryDelivery for SinkDelivery {
    async fn deliver(&self, _row: &migo_auth::RecoveryRow) -> migo_core::Result<()> {
        Ok(())
    }
}

/// The shared, erased form the API state holds.
pub type SharedRecoveryDelivery = std::sync::Arc<dyn RecoveryDelivery>;

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
            .route("/passphrase", post(change_passphrase))
            .route("/contact", get(contact_flag).put(set_contact))
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
    passphrase: String,
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
    passphrase: String,
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
    let had_captcha = body.captcha.is_some();
    let registration = Registration {
        username: body.username,
        email: body.email,
        phone: body.phone,
        passphrase: Secret::new(body.passphrase),
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
    let grant = match state.authenticator().register(registration, &context).await {
        Ok(grant) => grant,
        Err(error) => return Err(with_fresh_captcha(&state, had_captcha, error).await),
    };
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
    let had_captcha = body.captcha.is_some();
    let sign_in = SignIn {
        identifier: body.identifier,
        passphrase: Secret::new(body.passphrase),
        device: body.device.into_claim()?,
        captcha: body.captcha.map(migo_auth::CaptchaProof::from),
        server,
    };
    let context = facts.context(now);
    let grant = match state.authenticator().sign_in(sign_in, &context).await {
        Ok(grant) => grant,
        Err(error) => return Err(with_fresh_captcha(&state, had_captcha, error).await),
    };
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

/// Attaches a replacement captcha to a refused bootstrap attempt, when the refusal is one a
/// captcha form will retry against.
///
/// A submitted proof is spent the moment the gate reads it — the challenge row is deleted on
/// use, right or wrong — so the form that just watched its attempt fail is holding a dead
/// challenge id. The refresh control was the user's only way forward; this makes the refusal
/// itself carry the next challenge, so the form swaps the picture on the spot and the retry
/// starts from a live id with no extra round trip.
///
/// Attached in exactly two situations, both of which mean a captcha was on the user's screen:
///
/// - The attempt carried a proof. The proof is spent whatever the refusal says, so the widget's
///   challenge is dead even when the refusal is about something else (a taken username, a weak
///   passphrase) — the common case, and the one the refresh click existed for.
/// - The refusal is the gate's own (`CAPTCHA_REQUIRED`, `INVALID_CAPTCHA`, `CAPTCHA_EXPIRED`):
///   the client is being told to go get a challenge, and the challenge arrives in the same
///   response.
///
/// Everything else — a wrong-passphrase login from a network that never tripped the gate, a
/// malformed body — never showed the user a captcha, so there is nothing to reload. A disabled
/// captcha service mints nothing and the refusal crosses as it always did.
async fn with_fresh_captcha(
    state: &ApiState,
    had_proof: bool,
    error: migo_core::Error,
) -> crate::ApiError {
    const GATE_CODES: &[u32] = &[
        migo_protocol::codes::CAPTCHA_REQUIRED,
        migo_protocol::codes::INVALID_CAPTCHA,
        migo_protocol::codes::CAPTCHA_EXPIRED,
    ];
    if !had_proof && !GATE_CODES.contains(&error.code()) {
        return crate::ApiError::from(error);
    }
    match state
        .authenticator()
        .issue_captcha(migo_captcha::CaptchaMode::Image, state.now())
        .await
    {
        Some(challenge) => crate::ApiError::from(error).with_captcha(challenge),
        None => crate::ApiError::from(error),
    }
}

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

/// `POST /v1/auth/recovery/request` — start a passphrase-recovery flow.
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
    // The row the service mints is the delivery envelope: the token id the
    // user comes back with and the tag they must prove possession of. It is
    // never put on this wire — a response carrying the tag would make the
    // unauthenticated caller the owner of an account-reset token, which is
    // account takeover by construction — so the route hands the envelope to
    // the delivery port the composition root wired. The port is exactly the
    // seam the audit's dead-end finding named: without it the row is minted,
    // persisted, and known to nobody, and the confirm route demands a tag
    // that was never delivered. A deployment with no port refuses here, at
    // the request, rather than answering `ok` over a flow that cannot
    // complete — an honest refusal instead of a dead end.
    let Some(delivery) = state.recovery_delivery() else {
        return Err(crate::ApiError::from(
            migo_protocol::fault::feature_disabled("recovery delivery"),
        ));
    };
    let row = match state
        .authenticator()
        .request_recovery(&body.identifier, &captcha, &context)
        .await
    {
        Ok(row) => row,
        Err(error) => return Err(with_fresh_captcha(&state, true, error).await),
    };
    // A row for an unknown identifier never persisted — `request_recovery`
    // mints it to keep the work identical — but a real one is already in the
    // store, so a delivery failure must not leave the answer `ok` over a
    // token the user will never see. The map is one arm: the row is either
    // delivered and the request answers `ok`, or the request fails and the
    // row expires on its own within the hour.
    if let Err(error) = delivery.deliver(&row).await {
        return Err(crate::ApiError::from(error));
    }
    Ok(Json(RecoveryRequestResponse { ok: true }))
}

#[derive(Deserialize)]
struct RecoveryConfirmBody {
    token_id: migo_core::Id,
    /// The hex-encoded HMAC tag the request route issued.
    tag: String,
    new_passphrase: String,
}
async fn recovery_confirm(
    State(state): State<ApiState>,
    facts: RequestFacts,
    Json(body): Json<RecoveryConfirmBody>,
) -> Result<Json<RecoveryRequestResponse>, crate::ApiError> {
    // The edge charge the service's own attempt price stacks on: the route
    // is an unauthenticated bootstrap surface, and every other one charges
    // this cost before the domain call. The service charges the attempt
    // (and the failure surcharge) itself, so a confirm costs an
    // attempt-shaped price the way a sign-in does — both on the shared
    // network bucket this charges and on the per-network anonymous surface.
    charge_ip(&state, facts.ip, BOOTSTRAP_COST).await?;
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
            &migo_core::Secret::new(body.new_passphrase),
            &context,
        )
        .await?;
    Ok(Json(RecoveryRequestResponse { ok: true }))
}

// --- passphrase, sessions, contact ------------------------------------------

#[derive(Deserialize)]
struct ChangePassphraseBody {
    current_passphrase: String,
    new_passphrase: String,
}
async fn change_passphrase(
    State(state): State<ApiState>,
    auth: Authenticated,
    Json(body): Json<ChangePassphraseBody>,
) -> Result<Json<GrantResponse>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let change = migo_auth::PassphraseChange {
        current: migo_core::Secret::new(body.current_passphrase),
        next: migo_core::Secret::new(body.new_passphrase),
    };
    let grant = state
        .authenticator()
        .change_passphrase(&auth.identity, change, &context)
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

/// The response of `GET /v1/auth/contact`: one flag, and nothing else.
#[derive(Serialize)]
struct ContactFlagResponse {
    configured: bool,
}

/// `GET /v1/auth/contact` — whether a recovery contact is set on the
/// caller's account.
///
/// Deliberately a boolean and never the contact itself: the web UI's
/// standing line is "your current email is never shown here", and the
/// screen asking this question needs to know whether to nag, not what it
/// would nag about. The value stays server-side; only the fact crosses.
async fn contact_flag(
    State(state): State<ApiState>,
    auth: Authenticated,
) -> Result<Json<ContactFlagResponse>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let configured = state
        .authenticator()
        .has_contact(&auth.identity, &context)
        .await?;
    Ok(Json(ContactFlagResponse { configured }))
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
