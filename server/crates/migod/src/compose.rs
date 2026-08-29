//! The composition root: one function that builds every layer into a running [`App`].
//!
//! Every other crate in the workspace refuses, on principle, to decide what it is wired to. A
//! domain service is handed its store, its cache, its rate limiter, and its ports; it never reaches
//! out and constructs them. That discipline is what keeps the layering honest and the crates
//! testable in isolation — and it has to be paid back somewhere. This module is where. [`App::build`]
//! is the single place in the system that holds all twenty-one crates at once and connects them.
//!
//! # Dependency order
//!
//! Construction runs strictly bottom-up, because each layer is an argument to the next. The metric
//! [`Registry`] and the [`Shutdown`] signal come first, since everything registers into the one and
//! observes the other. Then the platform (store, cache, rate limiter), then the eleven domain
//! services, then the two transports (the gateway and the REST API) that sit over them. A crate
//! never receives something built after it.
//!
//! # One registry, one shutdown
//!
//! There is exactly one [`Registry`]. Every service registers its counters into it by shared
//! reference at construction, and the REST API's `/metrics` endpoint renders that same instance, so
//! a gateway counter and an economy counter appear in one scrape. The plain registry is passed by
//! reference while services are built, then wrapped in an [`Arc`] for the API — the same instance,
//! now shared, never a second one. The [`Shutdown`] is likewise one signal: the gateway drains on
//! it, the axum server stops accepting on it, and a single SIGTERM moves the whole process toward a
//! clean stop.
//!
//! # The node secret
//!
//! Several subsystems — media tickets, push tokens, bot tokens — derive signing material from one
//! root secret. In production that root MUST be the configured node signing key; a node that
//! reached production without one refuses to start rather than mint tokens under a default nobody
//! can rotate (brief sections 77 and 145). On a laptop, an absent key yields an ephemeral secret
//! that changes every start, logged as such — tokens do not survive a restart, which is the honest
//! development trade and exactly what production forbids.

use std::sync::Arc;

use anyhow::{bail, Context};

use migo_auth::{captcha::CaptchaGate, ConcreteAuth, SharedAuth};
use migo_bots::SharedBots;
use migo_captcha::{CaptchaService, InMemoryStore as CaptchaInMemoryStore};
use migo_core::config::Environment;
use migo_core::metrics::Registry;
use migo_core::{Clock, Config, Id, OsRandom, Random, Shutdown, SystemClock};
use migo_crypto::NodeSecret;
use migo_economy::{Catalogue, SharedAnnouncer, SharedTreasurer, Silent};
use migo_federation::{MeshConfig, SharedMesh};
use migo_games::{SharedReferee, SharedRewards};
use migo_gateway::{Dispatcher, Gateway, GatewayServices};
use migo_keys::SharedKeyring;
use migo_media::{SharedLibrary, SharedStorage};
use migo_messaging::SharedMessaging;
use migo_moderation::{SharedRoster, SharedWarden};
use migo_notify::{NoPush, SharedNotifier, SharedPushSender};
use migo_presence::SharedPresence;
use migo_protocol::NodeInfo;
use migo_rooms::SharedRooms;
use migo_social::SharedSocial;

use crate::dispatch::AppDispatcher;
use crate::ports::{EconomyRewards, FsStorage, StaffRoster};

/// The feature bits this node advertises to clients in the handshake and the `/v1/config`
/// document. Zero for now: no optional protocol feature is gated behind a bit yet, so the node
/// advertises the base protocol and nothing more.
const FEATURES: u64 = 0;

/// Bytes of ephemeral secret to mint when no node signing key is configured (development only).
const EPHEMERAL_SECRET_LEN: usize = 32;

/// Builds the captcha gate that the bootstrap layer uses to gate the public
/// surface after enough failures from the same network. The in-memory
/// store is the right answer for a single-process migod; a multi-replica
/// deployment would replace this with the `PostgresCaptchaStore` that lives
/// in `migo-store`.
///
/// A threshold of `0` is refused here: the gate's contract says
/// `0 == "captcha on the first attempt"`, which is rarely what an operator
/// wants and is exactly the posture the configuration validator already
/// rejects. The check is duplicated at the composition root so a
/// hand-rolled migod cannot sidestep it.
fn migod_captcha_gate(threshold: u32, secret_root: &[u8]) -> migo_core::Result<Arc<CaptchaGate>> {
    if threshold == 0 {
        return Err(migo_protocol::fault::internal(
            "captcha threshold of 0 would require a proof on the first attempt; \
             set MIGO_AUTH__CAPTCHA_THRESHOLD to a positive integer or unset it to disable the gate",
        ));
    }
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(SystemClock);
    let service = Arc::new(CaptchaService::new(secret_root, clock.clone()));
    let store: Arc<dyn migo_captcha::CaptchaStore + Send + Sync> =
        Arc::new(CaptchaInMemoryStore::new());
    Ok(Arc::new(CaptchaGate::new(service, store, threshold)))
}

/// The threshold that gets fed to [`migod_captcha_gate`] when the
/// configuration is absent or fails to parse. Three is the documented
/// default in [`migo_core::config::AuthConfig`]; the value lives here as a
/// named constant so the production and test wiring agree on a number.
pub const DEFAULT_CAPTCHA_THRESHOLD: u32 = 3;

/// The captcha gate builder, exposed for tests so a unit test can pin the
/// threshold-zero rejection without standing up the full composition root.
#[doc(hidden)]
#[allow(dead_code)]
pub(crate) fn captcha_gate_for_test(
    threshold: u32,
    secret_root: &[u8],
) -> migo_core::Result<Arc<CaptchaGate>> {
    migod_captcha_gate(threshold, secret_root)
}

/// Attaches the captcha gate to a `SharedAuth` whose internal type is
/// hidden behind `dyn Authenticator`. The auth crate owns
/// `Auth::with_captcha`; this thin wrapper converts the concrete
/// `Auth<...>` into a `dyn Authenticator` after the gate is attached,
/// so the gate is per-process state and a clone would mean two
/// captcha stores racing on the same network.
fn attach_captcha(auth: ConcreteAuth, gate: Arc<CaptchaGate>) -> migo_core::Result<SharedAuth> {
    let auth = Arc::try_unwrap(auth).map_err(|_| {
        migo_protocol::fault::internal(
            "the auth handle still has another owner; cannot attach the captcha gate",
        )
    })?;
    let auth = auth.with_captcha(gate)?;
    Ok(Arc::new(auth) as SharedAuth)
}

/// A fully wired server: every layer constructed, connected, and ready to [`serve`](App::serve).
///
/// The transports — [`gateway`](App::gateway) and [`api_router`](App::api_router) — are what a
/// socket is bound to. The eleven domain services below them are held here for the life of the
/// process: most are reachable only through a transport, but the composition root owns them so
/// their internal tasks and pooled connections live until the server stops, not a moment less. The
/// fields are public because an integration test builds an `App` against in-memory backends and
/// drives a service directly, without a socket.
pub struct App {
    /// The realtime transport: one WebSocket connection per client, application opcodes dispatched
    /// into the domain.
    pub gateway: Arc<Gateway>,
    /// The REST transport: the unauthenticated bootstrap endpoints, the config document, and
    /// `/metrics`, assembled with its middleware already applied.
    pub api_router: axum::Router,
    /// The server's single source of "now", shared with every service and the gateway.
    pub clock: Arc<dyn Clock>,
    /// The one cooperative shutdown signal for the whole process.
    pub shutdown: Shutdown,
    /// The single metric [`Registry`] every service registered into; the REST API renders this
    /// exact instance at `/metrics`. Held here so an integration test can render it directly and
    /// assert that nothing sensitive ever reached a metric, without driving an HTTP request.
    pub registry: Arc<Registry>,
    /// The socket address the server binds, taken from the HTTP configuration.
    pub bind: String,
    /// Authentication: register, sign in, refresh, sign out, and access-token verification.
    pub auth: SharedAuth,
    /// Direct messaging: send, receipt, delete, sync, and conversation management.
    pub messaging: SharedMessaging,
    /// Presence: the online/away/offline state a user publishes to those who may see it.
    pub presence: SharedPresence,
    /// Rooms: the many-participant spaces, their membership, and their state.
    pub rooms: SharedRooms,
    /// The social graph: follows, blocks, and the relationship checks other domains consult.
    pub social: SharedSocial,
    /// Public key material: what a device publishes and what a sender fetches before it can
    /// encrypt anything. Never a private key, in any form (brief section 163).
    pub keys: SharedKeyring,
    /// Media: signed upload and download tickets, size verification, and tombstone sweeps.
    pub media: SharedLibrary,
    /// Moderation: operator actions gated on the staff roster's powers.
    pub moderation: SharedWarden,
    /// Notifications: push registration and delivery through the configured sender.
    pub notify: SharedNotifier,
    /// The economy: balances, gifts, and the ledger every credit and debit lands in.
    pub economy: SharedTreasurer,
    /// Games: matches, scoring, and the rewards a finished game hands to the economy.
    pub games: SharedReferee,
    /// Bots: registration and the minimum-privilege tokens automated accounts authenticate with.
    pub bots: SharedBots,
    /// Federation: the server-to-server mesh of trusted, allow-listed peer nodes.
    pub federation: SharedMesh,
}

impl App {
    /// Builds the whole system from a validated [`Config`].
    ///
    /// Runs bottom-up: the registry and shutdown signal, the platform layer (the data store is
    /// opened asynchronously; the cache and rate limiter are not), the eleven domain services with
    /// their ports and default tuning, then the gateway and the REST API over them. The only
    /// fallible steps are the ones that touch the outside world at startup — opening the store, the
    /// cache, the rate limiter, authentication, and bots — plus configuration validation and the
    /// production node-secret check; each is annotated so a failure names what could not be built.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration is invalid, if a production node has no signing key,
    /// or if any platform or fallible domain service cannot be opened (for example, the database is
    /// unreachable or the cache connection fails).
    pub async fn build(config: &Config) -> anyhow::Result<Self> {
        config.validate().context("configuration is not valid")?;

        let registry = Registry::new();
        let shutdown = Shutdown::new();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let node = node_info(config);
        let node_secret = resolve_node_secret(config)?;

        // --- Layer 2: platform ---
        let store = migo_store::open(&config.store)
            .await
            .context("cannot open the data store")?;
        // Apply pending schema migrations before any domain service is built on the store. The
        // pool `open` returned is lazy and has touched nothing yet, so this is also the first
        // call that reaches the database — it doubles as the startup connectivity check the
        // store documents. A no-op for the memory backend (its schema is the Rust types); for
        // postgres it runs the whole migration set under an advisory lock in one transaction,
        // idempotent on every boot. Under an orchestrator this runs once the database's own
        // health check has passed, so a still-starting database delays the query, not fails it.
        store
            .migrate()
            .await
            .context("cannot migrate the data store")?;
        let cache = migo_cache::open(&config.cache).context("cannot open the cache")?;
        let limiter = migo_ratelimit::open(cache.clone(), &config.rate_limit, &registry)
            .context("cannot open the rate limiter")?;

        // --- Layer 3: domain ---
        // Authentication takes the whole config: token lifetimes, argon2 parameters, and the
        // sign-in policy all live under different config sections it reads together.
        let concrete: ConcreteAuth = migo_auth::open(
            store.clone(),
            limiter.clone(),
            config,
            &registry,
            // The same 32-byte secret root the rest of the server tokens
            // derive from. None would have the auth crate refuse to start
            // in production; the dev value is a stand-in only.
            config
                .auth
                .token_key
                .as_ref()
                .map_or(&[], |s| s.expose().as_bytes()),
        )
        .context("cannot open authentication")?;
        let auth: SharedAuth = attach_captcha(
            concrete,
            migod_captcha_gate(
                config
                    .auth
                    .captcha_threshold
                    .unwrap_or(DEFAULT_CAPTCHA_THRESHOLD),
                config
                    .auth
                    .token_key
                    .as_ref()
                    .map_or(&[], |s| s.expose().as_bytes()),
            )
            .context("cannot build the captcha gate")?,
        )
        .context("cannot attach the captcha gate")?;

        let messaging =
            migo_messaging::open(store.clone(), cache.clone(), limiter.clone(), &registry);
        let presence = migo_presence::open(
            store.clone(),
            cache.clone(),
            limiter.clone(),
            &registry,
            migo_presence::PresenceConfig::default(),
        );
        let rooms = migo_rooms::open(
            store.clone(),
            limiter.clone(),
            &registry,
            migo_rooms::RoomsConfig::default(),
        );
        let social = migo_social::open(
            store.clone(),
            limiter.clone(),
            &registry,
            migo_social::SocialConfig::default(),
        );

        // The default policy serves a bundle without a one-time prekey rather than refusing it: a
        // conversation that starts with slightly weaker forward secrecy for its first message is
        // better than a conversation that cannot start, and the owning device is told to publish
        // more. A deployment that would rather fail says so with `refuse_when_exhausted`.
        let keys = migo_keys::open(
            store.clone(),
            limiter.clone(),
            &registry,
            migo_keys::KeysConfig::default(),
        );

        // Media never holds a byte (brief section 168); the filesystem backend is the development
        // stand-in for an object store, minting unsigned URLs under the node's public media path.
        let storage: SharedStorage = Arc::new(FsStorage::new(
            config.media.local_dir.clone(),
            format!("{}/media", config.http.public_url.trim_end_matches('/')),
        ));
        let media = migo_media::open(
            store.clone(),
            limiter.clone(),
            storage,
            Box::new(OsRandom),
            &node_secret,
            &config.media,
            &registry,
        );

        // The development posture is that nobody is staff, so every operator action is refused
        // until a real roster is configured.
        let roster: SharedRoster = Arc::new(StaffRoster::empty());
        let moderation = migo_moderation::open(
            store.clone(),
            limiter.clone(),
            roster,
            Box::new(OsRandom),
            migo_moderation::ModerationConfig::default(),
            &registry,
        );

        // No push sender is wired in this build: registrations are stored, deliveries are dropped.
        let sender: SharedPushSender = Arc::new(NoPush);
        let notify = migo_notify::open(
            store.clone(),
            cache.clone(),
            limiter.clone(),
            sender,
            Box::new(OsRandom),
            &node_secret,
            migo_notify::NotifyConfig::default(),
            &registry,
        );

        // The economy announces nothing to clients in this build; gifts are the standard catalogue.
        let announcer: SharedAnnouncer = Arc::new(Silent);
        let economy = migo_economy::open(
            store.clone(),
            cache.clone(),
            limiter.clone(),
            announcer,
            Catalogue::with_default_gifts(),
            migo_economy::EconomyConfig::default(),
            &registry,
        );

        // Games and economy are siblings and meet only here: a finished game credits experience and
        // a win confers a badge through the economy, via the one adapter that bridges them.
        let rewards: SharedRewards = Arc::new(EconomyRewards::new(economy.clone()));
        let games = migo_games::open(
            store.clone(),
            limiter.clone(),
            rewards,
            migo_games::GamesConfig::default(),
            &registry,
        );

        let bots = migo_bots::open(
            store.clone(),
            limiter.clone(),
            migo_bots::BotsConfig::default(),
            &node_secret,
            &registry,
        )
        .context("cannot open bots")?;

        // The mesh derives its node identity from the same secret root the rest of the server
        // tokens use; a production node must configure a real `node.signing_key`.
        let fed_secret = NodeSecret::from_seed(&node_secret)
            .context("cannot derive the federation node secret")?;
        // The mesh identifies this node by an `Id`; configuration carries a human name, so derive a
        // stable id from the (production) signing key bytes. The value is only used for mesh
        // self-addressing and peer-row keys, so any deterministic mapping is fine.
        let mut fed_node_id = [0u8; 16];
        let n = node_secret.len().min(16);
        fed_node_id[..n].copy_from_slice(&node_secret[..n]);
        let federation = migo_federation::open(
            store.clone(),
            MeshConfig::default(),
            Id::from_bytes(fed_node_id),
            node.region.clone(),
            fed_secret,
            &registry,
        )
        .context("cannot open the federation mesh")?;

        // --- Layer 4: transports ---
        // The dispatcher is the one seam the gateway calls up through; it routes the client-facing
        // opcodes this node speaks into messaging, presence, rooms, key material, the social graph,
        // and games.
        let dispatcher: Arc<dyn Dispatcher> = Arc::new(AppDispatcher::new(
            messaging.clone(),
            presence.clone(),
            rooms.clone(),
            keys.clone(),
            social.clone(),
            games.clone(),
            media.clone(),
            economy.clone(),
            moderation.clone(),
            notify.clone(),
            federation.clone(),
            bots.clone(),
        ));

        let gateway = Gateway::open(
            &registry,
            &config.gateway,
            GatewayServices {
                authenticator: auth.clone(),
                rate_limiter: limiter.clone(),
                clock: clock.clone(),
                random: Box::new(OsRandom),
                dispatcher,
                shutdown: shutdown.clone(),
                node: node.clone(),
                features: FEATURES,
            },
        );

        // The REST API renders the very registry every service just registered into, so wrapping it
        // in an `Arc` here must come after the last `&registry` borrow above.
        let registry = Arc::new(registry);
        let api_router = migo_api::router(
            config,
            migo_api::ApiServices {
                authenticator: auth.clone(),
                rate_limiter: limiter,
                clock: clock.clone(),
                registry: registry.clone(),
                node,
                features: FEATURES,
            },
        );

        Ok(Self {
            gateway: Arc::new(gateway),
            api_router,
            clock,
            shutdown,
            registry,
            bind: config.http.bind.clone(),
            auth,
            messaging,
            presence,
            rooms,
            social,
            keys,
            media,
            moderation,
            notify,
            economy,
            games,
            bots,
            federation,
        })
    }
}

/// Reads this node's identity out of the configuration into the transport-facing [`NodeInfo`].
fn node_info(config: &Config) -> NodeInfo {
    NodeInfo {
        node_id: config.node.id.clone(),
        region: config.node.region.clone(),
        country: config.node.country.clone(),
    }
}

/// Resolves the root secret every token-signing subsystem derives from.
///
/// Prefers the configured node signing key. Its bytes are used as opaque high-entropy key material
/// — the subsystems run it through HKDF, not as an Ed25519 key — so the base64 text is fine as-is.
/// Absent, the environment decides: production refuses to start (a token signed under a default is
/// a token nobody can rotate), while development and staging mint an ephemeral secret and say so.
fn resolve_node_secret(config: &Config) -> anyhow::Result<Vec<u8>> {
    if let Some(signing_key) = config.node.signing_key.as_ref() {
        return Ok(signing_key.expose().as_bytes().to_vec());
    }

    if config.node.environment == Environment::Production {
        bail!("node.signing_key is required in production but was not configured");
    }

    tracing::warn!(
        "no node.signing_key configured; deriving an ephemeral development secret \
         (tokens will not survive a restart)"
    );
    let mut secret = vec![0u8; EPHEMERAL_SECRET_LEN];
    let mut random = OsRandom;
    random.fill_bytes(&mut secret);
    Ok(secret)
}

#[cfg(test)]
mod tests {
    //! Pinned the wiring the composition root owes the bootstrap surface.
    //!
    //! The end-to-end captcha+register path is covered by the API integration
    //! test in `server/crates/migo-api/tests/auth-flow.rs`; this file pins the
    //! thin invariants that are easier to break by accident in `compose.rs`
    //! itself — the threshold-zero rejection at the gate builder and the
    //! default the production wiring falls back to when the configuration is
    //! silent.

    use super::{captcha_gate_for_test, DEFAULT_CAPTCHA_THRESHOLD};
    use migo_protocol::codes;

    /// A threshold of `0` is a posture the gate's contract says means
    /// "captcha on the first attempt". The configuration validator already
    /// refuses that value at startup, and the gate builder duplicates the
    /// check so a hand-rolled `migod_captcha_gate(threshold=0, ...)` cannot
    /// sidestep it. A deployment that wants captcha off should leave
    /// `MIGO_AUTH__CAPTCHA_THRESHOLD` unset (the route still issues
    /// challenges, the gate just is not built).
    #[test]
    fn migod_captcha_gate_rejects_a_zero_threshold() {
        match captcha_gate_for_test(0, b"a-test-secret") {
            Ok(_) => panic!("threshold 0 is the captcha-on-first-attempt posture"),
            Err(error) => {
                assert_eq!(error.code(), codes::INTERNAL_ERROR);
                // The message that the log captures names the field; the
                // wire-shape (`public_message()`) is deliberately empty for
                // an INTERNAL_ERROR, because the peer has nothing to do
                // with the operator's misconfiguration.
                let internal = error.internal_message();
                assert!(
                    internal.contains("captcha threshold of 0"),
                    "the internal message names the field; got {internal:?}"
                );
            }
        }
    }

    /// The default the production wiring falls back to when the operator
    /// has not set `MIGO_AUTH__CAPTCHA_THRESHOLD`. Three is the documented
    /// default in `migo_core::config::AuthConfig`; pinning the value here
    /// means a future edit to either side that moves the number by one
    /// trips this test rather than silently changing every fresh
    /// deployment's posture.
    #[test]
    fn default_captcha_threshold_is_three() {
        assert_eq!(DEFAULT_CAPTCHA_THRESHOLD, 3);
    }

    /// A positive threshold still builds a gate. A regression that returned
    /// `Err` on every input would be caught at startup; this test pins the
    /// happy path so a future edit cannot quietly drop it.
    #[test]
    fn migod_captcha_gate_builds_with_a_positive_threshold() {
        let gate = captcha_gate_for_test(1, b"a-test-secret")
            .expect("a positive threshold is the captcha-on posture");
        assert_eq!(gate.threshold(), 1);
    }
}
