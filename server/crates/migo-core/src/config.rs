//! Layered configuration.
//!
//! Precedence, lowest to highest: **built-in defaults → TOML files → environment
//! → CLI flags**. The layering exists so that a developer can clone the repo and
//! run `migod` with no configuration at all, while production overrides only what
//! it needs to and gets a hard failure on anything unsafe.
//!
//! Three rules are enforced by code rather than by convention:
//!
//! 1. **Unknown keys are errors.** `MIGO_STOR__URL` is a typo, and a typo that
//!    silently leaves the default in place is how a staging database ends up
//!    serving production traffic.
//! 2. **Empty environment variables mean "unset", not "empty string".** A
//!    `.env` file full of `KEY=` placeholders should behave like an absent key.
//! 3. **Production refuses to start unsafe.** No token key, a development
//!    default key, an in-memory store, a wildcard CORS origin, or a plaintext
//!    public URL each abort startup with a specific message. `migod` failing
//!    loudly at boot is cheap; discovering a week later that every session token
//!    was signed with a key from a README is not.
//!
//! Validation collects *all* problems and reports them together, because fixing
//! configuration one error per restart is a miserable way to spend an afternoon.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::secret::Secret;
use crate::telemetry::LogFormat;

/// The development placeholder token key. Rejected in production by name so
/// that copying it out of the docs cannot become a deployment.
pub const DEVELOPMENT_TOKEN_KEY: &str = "development-only-insecure-token-key";

/// Minimum accepted length, in bytes, of decoded session-token key material.
pub const MIN_TOKEN_KEY_BYTES: usize = 32;

/// Environment variable prefix for every configuration key.
pub const ENV_PREFIX: &str = "MIGO_";

/// Environment variables that are read by the binary but are not configuration
/// keys, and so must not be treated as unknown fields.
const RESERVED_ENV_KEYS: &[&str] = &["MIGO_CONFIG"];

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

/// Which subsystems this process runs. One binary, many shapes (ADR-0001).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// REST surface: registration, profiles, media tickets, admin.
    Api,
    /// WebSocket/QUIC session termination and fanout.
    Gateway,
    /// Room sequencing and moderation.
    Room,
    /// Game session hosting.
    Game,
    /// Server-to-server mesh (ADR-0005).
    Federation,
}

impl Role {
    /// Stable wire/config name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Api => "api",
            Role::Gateway => "gateway",
            Role::Room => "room",
            Role::Game => "game",
            Role::Federation => "federation",
        }
    }

    /// Parses a role name.
    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "api" => Ok(Role::Api),
            "gateway" => Ok(Role::Gateway),
            "room" => Ok(Role::Room),
            "game" => Ok(Role::Game),
            "federation" => Ok(Role::Federation),
            other => Err(ConfigError::Invalid(vec![format!(
                "unknown role {other:?}; expected one of api, gateway, room, game, federation"
            )])),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Deployment environment. Drives the safety checks in [`Config::validate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    /// A laptop. Insecure defaults are permitted and logged.
    #[default]
    Development,
    /// A shared pre-production deployment. Production checks apply, minus TLS.
    Staging,
    /// Real users. Every check applies.
    Production,
}

impl Environment {
    /// True when the strict startup checks apply.
    #[must_use]
    pub fn is_hardened(self) -> bool {
        matches!(self, Environment::Staging | Environment::Production)
    }

    /// Stable name for logs and metrics.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Environment::Development => "development",
            Environment::Staging => "staging",
            Environment::Production => "production",
        }
    }
}

/// Where durable state lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreBackend {
    /// In-process, non-durable. Real implementation of the storage traits, not a
    /// mock: `make dev` runs the whole server with no external dependency.
    #[default]
    Memory,
    /// PostgreSQL (ADR-0004).
    Postgres,
}

/// Where ephemeral state lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheBackend {
    /// In-process maps.
    #[default]
    Memory,
    /// Redis.
    Redis,
}

/// Where uploaded media lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaBackend {
    /// Local directory. Development only.
    #[default]
    Filesystem,
    /// S3-compatible object storage accessed through signed URLs.
    S3,
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// Identity and composition of this process.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NodeConfig {
    /// Stable node name. Appears in logs, metrics, and mesh handshakes.
    pub id: String,
    /// Geographic region, used for room home placement.
    pub region: String,
    /// ISO country code, used for legal routing and default language.
    pub country: String,
    /// Subsystems to run.
    #[serde(deserialize_with = "comma_separated_roles")]
    pub roles: Vec<Role>,
    /// Deployment environment.
    pub environment: Environment,
    /// Base64 Ed25519 node signing key for the mesh. Empty in development.
    pub signing_key: Option<Secret>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            id: "migo-local-1".to_string(),
            region: "local".to_string(),
            country: "ID".to_string(),
            roles: vec![Role::Api, Role::Gateway, Role::Room, Role::Game],
            environment: Environment::Development,
            signing_key: None,
        }
    }
}

/// HTTP and WebSocket listener.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HttpConfig {
    /// Socket to bind.
    pub bind: String,
    /// Externally reachable base URL, used to build absolute links.
    pub public_url: String,
    /// Allowed browser origins. Never `*` in a hardened environment.
    #[serde(deserialize_with = "comma_separated_strings")]
    pub cors_origins: Vec<String>,
    /// Maximum accepted REST request body.
    pub max_body_bytes: usize,
    /// Timeout applied to a whole REST request.
    pub request_timeout_ms: u64,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8080".to_string(),
            public_url: "http://localhost:8080".to_string(),
            cors_origins: vec!["http://localhost:19991".to_string()],
            max_body_bytes: 1024 * 1024,
            request_timeout_ms: 15_000,
        }
    }
}

/// Optional QUIC listener. Empty `bind` disables it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QuicConfig {
    /// Socket to bind, or `None` to disable QUIC.
    pub bind: Option<String>,
}

/// Durable storage.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StoreConfig {
    /// Which implementation to use.
    pub backend: StoreBackend,
    /// PostgreSQL connection URL. Contains a password, so it is a secret.
    pub url: Option<Secret>,
    /// Connection pool ceiling. Sized against the database, not the process.
    pub max_connections: u32,
    /// How long to wait for a pooled connection before failing the request.
    pub acquire_timeout_ms: u64,
    /// Server-side statement timeout, so one bad query cannot hold a connection.
    pub statement_timeout_ms: u64,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            backend: StoreBackend::Memory,
            url: None,
            max_connections: 16,
            acquire_timeout_ms: 3_000,
            statement_timeout_ms: 10_000,
        }
    }
}

/// Ephemeral, reconstructible state.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CacheConfig {
    /// Which implementation to use.
    pub backend: CacheBackend,
    /// Redis connection URL.
    pub url: Option<Secret>,
    /// Default entry lifetime.
    pub default_ttl_seconds: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            backend: CacheBackend::Memory,
            url: None,
            default_ttl_seconds: 300,
        }
    }
}

/// Media storage and upload policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MediaConfig {
    /// Which implementation to use.
    pub backend: MediaBackend,
    /// S3 endpoint.
    pub endpoint: Option<String>,
    /// Bucket name.
    pub bucket: String,
    /// S3 access key id.
    pub access_key: Option<Secret>,
    /// S3 secret access key.
    pub secret_key: Option<Secret>,
    /// Directory used by the filesystem backend.
    pub local_dir: PathBuf,
    /// Largest single upload.
    pub max_upload_bytes: u64,
    /// Lifetime of an issued signed URL. Short: a leaked link is a leaked file.
    pub signed_url_ttl_seconds: u64,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            backend: MediaBackend::Filesystem,
            endpoint: None,
            bucket: "migo-media".to_string(),
            access_key: None,
            secret_key: None,
            local_dir: PathBuf::from("./var/media"),
            max_upload_bytes: 32 * 1024 * 1024,
            signed_url_ttl_seconds: 300,
        }
    }
}

/// Authentication policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuthConfig {
    /// Base64 key used to sign session tokens.
    pub token_key: Option<Secret>,
    /// Access token lifetime. Short by design; refresh covers the gap.
    pub access_ttl_seconds: u64,
    /// Refresh token lifetime.
    pub refresh_ttl_seconds: u64,
    /// Whether self-service registration is open.
    pub allow_registration: bool,
    /// Devices one account may keep signed in.
    pub max_devices_per_user: u32,
    /// Minimum password length. Length beats composition rules.
    pub password_min_length: usize,
    /// Override the rate-limit price of one new-account registration, in tokens.
    /// Defaults to the full anonymous endpoint bucket, which is the safe and tight
    /// choice for the public internet. Lower it for local two-node smokes where
    /// the value is in the round trip, not the rate ceiling.
    pub registration_cost: Option<u32>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            token_key: None,
            access_ttl_seconds: 900,
            refresh_ttl_seconds: 2_592_000,
            allow_registration: true,
            max_devices_per_user: 8,
            password_min_length: 10,
            registration_cost: None,
        }
    }
}

/// Logging, metrics, tracing.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TelemetryConfig {
    /// `RUST_LOG`-style filter directives.
    pub log_level: String,
    /// Output shape.
    pub log_format: LogFormat,
    /// Socket for the Prometheus scrape endpoint.
    pub metrics_bind: Option<String>,
    /// OTLP collector endpoint. Disabled when absent.
    pub otlp_endpoint: Option<String>,
    /// Fraction of traces sampled, 0.0 to 1.0.
    pub trace_sample_ratio: f64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            log_level: "info,migo=debug".to_string(),
            log_format: LogFormat::Pretty,
            metrics_bind: Some("127.0.0.1:9090".to_string()),
            otlp_endpoint: None,
            trace_sample_ratio: 0.01,
        }
    }
}

/// Session handling limits. Defaults mirror the protocol limits in
/// `shared/protocol/schema/meta.json`; overriding them below the protocol
/// minimum is rejected by validation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GatewayConfig {
    /// Hard ceiling on concurrent sessions for this process.
    pub max_sessions: usize,
    /// Bounded outbound queue depth per session (ADR-0008).
    pub session_queue_capacity: usize,
    /// Heartbeat interval advertised to clients.
    pub heartbeat_ms: u64,
    /// How long a disconnected session may be resumed.
    pub resume_window_ms: u64,
    /// Frames retained per session for resume.
    pub resume_buffer_frames: usize,
    /// How long to hold a batch open waiting for more events.
    pub batch_linger_ms: u64,
    /// Grace period before a lagging session is closed.
    pub lagging_deadline_ms: u64,
    /// Whether to offer negotiated compression.
    pub compression_enabled: bool,
    /// Unauthenticated grace period after connect.
    pub handshake_timeout_ms: u64,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            max_sessions: 50_000,
            session_queue_capacity: 256,
            heartbeat_ms: 30_000,
            resume_window_ms: 120_000,
            resume_buffer_frames: 512,
            batch_linger_ms: 15,
            lagging_deadline_ms: 5_000,
            compression_enabled: true,
            handshake_timeout_ms: 10_000,
        }
    }
}

/// Server-to-server mesh.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FederationConfig {
    /// Whether to accept and originate mesh connections.
    pub enabled: bool,
    /// Explicit allow-list of peer node ids. Empty means accept none.
    #[serde(deserialize_with = "comma_separated_strings")]
    pub allowed_peers: Vec<String>,
    /// Rejection threshold for handshake clock skew.
    pub max_clock_skew_seconds: u64,
    /// Per-peer outbound queue depth.
    pub peer_queue_capacity: usize,
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_peers: Vec::new(),
            max_clock_skew_seconds: 60,
            peer_queue_capacity: 4096,
        }
    }
}

/// Cost-based abuse control (ADR-0006). Per-opcode costs live in the protocol
/// IDL; only the refill policy is configurable.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RateLimitConfig {
    /// Bucket capacity for an authenticated user session.
    pub user_burst: u32,
    /// Tokens restored per second for an authenticated user session.
    pub user_refill_per_second: u32,
    /// Bucket capacity before authentication. Deliberately small.
    pub anonymous_burst: u32,
    /// Tokens restored per second before authentication.
    pub anonymous_refill_per_second: u32,
    /// Bucket capacity for a bot account.
    pub bot_burst: u32,
    /// Tokens restored per second for a bot account.
    pub bot_refill_per_second: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            user_burst: 200,
            user_refill_per_second: 50,
            anonymous_burst: 20,
            anonymous_refill_per_second: 5,
            bot_burst: 500,
            bot_refill_per_second: 200,
        }
    }
}

/// The whole configuration.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Identity and role composition.
    pub node: NodeConfig,
    /// HTTP listener.
    pub http: HttpConfig,
    /// QUIC listener.
    pub quic: QuicConfig,
    /// Durable storage.
    pub store: StoreConfig,
    /// Ephemeral cache.
    pub cache: CacheConfig,
    /// Media storage.
    pub media: MediaConfig,
    /// Authentication policy.
    pub auth: AuthConfig,
    /// Observability.
    pub telemetry: TelemetryConfig,
    /// Session handling.
    pub gateway: GatewayConfig,
    /// Mesh.
    pub federation: FederationConfig,
    /// Abuse control.
    pub rate_limit: RateLimitConfig,
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Why configuration could not be built.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A configuration file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// File that failed.
        path: PathBuf,
        /// Underlying cause.
        source: std::io::Error,
    },
    /// A configuration file is not valid TOML.
    #[error("{path} is not valid TOML: {message}")]
    Parse {
        /// File that failed.
        path: PathBuf,
        /// Parser message.
        message: String,
    },
    /// The merged tree does not match the configuration schema. Unknown keys
    /// land here, which is the point.
    #[error("configuration does not match the schema: {0}")]
    Schema(String),
    /// The configuration parsed but is not safe or coherent.
    #[error("configuration is invalid:\n{}", .0.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n"))]
    Invalid(Vec<String>),
}

impl Config {
    /// Loads configuration for a real process: the file named by `MIGO_CONFIG`
    /// if set, otherwise `config/migod.toml` when it exists, then the
    /// environment. Validation runs before returning.
    pub fn load() -> Result<Self, ConfigError> {
        let mut files: Vec<PathBuf> = Vec::new();
        match std::env::var("MIGO_CONFIG") {
            Ok(path) if !path.trim().is_empty() => files.push(PathBuf::from(path)),
            _ => {
                let default_path = PathBuf::from("config/migod.toml");
                if default_path.is_file() {
                    files.push(default_path);
                }
            }
        }
        let env: Vec<(String, String)> = std::env::vars().collect();
        let config = Self::from_sources(&files, &env)?;
        config.validate()?;
        Ok(config)
    }

    /// Builds configuration from explicit sources without touching the process
    /// environment, and without validating. Tests use this; so does `migod` when
    /// CLI flags need to be applied between merge and validation.
    pub fn from_sources(files: &[PathBuf], env: &[(String, String)]) -> Result<Self, ConfigError> {
        let mut merged = toml::Table::new();
        for path in files {
            let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
                path: path.clone(),
                source,
            })?;
            let parsed: toml::Table =
                toml::from_str(&text).map_err(|error| ConfigError::Parse {
                    path: path.clone(),
                    message: error.to_string(),
                })?;
            merge_tables(&mut merged, parsed);
        }
        merge_tables(&mut merged, env_table(env));
        Config::deserialize(toml::Value::Table(merged))
            .map_err(|error| ConfigError::Schema(error.to_string()))
    }

    /// Loads from a TOML string plus environment pairs. Used by tests.
    pub fn from_toml_str(toml_text: &str, env: &[(String, String)]) -> Result<Self, ConfigError> {
        let mut merged: toml::Table =
            toml::from_str(toml_text).map_err(|error| ConfigError::Parse {
                path: PathBuf::from("<inline>"),
                message: error.to_string(),
            })?;
        merge_tables(&mut merged, env_table(env));
        Config::deserialize(toml::Value::Table(merged))
            .map_err(|error| ConfigError::Schema(error.to_string()))
    }

    /// True when this process runs the given role.
    #[must_use]
    pub fn has_role(&self, role: Role) -> bool {
        self.node.roles.contains(&role)
    }

    /// A single line safe to log at startup: composition and backends, no
    /// credentials.
    #[must_use]
    pub fn summary(&self) -> String {
        let roles = self
            .node
            .roles
            .iter()
            .map(|role| role.as_str())
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "node={} env={} region={} roles=[{}] store={:?} cache={:?} media={:?} http={} quic={} \
             metrics={} compression={}",
            self.node.id,
            self.node.environment.as_str(),
            self.node.region,
            roles,
            self.store.backend,
            self.cache.backend,
            self.media.backend,
            self.http.bind,
            self.quic.bind.as_deref().unwrap_or("disabled"),
            self.telemetry.metrics_bind.as_deref().unwrap_or("disabled"),
            self.gateway.compression_enabled,
        )
    }

    /// Checks coherence and, in hardened environments, safety.
    ///
    /// Every problem is collected before returning so one restart surfaces the
    /// whole list.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut problems: Vec<String> = Vec::new();
        let hardened = self.node.environment.is_hardened();

        if self.node.roles.is_empty() {
            problems.push("node.roles is empty: this process would do nothing".to_string());
        }
        if self.node.id.trim().is_empty() {
            problems.push("node.id must not be empty".to_string());
        }

        check_socket_addr("http.bind", &self.http.bind, &mut problems);
        if let Some(bind) = &self.quic.bind {
            check_socket_addr("quic.bind", bind, &mut problems);
        }
        if let Some(bind) = &self.telemetry.metrics_bind {
            check_socket_addr("telemetry.metrics_bind", bind, &mut problems);
        }

        if self.http.public_url.trim().is_empty() {
            problems.push("http.public_url must not be empty".to_string());
        }
        if self.http.max_body_bytes == 0 {
            problems.push("http.max_body_bytes must be greater than zero".to_string());
        }

        // --- storage coherence ---
        match self.store.backend {
            StoreBackend::Postgres => {
                if self.store.url.as_ref().is_none_or(Secret::is_empty) {
                    problems.push("store.backend is postgres but store.url is not set".to_string());
                }
            }
            StoreBackend::Memory => {}
        }
        if self.store.max_connections == 0 {
            problems.push("store.max_connections must be at least 1".to_string());
        }
        if matches!(self.cache.backend, CacheBackend::Redis)
            && self.cache.url.as_ref().is_none_or(Secret::is_empty)
        {
            problems.push("cache.backend is redis but cache.url is not set".to_string());
        }
        if matches!(self.media.backend, MediaBackend::S3) {
            if self
                .media
                .endpoint
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                problems.push("media.backend is s3 but media.endpoint is not set".to_string());
            }
            if self.media.access_key.as_ref().is_none_or(Secret::is_empty)
                || self.media.secret_key.as_ref().is_none_or(Secret::is_empty)
            {
                problems.push(
                    "media.backend is s3 but media.access_key/secret_key are not set".to_string(),
                );
            }
        }

        // --- auth coherence ---
        if self.auth.access_ttl_seconds == 0 {
            problems.push("auth.access_ttl_seconds must be greater than zero".to_string());
        }
        if self.auth.refresh_ttl_seconds < self.auth.access_ttl_seconds {
            problems.push(
                "auth.refresh_ttl_seconds must be at least auth.access_ttl_seconds: a refresh \
                 token that expires before the access token it renews is useless"
                    .to_string(),
            );
        }
        if self.auth.password_min_length < 8 {
            problems.push("auth.password_min_length must be at least 8".to_string());
        }
        if self.auth.max_devices_per_user == 0 {
            problems.push("auth.max_devices_per_user must be at least 1".to_string());
        }

        // --- gateway coherence ---
        if self.gateway.session_queue_capacity == 0 {
            problems.push(
                "gateway.session_queue_capacity must be greater than zero: an unbounded or zero \
                 queue defeats the drop policy in ADR-0008"
                    .to_string(),
            );
        }
        if self.gateway.heartbeat_ms < 1_000 {
            problems.push("gateway.heartbeat_ms must be at least 1000".to_string());
        }
        if self.gateway.resume_window_ms > 0 && self.gateway.resume_buffer_frames == 0 {
            problems.push(
                "gateway.resume_buffer_frames must be greater than zero when resume is enabled"
                    .to_string(),
            );
        }
        if self.gateway.max_sessions == 0 {
            problems.push("gateway.max_sessions must be greater than zero".to_string());
        }

        // --- rate limiting ---
        if self.rate_limit.user_burst == 0 || self.rate_limit.user_refill_per_second == 0 {
            problems.push(
                "rate_limit user burst and refill must both be greater than zero".to_string(),
            );
        }

        // --- telemetry ---
        if !(0.0..=1.0).contains(&self.telemetry.trace_sample_ratio) {
            problems.push(format!(
                "telemetry.trace_sample_ratio must be between 0.0 and 1.0, got {}",
                self.telemetry.trace_sample_ratio
            ));
        }

        // --- federation ---
        if self.has_role(Role::Federation) && !self.federation.enabled {
            problems
                .push("node.roles includes federation but federation.enabled is false".to_string());
        }
        if self.federation.enabled && self.federation.max_clock_skew_seconds > 300 {
            problems.push(
                "federation.max_clock_skew_seconds above 300 makes replay protection meaningless"
                    .to_string(),
            );
        }

        // --- safety checks that only apply outside development ---
        if hardened {
            match &self.auth.token_key {
                None => problems.push(
                    "auth.token_key must be set outside development: refusing to sign session \
                     tokens with a generated key that changes on every restart"
                        .to_string(),
                ),
                Some(key) if key.expose() == DEVELOPMENT_TOKEN_KEY => problems.push(
                    "auth.token_key is the documented development placeholder; generate a real one \
                     with `migod keygen token`"
                        .to_string(),
                ),
                Some(key) => {
                    if decoded_key_len(key.expose()) < MIN_TOKEN_KEY_BYTES {
                        problems.push(format!(
                            "auth.token_key must decode to at least {MIN_TOKEN_KEY_BYTES} bytes"
                        ));
                    }
                }
            }

            if self.store.backend == StoreBackend::Memory {
                problems.push(
                    "store.backend is memory outside development: every message would be lost on \
                     restart"
                        .to_string(),
                );
            }
            // The compose file and CI ship a well-known `migo:migo` login for the local Postgres;
            // it is documented in the open, so a node that reached staging or production still
            // pointed at it is authenticating real traffic with a credential every reader of the
            // repository already knows. Refuse to start, naming the field but never echoing the
            // credential itself into a log line or an error a user might paste somewhere.
            if self
                .store
                .url
                .as_ref()
                .is_some_and(|url| url.expose().contains("migo:migo@"))
            {
                problems.push(
                    "store.url carries the documented development database credential outside \
                     development: provision real database credentials before serving real users"
                        .to_string(),
                );
            }
            if self.media.backend == MediaBackend::Filesystem {
                problems.push(
                    "media.backend is filesystem outside development: uploads would not survive a \
                     redeploy and would not be shared between nodes"
                        .to_string(),
                );
            }
            if self
                .http
                .cors_origins
                .iter()
                .any(|origin| origin.trim() == "*")
            {
                problems.push(
                    "http.cors_origins contains '*' outside development: any site could drive an \
                     authenticated session"
                        .to_string(),
                );
            }
            if self.http.cors_origins.is_empty() {
                problems.push(
                    "http.cors_origins is empty: the web client could not connect".to_string(),
                );
            }
            if self.auth.allow_registration && self.node.environment == Environment::Production {
                // Not an error, but worth saying out loud once at boot.
                tracing::warn!("open registration is enabled in production");
            }
            if self.federation.enabled && self.federation.allowed_peers.is_empty() {
                problems.push(
                    "federation.enabled is true but federation.allowed_peers is empty: the mesh \
                     allow-list is mandatory (ADR-0005)"
                        .to_string(),
                );
            }
            if self.node.signing_key.as_ref().is_none_or(Secret::is_empty)
                && self.federation.enabled
            {
                problems.push(
                    "node.signing_key must be set when federation is enabled: an ephemeral node key \
                     invalidates every peer allow-list entry on restart"
                        .to_string(),
                );
            }
        }

        if self.node.environment == Environment::Production
            && self.http.public_url.starts_with("http://")
        {
            problems.push(
                "http.public_url is plaintext http in production: tokens would travel in the clear"
                    .to_string(),
            );
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(problems))
        }
    }
}

fn check_socket_addr(field: &str, value: &str, problems: &mut Vec<String>) {
    if value.parse::<SocketAddr>().is_err() {
        problems.push(format!(
            "{field} is not a socket address: {value:?} (expected host:port)"
        ));
    }
}

/// Decodes configured key material. Accepts standard base64, URL-safe base64, and
/// hex, because operators paste all three.
///
/// Public because the crate that *uses* a key has to decode it the same way the
/// crate that *validates* it does. Two decoders would eventually disagree, and the
/// disagreement would show up as a token that validation accepted and signing
/// rejected.
///
/// Unrecognised input is returned as its own raw bytes rather than refused: an
/// operator who pastes a long passphrase has supplied real entropy, and rejecting
/// it would push them toward something shorter.
#[must_use]
pub fn decode_key_material(value: &str) -> Vec<u8> {
    use base64::Engine as _;
    let trimmed = value.trim();
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(trimmed) {
        return bytes;
    }
    if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(trimmed) {
        return bytes;
    }
    if trimmed.len().is_multiple_of(2)
        && !trimmed.is_empty()
        && trimmed.chars().all(|c| c.is_ascii_hexdigit())
    {
        let mut out = Vec::with_capacity(trimmed.len() / 2);
        // Two hex digits per byte. The even, non-empty, all-hex length checked above means every
        // pair is a whole byte and the remainder is empty.
        let (pairs, _rest) = trimmed.as_bytes().as_chunks::<2>();
        for &[hi, lo] in pairs {
            let hi = (hi as char).to_digit(16).unwrap_or(0) as u8;
            let lo = (lo as char).to_digit(16).unwrap_or(0) as u8;
            out.push(hi << 4 | lo);
        }
        return out;
    }
    trimmed.as_bytes().to_vec()
}

/// Length of the key material once decoded.
fn decoded_key_len(value: &str) -> usize {
    decode_key_material(value).len()
}

/// Deep-merges `overlay` into `base`; tables recurse, everything else replaces.
fn merge_tables(base: &mut toml::Table, overlay: toml::Table) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(existing)), toml::Value::Table(incoming)) => {
                merge_tables(existing, incoming);
            }
            (_, incoming) => {
                base.insert(key, incoming);
            }
        }
    }
}

/// Turns `MIGO_SECTION__FIELD=value` pairs into a nested TOML table.
///
/// Empty values are skipped so that a `.env` full of placeholder keys behaves
/// like an absent file rather than a file full of empty strings.
fn env_table(env: &[(String, String)]) -> toml::Table {
    let mut root = toml::Table::new();
    // Sorted so that the merge order is deterministic when two variables differ
    // only in case, which is possible on some platforms.
    let mut pairs: BTreeMap<&str, &str> = BTreeMap::new();
    for (key, value) in env {
        pairs.insert(key.as_str(), value.as_str());
    }
    for (key, value) in pairs {
        if !key.starts_with(ENV_PREFIX) || RESERVED_ENV_KEYS.contains(&key) {
            continue;
        }
        if value.trim().is_empty() {
            continue;
        }
        let path: Vec<String> = key[ENV_PREFIX.len()..]
            .split("__")
            .map(|segment| segment.to_ascii_lowercase())
            .collect();
        if path.iter().any(String::is_empty) {
            continue;
        }
        insert_path(&mut root, &path, parse_env_value(value));
    }
    root
}

fn insert_path(table: &mut toml::Table, path: &[String], value: toml::Value) {
    match path {
        [] => {}
        [leaf] => {
            table.insert(leaf.clone(), value);
        }
        [head, rest @ ..] => {
            let entry = table
                .entry(head.clone())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if !entry.is_table() {
                *entry = toml::Value::Table(toml::Table::new());
            }
            if let Some(nested) = entry.as_table_mut() {
                insert_path(nested, rest, value);
            }
        }
    }
}

/// Types an environment string.
///
/// Numeric and boolean parsing is capped at 19 characters so that long opaque
/// values — base64 keys, connection URLs — always stay strings. Guessing wrong
/// on a credential would turn a typo into a schema error at best and a silent
/// truncation at worst.
fn parse_env_value(raw: &str) -> toml::Value {
    let trimmed = raw.trim();
    if trimmed.len() <= 19 {
        match trimmed.to_ascii_lowercase().as_str() {
            "true" => return toml::Value::Boolean(true),
            "false" => return toml::Value::Boolean(false),
            _ => {}
        }
        if let Ok(integer) = trimmed.parse::<i64>() {
            return toml::Value::Integer(integer);
        }
        if trimmed.contains('.') {
            if let Ok(float) = trimmed.parse::<f64>() {
                return toml::Value::Float(float);
            }
        }
    }
    toml::Value::String(trimmed.to_string())
}

// ---------------------------------------------------------------------------
// serde helpers
// ---------------------------------------------------------------------------

/// Accepts either a TOML array or a comma-separated string, so that
/// `roles = ["api", "gateway"]` in a file and `MIGO_NODE__ROLES=api,gateway` in
/// the environment mean the same thing.
fn comma_separated_strings<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        List(Vec<String>),
        Text(String),
    }

    Ok(match Either::deserialize(deserializer)? {
        Either::List(list) => list,
        Either::Text(text) => text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(ToString::to_string)
            .collect(),
    })
}

fn comma_separated_roles<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Role>, D::Error> {
    let names = comma_separated_strings(deserializer)?;
    let mut roles = Vec::with_capacity(names.len());
    for name in names {
        let role = Role::parse(&name).map_err(|_| {
            de::Error::custom(format!(
                "unknown role {name:?}; expected api, gateway, room, game, or federation"
            ))
        })?;
        if !roles.contains(&role) {
            roles.push(role);
        }
    }
    Ok(roles)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn defaults_are_a_working_development_configuration() {
        let config = Config::from_sources(&[], &[]).expect("builds");
        config.validate().expect("development defaults are valid");
        assert_eq!(config.store.backend, StoreBackend::Memory);
        assert_eq!(config.node.environment, Environment::Development);
        assert!(config.has_role(Role::Gateway));
    }

    #[test]
    fn environment_overrides_defaults_with_nesting() {
        let config = Config::from_sources(
            &[],
            &env(&[
                ("MIGO_NODE__ID", "edge-7"),
                ("MIGO_HTTP__BIND", "127.0.0.1:9999"),
                ("MIGO_STORE__MAX_CONNECTIONS", "48"),
                ("MIGO_GATEWAY__COMPRESSION_ENABLED", "false"),
                ("MIGO_TELEMETRY__TRACE_SAMPLE_RATIO", "0.25"),
            ]),
        )
        .expect("builds");
        assert_eq!(config.node.id, "edge-7");
        assert_eq!(config.http.bind, "127.0.0.1:9999");
        assert_eq!(config.store.max_connections, 48);
        assert!(!config.gateway.compression_enabled);
        assert!((config.telemetry.trace_sample_ratio - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn environment_wins_over_a_file() {
        let config = Config::from_toml_str(
            "[node]\nid = \"from-file\"\nregion = \"eu\"\n",
            &env(&[("MIGO_NODE__ID", "from-env")]),
        )
        .expect("builds");
        assert_eq!(config.node.id, "from-env");
        // Untouched keys survive the merge.
        assert_eq!(config.node.region, "eu");
    }

    #[test]
    fn roles_accept_a_comma_separated_list_or_an_array() {
        let from_env = Config::from_sources(
            &[],
            &env(&[("MIGO_NODE__ROLES", "api, gateway,federation")]),
        )
        .expect("builds");
        assert_eq!(
            from_env.node.roles,
            vec![Role::Api, Role::Gateway, Role::Federation]
        );

        let from_file =
            Config::from_toml_str("[node]\nroles = [\"room\", \"game\"]\n", &[]).expect("builds");
        assert_eq!(from_file.node.roles, vec![Role::Room, Role::Game]);
    }

    #[test]
    fn duplicate_roles_collapse() {
        let config = Config::from_sources(&[], &env(&[("MIGO_NODE__ROLES", "api,api,gateway")]))
            .expect("builds");
        assert_eq!(config.node.roles, vec![Role::Api, Role::Gateway]);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let error = Config::from_sources(&[], &env(&[("MIGO_STOR__URL", "postgres://x")]))
            .expect_err("typo must fail");
        assert!(matches!(error, ConfigError::Schema(_)), "{error:?}");
        let rendered = error.to_string();
        assert!(rendered.contains("stor"), "{rendered}");
    }

    #[test]
    fn unknown_role_names_are_rejected() {
        let error = Config::from_sources(&[], &env(&[("MIGO_NODE__ROLES", "api,teapot")]))
            .expect_err("bad role must fail");
        assert!(error.to_string().contains("teapot"), "{error}");
    }

    #[test]
    fn empty_environment_values_mean_unset() {
        let config = Config::from_sources(
            &[],
            &env(&[("MIGO_NODE__SIGNING_KEY", ""), ("MIGO_STORE__URL", "   ")]),
        )
        .expect("builds");
        assert!(config.node.signing_key.is_none());
        assert!(config.store.url.is_none());
    }

    #[test]
    fn reserved_environment_keys_are_not_configuration() {
        let config = Config::from_sources(&[], &env(&[("MIGO_CONFIG", "/etc/migo/migod.toml")]))
            .expect("MIGO_CONFIG must not be treated as a config key");
        assert_eq!(config.node.id, NodeConfig::default().id);
    }

    #[test]
    fn long_opaque_values_stay_strings() {
        // 44 characters of base64 that happen to be digits must not become an integer.
        let key = "1".repeat(44);
        let config =
            Config::from_sources(&[], &env(&[("MIGO_AUTH__TOKEN_KEY", &key)])).expect("builds");
        assert_eq!(config.auth.token_key.as_ref().map(Secret::len), Some(44));
    }

    #[test]
    fn production_refuses_a_missing_token_key() {
        let config = Config::from_sources(
            &[],
            &env(&[
                ("MIGO_NODE__ENVIRONMENT", "production"),
                ("MIGO_STORE__BACKEND", "postgres"),
                ("MIGO_STORE__URL", "postgres://migo@db/migo"),
                ("MIGO_MEDIA__BACKEND", "s3"),
                ("MIGO_MEDIA__ENDPOINT", "https://s3.example"),
                ("MIGO_MEDIA__ACCESS_KEY", "AKIAEXAMPLE"),
                ("MIGO_MEDIA__SECRET_KEY", "s3cret"),
                ("MIGO_HTTP__PUBLIC_URL", "https://migo.example"),
            ]),
        )
        .expect("builds");
        let error = config.validate().expect_err("must refuse to start");
        let rendered = error.to_string();
        assert!(rendered.contains("auth.token_key"), "{rendered}");
    }

    #[test]
    fn production_refuses_the_documented_development_key() {
        let config = Config::from_sources(
            &[],
            &env(&[
                ("MIGO_NODE__ENVIRONMENT", "production"),
                ("MIGO_AUTH__TOKEN_KEY", DEVELOPMENT_TOKEN_KEY),
            ]),
        )
        .expect("builds");
        let rendered = config.validate().expect_err("must refuse").to_string();
        assert!(rendered.contains("development placeholder"), "{rendered}");
    }

    #[test]
    fn production_refuses_memory_store_and_wildcard_cors() {
        let config = Config::from_sources(
            &[],
            &env(&[
                ("MIGO_NODE__ENVIRONMENT", "production"),
                ("MIGO_HTTP__CORS_ORIGINS", "*"),
            ]),
        )
        .expect("builds");
        let rendered = config.validate().expect_err("must refuse").to_string();
        assert!(rendered.contains("store.backend is memory"), "{rendered}");
        assert!(rendered.contains("cors_origins contains '*'"), "{rendered}");
        assert!(
            rendered.contains("media.backend is filesystem"),
            "{rendered}"
        );
    }

    #[test]
    fn production_refuses_plaintext_public_url() {
        let config = Config::from_sources(
            &[],
            &env(&[
                ("MIGO_NODE__ENVIRONMENT", "production"),
                ("MIGO_HTTP__PUBLIC_URL", "http://migo.example"),
            ]),
        )
        .expect("builds");
        let rendered = config.validate().expect_err("must refuse").to_string();
        assert!(
            rendered.contains("plaintext http in production"),
            "{rendered}"
        );
    }

    #[test]
    fn a_valid_production_configuration_passes() {
        use base64::Engine as _;
        let key = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let config = Config::from_sources(
            &[],
            &env(&[
                ("MIGO_NODE__ENVIRONMENT", "production"),
                ("MIGO_NODE__ROLES", "api,gateway,room"),
                ("MIGO_HTTP__PUBLIC_URL", "https://migo.example"),
                ("MIGO_HTTP__CORS_ORIGINS", "https://app.migo.example"),
                ("MIGO_STORE__BACKEND", "postgres"),
                ("MIGO_STORE__URL", "postgres://migo@db/migo"),
                ("MIGO_CACHE__BACKEND", "redis"),
                ("MIGO_CACHE__URL", "redis://cache:6379/0"),
                ("MIGO_MEDIA__BACKEND", "s3"),
                ("MIGO_MEDIA__ENDPOINT", "https://s3.example"),
                ("MIGO_MEDIA__ACCESS_KEY", "AKIAEXAMPLE"),
                ("MIGO_MEDIA__SECRET_KEY", "s3cret-value"),
                ("MIGO_AUTH__TOKEN_KEY", &key),
                ("MIGO_TELEMETRY__LOG_FORMAT", "json"),
            ]),
        )
        .expect("builds");
        config.validate().expect("valid production configuration");
    }

    #[test]
    fn short_token_keys_are_rejected() {
        use base64::Engine as _;
        let key = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
        let config = Config::from_sources(
            &[],
            &env(&[
                ("MIGO_NODE__ENVIRONMENT", "staging"),
                ("MIGO_AUTH__TOKEN_KEY", &key),
            ]),
        )
        .expect("builds");
        let rendered = config.validate().expect_err("must refuse").to_string();
        assert!(rendered.contains("at least 32 bytes"), "{rendered}");
    }

    #[test]
    fn incoherent_ttls_are_rejected() {
        let config = Config::from_sources(
            &[],
            &env(&[
                ("MIGO_AUTH__ACCESS_TTL_SECONDS", "3600"),
                ("MIGO_AUTH__REFRESH_TTL_SECONDS", "60"),
            ]),
        )
        .expect("builds");
        let rendered = config.validate().expect_err("must refuse").to_string();
        assert!(rendered.contains("refresh_ttl_seconds"), "{rendered}");
    }

    #[test]
    fn federation_role_requires_federation_enabled() {
        let config = Config::from_sources(&[], &env(&[("MIGO_NODE__ROLES", "gateway,federation")]))
            .expect("builds");
        let rendered = config.validate().expect_err("must refuse").to_string();
        assert!(
            rendered.contains("federation.enabled is false"),
            "{rendered}"
        );
    }

    #[test]
    fn bad_socket_addresses_are_reported_with_the_field_name() {
        let config = Config::from_sources(&[], &env(&[("MIGO_HTTP__BIND", "not-an-address")]))
            .expect("builds");
        let rendered = config.validate().expect_err("must refuse").to_string();
        assert!(
            rendered.contains("http.bind is not a socket address"),
            "{rendered}"
        );
    }

    #[test]
    fn validation_reports_every_problem_at_once() {
        let config = Config::from_sources(
            &[],
            &env(&[
                ("MIGO_HTTP__BIND", "nope"),
                ("MIGO_STORE__MAX_CONNECTIONS", "0"),
                ("MIGO_GATEWAY__SESSION_QUEUE_CAPACITY", "0"),
            ]),
        )
        .expect("builds");
        let ConfigError::Invalid(problems) = config.validate().expect_err("must refuse") else {
            panic!("expected Invalid");
        };
        assert!(problems.len() >= 3, "{problems:?}");
    }

    #[test]
    fn summary_never_contains_a_credential() {
        let config = Config::from_sources(
            &[],
            &env(&[
                ("MIGO_STORE__BACKEND", "postgres"),
                ("MIGO_STORE__URL", "postgres://migo:sup3rs3cret@db/migo"),
                ("MIGO_AUTH__TOKEN_KEY", "top-secret-token-key-value-here"),
            ]),
        )
        .expect("builds");
        let summary = config.summary();
        assert!(!summary.contains("sup3rs3cret"), "{summary}");
        assert!(!summary.contains("top-secret"), "{summary}");
        assert!(summary.contains("store=Postgres"), "{summary}");
    }

    #[test]
    fn debug_output_never_contains_a_credential() {
        let config = Config::from_sources(
            &[],
            &env(&[("MIGO_AUTH__TOKEN_KEY", "top-secret-token-key-value-here")]),
        )
        .expect("builds");
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("top-secret"), "{rendered}");
    }
}
