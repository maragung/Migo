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
    /// The device this registration comes from.
    pub device: DeviceClaim,
    /// Captcha proof, when the gate is engaged. `None` is rejected as
    /// `CAPTCHA_REQUIRED`; a present proof is consumed on the way in so
    /// the same `challenge_id` cannot be replayed across two attempts.
    pub captcha: Option<CaptchaProof>,
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
