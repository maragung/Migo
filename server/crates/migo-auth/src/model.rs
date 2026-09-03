//! Inputs and outputs of the authentication service.
//!
//! These are the crate's own types, not the wire types. The gateway and the HTTP API
//! translate at their edges. That translation looks like busywork and buys two things:
//! the service can be called from a test or from `migod` without constructing a frame,
//! and a field added to the wire protocol cannot silently become an input to
//! authentication just because the names happen to match.
//!
//! Two conventions run through everything here.
//!
//! Every request carries a [`RequestContext`], which carries `now`. No function in this
//! crate reads a clock. That is what makes an expiry boundary testable rather than
//! hopeful (ADR-0009).
//!
//! Every secret is a [`Secret`], which redacts itself in `Debug` and zeroes itself on
//! drop. A `String` password would end up in a log the first time somebody added
//! `#[derive(Debug)]` to a request wrapper and traced it.

use std::net::IpAddr;

use migo_captcha::CaptchaProof;
use migo_core::{Id, Secret, Timestamp};
use migo_protocol::Platform;

use crate::capability::Capabilities;
use crate::endpoint::ServerEndpoint;

/// Per-request facts that every operation needs and none of them computes.
#[derive(Clone, Debug)]
pub struct RequestContext {
    /// Server time for this request.
    pub now: Timestamp,
    /// Caller address, when the transport knows it.
    ///
    /// `Option` because a request may arrive over a unix socket, from an in-process
    /// caller, or from a load test. A missing address means the IP-scoped buckets are
    /// skipped rather than merged into one shared bucket — a shared "unknown" bucket
    /// would let one internal caller rate limit every other internal caller.
    ///
    /// Never stored whole: what reaches the database is the truncated network class
    /// (brief section 162), and what reaches a log is nothing.
    pub ip: Option<IpAddr>,
    /// Client user agent, shown in the user's own session list.
    pub user_agent: Option<String>,
    /// Correlation id, so an audit row can be joined against traces.
    pub request_id: Option<String>,
}

impl RequestContext {
    /// A context with only a time. For tests and in-process callers.
    #[must_use]
    pub fn at(now: Timestamp) -> Self {
        Self {
            now,
            ip: None,
            user_agent: None,
            request_id: None,
        }
    }

    /// Sets the caller address.
    #[must_use]
    pub fn from_ip(mut self, ip: IpAddr) -> Self {
        self.ip = Some(ip);
        self
    }

    /// Sets the user agent, truncated to something a session list can render.
    #[must_use]
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        let mut value: String = user_agent.into();
        truncate_chars(&mut value, MAX_USER_AGENT_CHARS);
        self.user_agent = Some(value);
        self
    }

    /// Sets the correlation id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// The truncated network class to persist, if any.
    #[must_use]
    pub fn ip_class(&self) -> Option<String> {
        self.ip.map(migo_ratelimit::network)
    }
}

/// Longest user agent kept. Anything longer is a client bug or a probe.
pub const MAX_USER_AGENT_CHARS: usize = 200;

/// What a client says about the device it is running on.
///
/// Every field is a claim. The platform is a claim, the model is a claim, the version
/// is a claim. None of them may influence a limit, a capability, or a permission —
/// see the `tier` module for why the platform in particular is load-bearing nowhere.
/// They exist so the user can recognise their own device in the session list.
#[derive(Clone, Debug)]
pub struct DeviceClaim {
    /// An existing device to reuse, or `None` to register a new one.
    ///
    /// A client that has signed in before sends the id it was given. Reusing it keeps
    /// one row per physical device, which is what makes "revoke this device" mean
    /// anything: a client that registered a new device on every sign-in would present
    /// the user with a list of forty entries that are all the same phone.
    pub device_id: Option<Id>,
    /// Claimed platform.
    pub platform: Platform,
    /// Name to show in the session list.
    pub display_name: String,
    /// Client version.
    pub app_version: String,
    /// OS version, if the client chooses to disclose it.
    pub os_version: Option<String>,
    /// Model, if the client chooses to disclose it.
    pub device_model: Option<String>,
    /// The device credential's ML-DSA-65 public key, when the client is
    /// registering with a root secret and has a credential to introduce.
    /// `None` on every legacy client.
    pub credential_public_key: Option<Vec<u8>>,
}

impl DeviceClaim {
    /// A minimal claim, for tests and for clients that disclose nothing.
    #[must_use]
    pub fn new(platform: Platform, display_name: impl Into<String>) -> Self {
        Self {
            device_id: None,
            platform,
            display_name: display_name.into(),
            app_version: String::new(),
            os_version: None,
            device_model: None,
            credential_public_key: None,
        }
    }

    /// Reuses an existing device row.
    #[must_use]
    pub fn on_device(mut self, device_id: Id) -> Self {
        self.device_id = Some(device_id);
        self
    }

    /// Sets the client version.
    #[must_use]
    pub fn with_app_version(mut self, app_version: impl Into<String>) -> Self {
        self.app_version = app_version.into();
        self
    }
}

/// Longest device display name.
pub const MAX_DEVICE_NAME_CHARS: usize = 60;

/// Longest client version string.
pub const MAX_APP_VERSION_CHARS: usize = 32;

/// Longest OS version or model string.
pub const MAX_DEVICE_DETAIL_CHARS: usize = 60;

/// How long an ML-DSA challenge stays answerable. Five minutes (brief
/// section 182): long enough for a human to find their `.migo` container,
/// short enough that a captured payload is worth almost nothing.
pub const IDENTITY_CHALLENGE_TTL_MS: i64 = 5 * 60 * 1_000;

/// Longest wallet address, as hex with no prefix.
pub const MAX_WALLET_ADDRESS_CHARS: usize = 40;

/// Longest chain type label.
pub const MAX_CHAIN_TYPE_CHARS: usize = 16;

/// Longest wallet label.
pub const MAX_WALLET_LABEL_CHARS: usize = 60;

/// A new account.
#[derive(Debug)]
pub struct Registration {
    /// Desired username. Validated by the `credential` module.
    pub username: String,
    /// Email, optional: Migo permits username-only registration, because requiring an
    /// address to talk to your friends is a data-collection decision dressed as a
    /// security one.
    pub email: Option<String>,
    /// Phone, optional, same reasoning.
    pub phone: Option<String>,
    /// Plaintext password, held only long enough to hash.
    pub password: Secret,
    /// BCP-47 language tag.
    pub locale: String,
    /// ISO-3166 alpha-2 country, if the client discloses it.
    pub country: Option<String>,
    /// Gender as the user disclosed it on the registration form. `None` is
    /// "not disclosed" and stays that way through to the column.
    pub gender: Option<migo_store::model::Gender>,
    /// The device this registration comes from.
    pub device: DeviceClaim,
    /// The account identity's ML-DSA-65 public key, when the client is
    /// registering with a root secret. The account is created with identity
    /// login available from its first breath; `None` registers the
    /// password-only account every legacy client still makes.
    pub identity_public_key: Option<Vec<u8>>,
    /// Captcha proof, when the gate is engaged. `None` is rejected as
    /// `CAPTCHA_REQUIRED`; a present proof is consumed on the way in so
    /// the same `challenge_id` cannot be replayed across two attempts.
    pub captcha: Option<CaptchaProof>,
    /// The server the client believes it is talking to, when it
    /// disclosed one on the form. The route layer can leave this as
    /// `None`; the authenticator resolves the effective server
    /// through [`Registration::server_or_default`], which applies
    /// [`ServerEndpoint::default_for_host`] for any `host` that
    /// arrived in the request, or the loopback default when nothing
    /// arrived at all.
    pub server: Option<ServerEndpoint>,
}

impl Registration {
    /// The server the request should be associated with, defaulting
    /// to the loopback posture when the client did not name one.
    #[must_use]
    pub fn server_or_default(&self) -> ServerEndpoint {
        self.server
            .clone()
            .unwrap_or_else(|| ServerEndpoint::default_for_host("localhost"))
    }
}

/// An existing account signing in.
#[derive(Debug)]
pub struct SignIn {
    /// Username or email. One field because the user does not think of them as
    /// different kinds of thing, and a form that demands they pick is a form that
    /// makes them wrong half the time.
    pub identifier: String,
    /// Plaintext password.
    pub password: Secret,
    /// The device signing in.
    pub device: DeviceClaim,
    /// Captcha proof, required when the per-IP failure counter is at or
    /// past the configured threshold. A first attempt against a fresh
    /// account never needs one; a tenth attempt against the same
    /// network after nine wrong passwords always does.
    pub captcha: Option<CaptchaProof>,
    /// The server the client believes it is talking to, when it
    /// disclosed one on the form. Same defaulting rule as
    /// [`Registration::server`].
    pub server: Option<ServerEndpoint>,
}

impl SignIn {
    /// The server the request should be associated with, defaulting
    /// to the loopback posture when the client did not name one.
    #[must_use]
    pub fn server_or_default(&self) -> ServerEndpoint {
        self.server
            .clone()
            .unwrap_or_else(|| ServerEndpoint::default_for_host("localhost"))
    }
}

/// A refresh token being exchanged.
#[derive(Debug)]
pub struct Refresh {
    /// The token the client holds.
    pub refresh_token: Secret,
    /// The device the client believes it is.
    ///
    /// Checked against the session's device. A refresh token that arrives from a
    /// different device than the one it was minted for is either a bug or a theft, and
    /// there is no way to tell which, so it is treated as the worse case.
    pub device_id: Id,
}

// --- the ML-DSA identity ceremonies (brief section 182) ---------------------

/// Which ceremony a challenge is being requested for, and what the client
/// has to prove to get a real one.
///
/// The two anonymous scopes are bound to facts a stranger cannot guess: a
/// login challenge names a device id the account already trusts, and an
/// add-device challenge names the account id a `.migo` container carries.
/// An identifier alone — a username somebody typed — is never enough to
/// learn whether an account exists, which is the property the fake
/// challenge in the service exists to preserve.
#[derive(Debug)]
pub enum IdentityChallengeScope {
    /// Sign in on a device the account already trusts.
    Login {
        /// Username, email, or phone — the same shapes sign-in accepts.
        identifier: String,
        /// The registered device asking.
        device_id: Id,
    },
    /// Restore the account onto a new device from a `.migo` container.
    AddDevice {
        /// The account id from the container's metadata. Not the username:
        /// an account id is unguessable, a username is not.
        account_id: Id,
    },
}

/// A request for an ML-DSA challenge.
#[derive(Debug)]
pub struct IdentityChallengeRequest {
    /// What the challenge will authorize.
    pub scope: IdentityChallengeScope,
    /// Claims about the new device. Read for `AddDevice` only — a login
    /// challenge is bound to a device that already exists.
    pub device: DeviceClaim,
}

/// What a client gets back when it asks for a challenge.
///
/// The payload is the whole ceremony: the canonical MSE bytes of the
/// protocol's `MlDsaChallenge`, signed exactly as received and never
/// re-encoded by any client. The other fields are conveniences a client
/// would otherwise have to decode the payload for.
#[derive(Debug)]
pub struct ChallengeView {
    /// The canonical bytes to sign.
    pub payload: Vec<u8>,
    /// The challenge's id, echoed back in the answer.
    pub challenge_id: Id,
    /// The device the challenge is bound to. For add-device, the pending
    /// device row that becomes the new device on success.
    pub device_id: Id,
    /// When the challenge stops being answerable.
    pub expires_at: Timestamp,
}

/// An answer to a login challenge: both halves of the ceremony.
///
/// The identity signature proves the account (the root secret's identity
/// domain); the device signature proves the device (the random per-device
/// seed). Neither alone opens a session, which is why a root secret that
/// leaks from a backup is not enough to impersonate a device that is still
/// registered.
#[derive(Debug)]
pub struct ChallengeAnswer {
    /// The challenge being answered.
    pub challenge_id: Id,
    /// Signature by the account identity key under the login context.
    pub identity_signature: Vec<u8>,
    /// Signature by the device credential under the device context.
    pub device_signature: Vec<u8>,
}

/// An answer to an add-device challenge: the account's proof and the new
/// device's introduction.
#[derive(Debug)]
pub struct AddDeviceAnswer {
    /// The challenge being answered.
    pub challenge_id: Id,
    /// Signature by the account identity key under the login context.
    pub identity_signature: Vec<u8>,
    /// The new device's credential public key.
    pub device_public_key: Vec<u8>,
    /// Signature by the new device's credential under the device context,
    /// proving the client holds the private half it is registering.
    pub device_signature: Vec<u8>,
}

/// An answer to a rotation challenge.
#[derive(Debug)]
pub struct RotationAnswer {
    /// The challenge being answered.
    pub challenge_id: Id,
    /// Signature by the *current* identity key under the rotate context:
    /// only the key being retired can authorise its successor.
    pub signature: Vec<u8>,
    /// The successor's public key.
    pub new_public_key: Vec<u8>,
}

/// The public halves an authenticated client is publishing on its own
/// account.
///
/// This is the legacy-upgrade door (brief section 182, migration of
/// password-era accounts): a client that just signed in with a password,
/// built a root secret locally, and derived its identity key registers the
/// public halves here. Idempotent by design — a retry sends the same keys
/// and reconciles to the rows that already exist.
#[derive(Debug)]
pub struct IdentityPublication {
    /// The account identity's ML-DSA-65 public key, 1952 bytes.
    pub identity_public_key: Vec<u8>,
    /// The caller's device credential public key, when it has one to
    /// register. `None` leaves the device row's credential alone.
    pub device_public_key: Option<Vec<u8>>,
}

/// One device of the caller's account, for their own security screen.
#[derive(Clone, Debug)]
pub struct DeviceSummary {
    /// Which device, the id a revoke call names.
    pub device_id: Id,
    /// Name to show, as the client reported it.
    pub display_name: String,
    /// Claimed platform.
    pub platform: Platform,
    /// `active`, `pending`, or `revoked`.
    pub status: String,
    /// When the device was registered.
    pub created_at: Timestamp,
    /// When the device last connected.
    pub last_seen_at: Timestamp,
    /// Whether this device can take part in the ML-DSA login ceremony.
    pub has_credential: bool,
    /// Whether this is the device asking.
    pub is_current: bool,
}

impl serde::Serialize for DeviceSummary {
    /// Hand-written for the same reason as `SessionSummary`'s: `Platform` is
    /// not serialisable, and the timestamps cross the wire as milliseconds.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("DeviceSummary", 8)?;
        s.serialize_field("device_id", &self.device_id)?;
        s.serialize_field("display_name", &self.display_name)?;
        s.serialize_field("platform", self.platform.as_str())?;
        s.serialize_field("status", &self.status)?;
        s.serialize_field("created_at_ms", &self.created_at.as_unix_ms())?;
        s.serialize_field("last_seen_at_ms", &self.last_seen_at.as_unix_ms())?;
        s.serialize_field("has_credential", &self.has_credential)?;
        s.serialize_field("is_current", &self.is_current)?;
        s.end()
    }
}

/// A wallet being registered on the caller's account.
///
/// Address and metadata only. The private key behind it never leaves the
/// device, and the server would not know what to do with it if it arrived.
#[derive(Debug)]
pub struct WalletRegistration {
    /// The EVM address. `0x`-prefixed and checksummed forms are accepted
    /// and normalised to the canonical lowercase-hex form.
    pub address: String,
    /// `"evm"` today.
    pub chain_type: String,
    /// User-chosen label, if any.
    pub label: Option<String>,
    /// The `i` in `m/44'/60'/0'/0/i`, so a restore re-registers in order.
    pub derivation_index: i32,
}

/// One registered wallet, for the caller's own wallet list.
#[derive(Clone, Debug)]
pub struct WalletSummary {
    /// Which wallet, the id an archive call names.
    pub wallet_id: Id,
    /// Lowercase hex, no prefix — the canonical stored form.
    pub address: String,
    /// `"evm"` today.
    pub chain_type: String,
    /// User-chosen label, if any.
    pub label: Option<String>,
    /// The derivation index that produced this address.
    pub derivation_index: i32,
    /// `active` or `archived`.
    pub status: String,
    /// When the wallet was first registered.
    pub created_at: Timestamp,
    /// When the user archived it, if they did.
    pub archived_at: Option<Timestamp>,
}

impl serde::Serialize for WalletSummary {
    /// Timestamps as milliseconds, matching every other summary the API
    /// returns.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("WalletSummary", 8)?;
        s.serialize_field("wallet_id", &self.wallet_id)?;
        s.serialize_field("address", &self.address)?;
        s.serialize_field("chain_type", &self.chain_type)?;
        if let Some(label) = &self.label {
            s.serialize_field("label", label)?;
        }
        s.serialize_field("derivation_index", &self.derivation_index)?;
        s.serialize_field("status", &self.status)?;
        s.serialize_field("created_at_ms", &self.created_at.as_unix_ms())?;
        if let Some(at) = self.archived_at {
            s.serialize_field("archived_at_ms", &at.as_unix_ms())?;
        }
        s.end()
    }
}

impl From<migo_store::traits::WalletRow> for WalletSummary {
    /// The store row carries the status as an enum this crate's callers do
    /// not see; the summary carries it as the one word a client renders.
    fn from(row: migo_store::traits::WalletRow) -> Self {
        Self {
            wallet_id: row.wallet_id,
            address: row.address,
            chain_type: row.chain_type,
            label: row.label,
            derivation_index: row.derivation_index,
            status: match row.status {
                migo_store::model::WalletStatus::Active => "active",
                migo_store::model::WalletStatus::Archived => "archived",
            }
            .to_string(),
            created_at: row.created_at,
            archived_at: row.archived_at,
        }
    }
}

/// What a successful authentication yields.
///
/// `Debug` is derived and safe: the refresh token is a [`Secret`], and the access token
/// is short-lived and inert without the signing key. That said, neither belongs in a
/// log, which is why brief section 174 forbids it and why nothing in this crate traces
/// this type.
#[derive(Debug)]
pub struct Grant {
    /// Who signed in.
    pub account_id: Id,
    /// On which device.
    pub device_id: Id,
    /// The session opened or rotated.
    pub session_id: Id,
    /// Bearer token for the next `access_ttl_seconds`.
    pub access_token: String,
    /// Single-use token for the next `refresh_ttl_seconds`.
    pub refresh_token: Secret,
    /// When the access token stops working.
    pub access_expires_at: Timestamp,
    /// When the refresh token stops working, after which the user signs in again.
    pub refresh_expires_at: Timestamp,
    /// What this session may do.
    pub capabilities: Capabilities,
    /// Whether the account was created by this call.
    ///
    /// The client uses it to decide between showing an onboarding flow and restoring a
    /// conversation list. Cheaper than a second round trip to find out.
    pub is_new_account: bool,
}

/// One live session, for the user's own security screen.
///
/// Deliberately not the store's `Session`: that row carries the refresh hash, and a
/// type that carries a credential hash should not be the type a handler serialises.
#[derive(Clone, Debug)]
pub struct SessionSummary {
    /// Session id, which is what a revoke call names.
    pub session_id: Id,
    /// Device this session belongs to.
    pub device_id: Id,
    /// Device name, for a human to recognise.
    pub device_name: String,
    /// Claimed platform.
    pub platform: Platform,
    /// When the session was opened.
    pub created_at: Timestamp,
    /// When the refresh window closes.
    pub refresh_expires_at: Timestamp,
    /// Truncated network class. Never a full address, not even to the account owner:
    /// brief section 162 does not carve out an exception for the data subject, and a
    /// screen that shows a full address is a screen that leaks one to whoever is
    /// looking over their shoulder.
    pub ip_class: Option<String>,
    /// User agent, as the client reported it.
    pub user_agent: Option<String>,
    /// Whether this is the session asking.
    pub is_current: bool,
}

impl serde::Serialize for SessionSummary {
    /// Hand-written because the `Platform` enum is not serialisable: a
    /// session-list UI does not need to know whether the device is a
    /// phone or a laptop, only how to label the row. The wire shape skips
    /// the field entirely.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("SessionSummary", 8)?;
        s.serialize_field("session_id", &self.session_id)?;
        s.serialize_field("device_id", &self.device_id)?;
        s.serialize_field("device_name", &self.device_name)?;
        s.serialize_field("created_at", &self.created_at)?;
        s.serialize_field("refresh_expires_at", &self.refresh_expires_at)?;
        if let Some(ip) = &self.ip_class {
            s.serialize_field("ip_class", ip)?;
        }
        if let Some(ua) = &self.user_agent {
            s.serialize_field("user_agent", ua)?;
        }
        s.serialize_field("is_current", &self.is_current)?;
        s.end()
    }
}

/// Truncates a string to a character count, not a byte count.
///
/// Byte truncation splits multi-byte characters, and a display name is exactly where
/// multi-byte characters live. `String::truncate` would panic on that boundary rather
/// than corrupt it, which is better but still a panic in a request handler.
pub(crate) fn truncate_chars(value: &mut String, max_chars: usize) {
    if value.chars().count() <= max_chars {
        return;
    }
    let cut = value
        .char_indices()
        .nth(max_chars)
        .map_or(value.len(), |(index, _)| index);
    value.truncate(cut);
}

/// One global admin as the owner's management list renders it: the store row
/// plus the username a human reads.
#[derive(Clone, Debug)]
pub struct AdminView {
    /// Which account may moderate every public room.
    pub account_id: Id,
    /// The account's display name, resolved at read time.
    pub username: String,
    /// Who appointed them — always the Owner/CEO in this version.
    pub granted_by: Id,
    /// When the grant happened.
    pub granted_at: Timestamp,
}

impl serde::Serialize for AdminView {
    /// Timestamps as milliseconds, matching every other summary the API
    /// returns.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("AdminView", 4)?;
        s.serialize_field("account_id", &self.account_id)?;
        s.serialize_field("username", &self.username)?;
        s.serialize_field("granted_by", &self.granted_by)?;
        s.serialize_field("granted_at_ms", &self.granted_at.as_unix_ms())?;
        s.end()
    }
}

/// What the signed-in account is allowed to open: the answer to "may I see
/// the admin surface?" before a single row is fetched.
#[derive(Clone, Copy, Debug)]
pub struct AdminStanding {
    /// True only for the account named by `owner_account_id` in config.
    pub owner: bool,
    /// True for any account with a `global_admin` row.
    pub admin: bool,
}

impl serde::Serialize for AdminStanding {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("AdminStanding", 2)?;
        s.serialize_field("owner", &self.owner)?;
        s.serialize_field("admin", &self.admin)?;
        s.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_characters_not_bytes() {
        let mut value = "ααααα".to_string();
        truncate_chars(&mut value, 3);
        assert_eq!(value, "ααα");
        assert_eq!(value.len(), 6, "three two-byte characters");
    }

    #[test]
    fn truncation_leaves_short_values_alone() {
        let mut value = "chrome".to_string();
        truncate_chars(&mut value, 60);
        assert_eq!(value, "chrome");
    }

    #[test]
    fn a_context_without_an_address_has_no_network_class() {
        let context = RequestContext::at(Timestamp::from_millis(1));
        assert!(context.ip_class().is_none());
    }

    #[test]
    fn a_context_stores_a_network_class_and_not_an_address() {
        let context = RequestContext::at(Timestamp::from_millis(1))
            .from_ip("203.0.113.77".parse().expect("literal address"));
        let class = context.ip_class().expect("address present");
        assert_eq!(class, "203.0.113.0/24");
        assert!(!class.contains("113.77"), "host octet must not survive");
    }

    #[test]
    fn a_long_user_agent_is_truncated_on_the_way_in() {
        let context =
            RequestContext::at(Timestamp::from_millis(1)).with_user_agent("u".repeat(5_000));
        assert_eq!(
            context.user_agent.expect("set").chars().count(),
            MAX_USER_AGENT_CHARS
        );
    }
}
