//! The authentication service.
//!
//! # Shape
//!
//! [`Auth`] is generic over the store and the limiter with `dyn` defaults, so it can be
//! held as `Auth` (both erased, one vtable call per operation) or as
//! `Auth<MemoryStore, CacheRateLimiter>` (monomorphised, no vtable at all). Being generic
//! over `?Sized` and bounded on the narrow traits accepts both a concrete store and a
//! fully erased one, which is what the tests and the composition root respectively need:
//! an `Arc<dyn Store>` satisfies a `?Sized` bound on `AccountStore` directly, with no
//! coercion at the call site, and a caller who has a concrete store keeps it. Trait
//! upcasting would narrow `Arc<dyn Store>` to `Arc<dyn AccountStore>` instead — it is
//! available (stable in 1.86; the workspace declares 1.94) and deliberately not used,
//! because it buys nothing for the erased case and forces a vtable on the concrete one.
//!
//! # What the caller must not learn
//!
//! Two properties are load-bearing and easy to break by accident.
//!
//! An unknown account and a wrong password produce the same error *and take the same
//! time*. The error is easy; the timing is not. A missing account has no hash to verify,
//! so the natural implementation returns in microseconds while a real account spends
//! forty milliseconds in Argon2id — a difference so large it is measurable over the
//! public internet, and it turns the sign-in endpoint into an account enumerator. So a
//! missing account is verified against a placeholder hash created at startup, and the
//! result is thrown away.
//!
//! A suspended account is reported as suspended only *after* the password has been
//! checked. Reporting it earlier would answer "does this account exist" to anyone who
//! asked.
//!
//! # Why failure is priced higher than success
//!
//! A wrong password costs the whole anonymous endpoint bucket; a right one costs a fifth
//! of it. Both numbers are derived from the resolved bucket at construction rather than
//! written down, because the limiter writes nothing on a refusal — a penalty larger than
//! the bucket is silently dropped, and a hardcoded penalty would become a no-op the day
//! an operator lowered `anonymous_burst`. Deriving it means the penalty is always exactly
//! affordable and therefore always actually charged.
//!
//! There is deliberately no per-*account* failure limit. A limit that locks an account
//! after N wrong passwords is a way for a stranger who knows a username to lock its owner
//! out, which converts a nuisance into an outage. The pressure goes on the network the
//! guesses come from.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use migo_core::config::{AuthConfig, Config};
use migo_core::metrics::Registry;
use migo_core::{Error, Id, OsRandom, Random, Result, Secret, Timestamp};
use migo_protocol::{codes, fault, Opcode, Platform};
use migo_ratelimit::{BucketKey, RateLimiter, Scope, SharedRateLimiter, TrustTier};
use migo_store::model::{
    Account, AccountStatus, AuditActorKind, AuditEntry, AuditTargetKind, Device, NewAccount,
    NewDevice, NewSession, Profile, RevokeReason, Session, Visibility,
};
use migo_store::traits::{AccountStore, DeviceStore, SafetyStore, SessionStore};
use migo_store::{SharedStore, Store};
use parking_lot::Mutex;

use crate::capability::Capabilities;
use crate::credential;
use crate::metrics::{Meters, RefreshOutcome, SignInOutcome};
use crate::model::{
    truncate_chars, DeviceClaim, Grant, Refresh, Registration, RequestContext, SessionSummary,
    SignIn, MAX_APP_VERSION_CHARS, MAX_DEVICE_DETAIL_CHARS, MAX_DEVICE_NAME_CHARS,
};
use crate::tier;
use crate::token::{Claims, Signer};
use crate::traits::{Authenticator, Identity, PasswordChange};

/// A shared, fully erased authenticator.
pub type SharedAuth = Arc<dyn Authenticator>;

/// The generation number of a family's first session.
const FIRST_GENERATION: i32 = 1;

/// What the placeholder hash is made from.
///
/// The value is irrelevant — nothing ever verifies against it successfully. It exists so
/// that the *work* of a verification happens on the unknown-account path too.
const ABSENT_ACCOUNT_PLACEHOLDER: &str = "migo placeholder for accounts that do not exist";

/// Prices for the operations that strangers can reach.
///
/// Derived from the resolved anonymous endpoint bucket, once, at construction. See the
/// module docs for why these are computed rather than written down.
#[derive(Clone, Copy, Debug)]
struct Prices {
    /// Charged before credentials are checked.
    attempt: u32,
    /// Charged on top, after a failure, so a failure costs the whole bucket.
    penalty: u32,
    /// Charged for creating an account.
    register: u32,
}

impl Prices {
    fn from_policies(policies: &migo_ratelimit::Policies) -> Self {
        // The tightest surface a stranger reaches: one opcode, one network, no standing.
        let full = policies
            .resolve(Scope::Endpoint, TrustTier::Anonymous)
            .capacity();
        let attempt = (full / 5).max(1);
        Self {
            attempt,
            penalty: full.saturating_sub(attempt),
            // Creating an account is the expensive one: it writes rows, hashes a
            // password, and is the thing a spam operation needs thousands of.
            register: full,
        }
    }
}

/// Registration, sign-in, refresh, and revocation.
pub struct Auth<S: ?Sized = dyn Store, L: ?Sized = dyn RateLimiter> {
    store: Arc<S>,
    limiter: Arc<L>,
    signer: Signer,
    config: AuthConfig,
    prices: Prices,
    /// A valid Argon2id hash of a value nobody knows. See the module docs.
    absent_hash: Secret,
    /// The randomness source, behind a lock because [`Random`] is `Send` and not `Sync`.
    ///
    /// Every use is a few bytes and the lock is never held across an `await`, which is
    /// what keeps a mutex on a hot path from becoming a scheduling problem.
    random: Mutex<Box<dyn Random>>,
    meters: Meters,
}

/// Builds an authenticator over the shared store and limiter.
///
/// Fails when the token key is missing or too short, when the node's region label does
/// not fit the token layout, or when the placeholder hash cannot be produced — all three
/// at startup, so `migod` declines to boot rather than serving a broken login.
pub fn open(
    store: SharedStore,
    limiter: SharedRateLimiter,
    config: &Config,
    registry: &Registry,
) -> Result<SharedAuth> {
    let auth = Auth::new(
        store,
        limiter,
        config,
        registry,
        Box::new(OsRandom) as Box<dyn Random>,
    )?;
    Ok(Arc::new(auth))
}

impl<S, L> Auth<S, L>
where
    S: AccountStore + DeviceStore + SessionStore + SafetyStore + ?Sized,
    L: RateLimiter + ?Sized,
{
    /// Builds an authenticator over a concrete or erased store and limiter.
    ///
    /// `random` is injected rather than fixed to [`OsRandom`] so a simulation can replay
    /// a run byte for byte (ADR-0009). It is used for session ids, family ids, device
    /// ids, refresh tokens, and password salts.
    pub fn new(
        store: Arc<S>,
        limiter: Arc<L>,
        config: &Config,
        registry: &Registry,
        random: Box<dyn Random>,
    ) -> Result<Self> {
        let key =
            config.auth.token_key.as_ref().ok_or_else(|| {
                fault::internal("auth.token_key is required to sign session tokens")
            })?;
        let signer = Signer::new(key, &config.node.region)?;
        let prices = Prices::from_policies(limiter.policies());
        let mut random = random;
        let absent_hash = migo_crypto::password::hash(ABSENT_ACCOUNT_PLACEHOLDER, &mut *random)
            .map_err(|error| {
                fault::internal(format!("could not build the placeholder hash: {error}"))
            })?;
        Ok(Self {
            store,
            limiter,
            signer,
            config: config.auth.clone(),
            prices,
            absent_hash,
            random: Mutex::new(random),
            meters: Meters::new(registry),
        })
    }

    /// The signer, for a caller that verifies tokens without the rest of the service.
    pub fn signer(&self) -> &Signer {
        &self.signer
    }

    // --- randomness ------------------------------------------------------------

    /// A fresh id stamped with `now`.
    fn new_id(&self, now: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(now, &mut **random)
    }

    /// Salt material for one password hash, drawn before the blocking work starts.
    fn draw_salt(&self) -> [u8; SALT_CARRY] {
        let mut bytes = [0u8; SALT_CARRY];
        let mut random = self.random.lock();
        random.fill_bytes(&mut bytes);
        bytes
    }

    /// A refresh token and the tag to store for it.
    fn new_refresh(&self) -> (Secret, [u8; 32]) {
        let mut random = self.random.lock();
        self.signer.mint_refresh(&mut **random)
    }

    // --- password work ---------------------------------------------------------

    /// Hashes a password off the async worker threads.
    ///
    /// Argon2id at the configured cost spends tens of milliseconds and 19 MiB in a tight
    /// loop. Doing that on a runtime worker stalls every other task that worker was
    /// going to poll — including the ones that have nothing to do with logging in — so it
    /// goes to the blocking pool, which exists for exactly this.
    ///
    /// The salt is drawn from the injected [`Random`] *before* the hop and replayed
    /// inside, so a seeded run stays reproducible even though the hashing happens on
    /// another thread.
    async fn hash_password(&self, password: &str, meters: &Meters) -> Result<Secret> {
        let salt = self.draw_salt();
        let owned = password.to_string();
        let started = std::time::Instant::now();
        let hashed = tokio::task::spawn_blocking(move || {
            let mut random = CarriedRandom::new(salt);
            migo_crypto::password::hash(&owned, &mut random)
        })
        .await
        .map_err(|_| fault::internal("password hashing task failed"))?;
        meters.hash_took(started.elapsed().as_secs_f64() * 1_000.0);
        hashed.map_err(|error| {
            // A refusal from the crypto crate at this point means the password broke a
            // length bound the `credential` module should already have caught, so it is
            // reported as our bug rather than as the user's.
            fault::internal(format!("password could not be hashed: {error}"))
        })
    }

    /// Verifies a password off the async worker threads. See [`Auth::hash_password`].
    async fn verify_password(
        &self,
        password: &str,
        stored: &Secret,
        meters: &Meters,
    ) -> Result<Option<migo_crypto::Verification>> {
        let owned = password.to_string();
        let stored = stored.clone();
        let started = std::time::Instant::now();
        let outcome =
            tokio::task::spawn_blocking(move || migo_crypto::password::verify(&owned, &stored))
                .await
                .map_err(|_| fault::internal("password verification task failed"))?;
        meters.hash_took(started.elapsed().as_secs_f64() * 1_000.0);
        match outcome {
            Ok(verification) => Ok(verification),
            // A stored hash that will not parse is a corrupt row, not a wrong password.
            // Reporting it as a wrong password would leave the owner locked out of their
            // account with nothing in the logs to explain it.
            Err(error) => Err(fault::internal(format!(
                "stored password hash is unusable: {error}"
            ))),
        }
    }

    // --- rate limiting ---------------------------------------------------------

    /// Charges the stranger surfaces for one attempt.
    ///
    /// Returns `Ok(())` when there is no address to charge: a request with no address
    /// came from inside the process or over a unix socket. The alternative — one shared
    /// bucket for every addressless caller — would let a load test rate limit the admin
    /// tooling.
    async fn charge_stranger(
        &self,
        context: &RequestContext,
        opcode: Opcode,
        cost: u32,
    ) -> Result<()> {
        let Some(ip) = context.ip else {
            return Ok(());
        };
        // Tightest surface first: a refusal short-circuits the rest, so the narrow bucket
        // should be the one that gets the chance to refuse.
        let keys = [BucketKey::endpoint_of_ip(ip, opcode), BucketKey::ip(ip)];
        self.limiter
            .charge(&keys, cost, TrustTier::Anonymous, context.now)
            .await?
            .into_result()
    }

    /// Charges the failure surcharge, ignoring the verdict.
    ///
    /// The verdict is ignored because the caller is already returning an error and the
    /// charge is a side effect, not a gate. An error from the limiter itself — a cache
    /// outage, say — must not replace the authentication error the caller is about to
    /// return, or a Redis blip would report itself as a login failure.
    async fn charge_penalty(&self, context: &RequestContext, opcode: Opcode) {
        let Some(ip) = context.ip else {
            return;
        };
        if self.prices.penalty == 0 {
            return;
        }
        let keys = [BucketKey::endpoint_of_ip(ip, opcode)];
        let _ = self
            .limiter
            .charge(
                &keys,
                self.prices.penalty,
                TrustTier::Anonymous,
                context.now,
            )
            .await;
    }

    /// Charges an authenticated caller's own surfaces.
    async fn charge_account(
        &self,
        identity: &Identity,
        context: &RequestContext,
        opcode: Opcode,
    ) -> Result<()> {
        let keys = [
            BucketKey::endpoint_of_account(identity.account_id(), opcode),
            BucketKey::account(identity.account_id()),
        ];
        self.limiter
            .charge(&keys, opcode.cost(), identity.tier, context.now)
            .await?
            .into_result()
    }

    // --- session minting -------------------------------------------------------

    /// Opens a session and returns the grant for it.
    ///
    /// `authenticated_at` is a parameter rather than `now` because a rotation carries the
    /// original value forward: see [`migo_store::model::Session::authenticated_at`].
    #[allow(clippy::too_many_arguments)]
    async fn open_session(
        &self,
        account: &Account,
        device: &Device,
        tier: TrustTier,
        authenticated_at: Timestamp,
        context: &RequestContext,
        previous: Option<Id>,
        family: Option<(Id, i32)>,
        is_new_account: bool,
    ) -> Result<Grant> {
        let now = context.now;
        let session_id = self.new_id(now);
        let (family_id, generation) =
            family.unwrap_or_else(|| (self.new_id(now), FIRST_GENERATION));
        let (refresh_token, refresh_tag) = self.new_refresh();

        let access_expires_at =
            now.saturating_add_millis(seconds_to_millis(self.config.access_ttl_seconds));
        let refresh_expires_at =
            now.saturating_add_millis(seconds_to_millis(self.config.refresh_ttl_seconds));

        let capabilities = Capabilities::for_account(account, tier);
        let new = NewSession {
            session_id,
            account_id: account.account_id,
            device_id: device.device_id,
            family_id,
            refresh_hash: refresh_tag.to_vec(),
            generation,
            created_at: now,
            authenticated_at,
            access_expires_at,
            refresh_expires_at,
            ip_class: context.ip_class(),
            user_agent: context.user_agent.clone(),
        };

        let session = match previous {
            Some(previous) => self.store.rotate_session(previous, new).await?,
            None => self.store.create_session(new).await?,
        };

        let claims = Claims {
            account_id: session.account_id,
            device_id: session.device_id,
            session_id: session.session_id,
            capabilities,
            issued_at: now,
            expires_at: access_expires_at,
            authenticated_at,
        };
        Ok(Grant {
            account_id: session.account_id,
            device_id: session.device_id,
            session_id: session.session_id,
            access_token: self.signer.mint(&claims),
            refresh_token,
            access_expires_at,
            refresh_expires_at,
            capabilities,
            is_new_account,
        })
    }

    // --- devices ---------------------------------------------------------------

    /// Finds or registers the device a sign-in is coming from.
    ///
    /// A claimed id that belongs to somebody else fails with `DEVICE_MISMATCH`. A claimed
    /// id that was revoked is *not* resurrected — a new device is registered instead,
    /// because reviving a revoked device on the next sign-in would make "revoke this
    /// device" mean "revoke it until the password is typed again", which is not what the
    /// user was told the button does.
    async fn resolve_device(
        &self,
        account: &Account,
        claim: &DeviceClaim,
        now: Timestamp,
    ) -> Result<Device> {
        if let Some(device_id) = claim.device_id {
            if let Some(existing) = self.store.device_by_id(device_id).await? {
                if existing.account_id != account.account_id {
                    return Err(fault::error(
                        codes::DEVICE_MISMATCH,
                        "the claimed device belongs to another account",
                    ));
                }
                if existing.revoked_at.is_none() {
                    self.store.touch_device(device_id, now).await?;
                    return Ok(Device {
                        last_seen_at: now,
                        ..existing
                    });
                }
            }
        }

        let live = self.store.devices_for_account(account.account_id).await?;
        if live.len() >= self.config.max_devices_per_user as usize {
            return Err(fault::error(
                codes::TOO_MANY_SESSIONS,
                "the account already has as many devices as it may have",
            ));
        }

        let mut display_name = claim.display_name.trim().to_string();
        if display_name.is_empty() {
            display_name = default_device_name(claim.platform).to_string();
        }
        truncate_chars(&mut display_name, MAX_DEVICE_NAME_CHARS);
        let mut app_version = claim.app_version.trim().to_string();
        truncate_chars(&mut app_version, MAX_APP_VERSION_CHARS);

        self.store
            .register_device(NewDevice {
                device_id: self.new_id(now),
                account_id: account.account_id,
                // Recorded as claimed, trusted for nothing. See the `tier` module.
                platform: claim.platform,
                display_name,
                app_version,
                os_version: claim.os_version.clone().map(|mut value| {
                    truncate_chars(&mut value, MAX_DEVICE_DETAIL_CHARS);
                    value
                }),
                device_model: claim.device_model.clone().map(|mut value| {
                    truncate_chars(&mut value, MAX_DEVICE_DETAIL_CHARS);
                    value
                }),
                created_at: now,
            })
            .await
    }

    // --- audit -----------------------------------------------------------------

    /// Appends a security event.
    ///
    /// Only the events an operator or a user would need to reconstruct an incident:
    /// registration, password change, device revocation, and family revocation. Not every
    /// sign-in — the session row and `last_login_at` already record those, and one audit
    /// row per sign-in would bury the four events that matter under millions that do not.
    ///
    /// A failure to write the audit row is logged and swallowed. The alternative is
    /// failing the operation the user asked for because the log was full, and an audit
    /// trail that can take down authentication is a denial-of-service surface rather than
    /// a security control.
    async fn audit(
        &self,
        actor: Option<Id>,
        actor_kind: AuditActorKind,
        action: &str,
        target_kind: AuditTargetKind,
        target: Option<Id>,
        summary: String,
        context: &RequestContext,
    ) {
        let entry = AuditEntry {
            audit_id: self.new_id(context.now),
            actor_id: actor,
            actor_kind: actor_kind.to_i16(),
            action: action.to_string(),
            target_kind: target_kind.to_i16(),
            target_id: target,
            summary,
            reason: None,
            request_id: context.request_id.clone(),
            ip_class: context.ip_class(),
            created_at: context.now,
        };
        if let Err(error) = self.store.append_audit(entry).await {
            tracing::warn!(action, %error, "could not append audit entry");
        }
    }

    /// Revokes every session on one device, and returns how many.
    ///
    /// Walks the account's live sessions rather than issuing one statement, because
    /// [`SessionStore`] has no by-device revocation and adding one for an operation that
    /// happens a handful of times per account per year would be a schema decision made
    /// for the convenience of one call site.
    async fn revoke_device_sessions(
        &self,
        account_id: Id,
        device_id: Id,
        reason: RevokeReason,
        now: Timestamp,
    ) -> Result<u64> {
        let sessions = self.store.sessions_for_account(account_id, now).await?;
        let mut revoked = 0;
        for session in sessions.iter().filter(|s| s.device_id == device_id) {
            self.store
                .revoke_session(session.session_id, reason, now)
                .await?;
            revoked += 1;
        }
        Ok(revoked)
    }

    /// Reads the account behind a verified token, or says why it cannot be used.
    async fn live_account(&self, account_id: Id) -> Result<Account> {
        let account =
            self.store.account_by_id(account_id).await?.ok_or_else(|| {
                fault::error(codes::TOKEN_REVOKED, "the account no longer exists")
            })?;
        if !account.status.can_sign_in() {
            return Err(suspended(account.status));
        }
        Ok(account)
    }
}

impl<S: ?Sized, L: ?Sized> fmt::Debug for Auth<S, L> {
    /// Prints the policy in force and nothing else.
    ///
    /// Not derived, and not derivable: this struct holds a signing key, a password hash,
    /// and a generator. A derived `Debug` on a type that holds a key is a key in a log
    /// line, waiting for somebody to add `?state` to a tracing span.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Auth")
            .field("region", &self.signer.region())
            .field("access_ttl_seconds", &self.config.access_ttl_seconds)
            .field("refresh_ttl_seconds", &self.config.refresh_ttl_seconds)
            .field("max_devices_per_user", &self.config.max_devices_per_user)
            .field("allow_registration", &self.config.allow_registration)
            .field("prices", &self.prices)
            .finish_non_exhaustive()
    }
}

/// How many salt bytes are carried across the blocking hop.
///
/// Argon2id needs sixteen. Thirty-two are carried so that a future change in the crypto
/// crate — a longer salt, a second draw — does not silently fall through to the operating
/// system RNG and break a seeded replay.
const SALT_CARRY: usize = 32;

/// A [`Random`] that replays bytes drawn earlier, then falls back to the OS.
///
/// It exists to move randomness *across a thread boundary* without moving the shared
/// generator, which is `Send` but not `Sync` and is behind a lock that must not be held
/// across an `await`.
///
/// The fallback is not a silent downgrade of quality — [`OsRandom`] is the stronger
/// source — but it is a silent downgrade of *reproducibility*, so it is sized never to be
/// reached in practice.
struct CarriedRandom {
    bytes: [u8; SALT_CARRY],
    used: usize,
}

impl CarriedRandom {
    const fn new(bytes: [u8; SALT_CARRY]) -> Self {
        Self { bytes, used: 0 }
    }
}

impl Random for CarriedRandom {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let available = SALT_CARRY - self.used;
        let take = available.min(dest.len());
        dest[..take].copy_from_slice(&self.bytes[self.used..self.used + take]);
        self.used += take;
        if take < dest.len() {
            OsRandom.fill_bytes(&mut dest[take..]);
        }
    }
}

/// The error for an account that may not sign in.
///
/// One function so that every path spells it the same way, and so that the three
/// non-active states cannot drift into three different codes.
fn suspended(status: AccountStatus) -> Error {
    let why = match status {
        AccountStatus::Suspended => "the account is suspended",
        AccountStatus::Deactivated => "the account is deactivated",
        AccountStatus::Deleted => "the account is deleted",
        AccountStatus::Active => "the account cannot sign in",
    };
    fault::error(codes::ACCOUNT_SUSPENDED, why)
}

/// Seconds to milliseconds, without overflowing on a configured absurdity.
fn seconds_to_millis(seconds: u64) -> i64 {
    i64::try_from(seconds.saturating_mul(1_000)).unwrap_or(i64::MAX)
}

/// A device name for a client that did not send one.
///
/// Better than an empty row in the user's security screen, which is the one place they go
/// to decide whether something unfamiliar is signed in.
const fn default_device_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Web => "Web browser",
        Platform::Android => "Android device",
        Platform::Ios => "iOS device",
        Platform::Desktop => "Desktop app",
        Platform::Bot => "Bot",
        Platform::LoadTest => "Load generator",
        Platform::Unknown => "Unknown device",
    }
}

/// Builds the summary a user sees in their own security screen.
fn summarise(session: &Session, device: Option<&Device>, current: Id) -> SessionSummary {
    SessionSummary {
        session_id: session.session_id,
        device_id: session.device_id,
        device_name: device.map_or_else(
            || "Unknown device".to_string(),
            |device| device.display_name.clone(),
        ),
        platform: device.map_or(Platform::Unknown, |device| device.platform),
        created_at: session.created_at,
        refresh_expires_at: session.refresh_expires_at,
        ip_class: session.ip_class.clone(),
        user_agent: session.user_agent.clone(),
        is_current: session.session_id == current,
    }
}

#[async_trait]
impl<S, L> Authenticator for Auth<S, L>
where
    S: AccountStore + DeviceStore + SessionStore + SafetyStore + ?Sized + Send + Sync + 'static,
    L: RateLimiter + ?Sized + Send + Sync + 'static,
{
    async fn register(&self, request: Registration, context: &RequestContext) -> Result<Grant> {
        if !self.config.allow_registration {
            self.meters.registration_refused();
            return Err(fault::feature_disabled("registration"));
        }
        // Priced before any work: registration is the endpoint a spam operation hits
        // hardest, and validating first would let it spend our CPU for free.
        if let Err(error) = self
            .charge_stranger(context, Opcode::Authenticate, self.prices.register)
            .await
        {
            self.meters.registration_refused();
            return Err(error);
        }

        let username = credential::username(&request.username).inspect_err(|_| {
            self.meters.registration_refused();
        })?;
        let email = request
            .email
            .as_deref()
            .map(credential::email)
            .transpose()
            .inspect_err(|_| self.meters.registration_refused())?;
        let phone = request
            .phone
            .as_deref()
            .map(credential::phone)
            .transpose()
            .inspect_err(|_| self.meters.registration_refused())?;
        credential::password(
            &request.password,
            self.config.password_min_length,
            Some(username.folded()),
        )
        .inspect_err(|_| self.meters.registration_refused())?;

        let now = context.now;
        let password_hash = self
            .hash_password(request.password.expose(), &self.meters)
            .await?;

        let account_id = self.new_id(now);
        let account = self
            .store
            .create_account(NewAccount {
                account_id,
                username: username.display().to_string(),
                email,
                phone,
                password_hash,
                locale: normalise_locale(&request.locale),
                country: request.country.clone(),
                created_at: now,
            })
            .await
            .map_err(|error| {
                self.meters.registration_refused();
                // The store reports a collision generically, because it does not know
                // which unique index tripped. A username collision is by far the most
                // common and the only one a client can act on, so it gets the specific
                // code.
                if error.code() == codes::ALREADY_EXISTS {
                    fault::error(codes::USERNAME_TAKEN, "that username is taken")
                } else {
                    error
                }
            })?;

        self.store
            .create_profile(Profile {
                account_id,
                display_name: username.display().to_string(),
                bio: None,
                avatar_media_id: None,
                birth_year: None,
                // Private by default. A social product that starts everybody visible has
                // decided on their behalf, and the people who would have chosen otherwise
                // are the ones who most needed the choice.
                show_last_seen: Visibility::Friends,
                who_can_message: Visibility::Friends,
                who_can_add: Visibility::Everyone,
                searchable: true,
                updated_at: now,
            })
            .await?;

        let tier = tier::of_account(&account, now);
        let device = self.resolve_device(&account, &request.device, now).await?;
        let grant = self
            .open_session(&account, &device, tier, now, context, None, None, true)
            .await?;
        self.store.record_login(account_id, now).await?;

        self.audit(
            Some(account_id),
            AuditActorKind::User,
            "account.register",
            AuditTargetKind::Account,
            Some(account_id),
            format!("account created as @{}", username.display()),
            context,
        )
        .await;
        self.meters.registered();
        Ok(grant)
    }

    async fn sign_in(&self, request: SignIn, context: &RequestContext) -> Result<Grant> {
        if let Err(error) = self
            .charge_stranger(context, Opcode::Authenticate, self.prices.attempt)
            .await
        {
            self.meters.signin(SignInOutcome::RateLimited);
            return Err(error);
        }

        let identifier = request.identifier.trim();
        let found = if credential::looks_like_email(identifier) {
            match credential::email(identifier) {
                Ok(email) => self.store.account_by_email(&email).await?,
                // A malformed identifier is not a validation error to the caller: telling
                // somebody their email is badly formed is one bit more than "those
                // credentials are wrong", and the client already validates the field.
                Err(_) => None,
            }
        } else {
            let folded = identifier.trim_start_matches('@').to_ascii_lowercase();
            self.store.account_by_username(&folded).await?
        };

        let Some(account) = found else {
            // Verify against the placeholder so this path costs what the real one costs.
            // Without it the response time answers "does this account exist".
            let _ = self
                .verify_password(request.password.expose(), &self.absent_hash, &self.meters)
                .await;
            self.charge_penalty(context, Opcode::Authenticate).await;
            self.meters.signin(SignInOutcome::UnknownUser);
            return Err(fault::invalid_credentials());
        };

        let verification = self
            .verify_password(
                request.password.expose(),
                &account.password_hash,
                &self.meters,
            )
            .await?;
        let Some(verification) = verification else {
            self.charge_penalty(context, Opcode::Authenticate).await;
            self.meters.signin(SignInOutcome::BadPassword);
            return Err(fault::invalid_credentials());
        };

        // Only now, with the password proven, is it safe to say anything specific about
        // the account.
        if !account.status.can_sign_in() {
            self.meters.signin(SignInOutcome::Suspended);
            return Err(suspended(account.status));
        }

        let now = context.now;
        if verification == migo_crypto::Verification::NeedsRehash {
            // The plaintext is in hand exactly once per sign-in, so this is the only
            // moment a stored hash can be upgraded without asking the user for anything.
            match self
                .hash_password(request.password.expose(), &self.meters)
                .await
            {
                Ok(hash) => {
                    if let Err(error) = self
                        .store
                        .set_password_hash(account.account_id, hash.expose(), now)
                        .await
                    {
                        // A failed upgrade must not fail the sign-in: the password was
                        // correct, and the old hash still works.
                        tracing::warn!(%error, "could not store a rehashed password");
                    } else {
                        self.meters.rehashed();
                    }
                }
                Err(error) => tracing::warn!(%error, "could not rehash a password"),
            }
        }

        let device = match self.resolve_device(&account, &request.device, now).await {
            Ok(device) => device,
            Err(error) => {
                if error.code() == codes::TOO_MANY_SESSIONS {
                    self.meters.signin(SignInOutcome::DeviceLimit);
                }
                return Err(error);
            }
        };
        let tier = tier::of_account(&account, now);
        let grant = self
            .open_session(&account, &device, tier, now, context, None, None, false)
            .await?;
        self.store.record_login(account.account_id, now).await?;
        self.meters.signin(SignInOutcome::Success);
        Ok(grant)
    }

    async fn refresh(&self, request: Refresh, context: &RequestContext) -> Result<Grant> {
        if let Err(error) = self
            .charge_stranger(context, Opcode::Authenticate, self.prices.attempt)
            .await
        {
            self.meters.refresh(RefreshOutcome::RateLimited);
            return Err(error);
        }

        let now = context.now;
        let tag = self.signer.refresh_tag(request.refresh_token.expose());
        let Some(session) = self.store.session_by_refresh_hash(&tag).await? else {
            self.charge_penalty(context, Opcode::Authenticate).await;
            self.meters.refresh(RefreshOutcome::Unknown);
            return Err(fault::error(
                codes::TOKEN_INVALID,
                "no session matches that refresh token",
            ));
        };

        // Reuse is checked before anything else, including expiry. A token that was
        // already exchanged is evidence of a copy, and that is true whether or not the
        // window has since closed.
        if session.rotated_at.is_some() || session.revoked_at.is_some() {
            let killed = self
                .store
                .revoke_family(session.family_id, RevokeReason::ReuseDetected, now)
                .await
                .unwrap_or(0);
            self.meters.family_revoked(killed);
            self.meters.refresh(RefreshOutcome::Reuse);
            self.audit(
                Some(session.account_id),
                AuditActorKind::System,
                "auth.session.reuse_detected",
                AuditTargetKind::Session,
                Some(session.session_id),
                format!("refresh token replayed; {killed} generations revoked"),
                context,
            )
            .await;
            self.charge_penalty(context, Opcode::Authenticate).await;
            return Err(fault::error(
                codes::REFRESH_REUSE_DETECTED,
                "that refresh token was already exchanged",
            ));
        }

        if session.device_id != request.device_id {
            // Either a client bug or a stolen token presented from elsewhere, and there
            // is no way to tell which. The family dies, which costs a buggy client one
            // sign-in and costs a thief the token they stole.
            let killed = self
                .store
                .revoke_family(session.family_id, RevokeReason::ReuseDetected, now)
                .await
                .unwrap_or(0);
            self.meters.family_revoked(killed);
            self.meters.refresh(RefreshOutcome::DeviceMismatch);
            self.charge_penalty(context, Opcode::Authenticate).await;
            return Err(fault::error(
                codes::DEVICE_MISMATCH,
                "that refresh token was minted for another device",
            ));
        }

        if !session.is_live(now) {
            self.meters.refresh(RefreshOutcome::Expired);
            return Err(fault::error(
                codes::TOKEN_EXPIRED,
                "the refresh window has closed; sign in again",
            ));
        }

        let account = match self.live_account(session.account_id).await {
            Ok(account) => account,
            Err(error) => {
                if error.code() == codes::ACCOUNT_SUSPENDED {
                    let killed = self
                        .store
                        .revoke_family(session.family_id, RevokeReason::AdminAction, now)
                        .await
                        .unwrap_or(0);
                    self.meters.sessions_revoked(killed);
                    self.meters.refresh(RefreshOutcome::Suspended);
                } else {
                    self.meters.refresh(RefreshOutcome::Unknown);
                }
                return Err(error);
            }
        };

        let device = self.store.device_by_id(session.device_id).await?;
        let Some(device) = device.filter(|device| device.revoked_at.is_none()) else {
            let _ = self
                .store
                .revoke_session(session.session_id, RevokeReason::DeviceRemoved, now)
                .await;
            self.meters.refresh(RefreshOutcome::Unknown);
            return Err(fault::error(
                codes::TOKEN_REVOKED,
                "the device this session belongs to was revoked",
            ));
        };
        self.store.touch_device(device.device_id, now).await?;

        let tier = tier::of_account(&account, now);
        let grant = self
            .open_session(
                &account,
                &device,
                tier,
                // Carried forward, not reset: a refresh is possession, not presence.
                session.authenticated_at,
                context,
                Some(session.session_id),
                Some((session.family_id, session.generation + 1)),
                false,
            )
            .await?;
        self.meters.refresh(RefreshOutcome::Success);
        Ok(grant)
    }

    async fn authenticate(
        &self,
        access_token: &str,
        device_id: Id,
        context: &RequestContext,
    ) -> Result<Identity> {
        let now = context.now;
        let claims = self.signer.verify(access_token, now).inspect_err(|_| {
            self.meters.verify_failed();
        })?;

        if claims.device_id != device_id {
            self.meters.verify_failed();
            return Err(fault::error(
                codes::DEVICE_MISMATCH,
                "the token was minted for another device",
            ));
        }

        // The session row is what makes revocation immediate. Reading it is the whole
        // difference between this method and `verify_access`.
        let session = self
            .store
            .session_by_id(claims.session_id)
            .await?
            .ok_or_else(|| fault::error(codes::TOKEN_REVOKED, "the session no longer exists"))?;
        if session.revoked_at.is_some() {
            self.meters.verify_failed();
            return Err(fault::error(
                codes::TOKEN_REVOKED,
                "the session was revoked",
            ));
        }
        if session.account_id != claims.account_id || session.device_id != claims.device_id {
            // A signed token whose claims contradict the row it names. Not reachable
            // without the signing key, so it is our bug rather than an attack — but it is
            // refused rather than reconciled, because guessing which of the two is right
            // is how one gets attributed to the other.
            self.meters.verify_failed();
            return Err(fault::error(
                codes::TOKEN_INVALID,
                "the token disagrees with the session it names",
            ));
        }

        let account = self.live_account(claims.account_id).await?;
        let device = self
            .store
            .device_by_id(claims.device_id)
            .await?
            .filter(|device| device.revoked_at.is_none())
            .ok_or_else(|| fault::error(codes::TOKEN_REVOKED, "the device was revoked"))?;
        self.store.touch_device(device.device_id, now).await?;

        let tier = tier::of_account(&account, now);
        Ok(Identity {
            claims,
            username: account.username.clone(),
            tier,
            // Recomputed from the account as it is now, with any bit this build does not
            // recognise carried over from the token rather than dropped.
            capabilities: Capabilities::for_account(&account, tier)
                .with(claims.capabilities.unknown()),
        })
    }

    fn verify_access(&self, access_token: &str, now: Timestamp) -> Result<Claims> {
        self.signer.verify(access_token, now).inspect_err(|_| {
            self.meters.verify_failed();
        })
    }

    fn token_region(&self, access_token: &str) -> Option<String> {
        Signer::peek_region(access_token)
    }

    async fn sign_out(
        &self,
        identity: &Identity,
        session_id: Id,
        context: &RequestContext,
    ) -> Result<()> {
        let session = self
            .store
            .session_by_id(session_id)
            .await?
            .filter(|session| session.account_id == identity.account_id())
            .ok_or_else(|| fault::not_found("session"))?;
        self.store
            .revoke_session(session.session_id, RevokeReason::Logout, context.now)
            .await?;
        self.meters.sessions_revoked(1);
        Ok(())
    }

    async fn sign_out_others(&self, identity: &Identity, context: &RequestContext) -> Result<u64> {
        identity.require_fresh(context.now)?;
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        let revoked = self
            .store
            .revoke_account_sessions(
                identity.account_id(),
                Some(identity.session_id()),
                RevokeReason::Logout,
                context.now,
            )
            .await?;
        self.meters.sessions_revoked(revoked);
        Ok(revoked)
    }

    async fn sessions(
        &self,
        identity: &Identity,
        context: &RequestContext,
    ) -> Result<Vec<SessionSummary>> {
        let sessions = self
            .store
            .sessions_for_account(identity.account_id(), context.now)
            .await?;
        let devices = self
            .store
            .devices_for_account(identity.account_id())
            .await?;
        Ok(sessions
            .iter()
            .map(|session| {
                let device = devices
                    .iter()
                    .find(|device| device.device_id == session.device_id);
                summarise(session, device, identity.session_id())
            })
            .collect())
    }

    async fn revoke_device(
        &self,
        identity: &Identity,
        device_id: Id,
        context: &RequestContext,
    ) -> Result<u64> {
        identity.require_fresh(context.now)?;
        let device = self
            .store
            .device_by_id(device_id)
            .await?
            .filter(|device| device.account_id == identity.account_id())
            .ok_or_else(|| fault::not_found("device"))?;

        let now = context.now;
        // Sessions first. If the process dies between the two writes, the device is left
        // registered with no live sessions, which is harmless; the other order would leave
        // a revoked device holding live sessions, which is not.
        let revoked = self
            .revoke_device_sessions(
                identity.account_id(),
                device.device_id,
                RevokeReason::DeviceRemoved,
                now,
            )
            .await?;
        self.store.revoke_device(device.device_id, now).await?;
        self.meters.device_revoked();
        self.meters.sessions_revoked(revoked);
        self.audit(
            Some(identity.account_id()),
            AuditActorKind::User,
            "account.device.revoke",
            AuditTargetKind::Device,
            Some(device.device_id),
            format!("device revoked; {revoked} sessions ended"),
            context,
        )
        .await;
        Ok(revoked)
    }

    async fn change_password(
        &self,
        identity: &Identity,
        change: PasswordChange,
        context: &RequestContext,
    ) -> Result<Grant> {
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        let now = context.now;
        let account = self.live_account(identity.account_id()).await?;

        let verified = self
            .verify_password(
                change.current.expose(),
                &account.password_hash,
                &self.meters,
            )
            .await?;
        if verified.is_none() {
            return Err(fault::invalid_credentials());
        }
        credential::password(
            &change.next,
            self.config.password_min_length,
            Some(&account.username),
        )?;

        let hash = self
            .hash_password(change.next.expose(), &self.meters)
            .await?;
        self.store
            .set_password_hash(account.account_id, hash.expose(), now)
            .await?;

        // Every session, including this one. The replacement grant is what keeps the
        // caller signed in; anything else on any other device has to sign in again.
        let revoked = self
            .store
            .revoke_account_sessions(account.account_id, None, RevokeReason::PasswordChanged, now)
            .await?;
        self.meters.sessions_revoked(revoked);

        let device = self
            .store
            .device_by_id(identity.device_id())
            .await?
            .filter(|device| device.revoked_at.is_none())
            .ok_or_else(|| fault::error(codes::TOKEN_REVOKED, "the device was revoked"))?;
        let tier = tier::of_account(&account, now);
        // `now` for `authenticated_at`: the current password was just typed, which is the
        // strongest presence signal there is.
        let grant = self
            .open_session(&account, &device, tier, now, context, None, None, false)
            .await?;
        self.store.record_login(account.account_id, now).await?;

        self.audit(
            Some(account.account_id),
            AuditActorKind::User,
            "account.password.change",
            AuditTargetKind::Account,
            Some(account.account_id),
            format!("password changed; {revoked} sessions ended"),
            context,
        )
        .await;
        Ok(grant)
    }
}

/// Normalises a language tag, or falls back to one that exists.
///
/// A missing or absurd locale becomes `en`, because a client that sends nothing still has
/// to be rendered something, and a blank locale column propagates as a blank date format
/// three layers away.
fn normalise_locale(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 35 || !trimmed.is_ascii() {
        return "en".to_string();
    }
    trimmed.to_lowercase()
}
