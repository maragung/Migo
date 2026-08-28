//! The authentication contract.
//!
//! One trait. The gateway, the HTTP API, and the bot runtime all take
//! `Arc<dyn Authenticator>` and none of them knows whether the accounts live in Postgres
//! or in a test double that accepts one password.
//!
//! # Why the whole surface is one trait
//!
//! [`migo_store`] splits its work across ten narrow traits, because a crate that only
//! needs to read a room should not be able to write a ledger. This one does not split,
//! because the operations here are not independent: registering has to open a session,
//! refreshing has to be able to revoke a family, changing a password has to revoke every
//! session and open a replacement. Handing out a narrow `Refresher` that could rotate a
//! session but not revoke its family would be handing out the half of the operation
//! without the safety property.

use async_trait::async_trait;
use migo_core::{Id, Result, Secret, Timestamp};
use migo_ratelimit::TrustTier;
use migo_store::traits::RecoveryRow;

use crate::capability::Capabilities;
use crate::model::{Grant, Refresh, Registration, RequestContext, SessionSummary, SignIn};
use crate::token::Claims;

/// How recently the human must have authenticated for a sensitive operation.
///
/// Five minutes. Short enough that a borrowed unlocked laptop is not an account
/// takeover, long enough that a user who is genuinely working through their security
/// settings is not asked twice. Brief section 125 requires the check; the number is an
/// engineering choice and it is here, once, rather than at each call site.
pub const REAUTH_WINDOW_MS: u64 = 5 * 60 * 1_000;

/// Who is calling, established from a verified token and the rows behind it.
///
/// Deliberately not [`migo_store::model::Account`]: that row carries the password hash,
/// and a type carrying a password hash should not be the type every handler holds and
/// passes around. What is here is what a caller downstream of authentication actually
/// needs.
#[derive(Clone, Debug)]
pub struct Identity {
    /// Verified claims, exactly as signed.
    pub claims: Claims,
    /// Username, for logs and for rendering an actor in an audit trail.
    ///
    /// The display form. Not a key — the username can change, the account id cannot.
    pub username: String,
    /// Standing, for the rate limiter.
    ///
    /// Recomputed here rather than taken from the token, so an account that left
    /// probation an hour ago is not held to a probationary limit until its token
    /// expires.
    pub tier: TrustTier,
    /// What this session may do.
    ///
    /// Also recomputed, and merged with any bits the token carried that this build does
    /// not recognise — a newer node may mint a capability this one has never heard of,
    /// and dropping it would silently downgrade the session.
    pub capabilities: Capabilities,
}

impl Identity {
    /// Who is calling.
    #[must_use]
    pub fn account_id(&self) -> Id {
        self.claims.account_id
    }

    /// From which device.
    #[must_use]
    pub fn device_id(&self) -> Id {
        self.claims.device_id
    }

    /// On which session.
    #[must_use]
    pub fn session_id(&self) -> Id {
        self.claims.session_id
    }

    /// Whether the human authenticated recently enough for a sensitive operation.
    #[must_use]
    pub fn is_fresh(&self, now: Timestamp) -> bool {
        self.claims.presence_is_fresh(now, REAUTH_WINDOW_MS)
    }

    /// `REAUTHENTICATION_REQUIRED` unless the human authenticated recently.
    ///
    /// A function rather than a note in a doc comment, so that every sensitive operation
    /// spells the requirement the same way and a new one cannot forget to.
    pub fn require_fresh(&self, now: Timestamp) -> Result<()> {
        if self.is_fresh(now) {
            return Ok(());
        }
        Err(migo_protocol::fault::error(
            migo_protocol::codes::REAUTHENTICATION_REQUIRED,
            "the password was last entered too long ago for this operation",
        ))
    }
}

/// A password change.
///
/// The current password is required and is what makes this operation authenticated: a
/// change that only needed a session token would turn a stolen token into a permanent
/// takeover, since the thief could lock the owner out. It is also why this operation
/// does not additionally demand [`Identity::require_fresh`] — typing the current
/// password *is* the reauthentication.
#[derive(Debug)]
pub struct PasswordChange {
    /// The password in force.
    pub current: Secret,
    /// The password to replace it with.
    pub next: Secret,
}

/// Registration, sign-in, refresh, and the revocation operations that go with them.
#[async_trait]
pub trait Authenticator: Send + Sync {
    /// Creates an account and opens its first session.
    ///
    /// Fails with `FEATURE_DISABLED` when the deployment has registration turned off,
    /// `USERNAME_TAKEN` or `ALREADY_EXISTS` on a collision, `USERNAME_RESERVED`,
    /// `WEAK_PASSWORD`, `VALIDATION_FAILED`, or `RATE_LIMITED`.
    async fn register(&self, request: Registration, context: &RequestContext) -> Result<Grant>;

    /// Exchanges a password for a session.
    ///
    /// Fails with `INVALID_CREDENTIALS` for both an unknown account and a wrong
    /// password — the two are indistinguishable to the caller on purpose, in the
    /// response *and* in the time taken. `ACCOUNT_SUSPENDED` is only reachable after the
    /// password has been verified, because reporting a suspension to whoever asks would
    /// confirm the account exists.
    async fn sign_in(&self, request: SignIn, context: &RequestContext) -> Result<Grant>;

    /// Exchanges a refresh token for the next generation of a session.
    ///
    /// Fails with `REFRESH_REUSE_DETECTED` when the presented token has already been
    /// exchanged, having first revoked every generation of its family: a token that
    /// comes back twice is a token that was copied, and there is no way to tell the
    /// legitimate holder from the thief, so both are logged out and the human is asked
    /// to sign in again.
    async fn refresh(&self, request: Refresh, context: &RequestContext) -> Result<Grant>;

    /// Verifies an access token and the rows behind it.
    ///
    /// The connection-establishment path. Reads the session, account, and device, so
    /// revocation takes effect immediately, and stamps the device's last-seen time.
    ///
    /// Does not charge the rate limiter. The caller charges `AUTHENTICATE` before
    /// getting here, and a second charge inside would price one operation twice and make
    /// the configured cost of an opcode a lie.
    async fn authenticate(
        &self,
        access_token: &str,
        device_id: Id,
        context: &RequestContext,
    ) -> Result<Identity>;

    /// Verifies an access token's signature and expiry, and nothing else.
    ///
    /// No I/O, so it is usable on a per-request path that cannot afford a database read.
    /// The cost is that revocation is not visible here: a revoked session's access token
    /// keeps verifying until it expires, which is `auth.access_ttl_seconds` — fifteen
    /// minutes by default. Anything that must observe a revocation immediately has to
    /// use [`Authenticator::authenticate`].
    fn verify_access(&self, access_token: &str, now: Timestamp) -> Result<Claims>;

    /// The region a token was minted in, without verifying it.
    ///
    /// For a node that failed to verify a token and wants to say where to send it rather
    /// than that it is invalid. Unauthenticated: usable to route a retry, and for
    /// nothing else.
    fn token_region(&self, access_token: &str) -> Option<String>;

    /// Revokes one session.
    ///
    /// `session_id` must belong to the caller's account. A session belonging to somebody
    /// else fails with `NOT_FOUND` rather than `PERMISSION_DENIED`, because
    /// `PERMISSION_DENIED` would confirm that the session id names something real and
    /// turn this into a probe.
    async fn sign_out(
        &self,
        identity: &Identity,
        session_id: Id,
        context: &RequestContext,
    ) -> Result<()>;

    /// Revokes every session except the caller's own, and returns how many.
    ///
    /// "Log out my other devices" (brief section 46). Requires recent authentication:
    /// somebody holding a borrowed unlocked laptop should not be able to lock the owner
    /// out of every other device with one tap.
    async fn sign_out_others(&self, identity: &Identity, context: &RequestContext) -> Result<u64>;

    /// The caller's live sessions, for their own security screen.
    async fn sessions(
        &self,
        identity: &Identity,
        context: &RequestContext,
    ) -> Result<Vec<SessionSummary>>;

    /// Revokes a device and every session on it.
    ///
    /// Section 47: a revoked device loses the ability to fetch new key bundles and its
    /// ratchet sessions stop. That part belongs to the key and messaging crates; this
    /// call is the authentication half — the device row is marked revoked and its
    /// sessions die, so nothing on that device can authenticate again.
    async fn revoke_device(
        &self,
        identity: &Identity,
        device_id: Id,
        context: &RequestContext,
    ) -> Result<u64>;

    /// Replaces the password, revokes every session, and opens a replacement.
    ///
    /// Every session including the caller's own, which is why a [`Grant`] comes back:
    /// the client swaps its tokens and stays signed in, and every other device is logged
    /// out. Keeping the current session alive instead would mean a password changed
    /// *because* a token was stolen left the thief's session running if they happened to
    /// be the one who changed it.
    async fn change_password(
        &self,
        identity: &Identity,
        change: PasswordChange,
        context: &RequestContext,
    ) -> Result<Grant>;

    /// Records a recoverable contact on the caller's account.
    ///
    /// The value is parsed as one of an email or a phone number. The auth crate
    /// validates the format; the store persists it on the right column so
    /// the `CONTACTABLE` capability bit kicks in on the next
    /// [`Authenticator::authenticate`].
    async fn set_contact(
        &self,
        identity: &Identity,
        contact: &str,
        context: &RequestContext,
    ) -> Result<()>;

    /// Mints a captcha challenge and returns its public view. Returns `None`
    /// when the gate is not wired, which the route layer surfaces as
    /// `FEATURE_DISABLED`.
    fn issue_captcha<'a>(
        &'a self,
        now: Timestamp,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<migo_captcha::CaptchaChallengeView>>
                + Send
                + 'a,
        >,
    >;

    /// Starts a password-recovery flow: looks the account up by `identifier`
    /// (the same email/phone/username shape [`Authenticator::sign_in`]
    /// accepts), mints a recovery row, and returns the row's `token_id` and
    /// the hex-encoded HMAC tag that the caller proves possession of when
    /// the user comes back to confirm.
    ///
    /// Enumeration-safe: an identifier that does not match any account
    /// returns the same shape and never reveals it was wrong. A wrong
    /// captcha, an invalid format, or a deployment with the captcha turned
    /// off entirely is the caller's responsibility to surface; this method
    /// assumes a valid proof was supplied.
    async fn request_recovery(
        &self,
        identifier: &str,
        captcha: &migo_captcha::CaptchaProof,
        context: &RequestContext,
    ) -> Result<RecoveryRow>;

    /// Confirms a recovery row, applies a new password, and revokes every
    /// other session.
    ///
    /// The HMAC `tag` is verified with the per-purpose `LABEL_RECOVERY`
    /// subkey; the row's `consumed_at` is stamped in one statement with the
    /// call so a confirm cannot replay. `Ok(())` on success.
    ///
    /// Fails with `RECOVERY_NOT_FOUND` when the row is unknown, already
    /// consumed, or expired; with `WEAK_PASSWORD` when the new password is
    /// too short or on the common list; and with `INVALID_CAPTCHA` if the
    /// tag does not verify (the row is left in place, so a typo on the
    /// first try does not consume the row).
    async fn confirm_recovery(
        &self,
        token_id: Id,
        tag: &[u8],
        new_password: &Secret,
        context: &RequestContext,
    ) -> Result<()>;
}
