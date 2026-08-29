//! The bot service, implemented: thin over the store, and the one place a bot's account,
//! profile, and row are created together.
//!
//! # Why registration is one store write
//!
//! A bot needs three rows to exist — the backing account, its profile, and the `bot` row —
//! and none of the three is usable without the others. An account with no bot row is an
//! account whose password is a hash of bytes nobody kept: it can never be signed into and
//! nothing knows how to speak for it. So [`Bots::register`] hands all three to
//! [`migo_store::traits::BotStore::register_bot`], which writes them in one transaction.
//! There is no valid intermediate state for this service to leave behind on a crash.
//!
//! # The locked password hash is built once
//!
//! Every bot account carries the same unusable-but-valid Argon2id hash, computed once in
//! [`BotService::new`] from random bytes that are then discarded. Argon2id costs tens of
//! milliseconds and megabytes of memory; running it per registration would make the register
//! endpoint a memory-amplification lever for an attacker who can script it. Computing it once
//! and cloning the result is safe because the hash is not a secret — it guards nothing a
//! password could unlock — only a value that must be present and must never verify. This is
//! the same choice, for the same reason, that `migo-auth` makes for its absent-account hash.
//!
//! # Ownership and the existence oracle
//!
//! Every management method resolves the bot through `BotService::owned`, which returns
//! [`fault::not_found`] both when the bot does not exist and when it belongs to someone else.
//! The two are indistinguishable on purpose (brief section 48): a distinguishable "not yours"
//! would confirm that a bot id an owner guessed is real.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use parking_lot::Mutex;

use migo_core::metrics::Registry;
use migo_core::{Error, Id, OsRandom, Random, Result, Secret, Timestamp};
use migo_protocol::{codes, fault};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter};
use migo_store::model::{Bot, NewBot};
use migo_store::{SharedStore, Store};

use crate::metrics::{AuthReject, Meters};
use crate::model::{
    BotIdentity, BotView, BotsConfig, Caller, NewBotSpec, Registered, Scopes,
    MAX_DISPLAY_NAME_CHARS, MAX_WEBHOOK_URL_BYTES,
};
use crate::token::Minter;
use crate::traits::{Bots, SharedBots, SharedWebhook};

/// What registering a bot costs the owner's rate-limit budget. The dearest action here: it
/// writes three rows and is the point a flood of throwaway accounts would be created.
const REGISTER_COST: u32 = 20;
/// What rotating a token costs. A write, priced above a read because it invalidates a
/// credential.
const ROTATE_COST: u32 = 10;
/// What changing a bot's scopes costs.
const SET_SCOPES_COST: u32 = 5;
/// What pausing or resuming a bot costs.
const PAUSE_COST: u32 = 5;
/// What listing an owner's bots costs.
const LIST_COST: u32 = 3;
/// What reading one bot costs.
const GET_COST: u32 = 3;
/// What commanding a bot costs. The §145 price of `BOT_COMMAND`: cheap enough that a
/// workflow asks its bot things all day, dear enough that a script pays per ask.
const COMMAND_COST: u32 = 2;

/// The bot service.
///
/// Generic over its collaborators so a test can supply an in-memory store and a permissive
/// limiter, while production erases them to trait objects. The defaults are those trait
/// objects, so the ordinary spelling is simply `BotService`.
pub struct BotService<S: ?Sized = dyn Store, L: ?Sized = dyn RateLimiter> {
    store: Arc<S>,
    limiter: Arc<L>,
    config: BotsConfig,
    minter: Minter,
    /// The unusable-but-valid hash every bot account is stamped with, built once.
    locked_hash: Secret,
    /// The server's randomness, behind a lock because [`Random`] takes `&mut self` and the
    /// service is shared. Drawn from for account and bot ids and for minting tokens.
    random: Mutex<Box<dyn Random>>,
    meters: Meters,
    /// The transport that carries a command to a bot's webhook. Supply-side, like media's
    /// `Storage`: the crate owns the policy, the composition root owns the client.
    sink: SharedWebhook,
}

impl<S, L> BotService<S, L>
where
    S: Store + ?Sized,
    L: RateLimiter + ?Sized,
{
    /// Assembles a service, building the locked password hash once from `random`.
    ///
    /// `token_root` is the deployment secret bot tokens are keyed under; `random` is injected
    /// rather than fixed so a simulation can replay a run byte for byte.
    ///
    /// # Errors
    ///
    /// Only if the one-time locked-hash computation fails — an Argon2id parameter fault, a
    /// misconfiguration rather than a per-request condition.
    pub fn new(
        store: Arc<S>,
        limiter: Arc<L>,
        config: BotsConfig,
        token_root: &[u8],
        mut random: Box<dyn Random>,
        registry: &Registry,
        sink: SharedWebhook,
    ) -> Result<Self> {
        let minter = Minter::new(token_root);
        let locked_hash = build_locked_hash(&mut *random)?;
        Ok(Self {
            store,
            limiter,
            config,
            minter,
            locked_hash,
            random: Mutex::new(random),
            meters: Meters::new(registry),
            sink,
        })
    }

    /// A fresh, time-ordered id, drawn under the randomness lock.
    fn new_id(&self, at: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(at, &mut **random)
    }

    /// A freshly minted token and the tag to store for it, drawn under the randomness lock.
    fn mint_token(&self) -> (Secret, Vec<u8>) {
        let mut random = self.random.lock();
        let (token, tag) = self.minter.mint(&mut **random);
        (token, tag.to_vec())
    }

    /// Charges the owner `cost` against their account budget, or returns a rate-limit error.
    ///
    /// An unidentified caller — one whose account or device id is nil — is refused before the
    /// limiter is touched. The charge is keyed on the caller's account id, so metering an
    /// unidentified request first is charging *some* account for a request that could not
    /// prove it is that account: an attacker who names a stranger's account id drains the
    /// stranger's budget, and a request that will be rejected anyway costs its own sender
    /// nothing. Identity is a precondition of every management method, and every one funnels
    /// through here, so proving it once at the charge is what makes it impossible to skip.
    async fn charge(&self, caller: &Caller, cost: u32) -> Result<()> {
        if caller.account_id.is_nil() || caller.device_id.is_nil() {
            return Err(fault::unauthenticated(
                "a bot management request needs an identified account and device",
            ));
        }
        self.limiter
            .charge(
                &[BucketKey::account_write(caller.account_id)],
                cost,
                caller.tier,
                caller.now,
            )
            .await?
            .into_result()
    }

    /// Resolves a bot the caller owns, hiding its existence otherwise (section 48).
    async fn owned(&self, owner: &Caller, bot_id: Id) -> Result<Bot> {
        match self.store.bot(bot_id).await? {
            Some(bot) if bot.owner_id == owner.account_id => Ok(bot),
            // Missing, or someone else's — the same answer either way, so an owner cannot
            // probe for bot ids that are not theirs.
            _ => Err(fault::not_found("bot")),
        }
    }
}

/// The opaque error a failed authentication returns.
///
/// One helper, called from both the unknown-token and disabled-bot paths, so the two are
/// byte-for-byte identical and no oracle distinguishes them (section 161).
fn token_invalid() -> Error {
    fault::error(codes::TOKEN_INVALID, "bot token is not recognised")
}

/// A valid Argon2id hash whose preimage was discarded, so no password verifies against it.
///
/// Thirty-two random bytes are drawn, base64url-encoded into a password string, hashed, and
/// the preimage dropped.
fn build_locked_hash(random: &mut dyn Random) -> Result<Secret> {
    let mut bytes = [0u8; 32];
    random.fill_bytes(&mut bytes);
    let password = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    migo_crypto::password::hash(&password, random)
        .map_err(|error| fault::internal(format!("could not build the bot password hash: {error}")))
}

/// Maps a stored [`Bot`] to the owner-facing [`BotView`], decoding its scopes.
fn view_of(bot: &Bot) -> BotView {
    BotView {
        bot_id: bot.bot_id,
        owner_id: bot.owner_id,
        account_id: bot.account_id,
        name: bot.name.clone(),
        scopes: Scopes::from_i64(bot.scopes),
        webhook_url: bot.webhook_url.clone(),
        created_at: bot.created_at,
        disabled_at: bot.disabled_at,
        disabled: bot.disabled_at.is_some(),
    }
}

/// Validates and trims a bot's display name.
fn validate_display_name(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(fault::field_required("display_name"));
    }
    if trimmed.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err(fault::field_too_long(
            "display_name",
            MAX_DISPLAY_NAME_CHARS,
        ));
    }
    Ok(trimmed.to_string())
}

/// Validates an optional webhook URL. An empty or whitespace value is treated as "no
/// webhook", not an error; a non-empty one must be `https` and within the length cap.
fn validate_webhook(raw: Option<String>) -> Result<Option<String>> {
    let Some(url) = raw else { return Ok(None) };
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > MAX_WEBHOOK_URL_BYTES {
        return Err(fault::field_too_long("webhook_url", MAX_WEBHOOK_URL_BYTES));
    }
    if !trimmed.starts_with("https://") {
        return Err(fault::validation("webhook_url", "must be an https URL"));
    }
    Ok(Some(trimmed.to_string()))
}

#[async_trait]
impl<S, L> Bots for BotService<S, L>
where
    S: Store + ?Sized,
    L: RateLimiter + ?Sized,
{
    async fn register(&self, owner: &Caller, spec: NewBotSpec) -> Result<Registered> {
        self.charge(owner, REGISTER_COST).await?;

        // Cap the owner's bot count. A soft limit: it races a concurrent registration, and a
        // small overshoot is harmless. The account and token uniqueness that must hold is the
        // store's atomic guarantee, not this count.
        let existing = self
            .store
            .bots_for_owner(owner.account_id, self.config.max_bots_per_owner)
            .await?;
        if existing.len() >= usize::from(self.config.max_bots_per_owner) {
            return Err(fault::error(
                codes::QUOTA_EXCEEDED,
                "bot limit reached for this owner",
            ));
        }

        // Validate every field the way a human registration would, before a row is written.
        let username = migo_auth::credential::username(&spec.username)?;
        let display_name = validate_display_name(&spec.display_name)?;
        let webhook_url = validate_webhook(spec.webhook_url)?;
        let locale = spec
            .locale
            .map(|locale| locale.trim().to_string())
            .filter(|locale| !locale.is_empty())
            .unwrap_or_else(|| self.config.default_locale.clone());

        let (token, token_hash) = self.mint_token();
        let account_id = self.new_id(owner.now);
        let bot_id = self.new_id(owner.now);

        let bot = self
            .store
            .register_bot(NewBot {
                bot_id,
                owner_id: owner.account_id,
                account_id,
                username: username.display().to_string(),
                display_name,
                password_hash: self.locked_hash.clone(),
                token_hash,
                scopes: spec.scopes.to_i64(),
                webhook_url,
                locale,
                created_at: owner.now,
            })
            .await
            .map_err(|error| {
                // The realistic collision is the username; the store reports it as
                // ALREADY_EXISTS, and the client-facing story is that the handle is taken.
                if error.code() == codes::ALREADY_EXISTS {
                    fault::error(codes::USERNAME_TAKEN, "that username is taken")
                } else {
                    error
                }
            })?;

        self.meters.registered();
        Ok(Registered {
            bot: view_of(&bot),
            token,
        })
    }

    async fn authenticate(&self, token: &str) -> Result<BotIdentity> {
        let tag = self.minter.tag_of(token);
        let Some(bot) = self.store.bot_by_token_hash(&tag).await? else {
            self.meters.auth_rejected(AuthReject::Unknown);
            return Err(token_invalid());
        };
        if bot.disabled_at.is_some() {
            // The same error as an unknown token: the caller never learns whether the token
            // was once valid.
            self.meters.auth_rejected(AuthReject::Disabled);
            return Err(token_invalid());
        }
        self.meters.authenticated();
        Ok(BotIdentity {
            bot_id: bot.bot_id,
            account_id: bot.account_id,
            owner_id: bot.owner_id,
            name: bot.name,
            scopes: Scopes::from_i64(bot.scopes),
        })
    }

    async fn rotate_token(&self, owner: &Caller, bot_id: Id) -> Result<Secret> {
        self.charge(owner, ROTATE_COST).await?;
        self.owned(owner, bot_id).await?;
        let (token, token_hash) = self.mint_token();
        self.store
            .set_bot_token_hash(bot_id, token_hash)
            .await?
            .ok_or_else(|| fault::not_found("bot"))?;
        self.meters.token_rotated();
        Ok(token)
    }

    async fn set_scopes(&self, owner: &Caller, bot_id: Id, scopes: Scopes) -> Result<BotView> {
        self.charge(owner, SET_SCOPES_COST).await?;
        self.owned(owner, bot_id).await?;
        let updated = self
            .store
            .set_bot_scopes(bot_id, scopes.to_i64())
            .await?
            .ok_or_else(|| fault::not_found("bot"))?;
        self.meters.scopes_changed();
        Ok(view_of(&updated))
    }

    async fn set_paused(&self, owner: &Caller, bot_id: Id, paused: bool) -> Result<BotView> {
        self.charge(owner, PAUSE_COST).await?;
        self.owned(owner, bot_id).await?;
        let disabled_at = if paused { Some(owner.now) } else { None };
        let updated = self
            .store
            .set_bot_disabled(bot_id, disabled_at)
            .await?
            .ok_or_else(|| fault::not_found("bot"))?;
        if paused {
            self.meters.disabled();
        } else {
            self.meters.enabled();
        }
        Ok(view_of(&updated))
    }

    async fn list(&self, owner: &Caller) -> Result<Vec<BotView>> {
        self.charge(owner, LIST_COST).await?;
        let bots = self
            .store
            .bots_for_owner(owner.account_id, self.config.max_bots_per_owner)
            .await?;
        Ok(bots.iter().map(view_of).collect())
    }

    async fn get(&self, owner: &Caller, bot_id: Id) -> Result<BotView> {
        self.charge(owner, GET_COST).await?;
        let bot = self.owned(owner, bot_id).await?;
        Ok(view_of(&bot))
    }

    async fn command(
        &self,
        caller: &Caller,
        bot_id: Id,
        command: &str,
        args: &[String],
    ) -> Result<()> {
        self.charge(caller, COMMAND_COST).await?;

        // Existence and being enabled are the gate, not ownership: a command is the §41
        // integration surface every user may reach, so a bot someone else owns answers
        // here as itself and stays invisible only when it does not exist at all. A paused
        // bot reads the same as a missing one — its token already fails that way (§161),
        // and a command that quietly queues for a paused bot would surprise the sender.
        let bot = match self.store.bot(bot_id).await? {
            Some(bot) if bot.disabled_at.is_none() => bot,
            _ => return Err(fault::not_found("bot")),
        };

        // The webhook is the one delivery channel this row carries. An empty one means the
        // owner never registered an integration, and refusing tells the commander so
        // instead of swallowing the command into nowhere.
        let Some(url) = bot.webhook_url.as_deref().filter(|url| !url.is_empty()) else {
            return Err(fault::validation(
                "webhook",
                "this bot has no delivery channel registered",
            ));
        };

        self.sink
            .deliver(url, &command_payload(caller, bot_id, command, args)?)
            .await
    }
}

/// The JSON body `POST`-ed to a bot's webhook.
///
/// The webhook is the bot SDK's own contract, so JSON is right here — the no-JSON rule of
/// section 169 governs the mesh, not a bot owner's endpoint. The caller's account id is
/// included so the bot can answer through the same channel it was reached on; args are
/// carried as an array, not spliced into the command, so an argument that looks like
/// another flag stays an argument.
fn command_payload(caller: &Caller, bot_id: Id, command: &str, args: &[String]) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "bot_id": bot_id.to_text(),
        "command": command,
        "args": args,
        "from": caller.account_id.to_text(),
    }))
    .map_err(|error| fault::internal(format!("could not encode the command payload: {error}")))
}

/// Assembles a bot service behind the erased [`Bots`] trait, with the operating-system
/// randomness a production deployment uses.
///
/// `token_root` is the deployment secret bot tokens are keyed under — the same root the other
/// MAC keys descend from, separated by label.
///
/// # Errors
///
/// Only if building the one-time locked password hash fails; see [`BotService::new`].
pub fn open(
    store: SharedStore,
    limiter: SharedRateLimiter,
    config: BotsConfig,
    token_root: &[u8],
    registry: &Registry,
    sink: SharedWebhook,
) -> Result<SharedBots> {
    let service = BotService::new(
        store,
        limiter,
        config,
        token_root,
        Box::new(OsRandom) as Box<dyn Random>,
        registry,
        sink,
    )?;
    Ok(Arc::new(service))
}
