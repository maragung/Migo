//! Bots, tested where a mistake is a security hole rather than a visible bug.
//!
//! The bot subsystem is small, but almost every rule it keeps is one whose failure is
//! silent and expensive. The tests here are organised around those rules:
//!
//! **A token is a secret that is shown exactly once.** What the store keeps is a keyed
//! HMAC tag, never the token; the raw token exists only in the reply to the call that
//! minted it, and it must never surface again — not in a `Debug`, not in a returned view,
//! not in an error, not in a metric. A leak here is a working credential in a log.
//!
//! **Every authentication failure looks the same.** An unknown token, a wrong token, a
//! disabled bot, and a malformed string all fail with one code and one public message,
//! because any difference between them is a valid-token oracle.
//!
//! **A bot has the minimum authority.** It is registered with the scopes it was granted
//! and nothing more, it has no method by which it could widen them, and its authority is
//! read from the store on every call rather than trusted from the caller.
//!
//! **Existence is a secret.** A caller who does not own a bot is told it does not exist,
//! never that it is forbidden, so a guessed id cannot be confirmed.
//!
//! **Identity is proved before anything is spent.** A request that cannot name a real
//! account and device is refused before the rate limiter is touched — a limiter charged
//! first would let an attacker drain a stranger's budget by naming them.
//!
//! The rate limiter is the real one over a real cache, so the arithmetic is part of the
//! test: an Established account's burst is two hundred, and each method has its own price,
//! which is why the budget tests count in twenties, tens, fives, and threes.

use std::sync::Arc;

use async_trait::async_trait;
use migo_protocol::{codes, fault};
use parking_lot::Mutex;
use serde_json::Value;

use migo_bots::model::{
    BotsConfig, Caller, NewBotSpec, Scopes, DEFAULT_MAX_BOTS_PER_OWNER, MAX_DISPLAY_NAME_CHARS,
    MAX_WEBHOOK_URL_BYTES,
};
use migo_bots::service::BotService;
use migo_bots::token::Minter;
use migo_bots::traits::{SharedWebhook, Webhook};
use migo_bots::{Bots, Registered};
use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::random::OsRandom;
use migo_core::{Error, ErrorKind, Id, Result, Timestamp};
use migo_ratelimit::{BucketKey, CacheRateLimiter, Policies, RateLimiter, TrustTier};
use migo_store::model::Bot;
use migo_store::traits::{AccountStore, BotStore};
use migo_store::MemoryStore;

// --- Time and identity helpers.

const SECOND: i64 = 1_000;
const NOW: i64 = 1_700_000_000 * SECOND;

/// The deployment secret bot tokens are keyed under. Any fixed value works for a test, as
/// long as the harness and any `Minter` a test builds by hand agree on it.
const TOKEN_ROOT: &[u8] = b"migo-bots integration-test token root key material";

/// Devices are derived from their owner so a caller helper needs only one number, and so
/// no test accidentally shares one device between two accounts.
const DEVICE_OFFSET: u128 = 1_000_000;

const OWNER: u128 = 1;
const OTHER: u128 = 2;
const VICTIM: u128 = 3;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

fn device_of(account: u128) -> Id {
    id(account + DEVICE_OFFSET)
}

/// An owner at `NOW`, on an Established connection. The bot service always speaks to an
/// owner — a human managing bots — never to a bot itself.
/// A second, unrelated account: the one commanding somebody else's bot.
fn commander() -> Caller {
    Caller {
        account_id: id(OTHER),
        device_id: device_of(OTHER),
        tier: TrustTier::Established,
        now: ts(NOW),
        request_id: None,
    }
}

fn owner(account: u128) -> Caller {
    Caller {
        account_id: id(account),
        device_id: device_of(account),
        tier: TrustTier::Established,
        now: ts(NOW),
        request_id: None,
    }
}

/// A caller with whatever identity a test wants to forge, for the identity-gate tests.
fn caller_with(account_id: Id, device_id: Id) -> Caller {
    Caller {
        account_id,
        device_id,
        tier: TrustTier::Established,
        now: ts(NOW),
        request_id: None,
    }
}

#[track_caller]
fn expect_code<T>(result: Result<T>, code: u32) {
    let error = result.err().expect("this call must be refused");
    assert_eq!(
        error.code(),
        code,
        "expected code {code}, got {} ({})",
        error.code(),
        error.internal_message()
    );
}

#[track_caller]
fn expect_error<T>(result: Result<T>) -> Error {
    result.err().expect("this call must be refused")
}

type TestService = BotService<MemoryStore, CacheRateLimiter<MemoryCache>>;

/// A webhook sink that records what it was handed instead of speaking HTTPS.
///
/// It can be told to fail, so a test can stand in for a bot backend that is unreachable,
/// and it answers asynchronously like the real transport does.
struct RecordingSink {
    deliveries: Mutex<Vec<(String, Vec<u8>)>>,
    broken: Mutex<bool>,
}

impl RecordingSink {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            deliveries: Mutex::new(Vec::new()),
            broken: Mutex::new(false),
        })
    }

    fn deliveries(&self) -> Vec<(String, Vec<u8>)> {
        self.deliveries.lock().clone()
    }

    fn fail_from_now_on(&self) {
        *self.broken.lock() = true;
    }
}

#[async_trait]
impl Webhook for RecordingSink {
    async fn deliver(&self, url: &str, payload: &[u8]) -> Result<()> {
        if *self.broken.lock() {
            return Err(fault::internal("the bot backend is unreachable"));
        }
        self.deliveries
            .lock()
            .push((url.to_string(), payload.to_vec()));
        Ok(())
    }
}

/// Everything a test needs, with the real limiter over a real cache and the real store.
struct Harness {
    service: TestService,
    store: Arc<MemoryStore>,
    limiter: Arc<CacheRateLimiter<MemoryCache>>,
    registry: Registry,
    sink: Arc<RecordingSink>,
}

impl Harness {
    fn new() -> Self {
        Self::configured(BotsConfig::default())
    }

    fn configured(config: BotsConfig) -> Self {
        let settings = Config::default();
        let store = Arc::new(MemoryStore::new());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(
            Arc::new(MemoryCache::new()),
            policies,
            &registry,
        ));
        let sink = RecordingSink::new();
        let service = BotService::new(
            Arc::clone(&store),
            Arc::clone(&limiter),
            config,
            TOKEN_ROOT,
            Box::new(OsRandom),
            &registry,
            {
                // The same coercion quirk as everywhere else in this suite: an explicit
                // local of the trait-object type is where `Arc<RecordingSink>` unsizes.
                let sink: SharedWebhook = sink.clone();
                sink
            },
        )
        .expect("the one-time locked hash builds");
        Self {
            service,
            store,
            limiter,
            registry,
            sink,
        }
    }

    /// A registration spec with no scopes and no webhook — the default, a bot that can do
    /// nothing until its owner grants it something.
    fn spec(username: &str) -> NewBotSpec {
        NewBotSpec {
            username: username.to_string(),
            display_name: format!("{username} display"),
            scopes: Scopes::NONE,
            webhook_url: None,
            locale: None,
        }
    }

    async fn register(&self, owner: &Caller, username: &str) -> Registered {
        self.service
            .register(owner, Self::spec(username))
            .await
            .expect("registration succeeds")
    }

    async fn register_with(&self, owner: &Caller, username: &str, scopes: Scopes) -> Registered {
        let mut spec = Self::spec(username);
        spec.scopes = scopes;
        self.service
            .register(owner, spec)
            .await
            .expect("registration succeeds")
    }

    /// A registration with an explicit spec, for the tests that need a webhook or other
    /// non-default fields.
    async fn register_spec(&self, owner: &Caller, spec: NewBotSpec) -> Registered {
        self.service
            .register(owner, spec)
            .await
            .expect("registration succeeds")
    }

    async fn bot_row(&self, bot_id: Id) -> Bot {
        self.store
            .bot(bot_id)
            .await
            .expect("the store can be read")
            .expect("the bot row exists")
    }

    fn counter(&self, name: &'static str, labels: &[(&str, &str)]) -> u64 {
        self.registry.counter(name, "", labels).get()
    }

    fn plain(&self, name: &'static str) -> u64 {
        self.counter(name, &[])
    }

    fn rejected(&self, reason: &'static str) -> u64 {
        self.counter("migo_bots_auth_rejected_total", &[("reason", reason)])
    }

    /// The account bucket's balance for a caller, in whole tokens.
    ///
    /// The write surface, because that is the one this crate bills: the gateway edge keeps
    /// its own bucket on `BucketKey::account`, so peeking that one would read a balance
    /// nothing here ever spends.
    async fn account_balance(&self, caller: &Caller) -> u32 {
        self.limiter
            .peek(
                &BucketKey::account_write(caller.account_id),
                caller.tier,
                caller.now,
            )
            .await
            .expect("the bucket can be peeked")
    }

    /// Spends the caller's whole remaining account budget, so the next charged call is refused.
    ///
    /// What is left is charged rather than the bucket's capacity: a test that made a charged
    /// call before draining has already spent part of the budget, and asking for the full
    /// capacity would be refused instead of emptying what remains.
    async fn drain_account(&self, caller: &Caller) {
        let balance = self.account_balance(caller).await;
        if balance == 0 {
            return;
        }
        let verdict = self
            .limiter
            .charge(
                &[BucketKey::account_write(caller.account_id)],
                balance,
                caller.tier,
                caller.now,
            )
            .await
            .expect("the drain charge is accepted");
        assert!(
            verdict.is_allowed(),
            "draining exactly the remaining balance must itself be allowed"
        );
        assert_eq!(
            self.account_balance(caller).await,
            0,
            "the drain leaves nothing for the call under test to spend"
        );
    }
}

// ---------------------------------------------------------------------------
// A token is a secret, shown exactly once.
//
// The store keeps a keyed HMAC tag, never the token. The raw token exists only in the reply
// to the call that minted it, and it must not surface again in any debug, view, error, or
// metric. Each test here closes one path by which it could leak.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registration_hands_back_a_non_empty_token() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    assert!(
        !registered.token.expose().is_empty(),
        "the owner must receive a usable token exactly once"
    );
}

#[tokio::test]
async fn two_registrations_mint_distinct_tokens() {
    let harness = Harness::new();
    let first = harness.register(&owner(OWNER), "weatherbot").await;
    let second = harness.register(&owner(OWNER), "chronosbot").await;
    assert_ne!(
        first.token.expose(),
        second.token.expose(),
        "each bot's token is independent randomness, never a shared or derived value"
    );
}

#[tokio::test]
async fn the_tokens_debug_form_redacts_the_secret() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let rendered = format!("{:?}", registered.token);
    assert!(
        !rendered.contains(registered.token.expose()),
        "a token that appears in its own Debug output is a token in every log line: {rendered}"
    );
    assert!(
        rendered.contains("Secret(***"),
        "the redacted form is what should print, got {rendered}"
    );
}

#[tokio::test]
async fn debugging_the_whole_registration_never_prints_the_token() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    // A struct is often logged whole; the Secret field must redact even then.
    let rendered = format!("{registered:?}");
    assert!(
        !rendered.contains(registered.token.expose()),
        "the token leaked through the Registered struct's Debug: {rendered}"
    );
}

#[tokio::test]
async fn the_stored_tag_is_not_the_token() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let row = harness.bot_row(registered.bot.bot_id).await;
    assert_ne!(
        row.token_hash,
        registered.token.expose().as_bytes(),
        "the row must keep a tag, not the token — a database dump must yield no credential"
    );
}

#[tokio::test]
async fn the_stored_tag_is_a_thirty_two_byte_mac() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let row = harness.bot_row(registered.bot.bot_id).await;
    assert_eq!(
        row.token_hash.len(),
        32,
        "the stored tag is an HMAC-SHA-256 tag, fixed at 32 bytes regardless of token length"
    );
}

#[tokio::test]
async fn the_owner_view_carries_no_token() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    // The view is what a management UI renders and logs; the token must not travel in it.
    let rendered = format!("{:?}", registered.bot);
    assert!(
        !rendered.contains(registered.token.expose()),
        "the owner-facing view must not carry the token: {rendered}"
    );
}

#[tokio::test]
async fn a_failed_authentication_never_echoes_the_token() {
    let harness = Harness::new();
    // A wrong token must not come back in the error, or the error log becomes a token log.
    let bogus = "this-is-not-a-real-bot-token-value-at-all";
    let error = expect_error(harness.service.authenticate(bogus).await);
    assert!(
        !error.internal_message().contains(bogus) && !error.public_message().contains(bogus),
        "the presented token must not appear in the rejection: {}",
        error.internal_message()
    );
}

// ---------------------------------------------------------------------------
// The minter: a token is a lookup key with no structure, stored only as a keyed tag.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_same_token_always_tags_to_the_same_value() {
    // Authentication is a tag lookup, so the tag must be a pure function of the token.
    let minter = Minter::new(TOKEN_ROOT);
    let token = "aGVsbG8td29ybGQtdGhpcy1pcy1hLXRlc3QtdG9rZW4";
    assert_eq!(
        minter.tag_of(token),
        minter.tag_of(token),
        "tagging is deterministic or a valid token would stop authenticating at random"
    );
}

#[tokio::test]
async fn different_tokens_tag_to_different_values() {
    let minter = Minter::new(TOKEN_ROOT);
    assert_ne!(
        minter.tag_of("token-one-here"),
        minter.tag_of("token-two-here"),
        "distinct tokens must not collide onto one tag"
    );
}

#[tokio::test]
async fn a_malformed_token_still_produces_a_tag_that_matches_nothing() {
    // tag_of accepts any input rather than refusing early on shape, so a garbage token is a
    // failed lookup, not a distinguishable "wrong shape" answer.
    let minter = Minter::new(TOKEN_ROOT);
    let garbage = "!!! not base64 at all !!!";
    let tag = minter.tag_of(garbage);
    assert_eq!(tag.len(), 32, "even garbage tags to a full-width value");
}

#[tokio::test]
async fn a_freshly_minted_token_authenticates_to_its_bot() {
    let harness = Harness::new();
    let registered = harness
        .register_with(&owner(OWNER), "weatherbot", Scopes::SEND_MESSAGES)
        .await;
    let identity = harness
        .service
        .authenticate(registered.token.expose())
        .await
        .expect("the minted token authenticates");
    assert_eq!(identity.bot_id, registered.bot.bot_id);
    assert_eq!(identity.account_id, registered.bot.account_id);
    assert_eq!(identity.owner_id, id(OWNER));
    assert_eq!(identity.scopes, Scopes::SEND_MESSAGES);
}

#[tokio::test]
async fn rotating_a_token_returns_a_new_distinct_secret() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let rotated = harness
        .service
        .rotate_token(&owner(OWNER), registered.bot.bot_id)
        .await
        .expect("the owner can rotate their bot's token");
    assert_ne!(
        registered.token.expose(),
        rotated.expose(),
        "a rotation that returned the same token would not be a rotation"
    );
}

#[tokio::test]
async fn rotation_invalidates_the_previous_token() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let old = registered.token.expose().to_string();
    harness
        .service
        .rotate_token(&owner(OWNER), registered.bot.bot_id)
        .await
        .expect("rotation succeeds");
    // The whole point of rotation is that a leaked old credential stops working.
    expect_code(
        harness.service.authenticate(&old).await,
        codes::TOKEN_INVALID,
    );
}

#[tokio::test]
async fn only_the_newest_token_authenticates_after_two_rotations() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let first = registered.token.expose().to_string();
    let second = harness
        .service
        .rotate_token(&owner(OWNER), registered.bot.bot_id)
        .await
        .expect("first rotation succeeds")
        .expose()
        .to_string();
    let third = harness
        .service
        .rotate_token(&owner(OWNER), registered.bot.bot_id)
        .await
        .expect("second rotation succeeds")
        .expose()
        .to_string();
    // Three distinct credentials have existed; only the last is live.
    assert_ne!(first, second);
    assert_ne!(second, third);
    assert_ne!(first, third);
    expect_code(
        harness.service.authenticate(&first).await,
        codes::TOKEN_INVALID,
    );
    expect_code(
        harness.service.authenticate(&second).await,
        codes::TOKEN_INVALID,
    );
    let identity = harness
        .service
        .authenticate(&third)
        .await
        .expect("the newest token still authenticates");
    assert_eq!(identity.bot_id, registered.bot.bot_id);
}

// ---------------------------------------------------------------------------
// Every authentication failure looks the same.
//
// Unknown token, wrong token, disabled bot, malformed string: one code, one public message,
// one symbol, one internal message. Any difference is a valid-token oracle (section 161).
// ---------------------------------------------------------------------------

/// Registers a bot and returns a token guaranteed to fail: the real one, then rotated away.
async fn a_revoked_token(harness: &Harness, username: &str) -> String {
    let registered = harness.register(&owner(OWNER), username).await;
    let token = registered.token.expose().to_string();
    harness
        .service
        .rotate_token(&owner(OWNER), registered.bot.bot_id)
        .await
        .expect("rotation succeeds");
    token
}

#[tokio::test]
async fn an_empty_token_is_rejected_as_token_invalid() {
    let harness = Harness::new();
    let error = expect_error(harness.service.authenticate("").await);
    assert_eq!(error.code(), codes::TOKEN_INVALID);
}

#[tokio::test]
async fn unknown_and_disabled_failures_are_byte_for_byte_identical() {
    let harness = Harness::new();
    // A disabled bot whose token is otherwise valid...
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .service
        .set_paused(&owner(OWNER), registered.bot.bot_id, true)
        .await
        .expect("pause succeeds");
    let disabled = expect_error(
        harness
            .service
            .authenticate(registered.token.expose())
            .await,
    );
    // ...and a token that was never issued.
    let unknown = expect_error(harness.service.authenticate("bm90LWEtcmVhbC10b2tlbg").await);
    // If any of these four differed, the difference would tell an attacker which case they hit.
    assert_eq!(disabled.code(), unknown.code(), "codes must match");
    assert_eq!(disabled.symbol(), unknown.symbol(), "symbols must match");
    assert_eq!(
        disabled.public_message(),
        unknown.public_message(),
        "public messages must match"
    );
    assert_eq!(
        disabled.internal_message(),
        unknown.internal_message(),
        "even the internal message is shared, from the one token_invalid() helper"
    );
}

#[tokio::test]
async fn malformed_and_unknown_failures_are_indistinguishable() {
    let harness = Harness::new();
    let malformed = expect_error(harness.service.authenticate("@@@").await);
    let unknown = expect_error(harness.service.authenticate("bm90LWEtcmVhbC10b2tlbg").await);
    assert_eq!(malformed.code(), unknown.code());
    assert_eq!(malformed.public_message(), unknown.public_message());
    assert_eq!(malformed.internal_message(), unknown.internal_message());
}

#[tokio::test]
async fn the_authentication_failure_discloses_nothing_to_the_public() {
    let harness = Harness::new();
    let error = expect_error(harness.service.authenticate("bm90LWEtcmVhbC10b2tlbg").await);
    // An opaque failure carries no public detail at all — there is nothing safe to say.
    assert!(
        error.public_message().is_empty(),
        "a token failure must not disclose a public reason, got {:?}",
        error.public_message()
    );
}

#[tokio::test]
async fn a_revoked_token_fails_exactly_like_an_unknown_one() {
    let harness = Harness::new();
    let revoked = a_revoked_token(&harness, "weatherbot").await;
    let revoked_error = expect_error(harness.service.authenticate(&revoked).await);
    let unknown_error = expect_error(harness.service.authenticate("bm90LWEtcmVhbC10b2tlbg").await);
    assert_eq!(revoked_error.code(), unknown_error.code());
    assert_eq!(
        revoked_error.internal_message(),
        unknown_error.internal_message()
    );
}

#[tokio::test]
async fn re_enabling_a_bot_restores_its_token() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .service
        .set_paused(&owner(OWNER), registered.bot.bot_id, true)
        .await
        .expect("pause succeeds");
    harness
        .service
        .set_paused(&owner(OWNER), registered.bot.bot_id, false)
        .await
        .expect("resume succeeds");
    // Disabling is reversible: the same token works again once the bot is re-enabled.
    let identity = harness
        .service
        .authenticate(registered.token.expose())
        .await
        .expect("the token authenticates again after re-enabling");
    assert_eq!(identity.bot_id, registered.bot.bot_id);
}

// ---------------------------------------------------------------------------
// Minimum authority by default, and authority read from the store on every call.
//
// A bot is born with no permissions (section 41). Its scopes change only through the owner's
// set_scopes, and what it is allowed to do is whatever the store row says right now — never
// what a caller asserted.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_bot_is_registered_with_no_scopes_by_default() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    assert_eq!(
        registered.bot.scopes,
        Scopes::NONE,
        "the section 41 default is the empty set — a bot can do nothing until granted"
    );
}

#[tokio::test]
async fn a_default_bots_token_grants_no_capability() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let identity = harness
        .service
        .authenticate(registered.token.expose())
        .await
        .expect("authentication succeeds");
    assert!(identity.scopes.is_empty(), "no scope is set");
    for (scope, slug) in Scopes::NAMED {
        assert!(!identity.may(scope), "a fresh bot must not hold {slug}");
    }
}

#[tokio::test]
async fn an_owner_may_grant_scopes_at_registration() {
    let harness = Harness::new();
    let granted = Scopes::READ_MESSAGES.with(Scopes::SEND_MESSAGES);
    let registered = harness
        .register_with(&owner(OWNER), "weatherbot", granted)
        .await;
    assert_eq!(
        registered.bot.scopes, granted,
        "an explicit grant at creation is honoured exactly, no more and no less"
    );
}

#[tokio::test]
async fn set_scopes_can_widen_a_bots_permissions() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let view = harness
        .service
        .set_scopes(&owner(OWNER), registered.bot.bot_id, Scopes::MODERATE)
        .await
        .expect("the owner can change scopes");
    assert_eq!(view.scopes, Scopes::MODERATE);
}

#[tokio::test]
async fn authentication_reflects_a_scope_change_made_after_the_token_was_issued() {
    let harness = Harness::new();
    // The token is minted once; its authority is not baked in. Widen the bot, and the same
    // token now authenticates with the wider set — proof the scopes come from the store.
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .service
        .set_scopes(&owner(OWNER), registered.bot.bot_id, Scopes::MANAGE_GAMES)
        .await
        .expect("scope change succeeds");
    let identity = harness
        .service
        .authenticate(registered.token.expose())
        .await
        .expect("authentication succeeds");
    assert!(
        identity.may(Scopes::MANAGE_GAMES),
        "the token issued before the grant now carries it, because scopes are read live"
    );
}

#[tokio::test]
async fn authentication_reflects_a_scope_revocation() {
    let harness = Harness::new();
    let registered = harness
        .register_with(&owner(OWNER), "weatherbot", Scopes::ALL)
        .await;
    harness
        .service
        .set_scopes(&owner(OWNER), registered.bot.bot_id, Scopes::READ_MESSAGES)
        .await
        .expect("scope change succeeds");
    let identity = harness
        .service
        .authenticate(registered.token.expose())
        .await
        .expect("authentication succeeds");
    assert!(
        identity.may(Scopes::READ_MESSAGES),
        "the surviving scope is present"
    );
    assert!(
        !identity.may(Scopes::MODERATE),
        "a revoked scope is gone the instant the row changes, not at the next token issue"
    );
}

#[tokio::test]
async fn authority_comes_from_the_row_not_from_a_stale_view() {
    let harness = Harness::new();
    // Mutate the store directly, behind the service's back. The next read through the service
    // reflects it — the service holds no authority of its own, only what the Store trait says.
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .store
        .set_bot_scopes(registered.bot.bot_id, Scopes::SEND_ANNOUNCEMENTS.to_i64())
        .await
        .expect("the store accepts the write")
        .expect("the bot row exists");
    let identity = harness
        .service
        .authenticate(registered.token.expose())
        .await
        .expect("authentication succeeds");
    assert!(
        identity.may(Scopes::SEND_ANNOUNCEMENTS),
        "the service is a thin reader over the store, so a direct row change is authoritative"
    );
}

#[tokio::test]
async fn undefined_high_bits_in_the_row_are_dropped_not_honoured() {
    let harness = Harness::new();
    // A hostile or corrupt row with every bit set must not grant a capability this build has
    // never heard of — only the defined bits survive, the rest are masked away.
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .store
        .set_bot_scopes(registered.bot.bot_id, i64::from(u32::MAX))
        .await
        .expect("the store accepts the write")
        .expect("the bot row exists");
    let identity = harness
        .service
        .authenticate(registered.token.expose())
        .await
        .expect("authentication succeeds");
    assert_eq!(
        identity.scopes,
        Scopes::ALL,
        "the undefined high bits are dropped; only the six defined scopes remain"
    );
}

#[tokio::test]
async fn a_negative_scope_value_in_the_row_decodes_to_no_permission() {
    let harness = Harness::new();
    // A negative integer is a corrupt or hostile row; it decodes to the empty set — the safe
    // direction, granting nothing — rather than failing every authentication that reads it.
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .store
        .set_bot_scopes(registered.bot.bot_id, -1)
        .await
        .expect("the store accepts the write")
        .expect("the bot row exists");
    let identity = harness
        .service
        .authenticate(registered.token.expose())
        .await
        .expect("authentication succeeds");
    assert!(
        identity.scopes.is_empty(),
        "a nonsensical row grants nothing, so one bad value cannot become an escalation"
    );
}

// ---------------------------------------------------------------------------
// Existence is a secret.
//
// A caller who does not own a bot is told it does not exist — never that it is forbidden —
// so a guessed id cannot be confirmed (section 48). In this crate ownership is binary: you
// own the bot or, for every purpose, it is not there. There is no authorized-but-forbidden
// state, so nothing here ever returns PERMISSION_DENIED.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn getting_a_bot_that_does_not_exist_is_not_found() {
    let harness = Harness::new();
    expect_code(
        harness.service.get(&owner(OWNER), id(999_999)).await,
        codes::NOT_FOUND,
    );
}

#[tokio::test]
async fn getting_another_owners_bot_is_not_found_not_forbidden() {
    let harness = Harness::new();
    let registered = harness.register(&commander(), "weatherbot").await;
    // OWNER may not see OTHER's bot. The answer is "no such bot", not "not yours".
    let error = expect_error(
        harness
            .service
            .get(&owner(OWNER), registered.bot.bot_id)
            .await,
    );
    assert_eq!(
        error.code(),
        codes::NOT_FOUND,
        "a distinguishable 'forbidden' would confirm the id is real"
    );
    assert_ne!(
        error.code(),
        codes::PERMISSION_DENIED,
        "ownership here is binary; there is no authorized-but-forbidden answer to leak"
    );
}

#[tokio::test]
async fn a_missing_bot_and_someone_elses_bot_are_the_same_error() {
    let harness = Harness::new();
    let registered = harness.register(&commander(), "weatherbot").await;
    let missing = expect_error(harness.service.get(&owner(OWNER), id(999_999)).await);
    let not_mine = expect_error(
        harness
            .service
            .get(&owner(OWNER), registered.bot.bot_id)
            .await,
    );
    // If these differed, the difference would separate "real id you can't see" from "no such
    // id" — exactly the oracle section 48 forbids.
    assert_eq!(missing.code(), not_mine.code());
    assert_eq!(missing.internal_message(), not_mine.internal_message());
    assert_eq!(missing.public_message(), not_mine.public_message());
}

#[tokio::test]
async fn rotating_another_owners_bot_is_not_found() {
    let harness = Harness::new();
    let registered = harness.register(&commander(), "weatherbot").await;
    expect_code(
        harness
            .service
            .rotate_token(&owner(OWNER), registered.bot.bot_id)
            .await,
        codes::NOT_FOUND,
    );
}

#[tokio::test]
async fn setting_scopes_on_another_owners_bot_is_not_found() {
    let harness = Harness::new();
    let registered = harness.register(&commander(), "weatherbot").await;
    expect_code(
        harness
            .service
            .set_scopes(&owner(OWNER), registered.bot.bot_id, Scopes::ALL)
            .await,
        codes::NOT_FOUND,
    );
}

#[tokio::test]
async fn pausing_another_owners_bot_is_not_found() {
    let harness = Harness::new();
    let registered = harness.register(&commander(), "weatherbot").await;
    expect_code(
        harness
            .service
            .set_paused(&owner(OWNER), registered.bot.bot_id, true)
            .await,
        codes::NOT_FOUND,
    );
}

#[tokio::test]
async fn a_foreign_bot_is_untouched_by_a_rejected_management_call() {
    let harness = Harness::new();
    // A denied call must have no effect on the bot it could not reach.
    let registered = harness
        .register_with(&commander(), "weatherbot", Scopes::NONE)
        .await;
    let _ = harness
        .service
        .set_scopes(&owner(OWNER), registered.bot.bot_id, Scopes::ALL)
        .await;
    let row = harness.bot_row(registered.bot.bot_id).await;
    assert_eq!(
        Scopes::from_i64(row.scopes),
        Scopes::NONE,
        "the foreign owner's failed set_scopes must not have altered the row"
    );
}

#[tokio::test]
async fn list_returns_only_the_callers_own_bots() {
    let harness = Harness::new();
    harness.register(&owner(OWNER), "weatherbot").await;
    harness.register(&owner(OWNER), "chronosbot").await;
    harness.register(&commander(), "greeter").await;
    let mine = harness
        .service
        .list(&owner(OWNER))
        .await
        .expect("list succeeds");
    assert_eq!(mine.len(), 2, "an owner sees their own bots and no others");
    assert!(
        mine.iter().all(|view| view.owner_id == id(OWNER)),
        "no bot belonging to another owner may appear in the list"
    );
}

// ---------------------------------------------------------------------------
// Identity is proved before anything is spent.
//
// The rate-limit charge is keyed on the caller's account id. If an unidentified request were
// metered first, a request naming a stranger's account would drain the stranger's budget, and
// a request bound to be rejected would still cost its sender nothing. So a caller whose
// account or device id is nil is refused — UNAUTHENTICATED — before the limiter is touched.
// Every management method funnels through the one charge, so proving it once proves it for all.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_caller_with_a_nil_account_is_refused_as_unauthenticated() {
    let harness = Harness::new();
    let caller = caller_with(Id::NIL, device_of(OWNER));
    expect_code(
        harness
            .service
            .register(&caller, Harness::spec("weatherbot"))
            .await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn a_caller_with_a_nil_device_is_refused_as_unauthenticated() {
    let harness = Harness::new();
    let caller = caller_with(id(OWNER), Id::NIL);
    expect_code(
        harness
            .service
            .register(&caller, Harness::spec("weatherbot"))
            .await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn the_unidentified_refusal_is_an_auth_error() {
    let harness = Harness::new();
    let caller = caller_with(Id::NIL, Id::NIL);
    let error = expect_error(
        harness
            .service
            .register(&caller, Harness::spec("weatherbot"))
            .await,
    );
    assert_eq!(
        error.kind(),
        ErrorKind::Auth,
        "an unidentified caller is an auth failure"
    );
}

#[tokio::test]
async fn an_unidentified_request_does_not_drain_the_named_accounts_budget() {
    // The defect this pins: charge() is keyed on caller.account_id, so a request that names a
    // real account it cannot prove it owns — here VICTIM, with no valid device — must not be
    // metered. If the identity gate ran after the charge, VICTIM's bucket would be debited by
    // an attacker who never authenticated as VICTIM.
    let harness = Harness::new();
    let victim = owner(VICTIM);
    let before = harness.account_balance(&victim).await;

    let attacker = caller_with(id(VICTIM), Id::NIL);
    expect_code(
        harness
            .service
            .register(&attacker, Harness::spec("weatherbot"))
            .await,
        codes::UNAUTHENTICATED,
    );

    let after = harness.account_balance(&victim).await;
    assert_eq!(
        before, after,
        "the victim's budget must be untouched — the charge must never run for an \
         unidentified caller (server/crates/migo-bots/src/service.rs, charge())"
    );
}

#[tokio::test]
async fn rotate_refuses_an_unidentified_caller() {
    let harness = Harness::new();
    let caller = caller_with(Id::NIL, Id::NIL);
    expect_code(
        harness.service.rotate_token(&caller, id(42)).await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn set_scopes_refuses_an_unidentified_caller() {
    let harness = Harness::new();
    let caller = caller_with(Id::NIL, Id::NIL);
    expect_code(
        harness
            .service
            .set_scopes(&caller, id(42), Scopes::ALL)
            .await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn set_paused_refuses_an_unidentified_caller() {
    let harness = Harness::new();
    let caller = caller_with(Id::NIL, Id::NIL);
    expect_code(
        harness.service.set_paused(&caller, id(42), true).await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn list_refuses_an_unidentified_caller() {
    let harness = Harness::new();
    let caller = caller_with(Id::NIL, Id::NIL);
    expect_code(harness.service.list(&caller).await, codes::UNAUTHENTICATED);
}

#[tokio::test]
async fn get_refuses_an_unidentified_caller() {
    let harness = Harness::new();
    let caller = caller_with(Id::NIL, Id::NIL);
    expect_code(
        harness.service.get(&caller, id(42)).await,
        codes::UNAUTHENTICATED,
    );
}

#[tokio::test]
async fn an_unidentified_caller_is_refused_before_the_bot_is_even_looked_up() {
    let harness = Harness::new();
    // The bot exists and belongs to OWNER, but the caller cannot identify itself. The answer
    // is UNAUTHENTICATED, not NOT_FOUND: the identity gate is upstream of the ownership lookup.
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let caller = caller_with(Id::NIL, Id::NIL);
    expect_code(
        harness.service.get(&caller, registered.bot.bot_id).await,
        codes::UNAUTHENTICATED,
    );
}

// ---------------------------------------------------------------------------
// Each method charges its own price, and an exhausted budget refuses the next call.
//
// The account bucket is measured by peeking before and after a single call; since every call
// here shares one `now`, no refill happens between the two peeks, so the difference is exactly
// what that method cost. The prices are REGISTER 20, ROTATE 10, SET_SCOPES 5, PAUSE 5, LIST 3,
// GET 3.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_costs_twenty_from_the_account_budget() {
    let harness = Harness::new();
    let caller = owner(OWNER);
    let before = harness.account_balance(&caller).await;
    harness.register(&caller, "weatherbot").await;
    let after = harness.account_balance(&caller).await;
    assert_eq!(
        before - after,
        20,
        "registration is the dearest action, priced at 20"
    );
}

#[tokio::test]
async fn rotate_costs_ten_from_the_account_budget() {
    let harness = Harness::new();
    let caller = owner(OWNER);
    let registered = harness.register(&caller, "weatherbot").await;
    let before = harness.account_balance(&caller).await;
    harness
        .service
        .rotate_token(&caller, registered.bot.bot_id)
        .await
        .expect("rotation succeeds");
    let after = harness.account_balance(&caller).await;
    assert_eq!(before - after, 10, "a token rotation is priced at 10");
}

#[tokio::test]
async fn set_scopes_costs_five_from_the_account_budget() {
    let harness = Harness::new();
    let caller = owner(OWNER);
    let registered = harness.register(&caller, "weatherbot").await;
    let before = harness.account_balance(&caller).await;
    harness
        .service
        .set_scopes(&caller, registered.bot.bot_id, Scopes::MODERATE)
        .await
        .expect("scope change succeeds");
    let after = harness.account_balance(&caller).await;
    assert_eq!(before - after, 5, "a scope change is priced at 5");
}

#[tokio::test]
async fn pausing_costs_five_from_the_account_budget() {
    let harness = Harness::new();
    let caller = owner(OWNER);
    let registered = harness.register(&caller, "weatherbot").await;
    let before = harness.account_balance(&caller).await;
    harness
        .service
        .set_paused(&caller, registered.bot.bot_id, true)
        .await
        .expect("pause succeeds");
    let after = harness.account_balance(&caller).await;
    assert_eq!(before - after, 5, "pausing is priced at 5");
}

#[tokio::test]
async fn listing_costs_three_from_the_account_budget() {
    let harness = Harness::new();
    let caller = owner(OWNER);
    let before = harness.account_balance(&caller).await;
    harness.service.list(&caller).await.expect("list succeeds");
    let after = harness.account_balance(&caller).await;
    assert_eq!(before - after, 3, "listing is a cheap read, priced at 3");
}

#[tokio::test]
async fn getting_costs_three_from_the_account_budget() {
    let harness = Harness::new();
    let caller = owner(OWNER);
    let registered = harness.register(&caller, "weatherbot").await;
    let before = harness.account_balance(&caller).await;
    harness
        .service
        .get(&caller, registered.bot.bot_id)
        .await
        .expect("get succeeds");
    let after = harness.account_balance(&caller).await;
    assert_eq!(before - after, 3, "reading one bot is priced at 3");
}

#[tokio::test]
async fn authentication_is_never_charged_to_an_account_budget() {
    let harness = Harness::new();
    // Authentication is the bot connecting, not an owner spending; it takes no Caller and must
    // not touch the owner's account bucket at all.
    let caller = owner(OWNER);
    let registered = harness.register(&caller, "weatherbot").await;
    let before = harness.account_balance(&caller).await;
    harness
        .service
        .authenticate(registered.token.expose())
        .await
        .expect("authentication succeeds");
    let after = harness.account_balance(&caller).await;
    assert_eq!(
        before, after,
        "authenticating a bot does not spend the owner's budget"
    );
}

#[tokio::test]
async fn a_rate_limited_error_is_of_the_rate_limit_kind() {
    let harness = Harness::new();
    let caller = owner(OWNER);
    harness.drain_account(&caller).await;
    let error = expect_error(harness.service.list(&caller).await);
    assert_eq!(
        error.kind(),
        ErrorKind::RateLimit,
        "the retry class must be RateLimit"
    );
    assert_eq!(error.code(), codes::RATE_LIMITED);
}

#[tokio::test]
async fn every_method_is_refused_once_the_budget_is_gone() {
    let harness = Harness::new();
    let caller = owner(OWNER);
    // Register one bot while there is budget, so a valid id exists for the read/write methods.
    let registered = harness.register(&caller, "weatherbot").await;
    harness.drain_account(&caller).await;
    expect_code(
        harness
            .service
            .register(&caller, Harness::spec("chronosbot"))
            .await,
        codes::RATE_LIMITED,
    );
    expect_code(
        harness
            .service
            .rotate_token(&caller, registered.bot.bot_id)
            .await,
        codes::RATE_LIMITED,
    );
    expect_code(
        harness
            .service
            .set_scopes(&caller, registered.bot.bot_id, Scopes::ALL)
            .await,
        codes::RATE_LIMITED,
    );
    expect_code(
        harness
            .service
            .set_paused(&caller, registered.bot.bot_id, true)
            .await,
        codes::RATE_LIMITED,
    );
    expect_code(harness.service.list(&caller).await, codes::RATE_LIMITED);
    expect_code(
        harness.service.get(&caller, registered.bot.bot_id).await,
        codes::RATE_LIMITED,
    );
}

#[tokio::test]
async fn the_charge_runs_before_the_ownership_lookup() {
    let harness = Harness::new();
    let caller = owner(OWNER);
    harness.drain_account(&caller).await;
    // The bot id does not exist. With budget, this would be NOT_FOUND; drained, it is
    // RATE_LIMITED — proof the charge is upstream of the existence check, so a flood of reads
    // for bogus ids is still metered rather than being a free existence probe.
    expect_code(
        harness.service.get(&caller, id(999_999)).await,
        codes::RATE_LIMITED,
    );
}

// ---------------------------------------------------------------------------
// Every documented limit is enforced at its boundary: the value at the limit is accepted,
// one past it is refused with a validation-class error (or a quota error, for the bot count).
// ---------------------------------------------------------------------------

/// A registration spec with a chosen display name and a valid username.
fn spec_named(username: &str, display_name: String) -> NewBotSpec {
    NewBotSpec {
        username: username.to_string(),
        display_name,
        scopes: Scopes::NONE,
        webhook_url: None,
        locale: None,
    }
}

#[tokio::test]
async fn a_display_name_at_the_length_limit_is_accepted() {
    let harness = Harness::new();
    let name = "a".repeat(MAX_DISPLAY_NAME_CHARS);
    let registered = harness
        .service
        .register(&owner(OWNER), spec_named("weatherbot", name.clone()))
        .await
        .expect("a display name exactly at the limit is valid");
    assert_eq!(registered.bot.name, name);
}

#[tokio::test]
async fn a_display_name_one_past_the_limit_is_rejected() {
    let harness = Harness::new();
    let name = "a".repeat(MAX_DISPLAY_NAME_CHARS + 1);
    let error = expect_error(
        harness
            .service
            .register(&owner(OWNER), spec_named("weatherbot", name))
            .await,
    );
    assert_eq!(error.code(), codes::FIELD_TOO_LONG);
    assert_eq!(
        error.kind(),
        ErrorKind::Validation,
        "one past a limit is a validation error"
    );
}

#[tokio::test]
async fn the_display_name_limit_counts_characters_not_bytes() {
    let harness = Harness::new();
    // Forty-eight two-byte characters is ninety-six bytes but forty-eight characters, and the
    // limit is in characters, so it must be accepted — a byte-based check would wrongly reject.
    let name = "é".repeat(MAX_DISPLAY_NAME_CHARS);
    harness
        .service
        .register(&owner(OWNER), spec_named("weatherbot", name))
        .await
        .expect("a multi-byte name at the character limit is valid");
}

#[tokio::test]
async fn an_empty_display_name_is_rejected_as_required() {
    let harness = Harness::new();
    expect_code(
        harness
            .service
            .register(&owner(OWNER), spec_named("weatherbot", String::new()))
            .await,
        codes::FIELD_REQUIRED,
    );
}

#[tokio::test]
async fn a_webhook_url_at_the_byte_limit_is_accepted() {
    let harness = Harness::new();
    let url = format!(
        "https://{}",
        "a".repeat(MAX_WEBHOOK_URL_BYTES - "https://".len())
    );
    assert_eq!(
        url.len(),
        MAX_WEBHOOK_URL_BYTES,
        "the test URL is exactly at the limit"
    );
    let mut spec = Harness::spec("weatherbot");
    spec.webhook_url = Some(url.clone());
    let registered = harness
        .service
        .register(&owner(OWNER), spec)
        .await
        .expect("a webhook exactly at the byte limit is valid");
    assert_eq!(registered.bot.webhook_url.as_deref(), Some(url.as_str()));
}

#[tokio::test]
async fn a_webhook_url_one_byte_past_the_limit_is_rejected() {
    let harness = Harness::new();
    let url = format!(
        "https://{}",
        "a".repeat(MAX_WEBHOOK_URL_BYTES - "https://".len() + 1)
    );
    let mut spec = Harness::spec("weatherbot");
    spec.webhook_url = Some(url);
    let error = expect_error(harness.service.register(&owner(OWNER), spec).await);
    assert_eq!(error.code(), codes::FIELD_TOO_LONG);
    assert_eq!(error.kind(), ErrorKind::Validation);
}

#[tokio::test]
async fn registering_up_to_the_owner_cap_succeeds_and_one_more_is_refused() {
    let config = BotsConfig {
        max_bots_per_owner: 3,
        default_locale: "en".to_string(),
    };
    let harness = Harness::configured(config);
    let caller = owner(OWNER);
    // Three registrations fill the cap exactly.
    for username in ["pollbot", "quizbot", "notifier"] {
        harness
            .service
            .register(&caller, Harness::spec(username))
            .await
            .expect("a registration within the cap succeeds");
    }
    // The fourth is one past the cap.
    expect_code(
        harness
            .service
            .register(&caller, Harness::spec("archiver"))
            .await,
        codes::QUOTA_EXCEEDED,
    );
}

#[tokio::test]
async fn the_owner_cap_is_per_owner_not_global() {
    let config = BotsConfig {
        max_bots_per_owner: 1,
        default_locale: "en".to_string(),
    };
    let harness = Harness::configured(config);
    harness
        .service
        .register(&owner(OWNER), Harness::spec("weatherbot"))
        .await
        .expect("OWNER's first bot fits");
    // OTHER has their own separate allowance; OWNER filling theirs does not exhaust it.
    harness
        .service
        .register(&commander(), Harness::spec("greeter"))
        .await
        .expect("OTHER's first bot fits under their own cap");
    expect_code(
        harness
            .service
            .register(&owner(OWNER), Harness::spec("chronosbot"))
            .await,
        codes::QUOTA_EXCEEDED,
    );
}

#[tokio::test]
async fn the_default_owner_cap_is_the_documented_constant() {
    assert_eq!(
        BotsConfig::default().max_bots_per_owner,
        DEFAULT_MAX_BOTS_PER_OWNER,
        "the default config must use the documented ceiling"
    );
}

// ---------------------------------------------------------------------------
// Metrics: one counter per lifecycle event, and not one of them names a person.
//
// Section 174 forbids a series labelled by account; this crate adds bot and owner to that.
// Every series here is either unlabelled or labelled only by the closed auth-rejection reason.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_counter_starts_at_zero() {
    let harness = Harness::new();
    // Registering the whole set up front gives a dashboard a flat line, not a gap, before the
    // first event of each kind.
    for name in [
        "migo_bots_registered_total",
        "migo_bots_authenticated_total",
        "migo_bots_token_rotated_total",
        "migo_bots_scopes_changed_total",
        "migo_bots_disabled_total",
        "migo_bots_enabled_total",
    ] {
        assert_eq!(harness.plain(name), 0, "{name} must be registered at zero");
    }
    assert_eq!(harness.rejected("unknown"), 0);
    assert_eq!(harness.rejected("disabled"), 0);
}

#[tokio::test]
async fn registering_a_bot_increments_the_registered_counter() {
    let harness = Harness::new();
    harness.register(&owner(OWNER), "weatherbot").await;
    assert_eq!(harness.plain("migo_bots_registered_total"), 1);
}

#[tokio::test]
async fn a_successful_authentication_increments_the_authenticated_counter() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .service
        .authenticate(registered.token.expose())
        .await
        .expect("authentication succeeds");
    assert_eq!(harness.plain("migo_bots_authenticated_total"), 1);
}

#[tokio::test]
async fn an_unknown_token_increments_only_the_unknown_rejection_counter() {
    let harness = Harness::new();
    let _ = harness.service.authenticate("bm90LWEtcmVhbC10b2tlbg").await;
    assert_eq!(harness.rejected("unknown"), 1, "the reason is unknown");
    assert_eq!(harness.rejected("disabled"), 0, "and not disabled");
    assert_eq!(
        harness.plain("migo_bots_authenticated_total"),
        0,
        "a refused authentication must not count as a success"
    );
}

#[tokio::test]
async fn a_disabled_bot_increments_only_the_disabled_rejection_counter() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .service
        .set_paused(&owner(OWNER), registered.bot.bot_id, true)
        .await
        .expect("pause succeeds");
    let _ = harness
        .service
        .authenticate(registered.token.expose())
        .await;
    assert_eq!(harness.rejected("disabled"), 1, "the reason is disabled");
    assert_eq!(harness.rejected("unknown"), 0, "and not unknown");
}

#[tokio::test]
async fn rotating_scopes_and_pausing_each_move_their_own_counter() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .service
        .rotate_token(&owner(OWNER), registered.bot.bot_id)
        .await
        .expect("rotation succeeds");
    harness
        .service
        .set_scopes(&owner(OWNER), registered.bot.bot_id, Scopes::MODERATE)
        .await
        .expect("scope change succeeds");
    harness
        .service
        .set_paused(&owner(OWNER), registered.bot.bot_id, true)
        .await
        .expect("pause succeeds");
    harness
        .service
        .set_paused(&owner(OWNER), registered.bot.bot_id, false)
        .await
        .expect("resume succeeds");
    assert_eq!(harness.plain("migo_bots_token_rotated_total"), 1);
    assert_eq!(harness.plain("migo_bots_scopes_changed_total"), 1);
    assert_eq!(harness.plain("migo_bots_disabled_total"), 1);
    assert_eq!(harness.plain("migo_bots_enabled_total"), 1);
}

#[tokio::test]
async fn a_rate_limited_change_moves_no_success_counter() {
    let harness = Harness::new();
    let caller = owner(OWNER);
    let registered = harness.register(&caller, "weatherbot").await;
    let before = harness.plain("migo_bots_scopes_changed_total");
    harness.drain_account(&caller).await;
    let _ = harness
        .service
        .set_scopes(&caller, registered.bot.bot_id, Scopes::ALL)
        .await;
    assert_eq!(
        harness.plain("migo_bots_scopes_changed_total"),
        before,
        "a refused change must not tick the success counter — the counter counts what happened"
    );
}

#[tokio::test]
async fn no_lifecycle_counter_is_keyed_by_the_account_that_caused_it() {
    let harness = Harness::new();
    // Register a bot for a specific account, then confirm the count landed on the unlabelled
    // series — reading it with no label returns 1. A series keyed by account would leave the
    // unlabelled read at 0.
    harness.register(&owner(OWNER), "weatherbot").await;
    assert_eq!(
        harness.plain("migo_bots_registered_total"),
        1,
        "the increment is on the unlabelled series, so metrics cannot rebuild who owns what"
    );
    // And a fabricated account label names a series that never received the increment.
    assert_eq!(
        harness.counter(
            "migo_bots_registered_total",
            &[("account", &id(OWNER).to_text())]
        ),
        0,
        "there is no per-account series to read; account is never a label here"
    );
}

#[tokio::test]
async fn the_only_label_this_crate_publishes_is_the_rejection_reason() {
    let harness = Harness::new();
    // Two rejections for different reasons live on two series that differ only by `reason`.
    let _ = harness.service.authenticate("bm90LWEtcmVhbC10b2tlbg").await;
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .service
        .set_paused(&owner(OWNER), registered.bot.bot_id, true)
        .await
        .expect("pause succeeds");
    let _ = harness
        .service
        .authenticate(registered.token.expose())
        .await;
    assert_eq!(harness.rejected("unknown"), 1);
    assert_eq!(harness.rejected("disabled"), 1);
    // A reason outside the closed set is simply a series that was never touched.
    assert_eq!(
        harness.rejected("some_new_reason"),
        0,
        "the reason label is a closed enum; nothing outside it is ever emitted"
    );
}

// ---------------------------------------------------------------------------
// Webhook URLs: what the source actually validates, and one thing it does not.
//
// The source enforces two rules — a non-empty webhook must be within the byte cap and must be
// an https URL — and the length check runs first. It does NOT reject loopback or private-range
// hosts, so an https URL pointing at the server's own network is accepted; see the SSRF tests
// below, which pin that gap rather than pretend it is closed.
// ---------------------------------------------------------------------------

/// A registration spec carrying a webhook URL.
fn spec_with_webhook(username: &str, webhook: &str) -> NewBotSpec {
    let mut spec = Harness::spec(username);
    spec.webhook_url = Some(webhook.to_string());
    spec
}

#[tokio::test]
async fn an_https_webhook_is_accepted_and_stored() {
    let harness = Harness::new();
    let registered = harness
        .service
        .register(
            &owner(OWNER),
            spec_with_webhook("weatherbot", "https://example.com/hook"),
        )
        .await
        .expect("an https webhook is valid");
    assert_eq!(
        registered.bot.webhook_url.as_deref(),
        Some("https://example.com/hook")
    );
}

#[tokio::test]
async fn a_plain_http_webhook_is_rejected() {
    let harness = Harness::new();
    let error = expect_error(
        harness
            .service
            .register(
                &owner(OWNER),
                spec_with_webhook("weatherbot", "http://example.com/hook"),
            )
            .await,
    );
    assert_eq!(error.code(), codes::VALIDATION_FAILED);
    assert_eq!(
        error.public_message(),
        "webhook_url: must be an https URL",
        "the client is told which field and why, since it can fix a validation error"
    );
}

#[tokio::test]
async fn an_absent_webhook_is_not_an_error() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    assert_eq!(
        registered.bot.webhook_url, None,
        "no webhook is a valid choice"
    );
}

#[tokio::test]
async fn an_empty_webhook_string_is_treated_as_no_webhook() {
    let harness = Harness::new();
    let registered = harness
        .service
        .register(&owner(OWNER), spec_with_webhook("weatherbot", ""))
        .await
        .expect("an empty webhook means none, not an error");
    assert_eq!(registered.bot.webhook_url, None);
}

#[tokio::test]
async fn a_webhook_is_trimmed_before_it_is_stored() {
    let harness = Harness::new();
    let registered = harness
        .service
        .register(
            &owner(OWNER),
            spec_with_webhook("weatherbot", "  https://example.com/hook  "),
        )
        .await
        .expect("surrounding whitespace is trimmed, not rejected");
    assert_eq!(
        registered.bot.webhook_url.as_deref(),
        Some("https://example.com/hook")
    );
}

#[tokio::test]
async fn the_length_cap_is_checked_before_the_scheme() {
    let harness = Harness::new();
    // An over-long, non-https URL trips the length cap, not the scheme rule — proof of the
    // order the two checks run in.
    let url = "http://".to_string() + &"a".repeat(MAX_WEBHOOK_URL_BYTES);
    let error = expect_error(
        harness
            .service
            .register(&owner(OWNER), spec_with_webhook("weatherbot", &url))
            .await,
    );
    assert_eq!(
        error.code(),
        codes::FIELD_TOO_LONG,
        "length is checked first, so an over-long non-https URL is 'too long', not 'not https'"
    );
}

// --- SSRF finding: the source does not reject loopback or private-range webhook hosts.
// These tests document the current behaviour (acceptance). If a host-range check is added
// later, they will fail and should be rewritten to assert rejection — that failure is the
// intended alarm, not a regression in the test.

#[tokio::test]
async fn finding_a_loopback_https_webhook_is_currently_accepted() {
    let harness = Harness::new();
    let registered = harness
        .service
        .register(
            &owner(OWNER),
            spec_with_webhook("weatherbot", "https://127.0.0.1/hook"),
        )
        .await
        .expect("the source has no loopback check, so this is accepted today");
    assert_eq!(
        registered.bot.webhook_url.as_deref(),
        Some("https://127.0.0.1/hook"),
        "documents the SSRF gap: a webhook pointed at loopback is stored, not refused"
    );
}

// ---------------------------------------------------------------------------
// Registration validates a bot's handle exactly as a person's, and builds the backing
// account, its profile, and the bot row as one unit.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_reserved_username_is_refused_with_its_own_code() {
    let harness = Harness::new();
    // A reserved handle gets USERNAME_RESERVED, not a generic validation error, so a client
    // can say "that name is reserved" rather than "invalid", which would read as its own bug.
    expect_code(
        harness
            .service
            .register(&owner(OWNER), Harness::spec("admin"))
            .await,
        codes::USERNAME_RESERVED,
    );
}

#[tokio::test]
async fn a_too_short_username_is_a_validation_error() {
    let harness = Harness::new();
    expect_code(
        harness
            .service
            .register(&owner(OWNER), Harness::spec("ab"))
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn a_username_with_illegal_characters_is_a_validation_error() {
    let harness = Harness::new();
    // A space is not among the letters, digits, dots, and underscores a handle may contain.
    expect_code(
        harness
            .service
            .register(&owner(OWNER), Harness::spec("weather bot"))
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn a_duplicate_username_is_reported_as_taken() {
    let harness = Harness::new();
    harness.register(&owner(OWNER), "weatherbot").await;
    // The store's unique constraint surfaces as USERNAME_TAKEN, the client-facing story, not
    // as the raw ALREADY_EXISTS the storage layer raised.
    let error = expect_error(
        harness
            .service
            .register(&commander(), Harness::spec("weatherbot"))
            .await,
    );
    assert_eq!(error.code(), codes::USERNAME_TAKEN);
    // The reason travels as the code and nothing else: the service records "that username is
    // taken" for the log, and the reply carries no server-authored prose for a client to show
    // in place of its own localised string.
    assert_eq!(
        error.internal_message(),
        "that username is taken",
        "the log gets the specific reason"
    );
    assert_eq!(
        error.public_message(),
        "",
        "the code is the whole client-facing story"
    );
}

#[tokio::test]
async fn registration_creates_the_backing_account() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    // The bot posts under a real account; that account must exist after registration, carrying
    // the validated handle. This is the one write that makes all three rows appear together.
    let account = harness
        .store
        .account_by_id(registered.bot.account_id)
        .await
        .expect("the store can be read")
        .expect("the backing account exists");
    assert_eq!(
        account.username, "weatherbot",
        "the account carries the bot's handle"
    );
}

#[tokio::test]
async fn the_backing_account_takes_the_default_locale_when_none_is_given() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let account = harness
        .store
        .account_by_id(registered.bot.account_id)
        .await
        .expect("the store can be read")
        .expect("the backing account exists");
    assert_eq!(
        account.locale, "en",
        "an unspecified locale falls back to the deployment default"
    );
}

#[tokio::test]
async fn the_backing_account_honours_an_explicit_locale() {
    let harness = Harness::new();
    let mut spec = Harness::spec("weatherbot");
    spec.locale = Some("fr".to_string());
    let registered = harness
        .service
        .register(&owner(OWNER), spec)
        .await
        .expect("registration succeeds");
    let account = harness
        .store
        .account_by_id(registered.bot.account_id)
        .await
        .expect("the store can be read")
        .expect("the backing account exists");
    assert_eq!(account.locale, "fr");
}

#[tokio::test]
async fn a_bots_owner_is_the_caller_and_its_ids_are_distinct() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    assert_eq!(
        registered.bot.owner_id,
        id(OWNER),
        "the owner is the caller, from the store"
    );
    assert!(!registered.bot.bot_id.is_nil(), "the bot id is generated");
    assert!(
        !registered.bot.account_id.is_nil(),
        "the account id is generated"
    );
    assert_ne!(
        registered.bot.bot_id, registered.bot.account_id,
        "the bot row and its backing account are separate identities"
    );
}

#[tokio::test]
async fn a_bot_is_created_at_the_callers_timestamp() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    assert_eq!(
        registered.bot.created_at,
        ts(NOW),
        "creation time is the server time on the request, not a clock read inside the service"
    );
}

#[tokio::test]
async fn a_display_name_is_trimmed_before_it_is_stored() {
    let harness = Harness::new();
    let registered = harness
        .service
        .register(
            &owner(OWNER),
            spec_named("weatherbot", "  Weather Bot  ".to_string()),
        )
        .await
        .expect("registration succeeds");
    assert_eq!(
        registered.bot.name, "Weather Bot",
        "surrounding whitespace is trimmed"
    );
}

// ---------------------------------------------------------------------------
// Pausing and resuming, and the structural fact underneath every read: the service reaches
// persistent state only through the Store trait it was handed. It holds an `Arc<S: Store>`
// and nothing else that could reach the data, so a change written straight to the store is
// fully authoritative for the service's next read — there is no second, unmediated path a bug
// could take around it (invariant: no direct DB access).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pausing_a_bot_sets_disabled_and_records_when() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    let view = harness
        .service
        .set_paused(&owner(OWNER), registered.bot.bot_id, true)
        .await
        .expect("pause succeeds");
    assert!(view.disabled, "the bot reads as disabled");
    assert_eq!(
        view.disabled_at,
        Some(ts(NOW)),
        "the disable is stamped with the request's time, for a UI that shows when"
    );
}

#[tokio::test]
async fn resuming_a_bot_clears_disabled_and_its_timestamp() {
    let harness = Harness::new();
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .service
        .set_paused(&owner(OWNER), registered.bot.bot_id, true)
        .await
        .expect("pause succeeds");
    let view = harness
        .service
        .set_paused(&owner(OWNER), registered.bot.bot_id, false)
        .await
        .expect("resume succeeds");
    assert!(!view.disabled, "the bot reads as live again");
    assert_eq!(
        view.disabled_at, None,
        "and its disabled timestamp is cleared"
    );
}

#[tokio::test]
async fn a_read_reflects_a_disable_written_straight_to_the_store() {
    let harness = Harness::new();
    // Invariant, made observable: the service has no data path but the Store trait. Disable the
    // bot behind its back, and the very next service read sees it — because there is nowhere
    // else the service could have been reading from.
    let registered = harness.register(&owner(OWNER), "weatherbot").await;
    harness
        .store
        .set_bot_disabled(registered.bot.bot_id, Some(ts(NOW)))
        .await
        .expect("the store accepts the write")
        .expect("the bot row exists");
    let view = harness
        .service
        .get(&owner(OWNER), registered.bot.bot_id)
        .await
        .expect("get succeeds");
    assert!(
        view.disabled,
        "the service read reflects a store-only change; it holds no state of its own"
    );
}

// ---------------------------------------------------------------------------
// Commanding a bot
// ---------------------------------------------------------------------------

/// An argument shaped to break out of a naive splice into a command line or a JSON
/// document. The payload is built with a serializer, so it must arrive as one string
/// inside the args array and nothing more.
const SNEAKY_ARG: &str = "\"}";

/// A webhook the command path can deliver to, plus a spec that registers it.
fn webhook_spec(username: &str, url: &str) -> NewBotSpec {
    NewBotSpec {
        username: username.to_string(),
        display_name: format!("{username} display"),
        scopes: Scopes::NONE,
        webhook_url: Some(url.to_string()),
        locale: None,
    }
}

/// The §41 integration surface: any identified account commands an enabled bot, and the
/// command lands on the webhook its owner registered, addressed from the commander.
#[tokio::test]
async fn a_command_reaches_the_bot_webhook_with_the_callers_identity() {
    let harness = Harness::new();
    let owner = owner(OWNER);
    let registered = harness
        .register_spec(
            &owner,
            webhook_spec("weather", "https://bots.example.test/hook"),
        )
        .await;

    let commander = commander();
    harness
        .service
        .command(
            &commander,
            registered.bot.bot_id,
            "forecast",
            &["jakarta".to_string(), "--days=3".to_string()],
        )
        .await
        .expect("a command to an enabled bot is delivered");

    let deliveries = harness.sink.deliveries();
    assert_eq!(deliveries.len(), 1, "exactly one delivery");
    let (url, body) = &deliveries[0];
    assert_eq!(url, "https://bots.example.test/hook");

    let payload: Value = serde_json::from_slice(body).expect("the payload is JSON");
    assert_eq!(payload["command"], "forecast");
    assert_eq!(payload["args"][0], "jakarta");
    assert_eq!(payload["args"][1], "--days=3");
    assert_eq!(
        payload["from"],
        commander.account_id.to_text(),
        "the bot learns who asked, so it can answer"
    );
    assert_eq!(payload["bot_id"], registered.bot.bot_id.to_text());
}

/// An argument that looks like structure stays an argument: the payload is one JSON array,
/// not a spliced command line somebody can grow a second command out of.
#[tokio::test]
async fn an_argument_stays_an_argument() {
    let harness = Harness::new();
    let owner = owner(OWNER);
    let registered = harness
        .register_spec(
            &owner,
            webhook_spec("calc", "https://bots.example.test/hook"),
        )
        .await;

    harness
        .service
        .command(
            &commander(),
            registered.bot.bot_id,
            "eval",
            &[SNEAKY_ARG.to_string()],
        )
        .await
        .expect("a hostile-looking argument is still just an argument");

    let (_, body) = &harness.sink.deliveries()[0];
    let payload: Value = serde_json::from_slice(body).expect("the payload parses as JSON");
    assert_eq!(payload["args"][0], SNEAKY_ARG);
    assert_eq!(
        payload["command"], "eval",
        "exactly one command left the room"
    );
}

/// A bot with no webhook has no delivery channel on this wire. Refusing tells the truth;
/// swallowing the command would make the user wait for a reply that is never coming.
#[tokio::test]
async fn a_command_to_a_bot_without_a_webhook_is_refused() {
    let harness = Harness::new();
    let owner = owner(OWNER);
    let registered = harness.register(&owner, "plain").await;

    expect_code(
        harness
            .service
            .command(&commander(), registered.bot.bot_id, "ping", &[])
            .await,
        codes::VALIDATION_FAILED,
    );
    assert!(
        harness.sink.deliveries().is_empty(),
        "nothing was delivered, because there was nowhere to deliver to"
    );
}

/// A paused bot reads as missing, the same answer its token gives. A disabled integration
/// must not keep accepting work, and a difference between the two answers would let a
/// caller probe which bot ids ever existed.
#[tokio::test]
async fn a_command_to_a_paused_bot_is_not_distinguishable_from_an_unknown_bot() {
    let harness = Harness::new();
    let owner = owner(OWNER);
    let registered = harness
        .register_spec(
            &owner,
            webhook_spec("paused", "https://bots.example.test/hook"),
        )
        .await;
    harness
        .service
        .set_paused(&owner, registered.bot.bot_id, true)
        .await
        .expect("pause succeeds");

    expect_code(
        harness
            .service
            .command(&commander(), registered.bot.bot_id, "ping", &[])
            .await,
        codes::NOT_FOUND,
    );
    expect_code(
        harness
            .service
            .command(&commander(), id(0xB0B), "ping", &[])
            .await,
        codes::NOT_FOUND,
    );
}

/// When the bot's backend cannot be reached, the failure is the same opaque error whoever
/// is asking and whatever went wrong — the commanding user is not told whether the URL
/// resolved, refused, or timed out.
#[tokio::test]
async fn an_unreachable_webhook_is_one_opaque_error() {
    let harness = Harness::new();
    let owner = owner(OWNER);
    let registered = harness
        .register_spec(
            &owner,
            webhook_spec("down", "https://bots.example.test/hook"),
        )
        .await;
    harness.sink.fail_from_now_on();

    expect_code(
        harness
            .service
            .command(&commander(), registered.bot.bot_id, "ping", &[])
            .await,
        codes::INTERNAL_ERROR,
    );
}

/// Commanding is charged like everything else, and a refusal for a missing bot happens
/// after the charge — a caller fishing for bot ids pays per guess.
#[tokio::test]
async fn commanding_is_charged_even_when_it_refuses() {
    let harness = Harness::new();
    let caller = commander();
    let before = harness.account_balance(&caller).await;
    expect_code(
        harness
            .service
            .command(&caller, id(0xB0B), "ping", &[])
            .await,
        codes::NOT_FOUND,
    );
    let after = harness.account_balance(&caller).await;
    assert_eq!(before - after, 2, "BOT_COMMAND is priced at 2");
}
