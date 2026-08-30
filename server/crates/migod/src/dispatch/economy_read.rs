//! The ECONOMY read opcodes: the shop window, the statement, and the standing an
//! account wears in public.
//!
//! Five opcodes, each a thin translation from a wire frame onto one
//! [`Treasurer`](migo_economy::traits::Treasurer) method. The service owns every rule
//! — the price list, the page clamps, the leaderboard cache, the rate charge — so
//! these handlers only build the [`Caller`](migo_economy::Caller), decode the body
//! with [`from_frame`], await the service, and [`reply`](ClientContext::reply). The
//! shape follows the other dispatch modules exactly; the write side of the economy
//! (`GIFT_SEND`) and the wallet read live next door in
//! [`economy`](super::economy::handle_gift_send).
//!
//! # Opcode → method map
//!
//! | Opcode           | Wire payload      | Service method           | Response               |
//! |------------------|-------------------|--------------------------|------------------------|
//! | `GIFT_CATALOGUE` | `GiftCatalogueReq`| `Treasurer::listings`    | `GiftCatalogueResponse`|
//! | `LEDGER_HISTORY` | `LedgerReq`       | `Treasurer::statement`   | `LedgerResponse`       |
//! | `PROGRESSION`    | `ProgressionReq`  | `Treasurer::progression` | `ProgressionWire`      |
//! | `BADGES`         | `BadgesReq`       | `Treasurer::badges`      | `BadgesResponse`       |
//! | `LEADERBOARD`    | `LeaderboardReq`  | `Treasurer::leaderboard` | `LeaderboardResponse`  |
//!
//! Three of the five read an account the wire names rather than the caller. That is
//! the domain's own posture — a level is worn openly and a shelf is a profile
//! decoration — and the service enforces nothing further because there is nothing
//! further to enforce: the answers carry standing, never balances.

use migo_core::Error;
use migo_economy::{
    Board, BoardScope, Caller as EconomyCaller, LedgerEntry, Reason, SharedTreasurer, Window,
};
use migo_gateway::ClientContext;
use migo_protocol::{
    fault, from_frame, BadgeWire, BadgesReq, BadgesResponse, Frame, GiftCatalogueReq,
    GiftCatalogueResponse, GiftListing, LeaderboardReq, LeaderboardResponse, LedgerEntryWire,
    LedgerReq, LedgerResponse, ProgressionReq, ProgressionWire, RankWire,
};
use migo_store::{model::Currency, MAX_PAGE};

/// Lists the catalogue and replies with it.
///
/// `listings` is synchronous and uncharged — the price list is configuration held in
/// memory, not a store read — so this is the whole handler. The wire entry carries its
/// own `category`, so a deployment whose `Treasurer` also sells themes and frames is
/// not mislabeled by appearing in the same list as the gifts; filtering here would
/// make "what the shop shows" a property of this function rather than of the one
/// catalogue method.
///
/// `name` is the listing's slug: a price list has machine names, not localized ones,
/// and a client that renders the shop already localizes by slug — the same posture as
/// the notification kinds, whose words are the client's to choose. `price` is positive
/// by the catalogue's own construction; the `max(0)` is the unsigned narrowing, not a
/// correction.
pub(crate) async fn handle_gift_catalogue(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedTreasurer,
) -> Result<(), Error> {
    let _request: GiftCatalogueReq = from_frame(frame).map_err(fault::from_wire)?;
    let gifts = svc
        .listings()
        .into_iter()
        .map(|listing| GiftListing {
            sku: listing.sku.code(),
            name: listing.sku.slug().to_string(),
            price: listing.price.amount.max(0) as u64,
            category: listing.sku.category().slug().to_string(),
        })
        .collect();
    ctx.reply(&GiftCatalogueResponse { gifts })
}

/// Reads the caller's statement and replies with the page.
///
/// The wire names no currency, so the statement is the coins one — the spendable
/// balance `BALANCE_FETCH` reports, which is the account a statement screen is about;
/// a gems or points statement is a gap in this build's wire, not a choice the handler
/// makes per request. An absent `limit` asks for the store's own largest page
/// ([`MAX_PAGE`]), and a present one is narrowed into the same bounds here only
/// because the wire carries a `u32` and the method a `u16`, and the narrowing must not
/// wrap — the service clamps again inside the call, so the real page ceiling stays with
/// the rule.
pub(crate) async fn handle_ledger_history(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedTreasurer,
) -> Result<(), Error> {
    let caller = EconomyCaller {
        account_id: ctx.identity().account_id(),
        device_id: ctx.identity().device_id(),
        tier: ctx.identity().tier,
        now: ctx.now(),
        request_id: None,
    };
    let request: LedgerReq = from_frame(frame).map_err(fault::from_wire)?;
    let limit = request
        .limit
        .map_or(MAX_PAGE, |limit| limit.clamp(1, u32::from(MAX_PAGE)) as u16);
    let entries = svc.statement(&caller, Currency::Coins, limit).await?;
    ctx.reply(&LedgerResponse {
        entries: entries.into_iter().map(wire_entry).collect(),
    })
}

/// Reads one account's XP and level and replies with the progress bar.
///
/// Public, like the badges: a level is worn openly, and an account that never earned
/// anything reads as level one with an empty bar, not as an error. The signed domain
/// fields are narrowed with `max(0)` — XP cannot be negative in the store, so the
/// clamp is the type boundary, not a correction.
pub(crate) async fn handle_progression(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedTreasurer,
) -> Result<(), Error> {
    let caller = EconomyCaller {
        account_id: ctx.identity().account_id(),
        device_id: ctx.identity().device_id(),
        tier: ctx.identity().tier,
        now: ctx.now(),
        request_id: None,
    };
    let request: ProgressionReq = from_frame(frame).map_err(fault::from_wire)?;
    let view = svc.progression(&caller, request.of_account).await?;
    ctx.reply(&ProgressionWire {
        account_id: view.account_id,
        xp: view.xp.max(0) as u64,
        level: view.level.max(0) as u32,
        xp_into_level: view.xp_into_level.max(0) as u64,
        xp_for_next_level: view.xp_for_next_level.max(0) as u64,
    })
}

/// Reads one account's badges and replies with them.
///
/// Public, like the progression. `BadgeAward`'s `ref_id` — what earned the badge, where
/// something nameable did — has no wire field and is dropped, the same honest silence
/// as the dropped fields in the profile projection.
pub(crate) async fn handle_badges(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedTreasurer,
) -> Result<(), Error> {
    let caller = EconomyCaller {
        account_id: ctx.identity().account_id(),
        device_id: ctx.identity().device_id(),
        tier: ctx.identity().tier,
        now: ctx.now(),
        request_id: None,
    };
    let request: BadgesReq = from_frame(frame).map_err(fault::from_wire)?;
    let badges = svc.badges(&caller, request.of_account).await?;
    ctx.reply(&BadgesResponse {
        badges: badges
            .into_iter()
            .map(|award| BadgeWire {
                badge_code: award.badge_code,
                awarded_at: award.awarded_at,
            })
            .collect(),
    })
}

/// Reads a leaderboard and replies with the page.
///
/// The wire names a board by string; the store can rank exactly one thing — XP — over
/// a scope and a window, so `xp` maps onto the global, all-time board and nothing
/// else. `reputation`, which the wire's own doc line offers as the other example,
/// would be a board over points, and no such ranking exists: answering it with the XP
/// board would be a board titled one thing and ranking another, so it is
/// `FEATURE_DISABLED` — the wired-but-unbuilt posture the room listing filters take —
/// and any further name is the client's fault.
///
/// An absent `limit` asks for `u16::MAX` and lets the service cut it down to the
/// deployment's own ceiling, because the ceiling is configuration the service holds
/// and this handler cannot see; a present one is narrowed to `u16` for the method's
/// signature and bounded the same way inside the call.
pub(crate) async fn handle_leaderboard(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedTreasurer,
) -> Result<(), Error> {
    let caller = EconomyCaller {
        account_id: ctx.identity().account_id(),
        device_id: ctx.identity().device_id(),
        tier: ctx.identity().tier,
        now: ctx.now(),
        request_id: None,
    };
    let request: LeaderboardReq = from_frame(frame).map_err(fault::from_wire)?;
    if request.board != "xp" {
        if request.board == "reputation" {
            return Err(fault::feature_disabled("the reputation leaderboard"));
        }
        return Err(fault::validation(
            "board",
            "this build ranks one board: 'xp'",
        ));
    }
    let board = Board {
        scope: BoardScope::Global,
        window: Window::AllTime,
        limit: request
            .limit
            .map_or(u16::MAX, |limit| u16::try_from(limit).unwrap_or(u16::MAX)),
    };
    let ranks = svc.leaderboard(&caller, board).await?;
    ctx.reply(&LeaderboardResponse {
        ranks: ranks
            .into_iter()
            .map(|rank| RankWire {
                position: rank.position,
                account_id: rank.account_id,
                xp: rank.xp.max(0) as u64,
                level: rank.level.max(0) as u32,
            })
            .collect(),
    })
}

/// Projects one statement line onto the wire struct.
///
/// The wire's `amount` is a magnitude — "the reason's direction is the sign", its own
/// doc line says — so the caller's signed movement becomes `unsigned_abs` and a client
/// reads debit-or-credit from the reason, exactly as the field intends.
/// `balance_after` is clamped into the unsigned narrowing rather than trusted, though
/// a user account cannot go negative: the store's overdraft floor is what makes the
/// clamp unreachable, and an unreachable clamp is cheaper than a wrapped eighteen
/// quintillion.
fn wire_entry(entry: LedgerEntry) -> LedgerEntryWire {
    LedgerEntryWire {
        tx_id: entry.tx_id,
        reason: wire_reason(entry.reason),
        amount: entry.amount.unsigned_abs(),
        balance_after: entry.balance_after.max(0) as u64,
        at: entry.at,
        ref_id: entry.ref_id,
    }
}

/// The word for a ledger reason.
///
/// The domain's [`Reason`] has no label of its own — the XP sources got one because
/// metrics needed it, and no metric is labelled by reason — so the wire vocabulary is
/// mapped here, one arm per variant, snake_case to match every other machine name in
/// the protocol. `None` is a reason a newer node wrote; the entry still shows, under
/// a word a client cannot branch on, because hiding the line would hide money that
/// moved.
fn wire_reason(reason: Option<Reason>) -> String {
    match reason {
        Some(Reason::Grant) => "grant",
        Some(Reason::GiftPurchase) => "gift_purchase",
        Some(Reason::GiftReputation) => "gift_reputation",
        Some(Reason::Purchase) => "purchase",
        Some(Reason::Refund) => "refund",
        Some(Reason::GameStake) => "game_stake",
        Some(Reason::GamePayout) => "game_payout",
        Some(Reason::Adjustment) => "adjustment",
        None => "unknown",
    }
    .to_string()
}
