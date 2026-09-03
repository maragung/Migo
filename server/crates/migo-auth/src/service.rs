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
//! An unknown account and a wrong passphrase produce the same error *and take the same
//! time*. The error is easy; the timing is not. A missing account has no hash to verify,
//! so the natural implementation returns in microseconds while a real account spends
//! forty milliseconds in Argon2id — a difference so large it is measurable over the
//! public internet, and it turns the sign-in endpoint into an account enumerator. So a
//! missing account is verified against a placeholder hash created at startup, and the
//! result is thrown away.
//!
//! A suspended account is reported as suspended only *after* the passphrase has been
//! checked. Reporting it earlier would answer "does this account exist" to anyone who
//! asked.
//!
//! # Why failure is priced higher than success
//!
//! A wrong passphrase costs the whole anonymous endpoint bucket; a right one costs a fifth
//! of it. Both numbers are derived from the resolved bucket at construction rather than
//! written down, because the limiter writes nothing on a refusal — a penalty larger than
//! the bucket is silently dropped, and a hardcoded penalty would become a no-op the day
//! an operator lowered `anonymous_burst`. Deriving it means the penalty is always exactly
//! affordable and therefore always actually charged.
//!
//! There is deliberately no per-*account* failure limit. A limit that locks an account
//! after N wrong passphrases is a way for a stranger who knows a username to lock its owner
//! out, which converts a nuisance into an outage. The pressure goes on the network the
//! guesses come from.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use migo_account::identity::{
    verify_identity, CONTEXT_LOGIN, CONTEXT_LOGIN_DEVICE, CONTEXT_ROTATE,
};
use migo_account::{IDENTITY_ALGORITHM, KEY_VERSION_ONE, PUBLIC_KEY_LEN};
use migo_captcha::CaptchaProof;
use migo_core::config::{AuthConfig, Config};
use migo_core::metrics::Registry;
use migo_core::{Error, Id, OsRandom, Random, Result, Secret, Timestamp};
use migo_crypto::{MacKey, LABEL_RECOVERY};
use migo_protocol::{
    codes, fault, to_bytes, MlDsaChallenge, MlDsaPurpose, Opcode, Platform, PROTOCOL_VERSION,
};
use migo_ratelimit::{BucketKey, RateLimiter, Scope, SharedRateLimiter, TrustTier};
use migo_store::model::{
    Account, AccountStatus, AuditActorKind, AuditEntry, AuditTargetKind, Device, DeviceStatus,
    GlobalAdmin, IdentityKeyStatus, NewAccount, NewDevice, NewSession, Profile, RevokeReason,
    Session, Visibility, WalletStatus,
};
use migo_store::traits::{
    AccountStore, ChallengeStore, DeviceStore, GlobalAdminStore, IdentityKeyRow, IdentityStore,
    LoginChallengeRow, RecoveryRow, RecoveryStore, SafetyStore, SessionStore, WalletRow,
    WalletStore,
};
use migo_store::{SharedStore, Store};
use parking_lot::Mutex;

use crate::capability::Capabilities;
use crate::captcha::CaptchaGate;
use crate::credential;
use crate::lockout::{error_locked, LockoutConfig, LockoutGate};
use crate::metrics::{Meters, RefreshOutcome, SignInOutcome};
use crate::model::{
    truncate_chars, AddDeviceAnswer, AdminStanding, AdminView, ChallengeAnswer, ChallengeView,
    DeviceClaim, DeviceSummary, Grant, IdentityChallengeRequest, IdentityChallengeScope,
    IdentityPublication, Refresh, Registration, RequestContext, RotationAnswer, SessionSummary,
    SignIn, WalletRegistration, WalletSummary, IDENTITY_CHALLENGE_TTL_MS, MAX_APP_VERSION_CHARS,
    MAX_CHAIN_TYPE_CHARS, MAX_DEVICE_DETAIL_CHARS, MAX_DEVICE_NAME_CHARS, MAX_WALLET_ADDRESS_CHARS,
    MAX_WALLET_LABEL_CHARS,
};
use crate::tier;
use crate::token::{Claims, Signer};
use crate::traits::{Authenticator, Identity, PassphraseChange};

/// The opaque, dyn-erased handle every route layer and the chat
/// shell hold onto. Its concrete type is `Auth<...>`; the
/// `Authenticator` trait hides the storage behind `dyn` so routes do
/// not depend on the inner type.
pub type SharedAuth = Arc<dyn Authenticator>;
/// The concrete handle the composition root owns between `open` and
/// the moment the captcha gate (and any other per-process state that
/// must be exactly-once) is attached. Routes only see `SharedAuth`; the
/// composition root uses this only briefly, to call `with_captcha` and
/// re-erase.
pub type ConcreteAuth = Arc<Auth<dyn Store, dyn RateLimiter>>;

/// The generation number of a family's first session.
const FIRST_GENERATION: i32 = 1;

/// How long a recovery token stays valid. One hour, picked because the
/// threat model is a user who forgot the passphrase and is reading the
/// recovery email on the same device; longer than an hour is a window
/// in which somebody with persistent access to the inbox can still
/// use a token the user never opened.
pub const RECOVERY_TTL: i64 = 60 * 60 * 1_000;

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
    fn from_policies(
        policies: &migo_ratelimit::Policies,
        config: &migo_core::config::AuthConfig,
    ) -> Self {
        // The tightest surface a stranger reaches: one opcode, one network, no standing.
        let full = policies
            .resolve(Scope::Endpoint, TrustTier::Anonymous)
            .capacity();
        let attempt = (full / 5).max(1);
        let register = config.registration_cost.unwrap_or(full);
        Self {
            attempt,
            penalty: full.saturating_sub(attempt),
            // Creating an account is the expensive one: it writes rows, hashes a
            // passphrase, and is the thing a spam operation needs thousands of.
            // An override exists for local development only.
            register,
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
    /// The captcha gate, when the deployment has captcha turned on. The
    /// route layer mints challenges through it; the authenticator
    /// consults it to decide whether a register or sign-in attempt
    /// has to carry a proof. `None` means the captcha is off for the
    /// whole process and the auth path skips the checks entirely.
    captcha: Option<Arc<CaptchaGate>>,
    /// The sign-in lockout, when the deployment has it turned on (the default).
    /// Keyed by account id and holding the whole ladder — see [`LockoutGate`].
    lockout: Option<Arc<LockoutGate>>,
    /// The recovery MAC key, when the deployment has passphrase-recovery
    /// turned on. `None` means the recovery endpoints are not mounted
    /// and `request_recovery` / `confirm_recovery` are short-circuited
    /// at the route layer. Holding only the key — and not the store —
    /// keeps the in-process tests that do not need recovery from
    /// having to know which store the test is running over.
    recovery: Option<MacKey>,
}

/// Builds an authenticator over the shared store and limiter.
///
/// `secret_root` is the per-deployment root from which the recovery
/// subkey is derived; pass `b""` for tests and an ephemeral dev
/// deployment that does not mount the recovery endpoints, or the same
/// value every other token-signing subsystem uses for production.
///
/// Fails when the token key is missing or too short, when the node's region label does
/// not fit the token layout, or when the placeholder hash cannot be produced — all three
/// at startup, so `migod` declines to boot rather than serving a broken login.
pub fn open(
    store: SharedStore,
    limiter: SharedRateLimiter,
    config: &Config,
    registry: &Registry,
    secret_root: &[u8],
) -> Result<Arc<Auth<dyn Store, dyn RateLimiter>>> {
    let auth = Auth::new(
        store,
        limiter,
        config,
        registry,
        Box::new(OsRandom) as Box<dyn Random>,
    )?
    .with_recovery(secret_root);
    Ok(Arc::new(auth))
}

impl<S, L> Auth<S, L>
where
    S: AccountStore
        + DeviceStore
        + SessionStore
        + SafetyStore
        + RecoveryStore
        + IdentityStore
        + ChallengeStore
        + WalletStore
        + ?Sized,
    L: RateLimiter + ?Sized,
{
    /// Builds an authenticator over a concrete or erased store and limiter.
    ///
    /// `random` is injected rather than fixed to [`OsRandom`] so a simulation can replay
    /// a run byte for byte (ADR-0009). It is used for session ids, family ids, device
    /// ids, refresh tokens, and passphrase salts.
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
        let prices = Prices::from_policies(limiter.policies(), &config.auth);
        let mut random = random;
        let absent_hash = migo_crypto::passphrase::hash(ABSENT_ACCOUNT_PLACEHOLDER, &mut *random)
            .map_err(|error| {
            fault::internal(format!("could not build the placeholder hash: {error}"))
        })?;
        let lockout = (config.auth.lockout.enabled).then(|| {
            Arc::new(LockoutGate::new(LockoutConfig {
                enabled: true,
                initial_failures: config.auth.lockout.initial_failures,
                step_failures: config.auth.lockout.step_failures,
                base_seconds: config.auth.lockout.base_seconds,
                step_seconds: config.auth.lockout.step_seconds,
                max_seconds: config.auth.lockout.max_seconds,
            }))
        });
        Ok(Self {
            store,
            limiter,
            signer,
            config: config.auth.clone(),
            prices,
            absent_hash,
            lockout,
            random: Mutex::new(random),
            meters: Meters::new(registry),
            // The captcha is opt-in via [`Self::with_captcha`]; the
            // default constructor leaves the gate out so the many
            // tests that build an `Auth` for unrelated reasons do not
            // need to know about the captcha plumbing.
            captcha: None,
            // The recovery key follows the same opt-in pattern: the
            // route layer is the only place that asks for it, and a
            // test that does not mount the recovery endpoints never
            // builds one.
            recovery: None,
        })
    }

    /// Attaches a captcha gate to the authenticator.
    ///
    /// Returns `self` rather than mutating in place so the call site
    /// reads top-to-bottom: `Auth::new(...).with_captcha(gate)?`.
    /// Returns an error rather than panicking if the gate's threshold
    /// is zero, which would require a captcha on the first attempt —
    /// the configuration layer already rejects that, but the check is
    /// duplicated here so a hand-rolled `Auth` cannot sidestep it.
    pub fn with_captcha(mut self, gate: Arc<CaptchaGate>) -> Result<Self> {
        if gate.threshold() == 0 {
            return Err(fault::internal(
                "captcha threshold of 0 would require a proof on the first attempt",
            ));
        }
        self.captcha = Some(gate);
        Ok(self)
    }

    /// Attaches a recovery MAC key.
    ///
    /// The key is derived from `secret_root` under
    /// [`migo_crypto::LABEL_RECOVERY`], the same way every other
    /// short-lived server token on Migo uses a labelled subkey. An
    /// empty `secret_root` is allowed only for tests; production
    /// builds it from the configured node signing key.
    pub fn with_recovery(mut self, secret_root: &[u8]) -> Self {
        self.recovery = Some(MacKey::derive(secret_root, LABEL_RECOVERY));
        self
    }

    /// The captcha gate, or `None` when captcha is off.
    #[must_use]
    pub fn captcha_gate(&self) -> Option<&CaptchaGate> {
        self.captcha.as_deref()
    }

    /// The recovery MAC key, or `None` when recovery is off.
    #[must_use]
    pub fn recovery_key(&self) -> Option<&MacKey> {
        self.recovery.as_ref()
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

    /// Salt material for one passphrase hash, drawn before the blocking work starts.
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

    // --- passphrase work ---------------------------------------------------------

    /// Hashes a passphrase off the async worker threads.
    ///
    /// Argon2id at the configured cost spends tens of milliseconds and 19 MiB in a tight
    /// loop. Doing that on a runtime worker stalls every other task that worker was
    /// going to poll — including the ones that have nothing to do with logging in — so it
    /// goes to the blocking pool, which exists for exactly this.
    ///
    /// The salt is drawn from the injected [`Random`] *before* the hop and replayed
    /// inside, so a seeded run stays reproducible even though the hashing happens on
    /// another thread.
    async fn hash_passphrase(&self, passphrase: &str, meters: &Meters) -> Result<Secret> {
        let salt = self.draw_salt();
        let owned = passphrase.to_string();
        let started = std::time::Instant::now();
        let hashed = tokio::task::spawn_blocking(move || {
            let mut random = CarriedRandom::new(salt);
            migo_crypto::passphrase::hash(&owned, &mut random)
        })
        .await
        .map_err(|_| fault::internal("passphrase hashing task failed"))?;
        meters.hash_took(started.elapsed().as_secs_f64() * 1_000.0);
        hashed.map_err(|error| {
            // A refusal from the crypto crate at this point means the passphrase broke a
            // length bound the `credential` module should already have caught, so it is
            // reported as our bug rather than as the user's.
            fault::internal(format!("passphrase could not be hashed: {error}"))
        })
    }

    /// Verifies a passphrase off the async worker threads. See [`Auth::hash_passphrase`].
    async fn verify_passphrase(
        &self,
        passphrase: &str,
        stored: &Secret,
        meters: &Meters,
    ) -> Result<Option<migo_crypto::Verification>> {
        let owned = passphrase.to_string();
        let stored = stored.clone();
        let started = std::time::Instant::now();
        let outcome =
            tokio::task::spawn_blocking(move || migo_crypto::passphrase::verify(&owned, &stored))
                .await
                .map_err(|_| fault::internal("passphrase verification task failed"))?;
        meters.hash_took(started.elapsed().as_secs_f64() * 1_000.0);
        match outcome {
            Ok(verification) => Ok(verification),
            // A stored hash that will not parse is a corrupt row, not a wrong passphrase.
            // Reporting it as a wrong passphrase would leave the owner locked out of their
            // account with nothing in the logs to explain it.
            Err(error) => Err(fault::internal(format!(
                "stored passphrase hash is unusable: {error}"
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

    /// Admits only the account named by `owner_account_id` in config.
    ///
    /// The owner is named in configuration rather than derived from data, so the
    /// check is a comparison and not a lookup — and a deployment that names no
    /// owner has closed the surface for everyone, including the operator who
    /// forgot to set the value.
    fn require_owner(&self, identity: &Identity) -> Result<()> {
        if self.config.owner_account_id == Some(identity.account_id()) {
            Ok(())
        } else {
            Err(fault::permission_denied("owner"))
        }
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
    /// device" mean "revoke it until the passphrase is typed again", which is not what the
    /// user was told the button does.
    async fn resolve_device(
        &self,
        account: &Account,
        claim: &DeviceClaim,
        now: Timestamp,
    ) -> Result<Device> {
        // A credential of the wrong length is a client that is wrong, not an
        // input to trim: the length is part of what ML-DSA-65 is.
        if let Some(key) = claim.credential_public_key.as_ref() {
            if key.len() != PUBLIC_KEY_LEN {
                return Err(fault::validation(
                    "device.credential_public_key",
                    &format!("must be {PUBLIC_KEY_LEN} bytes of encoded ML-DSA-65"),
                ));
            }
        }
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
                // The ordinary path: the device is registered by a call that
                // has already authenticated the account. The add-device
                // ceremony is the one caller that registers a pending row
                // and activates it on challenge consumption.
                status: DeviceStatus::Active,
                // Introduced alongside the registration when the client
                // registered with a root secret; `None` for every legacy
                // client. Validated before it gets here.
                public_credential: claim.credential_public_key.clone(),
                created_at: now,
            })
            .await
    }

    // --- audit -----------------------------------------------------------------

    /// Appends a security event.
    ///
    /// Only the events an operator or a user would need to reconstruct an incident:
    /// registration, passphrase change, device revocation, and family revocation. Not every
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

    // --- captcha ---------------------------------------------------------------

    /// Enforces the captcha gate on a bootstrap attempt.
    ///
    /// Three outcomes:
    ///
    /// - The gate is off (no captcha deployment) — accept any body.
    /// - The gate is on and the network does not yet require a proof —
    ///   accept any body.
    /// - The gate is on and the network is over the threshold — the
    ///   body must carry a proof, and the proof must verify. A missing
    ///   proof is `CAPTCHA_REQUIRED`; a present-but-wrong proof is
    ///   `INVALID_CAPTCHA`. Either way the call short-circuits before
    ///   the rate limiter is charged or the credentials are touched,
    ///   so a flood of captcha-less attempts is priced at the captcha
    ///   check rather than at the rest of the pipeline.
    async fn enforce_captcha(
        &self,
        captcha: Option<&CaptchaProof>,
        context: &RequestContext,
    ) -> Result<()> {
        let Some(gate) = self.captcha.as_ref() else {
            return Ok(());
        };
        let Some(ip) = context.ip else {
            // An addressless caller cannot be measured against the per-IP
            // counter, so the captcha check would be a coin flip either
            // way. Skipping is the honest answer: an in-process caller
            // already has to be inside the service, and a unix-socket
            // call is the operator's own tooling.
            return Ok(());
        };
        if !gate.needs_captcha(ip) {
            return Ok(());
        }
        let Some(proof) = captcha else {
            return Err(crate::captcha::error_required());
        };
        match gate.verify(proof).await? {
            true => Ok(()),
            // The service reports "wrong answer" and "expired or never
            // existed" with the same boolean; the gate cannot tell the
            // two apart without its own clock. A wrong answer and an
            // expired answer are both the user's mistake and both
            // surface as `INVALID_CAPTCHA` so the client knows to
            // fetch a fresh challenge.
            false => Err(crate::captcha::error_invalid(
                "the captcha proof did not verify",
            )),
        }
    }

    /// Notes a per-IP failure past the captcha threshold, or no-ops when
    /// the gate is off.
    fn note_captcha_failure(&self, context: &RequestContext) {
        if let (Some(gate), Some(ip)) = (self.captcha.as_ref(), context.ip) {
            gate.record_failure(ip);
        }
    }

    /// Clears the per-IP failure counter on a successful authentication.
    fn note_captcha_success(&self, context: &RequestContext) {
        if let (Some(gate), Some(ip)) = (self.captcha.as_ref(), context.ip) {
            gate.record_success(ip);
        }
    }

    // --- the ML-DSA identity ceremonies ----------------------------------------

    /// Looks an account up by whatever the sign-in form accepts: email,
    /// phone, or username, folded. The one lookup every identifier-facing
    /// path (passphrase sign-in, recovery, challenge issue) shares, so the
    /// three cannot drift into accepting different shapes.
    async fn find_account(&self, identifier: &str) -> Result<Option<Account>> {
        let identifier = identifier.trim();
        if credential::looks_like_email(identifier) {
            match credential::email(identifier) {
                Ok(email) => Ok(self.store.account_by_email(&email).await?),
                // A malformed identifier is not a validation error to the caller:
                // telling somebody their email is badly formed is one bit more
                // than "that account does not exist".
                Err(_) => Ok(None),
            }
        } else if let Ok(phone) = credential::phone(identifier) {
            Ok(self.store.account_by_phone(&phone).await?)
        } else {
            let folded = identifier.trim_start_matches('@').to_ascii_lowercase();
            Ok(self.store.account_by_username(&folded).await?)
        }
    }

    /// Builds and stores one challenge, bound to an account, a device, and
    /// a purpose. The payload is the canonical MSE encoding the client will
    /// sign, stored exactly as issued so the answer is verified against the
    /// bytes that were sent, not against a re-encoding of them.
    async fn mint_challenge(
        &self,
        account_id: Id,
        device_id: Id,
        purpose: MlDsaPurpose,
        now: Timestamp,
    ) -> Result<ChallengeView> {
        let challenge_id = self.new_id(now);
        let expires_at = now.saturating_add_millis(IDENTITY_CHALLENGE_TTL_MS);
        let challenge = MlDsaChallenge {
            protocol_version: PROTOCOL_VERSION,
            purpose,
            account_id,
            device_id,
            challenge_id,
            // From the injected generator, never from anything the client
            // controls: a nonce the client could predict is a challenge the
            // client could pre-sign.
            nonce: self.draw_nonce(),
            issued_at: now,
            expires_at,
            server_region: self.signer.region().to_string(),
        };
        let payload = to_bytes(&challenge)
            .map_err(|error| fault::internal(format!("a challenge could not be encoded: {error}")))?
            .to_vec();
        self.store
            .put_login_challenge(LoginChallengeRow {
                challenge_id,
                account_id,
                device_id,
                purpose,
                payload: payload.clone(),
                expires_at,
                consumed_at: None,
                created_at: now,
            })
            .await?;
        Ok(ChallengeView {
            payload,
            challenge_id,
            device_id,
            expires_at,
        })
    }

    /// Builds a challenge that is never stored and never answerable.
    ///
    /// The enumeration defence for challenge issue: an unknown identifier, a
    /// device that is not the account's, or an account that may not sign in
    /// all produce this — a well-formed challenge over random ids, costing
    /// what a real one costs to build. The client signs it, answers it, and
    /// is told `CHALLENGE_INVALID`, exactly what an expired real challenge
    /// would have said. Nothing along the way reveals which case it was.
    fn fake_challenge(&self, now: Timestamp) -> Result<ChallengeView> {
        let challenge = MlDsaChallenge {
            protocol_version: PROTOCOL_VERSION,
            purpose: MlDsaPurpose::Login,
            account_id: self.new_id(now),
            device_id: self.new_id(now),
            challenge_id: self.new_id(now),
            nonce: self.draw_nonce(),
            issued_at: now,
            expires_at: now.saturating_add_millis(IDENTITY_CHALLENGE_TTL_MS),
            server_region: self.signer.region().to_string(),
        };
        let expires_at = challenge.expires_at;
        let payload = to_bytes(&challenge)
            .map_err(|error| fault::internal(format!("a challenge could not be encoded: {error}")))?
            .to_vec();
        Ok(ChallengeView {
            payload,
            challenge_id: challenge.challenge_id,
            device_id: challenge.device_id,
            expires_at,
        })
    }

    /// Thirty-two bytes from the injected generator.
    fn draw_nonce(&self) -> Vec<u8> {
        let mut nonce = vec![0u8; 32];
        let mut random = self.random.lock();
        random.fill_bytes(&mut nonce);
        nonce
    }

    /// Reads a live challenge and insists on its purpose, so a challenge
    /// issued for one ceremony can never be answered for another even
    /// before the purpose-mixed signature check would have caught it.
    async fn challenge_for(
        &self,
        challenge_id: Id,
        purpose: MlDsaPurpose,
        now: Timestamp,
    ) -> Result<Option<LoginChallengeRow>> {
        Ok(self
            .store
            .get_login_challenge(challenge_id, now)
            .await?
            .filter(|row| row.purpose == purpose))
    }

    /// Verifies one ML-DSA signature, collapsing every failure into the
    /// same refusal a wrong passphrase produces. A wrong length, an
    /// undecodable key, and a forged signature are one answer, so the
    /// endpoint is not an oracle for which half failed.
    fn check_signature(
        public_key: &[u8],
        payload: &[u8],
        context: &[u8],
        signature: &[u8],
    ) -> std::result::Result<(), ()> {
        verify_identity(public_key, payload, context, signature).map_err(|_| ())
    }
}

impl<S: ?Sized, L: ?Sized> fmt::Debug for Auth<S, L> {
    /// Prints the policy in force and nothing else.
    ///
    /// Not derived, and not derivable: this struct holds a signing key, a passphrase hash,
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

/// The one refusal every unusable challenge produces: unknown, expired,
/// already consumed, or issued for another ceremony. Deliberately a single
/// code across all four (brief section 182) — the difference between them
/// is exactly what a replay probe wants to learn.
fn challenge_invalid() -> Error {
    fault::error(
        codes::CHALLENGE_INVALID,
        "the challenge is unknown, expired, already used, or bound to another ceremony",
    )
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
    S: AccountStore
        + DeviceStore
        + SessionStore
        + SafetyStore
        + RecoveryStore
        + IdentityStore
        + ChallengeStore
        + WalletStore
        + GlobalAdminStore
        + ?Sized
        + Send
        + Sync
        + 'static,
    L: RateLimiter + ?Sized + Send + Sync + 'static,
{
    async fn register(&self, request: Registration, context: &RequestContext) -> Result<Grant> {
        if !self.config.allow_registration {
            self.meters.registration_refused();
            return Err(fault::feature_disabled("registration"));
        }
        // Captcha is the cheapest gate to apply, so it goes first: a
        // flood of captcha-less attempts is priced at the captcha
        // check rather than the bucket that funds the rest of the
        // pipeline. The bucket stays untouched on a captcha refusal,
        // which is the right shape — captcha failures should not
        // look like rate-limit failures to a client.
        self.enforce_captcha(request.captcha.as_ref(), context)
            .await?;

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
        credential::passphrase(
            &request.passphrase,
            self.config.passphrase_min_length,
            Some(username.folded()),
        )
        .inspect_err(|_| self.meters.registration_refused())?;
        // Validated before the account exists: a registration that cannot
        // complete must not leave half an account behind.
        if let Some(public_key) = request.identity_public_key.as_ref() {
            if public_key.len() != PUBLIC_KEY_LEN {
                self.meters.registration_refused();
                return Err(fault::validation(
                    "identity_public_key",
                    &format!("must be {PUBLIC_KEY_LEN} bytes of encoded ML-DSA-65"),
                ));
            }
        }

        let now = context.now;
        let passphrase_hash = self
            .hash_passphrase(request.passphrase.expose(), &self.meters)
            .await?;

        let account_id = self.new_id(now);
        let account = match self
            .store
            .create_account(NewAccount {
                account_id,
                username: username.display().to_string(),
                email,
                phone,
                passphrase_hash,
                locale: normalise_locale(&request.locale),
                country: request.country.clone(),
                created_at: now,
            })
            .await
        {
            Ok(account) => account,
            Err(error) => {
                self.meters.registration_refused();
                // The store reports a collision generically, because it does not know
                // which unique index tripped. A username collision is by far the most
                // common and the only one a client can act on, so it gets the specific
                // code.
                if error.code() != codes::ALREADY_EXISTS {
                    return Err(error);
                }
                // §12: a retried registration must not mint a second account. The
                // account already exists under this username; the only retry that
                // may fold into it is one whose identity key is the account's own
                // active key — the proof the first attempt came from this client,
                // because that key was generated on this device and never left it.
                // A retry with a different (or no) identity key keeps the
                // USERNAME_TAKEN answer: for a different key that is the honest
                // "someone else owns that name" verdict, and without a key there is
                // nothing to reconcile on but the passphrase, which sign-in already
                // exists to check.
                return self
                    .reconcile_registration(&request, &username, context)
                    .await;
            }
        };

        self.store
            .create_profile(Profile {
                account_id,
                display_name: username.display().to_string(),
                bio: None,
                avatar_media_id: None,
                birth_year: None,
                gender: request.gender,
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

        // An account born with a root secret publishes its identity key up
        // front, so the challenge login works from the first breath (brief
        // section 182). A passphrase-only registration — every legacy client —
        // skips this and can publish later through the upgrade door.
        if let Some(public_key) = request.identity_public_key.as_ref() {
            let key_id = self.new_id(now);
            self.store
                .put_identity_key(IdentityKeyRow {
                    key_id,
                    account_id,
                    algorithm: IDENTITY_ALGORITHM.to_string(),
                    key_version: i32::from(KEY_VERSION_ONE),
                    public_key: public_key.clone(),
                    status: IdentityKeyStatus::Active,
                    created_at: now,
                    revoked_at: None,
                })
                .await?;
            self.audit(
                Some(account_id),
                AuditActorKind::User,
                "account.identity.created",
                AuditTargetKind::IdentityKey,
                Some(key_id),
                format!("identity key v{KEY_VERSION_ONE} registered at account creation"),
                context,
            )
            .await;
        }

        let tier = tier::of_account(&account, now);
        let device = self.resolve_device(&account, &request.device, now).await?;
        let grant = self
            .open_session(&account, &device, tier, now, context, None, None, true)
            .await?;
        self.store.record_login(account_id, now).await?;

        // The effective server is the explicit `ServerEndpoint` the
        // client disclosed on the form, or the loopback default when
        // the request did not name one. Recorded in the audit summary
        // so an operator can correlate a registration with the
        // deployment the request reached, and so the absence of a
        // field is itself a meaningful row in the audit trail.
        let server = request.server_or_default();
        self.audit(
            Some(account_id),
            AuditActorKind::User,
            "account.register",
            AuditTargetKind::Account,
            Some(account_id),
            format!(
                "account created as @{} via {}:{}",
                username.display(),
                server.host,
                server.port
            ),
            context,
        )
        .await;
        self.meters.registered();
        Ok(grant)
    }

    async fn sign_in(&self, request: SignIn, context: &RequestContext) -> Result<Grant> {
        // The progressive lockout leads: five wrong passphrases for one identifier buy a
        // minute, every further three buy two more, capped. The key is the identifier the
        // attempt named (folded), so the lock follows the account's username across every
        // network — the product decision, with its trade accepted. The check runs before
        // the attempt is priced, so a locked-out guesser spends nothing and learns nothing.
        if let Some(gate) = self.lockout.as_ref() {
            let key = request
                .identifier
                .trim()
                .trim_start_matches('@')
                .to_ascii_lowercase();
            if let Err(remaining_ms) = gate.check(context.now, &key) {
                self.meters.signin(SignInOutcome::RateLimited);
                return Err(error_locked(remaining_ms));
            }
        }

        // Sign-in deliberately runs WITHOUT the captcha gate, whatever the config says: a
        // returning member is the person the product exists for, and standing between them and
        // their account because a stranger from their network mistyped passphrases is punishing
        // the wrong party. The flood-side pricing still applies — this call charges the
        // anonymous rate-limit bucket for the attempt — and `register` (plus recovery) keeps
        // its captcha gate. The gate's own failure counter is still recorded below on a failed
        // attempt, so a hostile IP hardens `register` against itself too.

        if let Err(error) = self
            .charge_stranger(context, Opcode::Authenticate, self.prices.attempt)
            .await
        {
            self.meters.signin(SignInOutcome::RateLimited);
            return Err(error);
        }

        let identifier = request.identifier.trim();
        let found = self.find_account(identifier).await?;

        let Some(account) = found else {
            // Verify against the placeholder so this path costs what the real one costs.
            // Without it the response time answers "does this account exist".
            let _ = self
                .verify_passphrase(request.passphrase.expose(), &self.absent_hash, &self.meters)
                .await;
            self.charge_penalty(context, Opcode::Authenticate).await;
            self.note_captcha_failure(context);
            // The lockout counts this failure for the identifier the attempt named. A
            // nonexistent account locks nothing real, but the count is what keeps an
            // enumeration attempt from riding the ladder for free.
            if let Some(gate) = self.lockout.as_ref() {
                let now = context.now;
                let key = identifier.trim_start_matches('@').to_ascii_lowercase();
                if let Some(seconds) = gate.record_failure(now, &key) {
                    tracing::warn!(seconds, "sign-in lockout engaged for an identifier");
                }
            }
            self.meters.signin(SignInOutcome::UnknownUser);
            return Err(fault::invalid_credentials());
        };

        let verification = self
            .verify_passphrase(
                request.passphrase.expose(),
                &account.passphrase_hash,
                &self.meters,
            )
            .await?;
        let Some(verification) = verification else {
            self.charge_penalty(context, Opcode::Authenticate).await;
            self.note_captcha_failure(context);
            if let Some(gate) = self.lockout.as_ref() {
                let now = context.now;
                let key = identifier.trim_start_matches('@').to_ascii_lowercase();
                if let Some(seconds) = gate.record_failure(now, &key) {
                    tracing::warn!(seconds, "sign-in lockout engaged for an identifier");
                }
            }
            self.meters.signin(SignInOutcome::BadPassphrase);
            return Err(fault::invalid_credentials());
        };

        // Only now, with the passphrase proven, is it safe to say anything specific about
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
                .hash_passphrase(request.passphrase.expose(), &self.meters)
                .await
            {
                Ok(hash) => {
                    if let Err(error) = self
                        .store
                        .set_passphrase_hash(account.account_id, hash.expose(), now)
                        .await
                    {
                        // A failed upgrade must not fail the sign-in: the passphrase was
                        // correct, and the old hash still works.
                        tracing::warn!(%error, "could not store a rehashed passphrase");
                    } else {
                        self.meters.rehashed();
                    }
                }
                Err(error) => tracing::warn!(%error, "could not rehash a passphrase"),
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
        // A successful sign-in clears the per-IP captcha counter. The
        // gate suspects a network, not a person; the person just
        // proved they own the account, so the suspicion is over.
        self.note_captcha_success(context);
        // …and the lockout ladder with it: the owner proved themselves, so the guessing
        // attack the ladder was pricing is over.
        if let Some(gate) = self.lockout.as_ref() {
            let key = identifier.trim_start_matches('@').to_ascii_lowercase();
            gate.record_success(&key);
        }
        self.meters.signin(SignInOutcome::Success);
        // The effective server the request reached. Recorded for
        // parity with the registration path so the operator can
        // correlate a successful sign-in with the deployment the
        // client believed it was talking to. The default applies
        // when the client did not name a server; the value is the
        // same loopback posture every other default falls back to.
        let server = request.server_or_default();
        let _ = server;
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

    fn issue_captcha<'a>(
        &'a self,
        mode: migo_captcha::CaptchaMode,
        _now: Timestamp,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<migo_captcha::CaptchaChallengeView>>
                + Send
                + 'a,
        >,
    > {
        // The gate, when present, owns both the service and the store. When
        // absent, captcha is off for the whole process and the route layer
        // surfaces `FEATURE_DISABLED` rather than answering with a
        // meaningless stub. The `now` parameter is reserved for a future
        // time-anchored challenge-id (UUIDv7) when the gate is wired to a
        // multi-replica store that needs monotonic ids.
        let gate = self.captcha.clone();
        Box::pin(async move {
            let gate = gate?;
            gate.request(mode).await.ok()
        })
    }

    async fn change_passphrase(
        &self,
        identity: &Identity,
        change: PassphraseChange,
        context: &RequestContext,
    ) -> Result<Grant> {
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        let now = context.now;
        let account = self.live_account(identity.account_id()).await?;

        let verified = self
            .verify_passphrase(
                change.current.expose(),
                &account.passphrase_hash,
                &self.meters,
            )
            .await?;
        if verified.is_none() {
            return Err(fault::invalid_credentials());
        }
        credential::passphrase(
            &change.next,
            self.config.passphrase_min_length,
            Some(&account.username),
        )?;

        let hash = self
            .hash_passphrase(change.next.expose(), &self.meters)
            .await?;
        self.store
            .set_passphrase_hash(account.account_id, hash.expose(), now)
            .await?;

        // Every session, including this one. The replacement grant is what keeps the
        // caller signed in; anything else on any other device has to sign in again.
        let revoked = self
            .store
            .revoke_account_sessions(
                account.account_id,
                None,
                RevokeReason::PassphraseChanged,
                now,
            )
            .await?;
        self.meters.sessions_revoked(revoked);

        let device = self
            .store
            .device_by_id(identity.device_id())
            .await?
            .filter(|device| device.revoked_at.is_none())
            .ok_or_else(|| fault::error(codes::TOKEN_REVOKED, "the device was revoked"))?;
        let tier = tier::of_account(&account, now);
        // `now` for `authenticated_at`: the current passphrase was just typed, which is the
        // strongest presence signal there is.
        let grant = self
            .open_session(&account, &device, tier, now, context, None, None, false)
            .await?;
        self.store.record_login(account.account_id, now).await?;

        self.audit(
            Some(account.account_id),
            AuditActorKind::User,
            "account.passphrase.change",
            AuditTargetKind::Account,
            Some(account.account_id),
            format!("passphrase changed; {revoked} sessions ended"),
            context,
        )
        .await;
        Ok(grant)
    }

    async fn set_contact(
        &self,
        identity: &Identity,
        contact: &str,
        context: &RequestContext,
    ) -> Result<()> {
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        let account = self.live_account(identity.account_id()).await?;
        // The auth crate validates the format up front so the store's
        // structural gate (and the unique indexes behind it) see a
        // canonical value rather than the user's first guess.
        let normalised = if credential::looks_like_email(contact) {
            credential::email(contact)?
        } else if let Ok(phone) = credential::phone(contact) {
            phone
        } else {
            return Err(fault::validation(
                "contact",
                "must be an email (containing @) or a phone (starting with +)",
            ));
        };
        self.store
            .set_contact(account.account_id, &normalised, context.now)
            .await?;
        self.audit(
            Some(account.account_id),
            AuditActorKind::User,
            "account.contact.set",
            AuditTargetKind::Account,
            Some(account.account_id),
            "contact recorded".to_string(),
            context,
        )
        .await;
        Ok(())
    }

    async fn request_recovery(
        &self,
        identifier: &str,
        captcha: &CaptchaProof,
        context: &RequestContext,
    ) -> Result<RecoveryRow> {
        // The captcha proof is consumed on the way in: a flood of recovery
        // requests cannot spend the captcha store's budget without a valid
        // challenge, and a captured proof cannot be replayed to mint a
        // second row for the same identifier.
        let Some(gate) = self.captcha.as_ref() else {
            return Err(crate::captcha::error_required());
        };
        match gate.verify(captcha).await? {
            true => {}
            false => {
                return Err(crate::captcha::error_invalid(
                    "the captcha proof did not verify",
                ))
            }
        }
        let Some(recovery_key) = self.recovery.as_ref() else {
            // Recovery is not configured for this deployment. The route
            // layer is the place that decides whether to mount the
            // endpoint at all; surfacing `CAPTCHA_REQUIRED` here is the
            // safe fail-closed answer.
            return Err(crate::captcha::error_required());
        };
        // Look the account up the same way `sign_in` does. The result is
        // not allowed to leak: an unknown identifier produces a row
        // anyway, and a real one does too, so the caller's response
        // shape is identical.
        let now = context.now;
        let identifier = identifier.trim();
        let account = self.find_account(identifier).await?;
        // A short-lived fake account id is used when the identifier does
        // not resolve; it never escapes, never persists, and keeps the
        // work the recovery flow does for a real account (mint a row,
        // compute a tag) identical to the work it does for a fake one.
        let (account_id, real) = match account {
            Some(account) => (account.account_id, true),
            None => (Id::generate_at(now, &mut **self.random.lock()), false),
        };
        let token_id = self.new_id(now);
        let expires_at = now.saturating_add_millis(RECOVERY_TTL);
        let tag = recovery_key.tag_parts(&[token_id.as_bytes(), LABEL_RECOVERY]);
        let row = RecoveryRow {
            token_id,
            account_id,
            tag: tag.to_vec(),
            expires_at,
            consumed_at: None,
            created_at: now,
        };
        // The row is only persisted for a real account. A row that names
        // a fake account id is what an attacker probing for which
        // identifier exists would scan for, and skipping the write is
        // the only way to deny them that signal.
        if real {
            self.store.recovery_put(row.clone()).await?;
        }
        Ok(row)
    }

    async fn confirm_recovery(
        &self,
        token_id: Id,
        tag: &[u8],
        new_passphrase: &Secret,
        context: &RequestContext,
    ) -> Result<()> {
        let Some(recovery_key) = self.recovery.as_ref() else {
            return Err(fault::error(
                codes::RECOVERY_NOT_FOUND,
                "recovery is not available on this deployment",
            ));
        };
        // The HMAC tag is verified before the row is touched: a wrong tag
        // does not consume the row, so a typo on the first try does not
        // lock the user out of a working recovery.
        if recovery_key
            .verify_parts(&[token_id.as_bytes(), LABEL_RECOVERY], tag)
            .is_err()
        {
            return Err(fault::error(
                codes::RECOVERY_NOT_FOUND,
                "the recovery tag did not verify",
            ));
        }
        let now = context.now;
        let row = match self.store.recovery_consume(token_id, now).await? {
            Some(row) => row,
            None => {
                return Err(fault::error(
                    codes::RECOVERY_NOT_FOUND,
                    "the recovery token is unknown, expired, or already consumed",
                ));
            }
        };
        let account = self
            .store
            .account_by_id(row.account_id)
            .await?
            .ok_or_else(|| {
                fault::error(
                    codes::RECOVERY_NOT_FOUND,
                    "the account behind this recovery token is gone",
                )
            })?;
        // Mirror the passphrase rules that `change_passphrase` enforces. A
        // weak replacement is the failure mode a recovery flow is most
        // likely to attract — the user forgot the old passphrase, the
        // temptation is to set something memorable, and memorable
        // collides with the common list more often than not.
        credential::passphrase(
            new_passphrase,
            self.config.passphrase_min_length,
            Some(&account.username),
        )?;
        let hash = self
            .hash_passphrase(new_passphrase.expose(), &self.meters)
            .await?;
        self.store
            .set_passphrase_hash(account.account_id, hash.expose(), now)
            .await?;
        // Revoke every session, including the one the user is on: a
        // recovery is exactly the moment a stolen token is most useful,
        // and the only safe assumption is that whatever signed in
        // before is now signed in by whoever clicked the link.
        let revoked = self
            .store
            .revoke_account_sessions(
                account.account_id,
                None,
                RevokeReason::PassphraseChanged,
                now,
            )
            .await?;
        self.meters.sessions_revoked(revoked);
        self.audit(
            Some(account.account_id),
            AuditActorKind::User,
            "account.passphrase.recover",
            AuditTargetKind::Account,
            Some(account.account_id),
            format!("passphrase recovered; {revoked} sessions ended"),
            context,
        )
        .await;
        Ok(())
    }

    // --- the ML-DSA identity ceremonies (brief section 182) ------------------

    async fn issue_identity_challenge(
        &self,
        request: IdentityChallengeRequest,
        context: &RequestContext,
    ) -> Result<ChallengeView> {
        let now = context.now;
        // Add-device writes two rows (the pending device and the challenge),
        // so it carries the price of a registration; a login challenge writes
        // one and carries the price of a sign-in attempt.
        let price = match request.scope {
            IdentityChallengeScope::AddDevice { .. } => self.prices.register,
            IdentityChallengeScope::Login { .. } => self.prices.attempt,
        };
        self.charge_stranger(context, Opcode::Authenticate, price)
            .await?;
        match request.scope {
            IdentityChallengeScope::Login {
                identifier,
                device_id,
            } => {
                // The challenge is real only when the identifier names an
                // account that may sign in AND the device is that account's,
                // active and credentialed. Anything else gets the fake — the
                // same bytes, the same shape, no way to tell which happened.
                let account = self.find_account(&identifier).await?;
                let trusted = match &account {
                    Some(account) if account.status.can_sign_in() => {
                        match self.store.device_by_id(device_id).await? {
                            Some(device) => {
                                device.account_id == account.account_id
                                    && device.status.may_authenticate()
                                    && device.public_credential.is_some()
                            }
                            None => false,
                        }
                    }
                    _ => false,
                };
                match (account, trusted) {
                    (Some(account), true) => {
                        self.mint_challenge(account.account_id, device_id, MlDsaPurpose::Login, now)
                            .await
                    }
                    _ => self.fake_challenge(now),
                }
            }
            IdentityChallengeScope::AddDevice { account_id } => {
                // The account id comes from the .migo container, not from a
                // username somebody typed, so a stranger probing for accounts
                // has nothing to probe with. The device limit is enforced at
                // issue time: a restore onto an account that is already full
                // is refused here rather than at the answer, after the rows
                // were written.
                let account = self.store.account_by_id(account_id).await?;
                let restorable = match &account {
                    Some(account) if account.status.can_sign_in() => {
                        let live = self.store.devices_for_account(account.account_id).await?;
                        (live.len() as u32) < self.config.max_devices_per_user
                    }
                    _ => false,
                };
                let Some(account) = account.filter(|_| restorable) else {
                    return self.fake_challenge(now);
                };
                // The pending row exists only to bind the challenge: it is
                // invisible in the device list and cannot authenticate until
                // the answer activates it.
                let device_id = self.new_id(now);
                let mut display_name = request.device.display_name.trim().to_string();
                if display_name.is_empty() {
                    display_name = default_device_name(request.device.platform).to_string();
                }
                truncate_chars(&mut display_name, MAX_DEVICE_NAME_CHARS);
                let mut app_version = request.device.app_version.trim().to_string();
                truncate_chars(&mut app_version, MAX_APP_VERSION_CHARS);
                self.store
                    .register_device(NewDevice {
                        device_id,
                        account_id: account.account_id,
                        platform: request.device.platform,
                        display_name,
                        app_version,
                        os_version: request.device.os_version.clone().map(|mut value| {
                            truncate_chars(&mut value, MAX_DEVICE_DETAIL_CHARS);
                            value
                        }),
                        device_model: request.device.device_model.clone().map(|mut value| {
                            truncate_chars(&mut value, MAX_DEVICE_DETAIL_CHARS);
                            value
                        }),
                        status: DeviceStatus::Pending,
                        // Set on the answer, when the presented key has been
                        // proven by a signature made with it.
                        public_credential: None,
                        created_at: now,
                    })
                    .await?;
                self.mint_challenge(account.account_id, device_id, MlDsaPurpose::AddDevice, now)
                    .await
            }
        }
    }

    async fn answer_identity_challenge(
        &self,
        answer: ChallengeAnswer,
        context: &RequestContext,
    ) -> Result<Grant> {
        self.charge_stranger(context, Opcode::Authenticate, self.prices.attempt)
            .await?;
        let now = context.now;
        let Some(row) = self
            .challenge_for(answer.challenge_id, MlDsaPurpose::Login, now)
            .await?
        else {
            return Err(challenge_invalid());
        };

        // Signatures first, consumption second: a wrong signature does not
        // burn the challenge, so a client that mangled the payload gets its
        // retry without a fresh ceremony. The consume is atomic in the
        // store, so a race between two answers still admits exactly one.
        let Some(account) = self.store.account_by_id(row.account_id).await? else {
            self.charge_penalty(context, Opcode::Authenticate).await;
            return Err(fault::invalid_credentials());
        };
        let Some(key) = self.store.active_identity_key(account.account_id).await? else {
            self.charge_penalty(context, Opcode::Authenticate).await;
            return Err(fault::invalid_credentials());
        };
        let device = self.store.device_by_id(row.device_id).await?;
        let device = match device {
            Some(device)
                if device.account_id == account.account_id
                    && device.status.may_authenticate()
                    && device.public_credential.is_some() =>
            {
                device
            }
            _ => {
                self.charge_penalty(context, Opcode::Authenticate).await;
                self.audit(
                    Some(row.account_id),
                    AuditActorKind::System,
                    "auth.identity.login_failed",
                    AuditTargetKind::Device,
                    Some(row.device_id),
                    "challenge answer refused: the device check failed".to_string(),
                    context,
                )
                .await;
                return Err(fault::invalid_credentials());
            }
        };
        let credential = device.public_credential.clone().unwrap_or_default();
        if Self::check_signature(
            &key.public_key,
            &row.payload,
            CONTEXT_LOGIN,
            &answer.identity_signature,
        )
        .is_err()
            || Self::check_signature(
                &credential,
                &row.payload,
                CONTEXT_LOGIN_DEVICE,
                &answer.device_signature,
            )
            .is_err()
        {
            self.charge_penalty(context, Opcode::Authenticate).await;
            self.audit(
                Some(row.account_id),
                AuditActorKind::System,
                "auth.identity.login_failed",
                AuditTargetKind::Device,
                Some(row.device_id),
                "challenge answer refused: a signature did not verify".to_string(),
                context,
            )
            .await;
            return Err(fault::invalid_credentials());
        }

        // Both halves proven. Now the challenge is spent — the answer that
        // lost a concurrent race is told exactly what an expired one is.
        if self
            .store
            .consume_login_challenge(answer.challenge_id, now)
            .await?
            .is_none()
        {
            return Err(challenge_invalid());
        }
        // Only now, with both signatures proven, is it safe to say anything
        // specific about the account — the same rule passphrase sign-in obeys.
        if !account.status.can_sign_in() {
            return Err(suspended(account.status));
        }

        let tier = tier::of_account(&account, now);
        let grant = self
            .open_session(&account, &device, tier, now, context, None, None, false)
            .await?;
        self.store.record_login(account.account_id, now).await?;
        self.meters.signin(SignInOutcome::Success);
        self.audit(
            Some(account.account_id),
            AuditActorKind::User,
            "auth.identity.login",
            AuditTargetKind::Session,
            Some(grant.session_id),
            format!("challenge login on \"{}\"", device.display_name),
            context,
        )
        .await;
        Ok(grant)
    }

    async fn answer_add_device(
        &self,
        answer: AddDeviceAnswer,
        context: &RequestContext,
    ) -> Result<Grant> {
        self.charge_stranger(context, Opcode::Authenticate, self.prices.attempt)
            .await?;
        let now = context.now;
        if answer.device_public_key.len() != PUBLIC_KEY_LEN {
            return Err(fault::validation(
                "device_public_key",
                &format!("must be {PUBLIC_KEY_LEN} bytes of encoded ML-DSA-65"),
            ));
        }
        let Some(row) = self
            .challenge_for(answer.challenge_id, MlDsaPurpose::AddDevice, now)
            .await?
        else {
            return Err(challenge_invalid());
        };

        let Some(account) = self.store.account_by_id(row.account_id).await? else {
            self.charge_penalty(context, Opcode::Authenticate).await;
            return Err(fault::invalid_credentials());
        };
        let Some(key) = self.store.active_identity_key(account.account_id).await? else {
            self.charge_penalty(context, Opcode::Authenticate).await;
            return Err(fault::invalid_credentials());
        };
        if Self::check_signature(
            &key.public_key,
            &row.payload,
            CONTEXT_LOGIN,
            &answer.identity_signature,
        )
        .is_err()
            || Self::check_signature(
                &answer.device_public_key,
                &row.payload,
                CONTEXT_LOGIN_DEVICE,
                &answer.device_signature,
            )
            .is_err()
        {
            self.charge_penalty(context, Opcode::Authenticate).await;
            self.audit(
                Some(row.account_id),
                AuditActorKind::System,
                "auth.identity.login_failed",
                AuditTargetKind::Device,
                Some(row.device_id),
                "add-device answer refused: a signature did not verify".to_string(),
                context,
            )
            .await;
            return Err(fault::invalid_credentials());
        }

        if self
            .store
            .consume_login_challenge(answer.challenge_id, now)
            .await?
            .is_none()
        {
            return Err(challenge_invalid());
        }
        if !account.status.can_sign_in() {
            return Err(suspended(account.status));
        }

        // The pending row becomes a device: active, and carrying the
        // credential that just signed. Activation before the credential, so
        // a failure between the two leaves an active device with no
        // credential (which simply cannot use the ceremony) rather than a
        // credentialed device that was never activated.
        self.store.activate_device(row.device_id, now).await?;
        self.store
            .set_device_credential(row.device_id, &answer.device_public_key)
            .await?;
        let Some(device) = self.store.device_by_id(row.device_id).await? else {
            return Err(fault::internal(
                "the device row vanished between activation and session open",
            ));
        };

        let tier = tier::of_account(&account, now);
        let grant = self
            .open_session(&account, &device, tier, now, context, None, None, false)
            .await?;
        self.store.record_login(account.account_id, now).await?;
        self.meters.signin(SignInOutcome::Success);
        self.audit(
            Some(account.account_id),
            AuditActorKind::User,
            "account.device.added",
            AuditTargetKind::Device,
            Some(device.device_id),
            format!(
                "device \"{}\" restored from a root secret",
                device.display_name
            ),
            context,
        )
        .await;
        Ok(grant)
    }

    async fn issue_rotation_challenge(
        &self,
        identity: &Identity,
        context: &RequestContext,
    ) -> Result<ChallengeView> {
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        let now = context.now;
        let account = self.live_account(identity.account_id()).await?;
        let device = self
            .store
            .device_by_id(identity.device_id())
            .await?
            .filter(|device| {
                device.account_id == account.account_id && device.status.may_authenticate()
            })
            .ok_or_else(|| fault::error(codes::TOKEN_REVOKED, "the device was revoked"))?;
        self.mint_challenge(
            account.account_id,
            device.device_id,
            MlDsaPurpose::Rotate,
            now,
        )
        .await
    }

    async fn rotate_identity(
        &self,
        identity: &Identity,
        answer: RotationAnswer,
        context: &RequestContext,
    ) -> Result<()> {
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        let now = context.now;
        let Some(row) = self
            .challenge_for(answer.challenge_id, MlDsaPurpose::Rotate, now)
            .await?
        else {
            return Err(challenge_invalid());
        };
        // A rotate challenge issued to another account, or to another device
        // of this one, is not this caller's to answer.
        if row.account_id != identity.account_id() || row.device_id != identity.device_id() {
            return Err(challenge_invalid());
        }
        if answer.new_public_key.len() != PUBLIC_KEY_LEN {
            return Err(fault::validation(
                "new_public_key",
                &format!("must be {PUBLIC_KEY_LEN} bytes of encoded ML-DSA-65"),
            ));
        }
        let account = self.live_account(identity.account_id()).await?;
        let Some(current) = self.store.active_identity_key(account.account_id).await? else {
            return Err(fault::invalid_credentials());
        };
        if current.public_key == answer.new_public_key {
            return Err(fault::conflict(
                "an identity rotation must install a different key",
            ));
        }
        // Only the key being retired can authorise its successor: the
        // signature is verified against the *current* key under the rotate
        // context, which no login signature can satisfy.
        Self::check_signature(
            &current.public_key,
            &row.payload,
            CONTEXT_ROTATE,
            &answer.signature,
        )
        .map_err(|_| fault::invalid_credentials())?;
        if self
            .store
            .consume_login_challenge(answer.challenge_id, now)
            .await?
            .is_none()
        {
            return Err(challenge_invalid());
        }

        let key_id = self.new_id(now);
        let successor_version = current.key_version + 1;
        // One store call: the successor is inserted active and the
        // predecessor retired together, so a failure in the middle can never
        // leave the account with two live identities or none.
        self.store
            .rotate_identity_key(IdentityKeyRow {
                key_id,
                account_id: account.account_id,
                algorithm: IDENTITY_ALGORITHM.to_string(),
                key_version: successor_version,
                public_key: answer.new_public_key,
                status: IdentityKeyStatus::Active,
                created_at: now,
                revoked_at: None,
            })
            .await?;
        self.audit(
            Some(account.account_id),
            AuditActorKind::User,
            "account.identity.rotate",
            AuditTargetKind::IdentityKey,
            Some(key_id),
            format!(
                "identity key v{} rotated to v{}; sessions unaffected",
                current.key_version, successor_version
            ),
            context,
        )
        .await;
        Ok(())
    }

    async fn publish_identity_key(
        &self,
        identity: &Identity,
        publication: IdentityPublication,
        context: &RequestContext,
    ) -> Result<()> {
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        let now = context.now;
        if publication.identity_public_key.len() != PUBLIC_KEY_LEN {
            return Err(fault::validation(
                "identity_public_key",
                &format!("must be {PUBLIC_KEY_LEN} bytes of encoded ML-DSA-65"),
            ));
        }
        if let Some(key) = publication.device_public_key.as_ref() {
            if key.len() != PUBLIC_KEY_LEN {
                return Err(fault::validation(
                    "device_public_key",
                    &format!("must be {PUBLIC_KEY_LEN} bytes of encoded ML-DSA-65"),
                ));
            }
        }
        let account = self.live_account(identity.account_id()).await?;
        match self.store.active_identity_key(account.account_id).await? {
            None => {
                // The legacy account's first identity key: version one, the
                // same row a root-secret registration would have created.
                let key_id = self.new_id(now);
                self.store
                    .put_identity_key(IdentityKeyRow {
                        key_id,
                        account_id: account.account_id,
                        algorithm: IDENTITY_ALGORITHM.to_string(),
                        key_version: i32::from(KEY_VERSION_ONE),
                        public_key: publication.identity_public_key,
                        status: IdentityKeyStatus::Active,
                        created_at: now,
                        revoked_at: None,
                    })
                    .await?;
                self.audit(
                    Some(account.account_id),
                    AuditActorKind::User,
                    "account.identity.created",
                    AuditTargetKind::IdentityKey,
                    Some(key_id),
                    format!(
                        "identity key v{KEY_VERSION_ONE} registered on a passphrase-era account"
                    ),
                    context,
                )
                .await;
            }
            Some(existing) if existing.public_key == publication.identity_public_key => {
                // The idempotent retry: the account already carries exactly
                // this key, which is what an upgrade that ran twice does.
            }
            Some(_) => {
                return Err(fault::conflict(
                    "the account already has a different identity key; rotate instead",
                ));
            }
        }
        if let Some(public_key) = publication.device_public_key.as_ref() {
            let device = self
                .store
                .device_by_id(identity.device_id())
                .await?
                .filter(|device| device.account_id == account.account_id)
                .ok_or_else(|| fault::not_found("device"))?;
            match device.public_credential.as_deref() {
                None => {
                    self.store
                        .set_device_credential(device.device_id, public_key)
                        .await?;
                }
                // Same rule as the identity key: an identical retry is fine,
                // a different credential belongs to a different device.
                Some(existing) if existing == public_key.as_slice() => {}
                Some(_) => {
                    return Err(fault::conflict(
                        "the device already holds a different credential; add the device again instead",
                    ));
                }
            }
        }
        Ok(())
    }

    // --- devices and wallets ----------------------------------------------------

    async fn devices(
        &self,
        identity: &Identity,
        // The context is part of the trait's shape — every operation that can
        // charge or audit takes one — but this one reads a list the caller
        // owns and charges nothing.
        _context: &RequestContext,
    ) -> Result<Vec<DeviceSummary>> {
        let list = self
            .store
            .devices_for_account(identity.account_id())
            .await?;
        Ok(list
            .into_iter()
            .map(|device| DeviceSummary {
                status: match device.status {
                    DeviceStatus::Pending => "pending",
                    DeviceStatus::Active => "active",
                    DeviceStatus::Revoked => "revoked",
                }
                .to_string(),
                has_credential: device.public_credential.is_some(),
                is_current: device.device_id == identity.device_id(),
                device_id: device.device_id,
                display_name: device.display_name,
                platform: device.platform,
                created_at: device.created_at,
                last_seen_at: device.last_seen_at,
            })
            .collect())
    }

    async fn register_wallet(
        &self,
        identity: &Identity,
        registration: WalletRegistration,
        context: &RequestContext,
    ) -> Result<WalletSummary> {
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        let now = context.now;
        let account = self.live_account(identity.account_id()).await?;
        // Normalise to the canonical form the store's unique index holds:
        // no 0x prefix, lowercase, exactly forty hex characters. The
        // checksummed and prefixed forms a human pastes are accepted and
        // folded, never rejected.
        let address = registration
            .address
            .trim()
            .trim_start_matches("0x")
            .to_ascii_lowercase();
        if address.len() != MAX_WALLET_ADDRESS_CHARS
            || !address.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(fault::validation(
                "address",
                "must be a 40-character hex EVM address",
            ));
        }
        let mut chain_type = registration.chain_type.trim().to_ascii_lowercase();
        if chain_type.is_empty() {
            chain_type = "evm".to_string();
        }
        truncate_chars(&mut chain_type, MAX_CHAIN_TYPE_CHARS);
        let mut label = registration.label.map(|value| value.trim().to_string());
        if label.as_deref() == Some("") {
            label = None;
        }
        if let Some(label) = label.as_mut() {
            truncate_chars(label, MAX_WALLET_LABEL_CHARS);
        }

        // Read before writing, so the audit can tell a first registration
        // from the idempotent re-registration a restore performs.
        let existing = self
            .store
            .wallets_for_account(account.account_id)
            .await?
            .into_iter()
            .any(|wallet| wallet.address == address && wallet.chain_type == chain_type);
        self.store
            .put_wallet(WalletRow {
                wallet_id: self.new_id(now),
                account_id: account.account_id,
                address: address.clone(),
                chain_type: chain_type.clone(),
                label,
                derivation_index: registration.derivation_index,
                status: WalletStatus::Active,
                created_at: now,
                archived_at: None,
            })
            .await?;
        // The put is idempotent per address, so the row that stands may be
        // the one registered last year — read back what the account actually
        // holds rather than assuming the id just generated survived.
        let summary = self
            .store
            .wallets_for_account(account.account_id)
            .await?
            .into_iter()
            .find(|wallet| wallet.address == address && wallet.chain_type == chain_type)
            .map(WalletSummary::from)
            .ok_or_else(|| {
                fault::internal("the wallet just registered is missing from the account's list")
            })?;
        if !existing {
            self.audit(
                Some(account.account_id),
                AuditActorKind::User,
                "account.wallet.register",
                AuditTargetKind::Wallet,
                Some(summary.wallet_id),
                format!("wallet {address} registered on chain {chain_type}"),
                context,
            )
            .await;
        }
        Ok(summary)
    }

    async fn wallets(
        &self,
        identity: &Identity,
        // Same shape rule as `devices`: a read of the caller's own list.
        _context: &RequestContext,
    ) -> Result<Vec<WalletSummary>> {
        let list = self
            .store
            .wallets_for_account(identity.account_id())
            .await?;
        Ok(list.into_iter().map(WalletSummary::from).collect())
    }

    async fn archive_wallet(
        &self,
        identity: &Identity,
        wallet_id: Id,
        context: &RequestContext,
    ) -> Result<()> {
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        let now = context.now;
        // Only an active wallet of the caller's own is archived; anything
        // else is a no-op, because the list the client reads from already
        // reflects the outcome it wanted.
        let existing = self
            .store
            .wallets_for_account(identity.account_id())
            .await?
            .into_iter()
            .find(|wallet| wallet.wallet_id == wallet_id);
        if let Some(wallet) = existing.filter(|wallet| wallet.status == WalletStatus::Active) {
            let address = wallet.address.clone();
            self.store
                .archive_wallet(wallet_id, identity.account_id(), now)
                .await?;
            self.audit(
                Some(identity.account_id()),
                AuditActorKind::User,
                "account.wallet.archive",
                AuditTargetKind::Wallet,
                Some(wallet_id),
                format!("wallet {address} archived"),
                context,
            )
            .await;
        }
        Ok(())
    }

    async fn admin_standing(
        &self,
        identity: &Identity,
        // A read of the caller's own standing, same shape rule as `devices`.
        _context: &RequestContext,
    ) -> Result<AdminStanding> {
        Ok(AdminStanding {
            owner: self.config.owner_account_id == Some(identity.account_id()),
            admin: self.store.is_global_admin(identity.account_id()).await?,
        })
    }

    async fn global_admins(
        &self,
        identity: &Identity,
        _context: &RequestContext,
    ) -> Result<Vec<AdminView>> {
        self.require_owner(identity)?;
        let mut views = Vec::new();
        for row in self.store.global_admins().await? {
            // The foreign key cascades, so a vanished account takes its grant
            // with it; the lookup is only to resolve the username a human
            // reads, and a row that somehow lost its account is skipped rather
            // than allowed to blank the owner's whole list.
            let Some(account) = self.store.account_by_id(row.account_id).await? else {
                continue;
            };
            views.push(AdminView {
                account_id: row.account_id,
                username: account.username,
                granted_by: row.granted_by,
                granted_at: row.granted_at,
            });
        }
        Ok(views)
    }

    async fn grant_global_admin(
        &self,
        identity: &Identity,
        username: &str,
        context: &RequestContext,
    ) -> Result<AdminView> {
        self.require_owner(identity)?;
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        // Folded the same way sign-in folds its identifier, so "Admin" and
        // "@admin" name the same account here too.
        let folded = username.trim_start_matches('@').to_ascii_lowercase();
        let Some(account) = self.store.account_by_username(&folded).await? else {
            return Err(fault::not_found("account"));
        };
        let already = self.store.is_global_admin(account.account_id).await?;
        // The put keeps the original grant when the account is already an
        // admin, so a repeated appointment is a read, not a rewrite.
        let row = self
            .store
            .put_global_admin(GlobalAdmin {
                account_id: account.account_id,
                granted_by: identity.account_id(),
                granted_at: context.now,
            })
            .await?;
        if !already {
            self.audit(
                Some(identity.account_id()),
                AuditActorKind::Operator,
                "global_admin.grant",
                AuditTargetKind::Account,
                Some(row.account_id),
                format!("global admin granted to {}", account.username),
                context,
            )
            .await;
        }
        Ok(AdminView {
            account_id: row.account_id,
            username: account.username,
            granted_by: row.granted_by,
            granted_at: row.granted_at,
        })
    }

    async fn revoke_global_admin(
        &self,
        identity: &Identity,
        account_id: Id,
        context: &RequestContext,
    ) -> Result<()> {
        self.require_owner(identity)?;
        self.charge_account(identity, context, Opcode::Authenticate)
            .await?;
        // Revoking an account that is not an admin is quietly `Ok(())`, the
        // same shape rule as `archive_wallet`: the list the owner reads from
        // already reflects the outcome.
        if self.store.remove_global_admin(account_id).await? {
            self.audit(
                Some(identity.account_id()),
                AuditActorKind::Operator,
                "global_admin.revoke",
                AuditTargetKind::Account,
                Some(account_id),
                format!("global admin revoked from {account_id}"),
                context,
            )
            .await;
        }
        Ok(())
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

impl<S, L> Auth<S, L>
where
    S: AccountStore
        + DeviceStore
        + SessionStore
        + SafetyStore
        + RecoveryStore
        + IdentityStore
        + ChallengeStore
        + WalletStore
        + ?Sized,
    L: RateLimiter + ?Sized,
{
    /// The §12 half of registration: what happens when the account already
    /// exists. Called from `register` on a store collision for the username,
    /// with the request that collided.
    ///
    /// A retry may fold into the existing account only when the identity key
    /// it carries is the account's active key. The check is a comparison of
    /// public bytes, never a disclosure: a mismatched key learns nothing
    /// beyond the USERNAME_TAKEN verdict it would have received anyway, and
    /// the identity-key read it costs is priced by the register charge that
    /// already ran. The account's passphrase is verified too, so a leaked
    /// username plus a stolen identity key alone cannot be replayed into a
    /// session.
    async fn reconcile_registration(
        &self,
        request: &Registration,
        username: &credential::Username,
        context: &RequestContext,
    ) -> Result<Grant> {
        let taken = fault::error(codes::USERNAME_TAKEN, "that username is taken");
        let Some(existing) = self.store.account_by_username(username.folded()).await? else {
            // The collision was on the email or phone index, not the username.
            // Nothing here can reconcile that; the generic answer is the honest one.
            return Err(taken);
        };

        let Some(public_key) = request.identity_public_key.as_ref() else {
            // A passphrase-only registration carries nothing to reconcile on; the
            // sign-in endpoint is the reconciliation path for that client.
            return Err(taken);
        };
        let Some(active) = self.store.active_identity_key(existing.account_id).await? else {
            // The name's owner registered without an identity key (a legacy
            // passphrase-only account): no key match is possible.
            return Err(taken);
        };
        if public_key.as_slice() != active.public_key.as_slice() {
            return Err(taken);
        }
        // Only now, with the account proven to be this client's own, does the
        // passphrase get checked — and a wrong passphrase is the generic
        // invalid-credentials fault, so the reconcile path cannot be used as a
        // passphrase oracle for somebody else's name.
        if self
            .verify_passphrase(
                request.passphrase.expose(),
                &existing.passphrase_hash,
                &self.meters,
            )
            .await?
            .is_none()
        {
            return Err(fault::invalid_credentials());
        }
        if !existing.status.can_sign_in() {
            return Err(suspended(existing.status));
        }

        let now = context.now;
        let tier = tier::of_account(&existing, now);
        let device = self.resolve_device(&existing, &request.device, now).await?;
        let grant = self
            .open_session(&existing, &device, tier, now, context, None, None, false)
            .await?;
        self.store.record_login(existing.account_id, now).await?;
        let server = request.server_or_default();
        self.audit(
            Some(existing.account_id),
            AuditActorKind::User,
            "account.register.reconciled",
            AuditTargetKind::Account,
            Some(existing.account_id),
            format!(
                "registration retry for @{} folded into the existing account via {}:{}",
                username.display(),
                server.host,
                server.port
            ),
            context,
        )
        .await;
        self.meters.reconciled_registration();
        Ok(grant)
    }
}
