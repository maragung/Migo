//! The economy, implemented: a double-entry ledger that cannot pay out, the shop over it,
//! experience with anti-farming caps, and the leaderboards that read them.
//!
//! # Why the service is thin over the store
//!
//! Every rule that must hold even if this process crashes mid-operation lives in the store,
//! inside one database transaction: legs sum to zero, an idempotency key is honoured once, an
//! entitlement is unique per owner, a user account may not go below zero. This service does
//! not re-check any of them, because a check here is a check a second writer can race past.
//! It composes ledger accounts, prices an operation from the catalogue, hands the store a
//! transaction, and translates the store's answer into the shape the layer above reads.
//!
//! That division is what makes the awkward cases fall out for free:
//!
//! * A **retry** of a purchase or gift carries the same idempotency key. The store checks the
//!   key *before* anything else and returns the original transaction, so the service reports
//!   `duplicate` without ever re-pricing or re-charging. This is why [`Treasurer::purchase`]
//!   does not pre-check ownership: a pre-check would turn a legitimate retry of an
//!   already-owned item into `ALREADY_EXISTS` instead of returning the first purchase.
//! * A **fresh** attempt to buy something already owned reaches the store's receipt
//!   validation, which refuses it with `ALREADY_EXISTS` — before the atomic write, so no money
//!   moves. "Before any money moves" and "a repeat returns the first purchase" are both
//!   satisfied by posting rather than pre-checking.
//! * An **unaffordable** debit hits the store's overdraft floor and comes back
//!   `INSUFFICIENT_BALANCE`, which the service counts and propagates. There is no
//!   "can they afford it" read first, because between that read and the write another
//!   transaction could spend the balance out from under it.
//!
//! # The one port this service asks for, and the one it does not
//!
//! It tells a recipient a gift arrived through the [`Announcer`] port, whose failure is
//! swallowed — a gift that could not buzz is still a gift that arrived and was paid for. It
//! never depends on `migo-notify` or `migo-games`; awarding is inverted so those crates own
//! the trait and this service satisfies it. See [`crate::traits`].

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use migo_cache::traits::KeyValueCache;
use migo_cache::{Cache, CacheKey, SharedCache, Ttl};
use migo_core::metrics::Registry;
use migo_core::{Id, OsRandom, Random, Result, Timestamp};
use migo_protocol::{codes, fault, NotificationKind};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter};
use migo_store::model::{
    BadgeAward, Currency, Entitlement, GiftReceipt, GiftSent, LedgerAccountKind, LedgerLeg,
    NewTransaction, NewXpAward, Posted, Receipt, Scope,
};
use migo_store::{SharedStore, Store, MAX_PAGE};

use crate::catalogue::Catalogue;
use crate::metrics::{CacheOutcome, Meters, TxOutcome};
use crate::model::{
    level_for_xp, Award, AwardOutcome, BadgeGrant, Board, BoardScope, Caller, Category,
    EconomyConfig, GiftOutcome, GiftTally, Grant, GrantReceipt, LedgerEntry, Listing,
    ProgressionView, PurchaseOutcome, Rank, Reason, SendGift, Sku, Wallet, Window,
};
use crate::traits::{Announcement, Announcer, SharedAnnouncer, SharedTreasurer, Treasurer};

/// What sending a gift costs the sender's rate-limit budget (brief section 145's `GIFT_SEND`).
const GIFT_COST: u32 = 20;
/// What buying an item costs the buyer's budget. A purchase is a write, priced like a gift.
const PURCHASE_COST: u32 = 20;
/// What reading a wallet costs (`BALANCE_FETCH`).
const BALANCE_COST: u32 = 3;
/// What any other read of one's own or a public profile costs.
const READ_COST: u32 = 3;
/// What a leaderboard page costs. Dearer than a plain read: even cached, it is a page a client
/// polls, and the budget is what stops one client polling a board into a hot loop.
const LEADERBOARD_COST: u32 = 5;

/// One day in milliseconds, the span of section 30's anti-farming caps.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Bytes in one cached leaderboard row: position `u32`, account id (16), xp `i64`, level `i32`.
///
/// This crate has no serialization dependency — no serde, no wire — because it does not speak
/// on the network; the gateway does. A leaderboard is the one thing it caches, so it uses a
/// fixed-width big-endian encoding of exactly the four fields a [`Rank`] carries, and treats
/// anything it cannot decode as a cache miss. Keeping the codec here, private and tiny, is
/// cheaper than taking on a serialization framework for one struct.
const RANK_BYTES: usize = 4 + 16 + 8 + 4;

/// The cache key tail for one board: scope, window, and page size, but never the instant.
///
/// The instant is deliberately absent. A weekly board asked for twice a second apart computes
/// two different `since` cutoffs, but it is the *same board*, and the short TTL is what makes
/// its staleness invisible (brief section 32). Folding `now` into the key would give every
/// request a unique key and turn the cache off.
fn board_tag(scope: &BoardScope, window: Window, limit: u16) -> String {
    let window_tag = match window {
        Window::Weekly => "w",
        Window::Monthly => "m",
        Window::AllTime => "a",
    };
    match scope {
        BoardScope::Global => format!("global:{window_tag}:{limit}"),
        BoardScope::Country(code) => format!("country:{code}:{window_tag}:{limit}"),
        BoardScope::Room(room_id) => format!("room:{room_id}:{window_tag}:{limit}"),
    }
}

/// Packs ranked rows into the fixed-width form the cache stores.
fn encode_ranks(ranks: &[Rank]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ranks.len() * RANK_BYTES);
    for rank in ranks {
        out.extend_from_slice(&rank.position.to_be_bytes());
        out.extend_from_slice(rank.account_id.as_bytes());
        out.extend_from_slice(&rank.xp.to_be_bytes());
        out.extend_from_slice(&rank.level.to_be_bytes());
    }
    out
}

/// Reads back what [`encode_ranks`] wrote, or `None` if the bytes are not that shape.
///
/// `None` is not an error: a value this build cannot parse is one an older or newer build
/// wrote in a shape it does not know, and the caller recomputes from the store rather than
/// failing a read over a cache-format skew.
fn decode_ranks(bytes: &[u8]) -> Option<Vec<Rank>> {
    if !bytes.len().is_multiple_of(RANK_BYTES) {
        return None;
    }
    let mut ranks = Vec::with_capacity(bytes.len() / RANK_BYTES);
    for chunk in bytes.chunks_exact(RANK_BYTES) {
        let position = u32::from_be_bytes(chunk[0..4].try_into().ok()?);
        let account_id = Id::from_bytes(chunk[4..20].try_into().ok()?);
        let xp = i64::from_be_bytes(chunk[20..28].try_into().ok()?);
        let level = i32::from_be_bytes(chunk[28..32].try_into().ok()?);
        ranks.push(Rank {
            position,
            account_id,
            xp,
            level,
        });
    }
    Some(ranks)
}

/// The economy service.
///
/// Generic over its four collaborators so a test can drive it with in-memory doubles and
/// production wires the real store, cache, limiter, and notifier; the `dyn` defaults are what
/// the composition root uses, so the shipped binary holds one boxed trait object of each
/// rather than a monomorphised copy per combination.
pub struct Economy<
    S: ?Sized = dyn Store,
    C: ?Sized = dyn Cache,
    L: ?Sized = dyn RateLimiter,
    A: ?Sized = dyn Announcer,
> {
    store: Arc<S>,
    cache: Arc<C>,
    limiter: Arc<L>,
    announcer: Arc<A>,
    catalogue: Catalogue,
    config: EconomyConfig,
    /// The id source, behind a lock because generation mutates it. The lock is a
    /// `parking_lot::Mutex` and is never held across an `.await`: [`Economy::new_id`] takes it,
    /// mints one id, and drops it, all synchronously.
    random: Mutex<Box<dyn Random>>,
    meters: Meters,
}

impl<S, C, L, A> Economy<S, C, L, A>
where
    S: Store + ?Sized,
    C: KeyValueCache + ?Sized,
    L: RateLimiter + ?Sized,
    A: Announcer + ?Sized,
{
    /// Assembles the service from its collaborators and an id source.
    ///
    /// Takes the [`Random`] rather than making one so a simulation can replay a run with a
    /// seeded generator and get the same ids; [`open`] supplies the operating-system source.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<S>,
        cache: Arc<C>,
        limiter: Arc<L>,
        announcer: Arc<A>,
        catalogue: Catalogue,
        config: EconomyConfig,
        random: Box<dyn Random>,
        registry: &Registry,
    ) -> Self {
        Self {
            store,
            cache,
            limiter,
            announcer,
            catalogue,
            config,
            random: Mutex::new(random),
            meters: Meters::new(registry),
        }
    }

    /// Mints one id stamped at `at`. Synchronous; the random lock is released before return.
    fn new_id(&self, at: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(at, &mut **random)
    }

    /// Charges a caller's rate-limit budget, refusing with the limiter's own error if empty.
    async fn charge(&self, caller: &Caller, cost: u32) -> Result<()> {
        self.limiter
            .charge(
                &[BucketKey::account(caller.account_id)],
                cost,
                caller.tier,
                caller.now,
            )
            .await?
            .into_result()
    }

    /// Finds or creates a ledger account, returning its id.
    ///
    /// A user has one account per currency; the mint, fee, and escrow accounts are singletons
    /// with no owner. The store's find-or-create ignores the id we offer when the account
    /// already exists, so minting one we may not use is cheap and always safe.
    async fn account_for(
        &self,
        owner: Option<Id>,
        kind: LedgerAccountKind,
        currency: Currency,
        at: Timestamp,
    ) -> Result<Id> {
        let create_with = self.new_id(at);
        let account = self
            .store
            .ledger_account(owner, kind, currency, create_with, at)
            .await?;
        Ok(account.ledger_account_id)
    }

    /// Posts a transaction, metering the outcome and counting an overdraft refusal.
    ///
    /// The store distinguishes a fresh write from a repeated key, which is the one signal that
    /// tells a healthy retry from a client stuck in a resend loop, so it is metered. An
    /// `INSUFFICIENT_BALANCE` is the overdraft floor turning a debit away; it is counted here
    /// and propagated. Every other error passes straight through.
    async fn post(&self, new: NewTransaction) -> Result<Posted> {
        match self.store.post_transaction(new).await {
            Ok(posted) => {
                self.meters.transaction(TxOutcome::of(posted.is_new()));
                Ok(posted)
            }
            Err(error) => {
                if error.code() == codes::INSUFFICIENT_BALANCE {
                    self.meters.insufficient_balance();
                }
                Err(error)
            }
        }
    }

    /// Clamps a caller's page size into the store's bounds.
    fn page(limit: u16) -> u16 {
        limit.clamp(1, MAX_PAGE)
    }

    /// The instant one day before `at`, the start of the anti-farming window.
    fn day_before(at: Timestamp) -> Timestamp {
        Timestamp::from_millis(at.as_millis().saturating_sub(DAY_MS))
    }

    /// Tells one account something happened, swallowing a notifier failure.
    ///
    /// Best-effort by contract (brief section 44): the economy does not depend on the buzz
    /// arriving, so a failure is logged by error code alone — never by who was to be told,
    /// which would put the gifting graph in the logs — and then dropped.
    async fn announce(&self, announcement: Announcement) {
        if let Err(error) = self.announcer.announce(announcement).await {
            tracing::warn!(code = error.code(), "economy announcement dropped");
        }
    }
}

#[async_trait]
impl<S, C, L, A> Treasurer for Economy<S, C, L, A>
where
    S: Store + ?Sized + Send + Sync,
    C: KeyValueCache + ?Sized + Send + Sync,
    L: RateLimiter + ?Sized + Send + Sync,
    A: Announcer + ?Sized + Send + Sync,
{
    fn listings(&self) -> Vec<Listing> {
        self.catalogue.all().into_iter().cloned().collect()
    }

    fn listing(&self, sku: &Sku) -> Option<Listing> {
        self.catalogue.get(sku).cloned()
    }

    async fn wallet(&self, caller: &Caller) -> Result<Wallet> {
        self.charge(caller, BALANCE_COST).await?;
        // One balance per currency. Reading creates the account if it is absent, which is the
        // only way the store exposes a balance and is harmless: a fresh account reads as zero.
        let mut wallet = Wallet::default();
        for currency in [Currency::Coins, Currency::Gems, Currency::Points] {
            let account = self
                .account_for(
                    Some(caller.account_id),
                    LedgerAccountKind::User,
                    currency,
                    caller.now,
                )
                .await?;
            wallet.set(currency, self.store.balance(account).await?);
        }
        self.meters.balance_read();
        Ok(wallet)
    }

    async fn statement(
        &self,
        caller: &Caller,
        currency: Currency,
        limit: u16,
    ) -> Result<Vec<LedgerEntry>> {
        self.charge(caller, READ_COST).await?;
        let account = self
            .account_for(
                Some(caller.account_id),
                LedgerAccountKind::User,
                currency,
                caller.now,
            )
            .await?;
        let history = self
            .store
            .ledger_history(account, Self::page(limit))
            .await?;
        Ok(history
            .into_iter()
            .map(|(tx, balance_after)| {
                // A statement shows this account's movement, which is the sum of the legs that
                // touched it — one leg in the ordinary case, summed so a transaction that
                // touched the account twice still reads correctly.
                let amount = tx
                    .legs
                    .iter()
                    .filter(|leg| leg.ledger_account_id == account)
                    .map(|leg| leg.amount)
                    .sum();
                LedgerEntry {
                    tx_id: tx.tx_id,
                    reason: Reason::from_i16(tx.reason),
                    amount,
                    balance_after,
                    ref_id: tx.ref_id,
                    at: tx.created_at,
                }
            })
            .collect())
    }

    async fn purchase(
        &self,
        caller: &Caller,
        sku: &Sku,
        client_key: &str,
    ) -> Result<PurchaseOutcome> {
        self.charge(caller, PURCHASE_COST).await?;
        let price = self
            .catalogue
            .get(sku)
            .ok_or_else(|| fault::not_found("item"))?
            .price;
        let payer = self
            .account_for(
                Some(caller.account_id),
                LedgerAccountKind::User,
                price.currency,
                caller.now,
            )
            .await?;
        let fee = self
            .account_for(None, LedgerAccountKind::Fee, price.currency, caller.now)
            .await?;
        // The item is the receipt of the transaction that paid for it: the store writes the
        // entitlement and the ledger legs in one transaction, refuses a second identical
        // ownership with ALREADY_EXISTS before writing, and refuses an unaffordable debit with
        // INSUFFICIENT_BALANCE. All three answers arrive from `post`; none is pre-checked here.
        let posted = self
            .post(NewTransaction {
                tx_id: self.new_id(caller.now),
                reason: Reason::Purchase.to_i16(),
                ref_id: None,
                idempotency_key: format!("purchase:{}:{client_key}", caller.account_id),
                created_by: Some(caller.account_id),
                currency: price.currency,
                legs: vec![
                    LedgerLeg {
                        ledger_account_id: payer,
                        amount: -price.amount,
                    },
                    LedgerLeg {
                        ledger_account_id: fee,
                        amount: price.amount,
                    },
                ],
                receipt: Some(Receipt::Entitlement { sku: sku.code() }),
                created_at: caller.now,
            })
            .await?;
        self.meters.purchase(sku.category());
        Ok(PurchaseOutcome {
            sku: sku.clone(),
            price,
            duplicate: !posted.is_new(),
        })
    }

    // A linear payment saga: charge the sender, mint the recipient's reputation, then notify.
    // The length is struct-literal `NewTransaction` blocks and the comments explaining each
    // step's idempotency and crash-recovery invariants, not branching depth — splitting it
    // would scatter a sequence the reader has to follow in order. Kept whole on purpose.
    #[allow(clippy::too_many_lines)]
    async fn send_gift(&self, caller: &Caller, gift: SendGift) -> Result<GiftOutcome> {
        self.charge(caller, GIFT_COST).await?;
        // A gift to oneself would launder spendable currency into reputation points, which
        // section 87 keeps non-cash-outable precisely so it cannot become a shadow balance.
        // Refuse it before anything is priced.
        if gift.recipient_id == caller.account_id {
            return Err(fault::validation(
                "recipient_id",
                "a gift needs a different recipient",
            ));
        }
        let sku = Sku::new(Category::Gift, gift.gift.slug())
            .ok_or_else(|| fault::internal("gift slug is not a valid sku"))?;
        let listing = self
            .catalogue
            .get(&sku)
            .ok_or_else(|| fault::not_found("gift"))?;
        let price = listing.price;
        let reputation = listing.reputation;
        let gift_code = gift.gift.code();

        // Transaction one — the sender pays, and the gift is recorded as this transaction's
        // receipt, so a gift that exists was always paid for. Keyed on the sender's client key
        // so a retry returns the original rather than charging twice.
        let payer = self
            .account_for(
                Some(caller.account_id),
                LedgerAccountKind::User,
                price.currency,
                caller.now,
            )
            .await?;
        let fee = self
            .account_for(None, LedgerAccountKind::Fee, price.currency, caller.now)
            .await?;
        let gift_id = self.new_id(caller.now);
        let posted = self
            .post(NewTransaction {
                tx_id: self.new_id(caller.now),
                reason: Reason::GiftPurchase.to_i16(),
                ref_id: Some(gift_id),
                idempotency_key: format!("gift:{}:{}", caller.account_id, gift.client_key),
                created_by: Some(caller.account_id),
                currency: price.currency,
                legs: vec![
                    LedgerLeg {
                        ledger_account_id: payer,
                        amount: -price.amount,
                    },
                    LedgerLeg {
                        ledger_account_id: fee,
                        amount: price.amount,
                    },
                ],
                receipt: Some(Receipt::Gift(GiftReceipt {
                    gift_id,
                    sender_id: caller.account_id,
                    recipient_id: gift.recipient_id,
                    gift_code: gift_code.clone(),
                    conversation_id: gift.conversation_id,
                })),
                created_at: caller.now,
            })
            .await?;
        // On a repeat the store returns the first transaction; its `ref_id` is the gift that
        // was actually recorded, which is the id the reputation half must key on — not the id
        // we just minted and did not use.
        let settled_gift_id = posted.transaction().ref_id.unwrap_or(gift_id);
        let duplicate = !posted.is_new();
        self.meters.gift_sent(gift.gift);

        // Transaction two — the recipient's reputation, in points minted for the purpose.
        // Always attempted and keyed on the settled gift id, so a crash between the two heals
        // on the next identical send: the purchase returns Duplicate and this completes. A gift
        // a deployment prices at no reputation skips it. Propagated on error rather than
        // swallowed, because the retry that carries the same key is what finishes the job.
        if reputation > 0 {
            let recipient = self
                .account_for(
                    Some(gift.recipient_id),
                    LedgerAccountKind::User,
                    Currency::Points,
                    caller.now,
                )
                .await?;
            let mint = self
                .account_for(None, LedgerAccountKind::Mint, Currency::Points, caller.now)
                .await?;
            self.post(NewTransaction {
                tx_id: self.new_id(caller.now),
                reason: Reason::GiftReputation.to_i16(),
                ref_id: Some(settled_gift_id),
                idempotency_key: format!("gift:points:{settled_gift_id}"),
                created_by: None,
                currency: Currency::Points,
                legs: vec![
                    LedgerLeg {
                        ledger_account_id: mint,
                        amount: -reputation,
                    },
                    LedgerLeg {
                        ledger_account_id: recipient,
                        amount: reputation,
                    },
                ],
                receipt: None,
                created_at: caller.now,
            })
            .await?;
        }

        // Tell the recipient a gift arrived — a wake-up, not its contents (section 44). Only on
        // a first send: a retry would otherwise buzz them again for a gift they already have.
        if !duplicate {
            self.announce(Announcement {
                account_id: gift.recipient_id,
                kind: NotificationKind::Gift,
                actor_id: Some(caller.account_id),
                subject_id: Some(settled_gift_id),
                at: caller.now,
            })
            .await;
        }

        Ok(GiftOutcome {
            gift_id: settled_gift_id,
            gift_code,
            price,
            reputation,
            recipient_id: gift.recipient_id,
            duplicate,
        })
    }

    async fn entitlements(&self, caller: &Caller) -> Result<Vec<Entitlement>> {
        self.charge(caller, READ_COST).await?;
        self.store.entitlements(caller.account_id).await
    }

    async fn gifts_received(&self, caller: &Caller, limit: u16) -> Result<Vec<GiftSent>> {
        self.charge(caller, READ_COST).await?;
        self.store
            .gifts_received(caller.account_id, Self::page(limit))
            .await
    }

    async fn gift_shelf(&self, caller: &Caller, of_account: Id) -> Result<Vec<GiftTally>> {
        self.charge(caller, READ_COST).await?;
        let tally = self.store.gift_tally(of_account).await?;
        Ok(tally
            .into_iter()
            .map(|(gift_code, count)| GiftTally { gift_code, count })
            .collect())
    }

    async fn progression(&self, caller: &Caller, of_account: Id) -> Result<ProgressionView> {
        self.charge(caller, READ_COST).await?;
        // An account that never earned anything has no row and reads as level one, not as an
        // error: a level is a projection of a total, and the total of nothing is zero.
        let xp = self
            .store
            .progression(of_account)
            .await?
            .map_or(0, |progression| progression.xp);
        Ok(ProgressionView::of(of_account, xp))
    }

    async fn badges(&self, caller: &Caller, of_account: Id) -> Result<Vec<BadgeAward>> {
        self.charge(caller, READ_COST).await?;
        self.store.badges(of_account).await
    }

    async fn leaderboard(&self, caller: &Caller, board: Board) -> Result<Vec<Rank>> {
        self.charge(caller, LEADERBOARD_COST).await?;
        let limit = board.limit.clamp(1, self.config.leaderboard_max);
        let key = CacheKey::new(
            "economy_leaderboard",
            &board_tag(&board.scope, board.window, limit),
        );
        if let Some(bytes) = self.cache.get(&key, caller.now).await? {
            if let Some(ranks) = decode_ranks(&bytes) {
                self.meters.leaderboard_read(CacheOutcome::Hit);
                return Ok(ranks);
            }
        }
        // A miss, or a value this build could not decode: compute from the store. The window
        // decides the cutoff, the scope the population; the caller's clock decides where a week
        // starts, because a server deciding would decide in its own timezone (section 32).
        let since = board.window.since(caller.now);
        let scope = match &board.scope {
            BoardScope::Global => Scope::Global,
            BoardScope::Country(code) => Scope::Country(code.as_str()),
            BoardScope::Room(room_id) => Scope::Room(*room_id),
        };
        let standings = self.store.leaderboard(scope, since, limit).await?;
        let ranks: Vec<Rank> = standings
            .into_iter()
            .enumerate()
            .map(|(index, standing)| Rank {
                position: u32::try_from(index + 1).unwrap_or(u32::MAX),
                account_id: standing.account_id,
                xp: standing.xp,
                level: standing.level,
            })
            .collect();
        // Cache best-effort: a board that could not be written is one recomputed next time,
        // not a failed read. The TTL is short by design so the staleness is invisible.
        let ttl = Ttl::from_millis(self.config.leaderboard_ttl_ms);
        if let Err(error) = self
            .cache
            .set(&key, &encode_ranks(&ranks), ttl, caller.now)
            .await
        {
            tracing::warn!(code = error.code(), "leaderboard cache write failed");
        }
        self.meters.leaderboard_read(CacheOutcome::Miss);
        Ok(ranks)
    }

    async fn grant(&self, grant: Grant) -> Result<GrantReceipt> {
        // Server-facing, so the guard is an assertion about the caller's own code, not about a
        // client: a non-positive grant is a bug in whatever asked for it.
        if grant.amount <= 0 {
            return Err(fault::validation("amount", "a grant must be positive"));
        }
        let recipient = self
            .account_for(
                Some(grant.account_id),
                LedgerAccountKind::User,
                grant.currency,
                grant.at,
            )
            .await?;
        // Currency is issued from the mint, which is the one account allowed to run negative:
        // the sum of every balance in a currency stays zero because the mint holds the debit
        // for everything ever granted.
        let mint = self
            .account_for(None, LedgerAccountKind::Mint, grant.currency, grant.at)
            .await?;
        let posted = self
            .post(NewTransaction {
                tx_id: self.new_id(grant.at),
                reason: grant.reason.to_i16(),
                ref_id: grant.ref_id,
                idempotency_key: format!("grant:{}", grant.idempotency_key),
                created_by: grant.created_by,
                currency: grant.currency,
                legs: vec![
                    LedgerLeg {
                        ledger_account_id: mint,
                        amount: -grant.amount,
                    },
                    LedgerLeg {
                        ledger_account_id: recipient,
                        amount: grant.amount,
                    },
                ],
                receipt: None,
                created_at: grant.at,
            })
            .await?;
        self.meters.grant(grant.currency);
        Ok(GrantReceipt {
            tx_id: posted.transaction().tx_id,
            created: posted.is_new(),
        })
    }

    async fn award(&self, award: Award) -> Result<AwardOutcome> {
        if award.amount <= 0 {
            return Err(fault::validation("amount", "an award must be positive"));
        }
        // Two caps bound the award over a rolling day (section 30): the global daily cap across
        // every source, and the per-source cap. The smaller remaining headroom binds. Both are
        // read from the durable award rows, not a cache counter, so a cache restart cannot
        // silently reset an abuser's daily limit.
        let since = Self::day_before(award.at);
        let earned_all = self
            .store
            .xp_earned_since(award.account_id, None, since)
            .await?;
        let earned_source = self
            .store
            .xp_earned_since(award.account_id, Some(award.source.to_i16()), since)
            .await?;
        let global_room = (self.config.daily_xp_cap - earned_all).max(0);
        let source_room = (self.config.source_cap(award.source) - earned_source).max(0);
        let granted = award.amount.min(global_room).min(source_room).max(0);
        let capped = granted < award.amount;

        if granted == 0 {
            // The cap is met; nothing is written and no id is minted. The outcome still reports
            // the real standing so a client can say "you have hit today's limit" rather than
            // showing a phantom award.
            self.meters.xp_capped(award.source);
            let current = self
                .store
                .progression(award.account_id)
                .await?
                .map_or(0, |progression| progression.xp);
            let level = level_for_xp(current);
            return Ok(AwardOutcome {
                requested: award.amount,
                granted: 0,
                before: current,
                after: current,
                level_before: level,
                level_after: level,
                capped: true,
            });
        }

        let change = match self
            .store
            .award_xp(NewXpAward {
                award_id: self.new_id(award.at),
                account_id: award.account_id,
                source: award.source.to_i16(),
                amount: granted,
                ref_id: award.ref_id,
                idempotency_key: award.idempotency_key,
                at: award.at,
            })
            .await
        {
            Ok(change) => change,
            // The key names an award already granted. The store refuses it so we cannot
            // announce a level-up twice; we report a no-op computed from the current total.
            Err(error) if error.code() == codes::ALREADY_EXISTS => {
                let current = self
                    .store
                    .progression(award.account_id)
                    .await?
                    .map_or(0, |progression| progression.xp);
                let level = level_for_xp(current);
                return Ok(AwardOutcome {
                    requested: award.amount,
                    granted: 0,
                    before: current,
                    after: current,
                    level_before: level,
                    level_after: level,
                    capped,
                });
            }
            Err(error) => return Err(error),
        };

        if capped {
            self.meters.xp_capped(award.source);
        }
        self.meters.xp_awarded(award.source);
        let level_before = level_for_xp(change.before);
        let level_after = level_for_xp(change.after);
        if level_after > level_before {
            // The cached level is a projection of the total; rewrite it only when a threshold
            // was crossed, and tell the account its level rose.
            self.store
                .set_level(award.account_id, level_after, award.at)
                .await?;
            self.meters.levelup();
            self.announce(Announcement {
                account_id: award.account_id,
                kind: NotificationKind::LevelUp,
                actor_id: None,
                subject_id: None,
                at: award.at,
            })
            .await;
        }
        Ok(AwardOutcome {
            requested: award.amount,
            granted,
            before: change.before,
            after: change.after,
            level_before,
            level_after,
            capped,
        })
    }

    async fn award_badge(&self, grant: BadgeGrant) -> Result<bool> {
        let granted = self
            .store
            .award_badge(BadgeAward {
                account_id: grant.account_id,
                badge_code: grant.badge.code(),
                awarded_at: grant.at,
                ref_id: grant.ref_id,
            })
            .await?;
        // A first grant is worth congratulating; a repeat is silent, because nobody wants to be
        // told twice they earned the same badge (a job that runs twice is the usual cause).
        if granted {
            self.meters.badge_awarded(grant.badge);
            self.announce(Announcement {
                account_id: grant.account_id,
                kind: NotificationKind::Achievement,
                actor_id: None,
                subject_id: grant.ref_id,
                at: grant.at,
            })
            .await;
        }
        Ok(granted)
    }
}

/// Assembles the economy over the platform's shared collaborators, boxed behind [`Treasurer`].
///
/// The composition root's one entry point. It supplies the operating-system id source; a test
/// that needs deterministic ids constructs [`Economy::new`] with a seeded [`Random`] instead.
pub fn open(
    store: SharedStore,
    cache: SharedCache,
    limiter: SharedRateLimiter,
    announcer: SharedAnnouncer,
    catalogue: Catalogue,
    config: EconomyConfig,
    registry: &Registry,
) -> SharedTreasurer {
    Arc::new(Economy::new(
        store,
        cache,
        limiter,
        announcer,
        catalogue,
        config,
        Box::new(OsRandom) as Box<dyn Random>,
        registry,
    ))
}
