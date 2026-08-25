//! Gifts, currency, experience, and badges — brief sections 28 to 32 — on a double-entry
//! ledger that is built so it cannot pay out.
//!
//! # The ledger, and the one rule that shapes everything
//!
//! Every movement of value is a transaction whose legs sum to zero: currency is not created
//! or destroyed by a gift or a purchase, only moved between accounts. Currency enters the
//! system only from a **mint** account, which is the single account allowed to run negative,
//! so the sum of every real balance stays exactly equal to what the mint has issued. That is
//! not bookkeeping neatness — it is the invariant a scheduled audit checks
//! ([`migo_store::traits::EconomyStore::currency_sum`]), and a currency whose sum drifts from
//! zero is a bug caught before anybody can spend the difference.
//!
//! Points — the reputation a gift confers — are minted the same way and are deliberately
//! **not transferable and not spendable**. Section 87 forbids anything resembling a cash-out
//! without regulatory review, and section 37 forbids real-money gambling; a reputation score
//! that could be moved between accounts or exchanged for spendable currency would be the first
//! step towards both. So points go up and never sideways.
//!
//! # What a client may and may not ask for
//!
//! A client spends its own money and reads its own standing — [`Treasurer::purchase`],
//! [`Treasurer::send_gift`], [`Treasurer::wallet`], and the public reads over profiles and
//! boards. It **cannot** ask for currency, experience, or a badge: there is no method with
//! which to ask. Those three — [`Treasurer::grant`], [`Treasurer::award`],
//! [`Treasurer::award_badge`] — are server-facing, called by the crate that observed the game
//! or the event, through a port that crate owns. Section 29's anti-abuse rule lives there:
//! experience is capped over a rolling day, per source and overall, read from durable rows so
//! a cache restart cannot reset an abuser's limit.
//!
//! # What it depends on, and what it refuses to
//!
//! The store holds every rule that must survive a crash. This crate composes accounts, prices
//! from the [`Catalogue`], and translates the store's answer. It notifies a gift's recipient
//! through the [`Announcer`] port — never `migo-notify` directly — and satisfies the awarding
//! ports other crates define rather than depending on them, so no arrow from this layer points
//! sideways into another layer-3 crate. See [`traits`] for the two shapes of method and why
//! they differ, and [`service`] for why the service is thin over the store.
//!
//! # Getting one
//!
//! ```ignore
//! let treasury = migo_economy::open(
//!     store,
//!     cache,
//!     limiter,
//!     announcer,                       // an adapter over migo-notify, or `Arc::new(Silent)`
//!     Catalogue::with_default_gifts(),
//!     EconomyConfig::default(),
//!     &registry,
//! );
//! let outcome = treasury.send_gift(&caller, SendGift { /* ... */ }).await?;
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]

pub mod catalogue;
mod metrics;
pub mod model;
pub mod service;
pub mod traits;

pub use crate::catalogue::Catalogue;
pub use crate::model::{
    level_for_xp, xp_for_level, Attributes, Award, AwardOutcome, Badge, BadgeGrant, Board,
    BoardScope, Caller, Category, EconomyConfig, Gift, GiftOutcome, GiftTally, Grant, GrantReceipt,
    LedgerEntry, Listing, Price, ProgressionView, PurchaseOutcome, Rank, Reason, SendGift, Sku,
    Source, Wallet, Window, MAX_SKU_LEN,
};
pub use crate::service::{open, Economy};
pub use crate::traits::{
    Announcement, Announcer, SharedAnnouncer, SharedTreasurer, Silent, Treasurer,
};
