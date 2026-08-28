//! The REST half of the client: everything that happens before a WebSocket exists.
//!
//! Authentication is HTTP rather than protocol frames on purpose. A token has to be obtained before a
//! connection can be authenticated, so putting it on the gateway would mean an unauthenticated
//! connection that exists only to get a credential — one more state to reason about, and one more
//! thing an unauthenticated peer can hold open. HTTP already has the semantics: a request, a status
//! code, a body, and a connection that closes.

use std::time::Duration;

use migo_core::Id;
use serde::{Deserialize, Serialize};

/// A REST failure, already reduced to something worth showing a person.
#[derive(Debug, thiserror::Error)]
pub enum RestError {
    #[error("cannot reach the server")]
    Transport,

    /// The server answered with its error envelope. The message is the server's own
    /// `public_message()`, which is the only string it ever puts on the wire — internal detail stays
    /// on the server by construction (brief section 161), so it is safe to show verbatim.
    #[error("{message}")]
    Server {
        code: u32,
        symbol: String,
        message: String,
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
    password: &'a str,
    locale: &'a str,
    device: DeviceRequest,
}

#[derive(Debug, Serialize)]
struct LoginRequest<'a> {
    identifier: &'a str,
    password: &'a str,
    device: DeviceRequest,
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

    /// Creates an account and a first device.
    pub async fn register(
        &self,
        username: &str,
        password: &str,
        device: DeviceRequest,
    ) -> Result<Grant, RestError> {
        let body = RegisterRequest {
            username,
            password,
            locale: "en",
            device,
        };
        self.post("/v1/auth/register", &body).await
    }

    /// Signs in an existing account.
    pub async fn login(
        &self,
        identifier: &str,
        password: &str,
        device: DeviceRequest,
    ) -> Result<Grant, RestError> {
        let body = LoginRequest {
            identifier,
            password,
            device,
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
