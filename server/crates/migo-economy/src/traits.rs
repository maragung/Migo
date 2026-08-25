//! What this crate offers the layer above, and the one port it asks for in return.
//!
//! # Two shapes of method, and why they differ
//!
//! Every method that a client reaches takes a [`Caller`]: it is spending its own money or
//! reading its own standing, and the rate limiter charges its budget. The three that do not
//! — [`Treasurer::grant`], [`Treasurer::award`], [`Treasurer::award_badge`] — are the
//! server crediting somebody, on behalf of an event it already observed. A client cannot ask
//! for XP, currency, or a badge, because there is no method with which to ask; the crate that
//! observed the game or the event calls those through its own port, and the composition root
//! wires this service in as the implementation.
//!
//! # Why awarding does not depend on `migo-games` or `migo-notify`
//!
//! Both would be a layer-3 crate depending on a layer-3 crate, which is how a dependency
//! graph grows a cycle. Awarding is inverted instead: `migo-games` defines the port it needs
//! and this service satisfies it, so the arrow points from games to a trait games owns, not
//! to this crate. Telling somebody their level rose is inverted the same way, through
//! [`Announcer`] — this crate hands an [`Announcement`] to a port, and the composition root's
//! adapter turns it into a `migo-notify` event. This crate never learns that `migo-notify`
//! exists.

use std::sync::Arc;

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::NotificationKind;
use migo_store::model::{BadgeAward, Currency, Entitlement, GiftSent};

use crate::model::{
    Award, AwardOutcome, BadgeGrant, Board, Caller, GiftOutcome, GiftTally, Grant,
    GrantReceipt, LedgerEntry, Listing, ProgressionView, PurchaseOutcome, Rank, SendGift, Sku,
    Wallet,
};

/// One thing worth telling somebody about: a gift arrived, a level rose, a badge was earned.
///
/// Deliberately small. It carries who to tell, what kind of thing happened, and the two ids a
/// notification renders — the actor who caused it and the subject it is about — and nothing
/// else. There is no text field, because the payload of a push is a wake-up and not a
/// sentence (brief section 44); the words are the notifying layer's to choose from the kind.
#[derive(Clone, Copy, Debug)]
pub struct Announcement {
    /// Who should be told.
    pub account_id: Id,
    /// What happened, in the protocol's closed vocabulary.
    pub kind: NotificationKind,
    /// Who caused it — the gift's sender — where somebody did.
    pub actor_id: Option<Id>,
    /// What it is about — the gift, the badge — where there is a subject.
    pub subject_id: Option<Id>,
    /// When it happened.
    pub at: Timestamp,
}

/// The port through which the economy tells someone something happened.
///
/// One method, because there is one thing to do. An implementation bridges to whatever the
/// deployment uses to notify — in production, an adapter over `migo-notify`; in a test, a
/// recorder. An `Err` is the notifying layer failing, and the service treats it the way
/// `migo-notify` treats a push that would not send: logged and swallowed, because a gift that
/// failed to buzz is still a gift that arrived and was paid for.
#[async_trait]
pub trait Announcer: Send + Sync {
    /// Tells one account that something happened.
    async fn announce(&self, announcement: Announcement) -> Result<()>;
}

/// An announcer that drops everything on the floor.
///
/// The default when a deployment has wired no notifier, which is the normal state of a
/// development machine and of every test that is not testing notifications. Silence is the
/// right default precisely because the economy does not depend on the announcement arriving:
/// the gift is recorded, the balance is right, and the recipient finds it the next time they
/// look, notification or none.
#[derive(Clone, Copy, Debug, Default)]
pub struct Silent;

#[async_trait]
impl Announcer for Silent {
    async fn announce(&self, _announcement: Announcement) -> Result<()> {
        Ok(())
    }
}

/// The economy, as the layer above reaches it.
///
/// Currencies, the catalogue, gifts, purchases, experience, badges, and the leaderboards
/// over them — the whole of brief sections 28 to 32 behind one erased trait.
#[async_trait]
pub trait Treasurer: Send + Sync {
    /// Everything the catalogue sells, in code order.
    ///
    /// Synchronous and uncharged: the catalogue is configuration held in memory, not a store
    /// read, so browsing the shop costs nothing and cannot fail. A deployment's whole price
    /// list is small enough to hand back whole.
    fn listings(&self) -> Vec<Listing>;

    /// The listing for one code, if it is sold.
    fn listing(&self, sku: &Sku) -> Option<Listing>;

    /// The caller's three balances.
    async fn wallet(&self, caller: &Caller) -> Result<Wallet>;

    /// The caller's recent movements in one currency, newest first.
    async fn statement(
        &self,
        caller: &Caller,
        currency: Currency,
        limit: u16,
    ) -> Result<Vec<LedgerEntry>>;

    /// Buys a catalogue item for the caller's own account.
    ///
    /// Refuses with `ALREADY_EXISTS` if the caller already owns it, and with
    /// `INSUFFICIENT_BALANCE` if they cannot afford it — both before any money moves. The
    /// `client_key` is the caller's idempotency key; a repeat returns the first purchase.
    async fn purchase(
        &self,
        caller: &Caller,
        sku: &Sku,
        client_key: &str,
    ) -> Result<PurchaseOutcome>;

    /// Sends a gift to another account.
    ///
    /// Two transactions, both idempotent: the sender is charged and the gift recorded, then
    /// the recipient is granted the gift's reputation in non-transferable points. The
    /// recipient is told through the [`Announcer`]. Refuses with `INSUFFICIENT_BALANCE` if
    /// the sender cannot afford it, before anything is written.
    async fn send_gift(&self, caller: &Caller, gift: SendGift) -> Result<GiftOutcome>;

    /// Everything the caller owns, oldest first.
    async fn entitlements(&self, caller: &Caller) -> Result<Vec<Entitlement>>;

    /// Gifts the caller has been given, newest first.
    async fn gifts_received(&self, caller: &Caller, limit: u16) -> Result<Vec<GiftSent>>;

    /// The public gift shelf of an account: how many of each gift it has been given.
    ///
    /// A profile decoration, so `of_account` may be anyone. It reveals only counts by gift
    /// code, which is what a profile already shows to anyone who visits it.
    async fn gift_shelf(&self, caller: &Caller, of_account: Id) -> Result<Vec<GiftTally>>;

    /// An account's XP and level, with the progress bar computed.
    ///
    /// Public, like the shelf: a level is worn openly. An account that has never earned
    /// anything reads as level one with no progress, not as an error.
    async fn progression(&self, caller: &Caller, of_account: Id) -> Result<ProgressionView>;

    /// An account's badges, newest first. Public.
    async fn badges(&self, caller: &Caller, of_account: Id) -> Result<Vec<BadgeAward>>;

    /// A page of a leaderboard, ranked and cached.
    ///
    /// The window decides the span (section 32's weekly, monthly, all-time), the scope the
    /// population (global, one country, one room). The result is cached for a short time, so
    /// a board that thousands watch is one read per cache period rather than one per viewer.
    async fn leaderboard(&self, caller: &Caller, board: Board) -> Result<Vec<Rank>>;

    /// Issues currency into an account. Server-facing; see [`Grant`].
    async fn grant(&self, grant: Grant) -> Result<GrantReceipt>;

    /// Awards XP for one of section 30's activities, applying the daily caps.
    ///
    /// Server-facing; see [`Award`]. The returned [`AwardOutcome`] says how much survived the
    /// caps and whether the account's level rose, so the caller need not read again to find
    /// out. A level-up is announced through the [`Announcer`].
    async fn award(&self, award: Award) -> Result<AwardOutcome>;

    /// Grants a badge, idempotently. Returns whether this call was the one that granted it.
    ///
    /// Server-facing; see [`BadgeGrant`]. A newly granted badge is announced through the
    /// [`Announcer`]; a repeat grant is silent, because nobody wants to be congratulated
    /// twice for the same badge.
    async fn award_badge(&self, grant: BadgeGrant) -> Result<bool>;
}

/// The economy service, shared.
pub type SharedTreasurer = Arc<dyn Treasurer>;

/// An announcer, shared.
pub type SharedAnnouncer = Arc<dyn Announcer>;
