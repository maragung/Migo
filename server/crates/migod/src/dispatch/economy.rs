//! The ECONOMY application opcodes: sending gifts and reading one's own wallet.
//!
//! Two opcodes, each a thin translation from a wire frame onto one
//! [`Treasurer`](migo_economy::traits::Treasurer) method. The service owns every rule — the
//! rate charge, the double-entry posting, the "a gift needs a different recipient" check, the
//! balance read — so these handlers only build the [`Caller`](migo_economy::Caller), decode
//! the body with [`from_frame`], await the single service method, and
//! [`reply`](ClientContext::reply) with the named response. The shape follows the other
//! dispatch modules exactly.
//!
//! # Opcode → method map
//!
//! | Opcode         | Wire payload    | Service method           | Response        |
//! |----------------|-----------------|--------------------------|-----------------|
//! | `GIFT_SEND`    | `GiftSend`      | `Treasurer::send_gift`   | `GiftSendResult`|
//! | `BALANCE_FETCH`| `WalletReq`     | `Treasurer::wallet`      | `WalletView`    |
//!
//! The wire names the gift by its catalogue slug (`GiftSend.gift`) and the recipient by id; the
//! handler maps the slug onto the closed [`Gift`](migo_economy::Gift) enum the service prices
//! against, refusing an unknown slug the same way the service refuses an unknown SKU. The
//! caller's own account — the payer for a gift, the subject of a wallet read — comes from the
//! session, never the frame.

use migo_core::Error;
use migo_economy::{Caller as EconomyCaller, Gift, SendGift, SharedTreasurer};
use migo_gateway::ClientContext;
use migo_protocol::{fault, from_frame, Frame, GiftSend, GiftSendResult, WalletReq, WalletView};

/// Sends a gift from the session account to `GiftSend.recipient` and replies with the result.
///
/// The wire names the gift by slug; that is mapped onto the [`Gift`] enum the catalogue knows,
/// and an unknown slug is the client's fault (`VALIDATION_FAILED`), never a panic. The id the
/// service returns keys the gift row; it is what the recipient's notification and the sender's
/// own shelf key on, so it goes back on the wire as `GiftSendResult.tx_id`.
pub(crate) async fn handle_gift_send(
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
    let request: GiftSend = from_frame(frame).map_err(fault::from_wire)?;
    let gift =
        Gift::from_slug(&request.gift).ok_or_else(|| fault::validation("gift", "unknown gift"))?;
    // The wire carries no client idempotency key, so one is derived from the recipient and the
    // sampled `now`. Stamping it with `now` keeps a fresh attempt from collapsing into a prior
    // one; a client that genuinely retries must send within the same instant to dedupe.
    let client_key = format!("{}:{}", request.recipient, ctx.now().as_millis());
    let outcome = svc
        .send_gift(
            &caller,
            SendGift {
                recipient_id: request.recipient,
                gift,
                conversation_id: request.conversation_id,
                client_key,
            },
        )
        .await?;
    ctx.reply(&GiftSendResult {
        ok: !outcome.duplicate,
        tx_id: Some(outcome.gift_id),
    })
}

/// Reads the session account's wallet and replies with its balances.
///
/// The request is empty; the subject is the caller. The spendable balance is the coins account
/// and the reputation balance is the points account, both of which the service creates on first
/// read — so a fresh account reads as zero, which is exactly what it is.
pub(crate) async fn handle_balance_fetch(
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
    let _request: WalletReq = from_frame(frame).map_err(fault::from_wire)?;
    let wallet = svc.wallet(&caller).await?;
    ctx.reply(&WalletView {
        balance: wallet.coins.max(0) as u64,
        points: wallet.points.max(0) as u64,
    })
}
