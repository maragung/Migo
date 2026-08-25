//! Counters for money moved, XP earned, badges granted, and boards served.
//!
//! # What may label a series here, and what may never
//!
//! Brief section 174 forbids a metric series labelled by account; this crate adds its own
//! prohibition, that no series is labelled by account *pair* either. A counter keyed on
//! "sender → recipient" would be the gifting graph — who favours whom, rebuilt from
//! Prometheus — and the social graph is exactly what section 174 keeps out of the metrics
//! endpoint. So a gift increments a counter for the *kind* of gift and nothing about who
//! sent it or who got it.
//!
//! There is also no series labelled by SKU. A SKU is unbounded — every seasonal theme, every
//! limited avatar item, mints one and leaves it behind forever — so spending is labelled by
//! [`Category`], which is closed at seven, and gifts by [`Gift`], closed at ten. A dashboard
//! that wants per-item revenue reads the ledger, which is where per-item questions belong;
//! the metrics endpoint answers "how much gifting is happening", not "who bought the
//! dragon".
//!
//! Every label domain in this module is a closed enum — currency, gift, category, source,
//! badge, and two small outcome enums — so the cardinality of the whole crate is fixed at
//! compile time, and adding a variant to any of them is a diff a reviewer sees.

use std::sync::Arc;

use migo_core::metrics::{Counter, Registry};
use migo_store::model::Currency;

use crate::model::{Badge, Category, Gift, Source};

/// A currency's metric label.
///
/// [`Currency`] lives in `migo-store` and carries no label of its own — a store has no
/// reason to name a currency for a dashboard — so the label is defined here, where the
/// dashboard is. Three values, closed.
const fn currency_label(currency: Currency) -> &'static str {
    match currency {
        Currency::Coins => "coins",
        Currency::Gems => "gems",
        Currency::Points => "points",
    }
}

/// The three currencies, in wire order, for registering every grant series at zero.
const CURRENCIES: [Currency; 3] = [Currency::Coins, Currency::Gems, Currency::Points];

/// What happened when a transaction was posted.
///
/// The idempotency signal, and the reason it is worth a series: a healthy client retries,
/// so a steady trickle of `duplicate` is normal and a *spike* is a client stuck in a resend
/// loop. Folding the two into one "posted" counter would hide the one number that tells a
/// retry storm from ordinary traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TxOutcome {
    /// The rows were written by this call.
    Created,
    /// The idempotency key already existed; nothing was written.
    Duplicate,
}

impl TxOutcome {
    pub(crate) const ALL: [Self; 2] = [Self::Created, Self::Duplicate];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Duplicate => "duplicate",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    /// The outcome of a store `Posted`.
    pub(crate) const fn of(is_new: bool) -> Self {
        if is_new {
            Self::Created
        } else {
            Self::Duplicate
        }
    }
}

/// Whether a cached read was served from the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheOutcome {
    /// Served from the cache.
    Hit,
    /// Computed from the store and then cached.
    Miss,
}

impl CacheOutcome {
    pub(crate) const ALL: [Self; 2] = [Self::Hit, Self::Miss];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Every series this crate publishes.
pub(crate) struct Meters {
    grants: Vec<Arc<Counter>>,
    gifts_sent: Vec<Arc<Counter>>,
    purchases: Vec<Arc<Counter>>,
    transactions: Vec<Arc<Counter>>,
    insufficient_balance: Arc<Counter>,
    xp_awarded: Vec<Arc<Counter>>,
    xp_capped: Vec<Arc<Counter>>,
    levelups: Arc<Counter>,
    badges: Vec<Arc<Counter>>,
    balance_reads: Arc<Counter>,
    leaderboard_reads: Vec<Arc<Counter>>,
}

/// Registers one counter per variant, each tagged `key` with the variant's own label.
///
/// The eight per-variant series share a shape — a name, a help string, and a single label
/// whose value is the variant's — so they share a builder rather than repeating the same
/// `map().collect()` eight times. Registering the whole variant set up front is what gives a
/// dashboard a flat line instead of a gap for an outcome nobody has hit yet; see [`Meters::new`].
fn per_variant<T>(
    registry: &Registry,
    name: &'static str,
    help: &'static str,
    key: &'static str,
    variants: &[T],
    label: impl Fn(&T) -> &'static str,
) -> Vec<Arc<Counter>> {
    variants
        .iter()
        .map(|variant| registry.counter(name, help, &[(key, label(variant))]))
        .collect()
}

impl Meters {
    /// Registers every series at zero.
    ///
    /// All of them, up front, so a dashboard shows a flat line rather than a gap for an
    /// outcome nobody has hit yet — a panel reading "no data" for "purchases refused for
    /// insufficient balance" is indistinguishable from a broken query, and the difference
    /// matters when somebody is asking why spending fell off a cliff.
    pub(crate) fn new(registry: &Registry) -> Self {
        Self {
            grants: per_variant(
                registry,
                "migo_economy_grants_total",
                "Currency issued into user accounts, by currency.",
                "currency",
                &CURRENCIES,
                |currency| currency_label(*currency),
            ),
            gifts_sent: per_variant(
                registry,
                "migo_economy_gifts_sent_total",
                "Gifts sent, by kind.",
                "gift",
                &Gift::ALL,
                |gift| gift.slug(),
            ),
            purchases: per_variant(
                registry,
                "migo_economy_purchases_total",
                "Items bought for the buyer's own account, by category.",
                "category",
                &Category::ALL,
                |category| category.slug(),
            ),
            transactions: per_variant(
                registry,
                "migo_economy_transactions_total",
                "Ledger transactions posted, by outcome.",
                "outcome",
                &TxOutcome::ALL,
                |outcome| outcome.label(),
            ),
            insufficient_balance: registry.counter(
                "migo_economy_insufficient_balance_total",
                "Purchases and gifts refused because the payer could not afford them.",
                &[],
            ),
            xp_awarded: per_variant(
                registry,
                "migo_economy_xp_awarded_total",
                "XP awards granted, by source.",
                "source",
                &Source::ALL,
                |source| source.label(),
            ),
            xp_capped: per_variant(
                registry,
                "migo_economy_xp_capped_total",
                "XP awards reduced or refused by a daily cap, by source.",
                "source",
                &Source::ALL,
                |source| source.label(),
            ),
            levelups: registry.counter(
                "migo_economy_levelups_total",
                "Times an account's level rose.",
                &[],
            ),
            badges: per_variant(
                registry,
                "migo_economy_badges_awarded_total",
                "Badges granted for the first time, by badge.",
                "badge",
                &Badge::ALL,
                |badge| badge.slug(),
            ),
            balance_reads: registry.counter(
                "migo_economy_balance_reads_total",
                "Balance and wallet reads served.",
                &[],
            ),
            leaderboard_reads: per_variant(
                registry,
                "migo_economy_leaderboard_reads_total",
                "Leaderboard pages served, by cache outcome.",
                "outcome",
                &CacheOutcome::ALL,
                |outcome| outcome.label(),
            ),
        }
    }

    pub(crate) fn grant(&self, currency: Currency) {
        if let Some(counter) = self.grants.get(currency as usize) {
            counter.inc();
        }
    }

    pub(crate) fn gift_sent(&self, gift: Gift) {
        if let Some(counter) = self.gifts_sent.get(gift.index()) {
            counter.inc();
        }
    }

    pub(crate) fn purchase(&self, category: Category) {
        if let Some(counter) = self.purchases.get(category.index()) {
            counter.inc();
        }
    }

    pub(crate) fn transaction(&self, outcome: TxOutcome) {
        if let Some(counter) = self.transactions.get(outcome.index()) {
            counter.inc();
        }
    }

    pub(crate) fn insufficient_balance(&self) {
        self.insufficient_balance.inc();
    }

    pub(crate) fn xp_awarded(&self, source: Source) {
        if let Some(counter) = self.xp_awarded.get(source.index()) {
            counter.inc();
        }
    }

    pub(crate) fn xp_capped(&self, source: Source) {
        if let Some(counter) = self.xp_capped.get(source.index()) {
            counter.inc();
        }
    }

    pub(crate) fn levelup(&self) {
        self.levelups.inc();
    }

    pub(crate) fn badge_awarded(&self, badge: Badge) {
        if let Some(counter) = self.badges.get(badge.index()) {
            counter.inc();
        }
    }

    pub(crate) fn balance_read(&self) {
        self.balance_reads.inc();
    }

    pub(crate) fn leaderboard_read(&self, outcome: CacheOutcome) {
        if let Some(counter) = self.leaderboard_reads.get(outcome.index()) {
            counter.inc();
        }
    }
}
