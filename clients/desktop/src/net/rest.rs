//! The REST half of the client: everything that happens before a WebSocket exists.
//!
//! Authentication is HTTP rather than protocol frames on purpose. A token has to be obtained before a
//! connection can be authenticated, so putting it on the gateway would mean an unauthenticated
//! connection that exists only to get a credential — one more state to reason about, and one more
//! thing an unauthenticated peer can hold open. HTTP already has the semantics: a request, a status
//! code, a body, and a connection that closes.

use std::time::Duration;

use base64::Engine as _;
use migo_core::{Id, Timestamp};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A REST failure, already reduced to something worth showing a person.
#[derive(Debug, thiserror::Error)]
pub enum RestError {
    #[error("cannot reach the server")]
    Transport,

    /// The server answered with its error envelope. The message is the server's own
    /// `public_message()`, which is the only string it ever puts on the wire — internal detail stays
    /// on the server by construction (brief section 161), so it is safe to show verbatim.
    ///
    /// `captcha` is the replacement challenge the server minted with this refusal, when it did: a
    /// submitted proof is spent whatever the verdict, so a refusal that carries one is the next
    /// challenge already in hand — no round trip to fetch what the server just offered. Boxed
    /// because the challenge carries a rendered PNG's worth of base64 and `Result<T, RestError>`
    /// crosses every call in this module — the error arm stays the size of a pointer, not of a
    /// picture.
    #[error("{message}")]
    Server {
        code: u32,
        symbol: String,
        message: String,
        captcha: Option<Box<CaptchaChallenge>>,
    },

    #[error("the server's answer was not in the expected form")]
    Malformed,

    #[error("that server address is not a valid URL")]
    BadUrl,
}

/// A device as described to the server at sign-in.
///
/// `device_id` is `None` on first registration and `Some` afterwards, which is what makes the server
/// reuse the same device row rather than issuing a new one on every launch. A new device id would mean
/// a new identity key, and every peer would see an unfamiliar device and a changed safety number.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<Id>,
    pub platform: String,
    pub display_name: String,
    pub app_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
}

impl DeviceRequest {
    /// Describes this machine, honestly but minimally.
    ///
    /// The OS name goes in; the kernel build, the hostname and the username do not. A device list is
    /// a security feature — it lets someone spot a session they do not recognise — and "Migo Desktop
    /// on Linux" serves that purpose. The fine-grained version string only serves fingerprinting.
    pub fn describe(device_id: Option<Id>) -> Self {
        Self {
            device_id,
            platform: "desktop".to_owned(),
            display_name: format!("Migo Desktop ({})", os_name()),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            os_version: Some(os_name().to_owned()),
            device_model: None,
        }
    }
}

const fn os_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "Linux"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "Desktop"
    }
}

#[derive(Debug, Serialize)]
struct RegisterRequest<'a> {
    username: &'a str,
    passphrase: &'a str,
    locale: &'a str,
    device: DeviceRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    captcha: Option<CaptchaProof<'a>>,
    /// The ML-DSA-65 public key, standard base64, when the registering device holds the root.
    #[serde(skip_serializing_if = "Option::is_none")]
    identity_public_key: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct LoginRequest<'a> {
    identifier: &'a str,
    passphrase: &'a str,
    device: DeviceRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    captcha: Option<CaptchaProof<'a>>,
}

#[derive(Debug, Serialize)]
struct RefreshRequest<'a> {
    refresh_token: &'a str,
    device_id: Id,
}

#[derive(Debug, Serialize)]
struct LogoutRequest {
    session_id: Id,
}

/// The body of a captcha request: empty, or carrying a mode.
///
/// An absent mode is deliberately not `"image"`: it asks the server for its default rendering,
/// which keeps this client correct if a deployment ever changes which challenge it leads with.
#[derive(Debug, Serialize)]
struct CaptchaRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'a str>,
}

/// The proof a register or sign-in body carries: the id the server issued and the characters the
/// user read off the picture.
///
/// Borrowed, like every other request body in this file, because it only has to outlive the
/// serialisation. The manual `Debug` keeps the answer out of traces: the server stores nothing
/// but a tag of it, and the client has no business being more careless with it than that.
#[derive(Serialize)]
pub struct CaptchaProof<'a> {
    /// The id of the challenge being answered.
    pub challenge_id: &'a str,
    /// What the user read off the image, already normalised by the form that collected it —
    /// upper-cased, whitespace-free. The server normalises again before comparing, so this is a
    /// courtesy, not a correctness requirement.
    pub answer: &'a str,
}

impl std::fmt::Debug for CaptchaProof<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptchaProof")
            .field("challenge_id", &self.challenge_id)
            .field("answer", &"***")
            .finish()
    }
}

/// A captcha challenge as the auth server issues it: a picture, and the id that answers it.
///
/// The picture is the whole question — nothing in this struct describes what it shows, so a
/// response body is not a solved captcha no matter who reads it. The manual `Debug` abbreviates
/// the base64 for a different reason: it is not secret, but a multi-kilobyte wall of text has
/// never made a trace line more readable.
#[derive(Clone, Deserialize)]
pub struct CaptchaChallenge {
    /// The id to echo back as the proof's `challenge_id` when this challenge is answered.
    pub challenge_id: String,
    /// The rendered challenge: standard base64 with padding, wrapping a PNG this client decodes
    /// and uploads as a texture.
    pub image_png_base64: String,
    /// The rendering mode (`"image"` or `"image_alt"`), echoed so a refresh can ask for the
    /// same kind again — the alternative rendering is gentler, and someone who needed it once
    /// will need it again.
    pub mode: String,
    /// How many seconds the challenge stays answerable. Informational only: the server is the
    /// arbiter of expiry, and a client-side countdown would just guess at the server's clock.
    pub ttl_seconds: u32,
}

impl std::fmt::Debug for CaptchaChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptchaChallenge")
            .field("challenge_id", &self.challenge_id)
            .field(
                "image_png_base64",
                &format!("<{} chars>", self.image_png_base64.len()),
            )
            .field("mode", &self.mode)
            .field("ttl_seconds", &self.ttl_seconds)
            .finish()
    }
}

/// A session, as the server issues it.
///
/// Never logged and never written anywhere but the sealed vault. The `Debug` impl below is what makes
/// that a property of the type rather than a rule everyone has to remember.
#[derive(Clone, Deserialize)]
pub struct Grant {
    pub account_id: Id,
    pub device_id: Id,
    pub session_id: Id,
    pub access_token: String,
    pub refresh_token: String,
}

// The response carries expiry timestamps, a capability mask and an is-new-account flag as well. They
// are deliberately not fields here: serde ignores what it is not asked for, and a field this client
// never reads is a field that will drift out of step with the server without anything noticing.

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Ids identify rows and are fine in a log; the two token fields are credentials and are not
        // (brief sections 77 and 174). A derived Debug would have put both in the first trace line
        // somebody added.
        f.debug_struct("Grant")
            .field("account_id", &self.account_id)
            .field("device_id", &self.device_id)
            .field("session_id", &self.session_id)
            .field("access_token", &"***")
            .field("refresh_token", &"***")
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: u32,
    symbol: String,
    message: String,
    /// The fresh challenge a captcha-gated refusal carries, when it carries one. Absent on
    /// every other envelope, and `default`ed so an older server's refusals parse unchanged.
    #[serde(default)]
    captcha: Option<CaptchaChallenge>,
}

/// One device of the signed-in account, as `GET /v1/auth/sessions` reports it.
///
/// Every field but the id is optional on purpose: the listing is a security feature the user
/// reads to spot a session they do not recognise, and a missing field should read as "not
/// disclosed" rather than fail the whole list. `current` is the server's own marker for the
/// session making the request.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionSummary {
    pub session_id: Id,
    #[serde(default)]
    pub device: Option<DeviceSummary>,
    #[serde(default)]
    pub created_at: Option<Timestamp>,
    #[serde(default)]
    pub last_active_at: Option<Timestamp>,
    #[serde(default)]
    pub current: bool,
}

/// The device half of a session row.
///
/// Only the fields this client reads are declared: serde ignores what it is not asked for, and a
/// field that is never read is a field that drifts out of step with the server without anything
/// noticing. `platform` is the fallback name when the row carries no display name.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceSummary {
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionsBody {
    #[serde(default)]
    sessions: Vec<SessionSummary>,
}

// --- the account-root surface: wire shapes -----------------------------------

/// A challenge for one of the ML-DSA ceremonies, as the challenge endpoint issues it.
///
/// `payload` is the canonical encoding, base64 — the bytes to sign exactly as received, never
/// re-encoded, which is what keeps three ports from disagreeing about what was signed.
#[derive(Debug, Clone, Deserialize)]
pub struct IdentityChallenge {
    /// The bytes to sign, base64.
    pub payload: String,
    pub challenge_id: Id,
}

/// One device of the account, as `GET /v1/devices` reports it — the caller's own security screen.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountDevice {
    pub device_id: Id,
    pub display_name: String,
    pub platform: String,
    /// `active`, `pending`, or `revoked`.
    pub status: String,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
    /// Whether this device can take part in the ML-DSA login ceremony.
    pub has_credential: bool,
    pub is_current: bool,
}

/// The answer to a device revoke: how many sessions ended with the device.
#[derive(Debug, Deserialize)]
pub struct DeviceRevoked {
    pub revoked: u64,
}

/// One registered wallet, as the wallet endpoints report it. Address and metadata only — the
/// private key behind it never left the device that derived it.
///
/// The response carries the chain type and timestamps as well; they are deliberately not fields
/// here, for the same reason `Grant`'s extras are not: a field this client never reads is a field
/// that drifts out of step with the server without anything noticing.
#[derive(Debug, Clone, Deserialize)]
pub struct RegisteredWallet {
    pub wallet_id: Id,
    /// Canonical lowercase hex, no prefix.
    pub address: String,
    #[serde(default)]
    pub label: Option<String>,
    pub derivation_index: i32,
    pub status: String,
}

#[derive(Debug, Serialize)]
struct IdentityChallengeBody<'a> {
    purpose: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    identifier: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<&'a DeviceRequest>,
}

#[derive(Debug, Serialize)]
struct IdentityLoginBody<'a> {
    challenge_id: Id,
    identity_signature: &'a str,
    device_signature: &'a str,
}

#[derive(Debug, Serialize)]
struct AddDeviceBody<'a> {
    challenge_id: Id,
    identity_signature: &'a str,
    device_public_key: &'a str,
    device_signature: &'a str,
}

#[derive(Debug, Serialize)]
struct PublishKeyBody<'a> {
    identity_public_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_public_key: Option<String>,
}

#[derive(Debug, Serialize)]
struct WalletBody<'a> {
    address: &'a str,
    chain_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
    derivation_index: i32,
}

#[derive(Debug, Serialize)]
struct PassphraseBody<'a> {
    current_passphrase: &'a str,
    new_passphrase: &'a str,
}

#[derive(Debug, Serialize)]
struct ContactBody<'a> {
    email_or_phone: &'a str,
}

#[derive(Debug, Deserialize)]
struct DevicesBody {
    #[serde(default)]
    devices: Vec<AccountDevice>,
}

#[derive(Debug, Deserialize)]
struct WalletsBody {
    #[serde(default)]
    wallets: Vec<RegisteredWallet>,
}

/// One appointed global admin, as the owner's list renders it. Wire field names are the
/// server's own (`granted_at_ms`), the same compromise every summary struct here makes so the
/// JSON the panel sees matches the API it came from.
#[derive(Debug, Clone, Deserialize)]
pub struct AdminView {
    pub account_id: Id,
    pub username: String,
    /// Who appointed them — always the Owner/CEO in this version.
    #[expect(
        dead_code,
        reason = "the pane names the appointer in prose, not per row"
    )]
    pub granted_by: Id,
    /// When the grant happened, in milliseconds since the epoch.
    pub granted_at_ms: i64,
}

/// What the caller may open of the admin surface. Owner comes from configuration, not data —
/// the deployment names its Owner/CEO — so `owner: false` is an answer the client can act on
/// (hide the surface) rather than a failure to catch. `admin` is unread here on purpose: the
/// pane gates on the owner bit and the admin bit is the server's own concern, carried anyway
/// because the wire sends it and a struct that dropped it would lie about the answer's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct AdminStanding {
    pub owner: bool,
    pub admin: bool,
}

/// Standard base64 with padding, the form every account-root endpoint speaks.
fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// An HTTP client bound to one server.
pub struct Rest {
    http: reqwest::Client,
    base: String,
}

impl Rest {
    /// Builds a client for `base_url`, e.g. `https://migo.example` or `http://127.0.0.1:8080`.
    pub fn new(base_url: &str) -> Result<Self, RestError> {
        let base = base_url.trim_end_matches('/').to_owned();
        if base.is_empty() || !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(RestError::BadUrl);
        }
        let http = reqwest::Client::builder()
            // A person waiting on a sign-in wants an answer or an error, not a spinner. Ten seconds
            // is long enough for Argon2 on the server plus a slow link, and short enough that a
            // black-holed connection surfaces as a failure rather than a hang.
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("migo-desktop/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| RestError::Transport)?;
        Ok(Self { http, base })
    }

    /// The server this client talks to.
    #[must_use]
    #[allow(dead_code)] // Used once the auth form reads the persisted base back to confirm.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The gateway URL for this server: the same host, `ws`/`wss`, path `/ws`.
    ///
    /// Derived rather than configured separately so a user who typed one address cannot end up with a
    /// client whose API and gateway point at different deployments.
    #[must_use]
    #[allow(dead_code)] // Used once the gateway worker is built off the persisted endpoint.
    pub fn gateway_url(&self) -> String {
        let rest = self
            .base
            .strip_prefix("https://")
            .map(|host| ("wss://", host));
        let (scheme, host) = rest
            .or_else(|| {
                self.base
                    .strip_prefix("http://")
                    .map(|host| ("ws://", host))
            })
            .unwrap_or(("wss://", self.base.as_str()));
        format!("{scheme}{host}/ws")
    }

    /// Fetches a fresh image captcha challenge.
    ///
    /// Anonymous by design: the gate exists to judge the very first contact, so there is no
    /// credential to offer. `mode` is `None` for the server's default rendering and
    /// `Some("image_alt")` for the gentler one a user asks for when they cannot read the first
    /// picture — the alternative is a fresh challenge, never the same code read aloud.
    pub async fn request_captcha(&self, mode: Option<&str>) -> Result<CaptchaChallenge, RestError> {
        self.post("/v1/auth/captcha", &CaptchaRequest { mode })
            .await
    }

    /// Creates an account and a first device.
    ///
    /// `captcha` is the proof for the challenge the form fetched; `None` sends no proof at all,
    /// which a server with the gate on answers with `CAPTCHA_REQUIRED`. The caller decides what
    /// that refusal means to the user.
    ///
    /// `identity_public_key` is the account identity's ML-DSA-65 public key when this device
    /// already holds the root it is founding the account with. Sending it is what makes the
    /// registration idempotent (brief §12): a retry carrying the same key reconciles into the
    /// account the first attempt already made. `None` registers the passphrase-only account.
    pub async fn register(
        &self,
        username: &str,
        passphrase: &str,
        device: DeviceRequest,
        captcha: Option<CaptchaProof<'_>>,
        identity_public_key: Option<&[u8]>,
    ) -> Result<Grant, RestError> {
        let identity_public_key = identity_public_key.map(b64);
        let body = RegisterRequest {
            username,
            passphrase,
            locale: "en",
            device,
            captcha,
            identity_public_key: identity_public_key.as_deref(),
        };
        self.post("/v1/auth/register", &body).await
    }

    /// Signs in an existing account.
    ///
    /// `captcha` as for [`Self::register`]: the proof for a challenge the form fetched, or
    /// `None` to submit without one.
    pub async fn login(
        &self,
        identifier: &str,
        passphrase: &str,
        device: DeviceRequest,
        captcha: Option<CaptchaProof<'_>>,
    ) -> Result<Grant, RestError> {
        let body = LoginRequest {
            identifier,
            passphrase,
            device,
            captcha,
        };
        self.post("/v1/auth/login", &body).await
    }

    /// Exchanges a saved refresh token for a fresh pair.
    pub async fn refresh(&self, refresh_token: &str, device_id: Id) -> Result<Grant, RestError> {
        let body = RefreshRequest {
            refresh_token,
            device_id,
        };
        self.post("/v1/auth/refresh", &body).await
    }

    /// Ends a session server-side.
    ///
    /// Failure is the caller's to ignore: the local keys are already gone by the time this runs, so a
    /// server that cannot be reached does not make the sign-out any less complete on this device.
    pub async fn logout(&self, access_token: &str, session_id: Id) -> Result<(), RestError> {
        let url = format!("{}/v1/auth/logout", self.base);
        let response = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .json(&LogoutRequest { session_id })
            .send()
            .await
            .map_err(|_| RestError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.failure(response).await)
        }
    }

    /// Lists every session of the signed-in account: `GET /v1/auth/sessions`.
    ///
    /// Used by the settings screen's device list. A server that does not expose the route
    /// surfaces as an ordinary [`RestError`] — the panel shows the message rather than an empty
    /// list, because "no other devices" and "could not check" are different facts and only one
    /// of them is reassuring.
    pub async fn sessions(&self, access_token: &str) -> Result<Vec<SessionSummary>, RestError> {
        let url = format!("{}/v1/auth/sessions", self.base);
        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| RestError::Transport)?;
        if !response.status().is_success() {
            return Err(self.failure(response).await);
        }
        response
            .json::<SessionsBody>()
            .await
            .map(|body| body.sessions)
            .map_err(|_| RestError::Malformed)
    }

    /// Ends one session of the signed-in account by id: `DELETE /v1/auth/sessions/{id}`.
    ///
    /// Revoking the session this request rides on is the server's business to refuse or honour;
    /// the settings panel never offers the button for it, because sign-out is the honest name
    /// for that action.
    pub async fn revoke_session(
        &self,
        access_token: &str,
        session_id: Id,
    ) -> Result<(), RestError> {
        let url = format!("{}/v1/auth/sessions/{}", self.base, session_id.to_text());
        let response = self
            .http
            .delete(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| RestError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.failure(response).await)
        }
    }

    // --- the account-root surface ---------------------------------------------

    /// Asks for the ML-DSA login ceremony's payload: `POST /v1/auth/identity/challenge`
    /// with purpose `login`.
    ///
    /// `identifier` names the account and `device_id` names *this* device — the one whose
    /// credential will co-sign. The server answers identically whether the pair is real, so a
    /// stranger probing for usernames learns nothing from the shape of the reply.
    pub async fn identity_login_challenge(
        &self,
        identifier: &str,
        device_id: Id,
    ) -> Result<IdentityChallenge, RestError> {
        self.post(
            "/v1/auth/identity/challenge",
            &IdentityChallengeBody {
                purpose: "login",
                identifier: Some(identifier),
                device_id: Some(device_id),
                account_id: None,
                device: None,
            },
        )
        .await
    }

    /// Asks for the add-device ceremony's payload: `POST /v1/auth/identity/challenge`
    /// with purpose `add-device`.
    ///
    /// `account_id` is the id the `.migo` container carried; `device` describes the machine the
    /// account is being restored onto, which the server registers as pending until the answer
    /// proves the identity signature.
    pub async fn identity_add_device_challenge(
        &self,
        account_id: Id,
        device: &DeviceRequest,
    ) -> Result<IdentityChallenge, RestError> {
        self.post(
            "/v1/auth/identity/challenge",
            &IdentityChallengeBody {
                purpose: "add-device",
                identifier: None,
                device_id: None,
                account_id: Some(account_id),
                device: Some(device),
            },
        )
        .await
    }

    /// Answers a login challenge with both signatures: `POST /v1/auth/identity/login`.
    ///
    /// The identity signature is the account half of the ceremony and the device signature is the
    /// device half; the server requires both, which is what makes a root secret leaked from a
    /// backup alone insufficient to sign in as a registered device.
    pub async fn identity_login(
        &self,
        challenge_id: Id,
        identity_signature: &[u8],
        device_signature: &[u8],
    ) -> Result<Grant, RestError> {
        self.post(
            "/v1/auth/identity/login",
            &IdentityLoginBody {
                challenge_id,
                identity_signature: &b64(identity_signature),
                device_signature: &b64(device_signature),
            },
        )
        .await
    }

    /// Answers an add-device challenge: `POST /v1/auth/identity/add-device`.
    ///
    /// Carries the identity signature plus the *new* device's credential public key and its
    /// signature over the same payload, which is the proof that activates the pending device row.
    pub async fn identity_add_device(
        &self,
        challenge_id: Id,
        identity_signature: &[u8],
        device_public_key: &[u8],
        device_signature: &[u8],
    ) -> Result<Grant, RestError> {
        self.post(
            "/v1/auth/identity/add-device",
            &AddDeviceBody {
                challenge_id,
                identity_signature: &b64(identity_signature),
                device_public_key: &b64(device_public_key),
                device_signature: &b64(device_signature),
            },
        )
        .await
    }

    /// Publishes the caller's identity (and optionally device) public key:
    /// `POST /v1/auth/identity/key`.
    ///
    /// The legacy upgrade door, idempotent by design — a retry sends the same keys and the server
    /// reconciles to the rows that already exist, so the worker can call it after every passphrase
    /// sign-in on a device that holds a root, without first asking whether it already did.
    pub async fn publish_identity_key(
        &self,
        access_token: &str,
        identity_public_key: &[u8],
        device_public_key: Option<&[u8]>,
    ) -> Result<(), RestError> {
        self.auth_expect_empty(
            access_token,
            "/v1/auth/identity/key",
            reqwest::Method::POST,
            &PublishKeyBody {
                identity_public_key: &b64(identity_public_key),
                device_public_key: device_public_key.map(b64),
            },
        )
        .await
    }

    /// The caller's own devices: `GET /v1/devices`.
    pub async fn devices(&self, access_token: &str) -> Result<Vec<AccountDevice>, RestError> {
        let body = self
            .auth_json::<(), DevicesBody>(access_token, "/v1/devices", reqwest::Method::GET, &())
            .await?;
        Ok(body.devices)
    }

    /// The caller's registered wallet addresses: `GET /v1/wallets`.
    pub async fn wallets(&self, access_token: &str) -> Result<Vec<RegisteredWallet>, RestError> {
        let body = self
            .auth_json::<(), WalletsBody>(access_token, "/v1/wallets", reqwest::Method::GET, &())
            .await?;
        Ok(body.wallets)
    }

    /// Registers (or idempotently re-registers) a wallet address: `PUT /v1/wallets`.
    pub async fn register_wallet(
        &self,
        access_token: &str,
        address: &str,
        derivation_index: i32,
        label: Option<&str>,
    ) -> Result<RegisteredWallet, RestError> {
        self.auth_json(
            access_token,
            "/v1/wallets",
            reqwest::Method::PUT,
            &WalletBody {
                address,
                chain_type: "evm",
                label,
                derivation_index,
            },
        )
        .await
    }

    /// Archives one of the caller's wallets: `POST /v1/wallets/{id}`, answered 204.
    pub async fn archive_wallet(&self, access_token: &str, wallet_id: Id) -> Result<(), RestError> {
        self.auth_expect_empty(
            access_token,
            &format!("/v1/wallets/{}", wallet_id.to_text()),
            reqwest::Method::POST,
            &(),
        )
        .await
    }

    /// Removes one of the caller's devices: `POST /v1/devices/{id}/revoke`.
    ///
    /// The device's sessions end with it — brief section 18 — so the answer says how many died,
    /// which is the confirmation a security screen owes the person who just said "this phone is
    /// gone".
    pub async fn revoke_device(
        &self,
        access_token: &str,
        device_id: Id,
    ) -> Result<DeviceRevoked, RestError> {
        self.auth_json(
            access_token,
            &format!("/v1/devices/{}/revoke", device_id.to_text()),
            reqwest::Method::POST,
            &(),
        )
        .await
    }

    /// Changes the account's sign-in passphrase: `POST /v1/auth/passphrase`.
    ///
    /// The answer is a fresh grant, because the server ends every session of the account — this
    /// one included — and hands the caller the replacement. The worker adopts it the moment it
    /// lands: the access token that paid for the change is already retired when the answer
    /// arrives.
    pub async fn change_passphrase(
        &self,
        access_token: &str,
        current_passphrase: &str,
        new_passphrase: &str,
    ) -> Result<Grant, RestError> {
        self.auth_json(
            access_token,
            "/v1/auth/passphrase",
            reqwest::Method::POST,
            &PassphraseBody {
                current_passphrase,
                new_passphrase,
            },
        )
        .await
    }

    /// Records (or replaces) the caller's recoverable contact: `PUT /v1/auth/contact`, answered
    /// 204.
    ///
    /// One string, and the server is the judge of the shape: an email containing `@` or a phone
    /// starting with `+`, normalised on arrival so the store's unique index sees one canonical
    /// value rather than every user's first guess.
    pub async fn set_contact(
        &self,
        access_token: &str,
        email_or_phone: &str,
    ) -> Result<(), RestError> {
        self.auth_expect_empty(
            access_token,
            "/v1/auth/contact",
            reqwest::Method::PUT,
            &ContactBody { email_or_phone },
        )
        .await
    }

    /// What the caller may open of the admin surface: `GET /v1/admins/whoami`.
    ///
    /// Never fails on standing — an account that is neither owner nor admin gets
    /// `{owner: false, admin: false}`, which is the answer, not an error — so a client that
    /// asks on sign-in can decide whether its owner surface exists without a refusal to catch.
    pub async fn admin_standing(&self, access_token: &str) -> Result<AdminStanding, RestError> {
        self.auth_json(access_token, "/v1/admins/whoami", reqwest::Method::GET, &())
            .await
    }

    /// Every global admin, with usernames resolved: `GET /v1/admins`. Owner-only — the server
    /// refuses it for anybody else, and that refusal is the list's own security rather than
    /// something this client adds on top.
    pub async fn global_admins(&self, access_token: &str) -> Result<Vec<AdminView>, RestError> {
        #[derive(Deserialize)]
        struct AdminsBody {
            #[serde(default)]
            admins: Vec<AdminView>,
        }
        let body: AdminsBody = self
            .auth_json(access_token, "/v1/admins", reqwest::Method::GET, &())
            .await?;
        Ok(body.admins)
    }

    /// Appoints a global admin by username: `PUT /v1/admins`, idempotent. Owner-only.
    pub async fn grant_global_admin(
        &self,
        access_token: &str,
        username: &str,
    ) -> Result<AdminView, RestError> {
        #[derive(Serialize)]
        struct GrantBody<'a> {
            username: &'a str,
        }
        self.auth_json(
            access_token,
            "/v1/admins",
            reqwest::Method::PUT,
            &GrantBody { username },
        )
        .await
    }

    /// Revokes a global admin: `DELETE /v1/admins/{id}`, answered 204. Owner-only. Revoking an
    /// account that is not one is a quiet 204 — the same shape rule the wallet archive follows,
    /// so the list that follows a revoke is the truth rather than the echo.
    pub async fn revoke_global_admin(
        &self,
        access_token: &str,
        account_id: Id,
    ) -> Result<(), RestError> {
        self.auth_expect_empty(
            access_token,
            &format!("/v1/admins/{}", account_id.to_text()),
            reqwest::Method::DELETE,
            &(),
        )
        .await
    }

    /// One authenticated request that answers with a JSON body.
    async fn auth_json<B: Serialize, T: DeserializeOwned>(
        &self,
        access_token: &str,
        path: &str,
        method: reqwest::Method,
        body: &B,
    ) -> Result<T, RestError> {
        let url = format!("{}{path}", self.base);
        let response = self
            .http
            .request(method, url)
            .bearer_auth(access_token)
            .json(body)
            .send()
            .await
            .map_err(|_| RestError::Transport)?;
        if !response.status().is_success() {
            return Err(self.failure(response).await);
        }
        response.json::<T>().await.map_err(|_| RestError::Malformed)
    }

    /// One authenticated request that answers 204 and nothing else.
    async fn auth_expect_empty<B: Serialize>(
        &self,
        access_token: &str,
        path: &str,
        method: reqwest::Method,
        body: &B,
    ) -> Result<(), RestError> {
        let url = format!("{}{path}", self.base);
        let response = self
            .http
            .request(method, url)
            .bearer_auth(access_token)
            .json(body)
            .send()
            .await
            .map_err(|_| RestError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.failure(response).await)
        }
    }

    async fn post<B: Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, RestError> {
        let url = format!("{}{path}", self.base);
        let response = self
            .http
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|_| RestError::Transport)?;
        if !response.status().is_success() {
            return Err(self.failure(response).await);
        }
        response.json::<T>().await.map_err(|_| RestError::Malformed)
    }

    /// Turns a non-success response into the best error it can.
    ///
    /// A well-formed envelope gives the server's own public message. Anything else — a proxy's HTML
    /// error page, a truncated body — becomes `Malformed`, because showing a fragment of someone
    /// else's error page to a user explains nothing.
    async fn failure(&self, response: reqwest::Response) -> RestError {
        let status = response.status().as_u16();
        match response.json::<ErrorEnvelope>().await {
            Ok(envelope) => RestError::Server {
                code: envelope.error.code,
                symbol: envelope.error.symbol,
                message: envelope.error.message,
                captcha: envelope.error.captcha.map(Box::new),
            },
            Err(_) => {
                if status >= 500 {
                    RestError::Transport
                } else {
                    RestError::Malformed
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal envelope may carry the replacement challenge the server minted with it, and
    /// an older server's refusals arrive without the field at all — both must parse. The
    /// mapping into `RestError::Server` is three field moves and a box, so parsing is the part
    /// that can rot.
    #[test]
    fn the_error_body_parses_a_replacement_captcha_and_tolerates_its_absence() {
        let with = serde_json::from_str::<ErrorEnvelope>(
            r#"{"error":{"code":1306,"symbol":"USERNAME_TAKEN","message":"that name is taken",
                "captcha":{"challenge_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "image_png_base64":"aGVsbG8=","mode":"image","ttl_seconds":120}}}"#,
        )
        .expect("an envelope with a captcha parses");
        let challenge = with.error.captcha.expect("the captcha crossed");
        assert_eq!(challenge.challenge_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(challenge.image_png_base64, "aGVsbG8=");
        assert_eq!(challenge.mode, "image");
        assert_eq!(challenge.ttl_seconds, 120);

        let without = serde_json::from_str::<ErrorEnvelope>(
            r#"{"error":{"code":1300,"symbol":"VALIDATION_FAILED","message":"no"}}"#,
        )
        .expect("an envelope without a captcha parses");
        assert!(without.error.captcha.is_none());
    }
}
