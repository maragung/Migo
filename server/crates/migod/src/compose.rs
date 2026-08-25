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

use migo_auth::SharedAuth;
use migo_bots::SharedBots;
use migo_core::config::Environment;
use migo_core::metrics::Registry;
use migo_core::{Clock, Config, OsRandom, Random, Shutdown, SystemClock};
use migo_economy::{Catalogue, SharedAnnouncer, SharedTreasurer, Silent};
use migo_games::{SharedReferee, SharedRewards};
use migo_gateway::{Dispatcher, Gateway, GatewayServices};
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
        let auth = migo_auth::open(store.clone(), limiter.clone(), config, &registry)
            .context("cannot open authentication")?;

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

        // --- Layer 4: transports ---
        // The dispatcher is the one seam the gateway calls up through; it routes the client-facing
        // opcodes this node speaks into messaging, presence, and rooms.
        let dispatcher: Arc<dyn Dispatcher> = Arc::new(AppDispatcher::new(
            messaging.clone(),
            presence.clone(),
            rooms.clone(),
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
        let api_router = migo_api::router(
            config,
            migo_api::ApiServices {
                authenticator: auth.clone(),
                rate_limiter: limiter,
                clock: clock.clone(),
                registry: Arc::new(registry),
                node,
                features: FEATURES,
            },
        );

        Ok(Self {
            gateway: Arc::new(gateway),
            api_router,
            clock,
            shutdown,
            bind: config.http.bind.clone(),
            auth,
            messaging,
            presence,
            rooms,
            social,
            media,
            moderation,
            notify,
            economy,
            games,
            bots,
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
