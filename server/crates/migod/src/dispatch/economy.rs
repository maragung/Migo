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
use migo_economy::{Caller as EconomyCaller, Gift, SendGift, SharedTreasurer, Sku};
use migo_gateway::ClientContext;
use migo_protocol::{
    fault, from_frame, EconomyEvent, Entitlement, EntitlementsReq, EntitlementsResponse, Frame,
    GiftSend, GiftSendResult, NotificationEvent, NotificationKind, Opcode, StorePurchase,
    StorePurchaseResult, Topic, TopicKind, WalletReq, WalletView,
};

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
    })?;

    // Two events, two audiences, and only on a first send — a retry that the service
    // deduplicated must not buzz the recipient a second time for a gift they already
    // have (the announcer inside the service holds the same rule for the row).
    if !outcome.duplicate {
        // The sender's own wallet changed, and their client may be watching for it:
        // an ECONOMY_EVENT on the sender's own topic is the live balance tick.
        let sender_topic = Topic {
            kind: TopicKind::User,
            id: caller.account_id,
        };
        ctx.publish(
            &sender_topic,
            Opcode::EconomyEvent,
            &EconomyEvent {
                kind: "gift_sent".to_string(),
                amount: u64::try_from(outcome.price.amount).unwrap_or(0),
                currency: "coins".to_string(),
            },
            None,
        )?;
        // The recipient's bell. The row is already stored by the announcer the
        // composition root bound; this is the realtime mirror of it, coalesced per
        // recipient the same way the out-of-band path coalesces.
        let recipient_topic = Topic {
            kind: TopicKind::User,
            id: outcome.recipient_id,
        };
        ctx.publish(
            &recipient_topic,
            Opcode::NotificationEvent,
            &NotificationEvent {
                kind: NotificationKind::Gift,
                at: ctx.now(),
                title: None,
                body: None,
                conversation_id: request.conversation_id,
                room_id: None,
                actor_id: Some(caller.account_id),
            },
            Some(crate::dispatch::coalesce_key_of(&outcome.recipient_id)),
        )?;
    }
    Ok(())
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

/// Buys a catalogue item for the session account and replies with the outcome.
///
/// The wire carries the catalogue code and the caller's idempotency key; the service owns every
/// rule — the price, the affordability, the single-ownership refusal — and the store writes the
/// entitlement and the ledger legs together. `tx_hash`, when the client paid on-chain, is logged
/// with the purchase for audit; the ledger is the accounting truth and the hash is the trail to
/// the chain, and neither is asked to be the other.
pub(crate) async fn handle_store_purchase(
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
    let request: StorePurchase = from_frame(frame).map_err(fault::from_wire)?;
    let sku = Sku::parse(&request.sku)
        .ok_or_else(|| fault::validation("sku", "unknown catalogue code"))?;
    let outcome = svc
        .purchase(&caller, &sku, &request.client_key)
        .await?;
    tracing::info!(
        account = %caller.account_id,
        sku = %request.sku,
        tx_hash = request.tx_hash.as_deref().unwrap_or(""),
        duplicate = outcome.duplicate,
        "store purchase"
    );
    ctx.reply(&StorePurchaseResult {
        sku: outcome.sku.code(),
        price: outcome.price.amount.max(0) as u64,
        duplicate: outcome.duplicate,
    })?;

    // The buyer's own wallet moved on a first purchase: an ECONOMY_EVENT on their own topic is
    // the live balance tick every other spend already publishes. A deduplicated retry is not a
    // second spend and must not tick twice.
    if !outcome.duplicate {
        let topic = Topic {
            kind: TopicKind::User,
            id: caller.account_id,
        };
        ctx.publish(
            &topic,
            Opcode::EconomyEvent,
            &EconomyEvent {
                kind: "purchase".to_string(),
                amount: u64::try_from(outcome.price.amount).unwrap_or(0),
                currency: "coins".to_string(),
            },
            None,
        )?;
    }
    Ok(())
}

/// Reads everything the session account owns and replies with it, oldest first.
///
/// The subject is the caller; ownership is private like a wallet, not public like a badge. The
/// store's entitlement row carries the acquiring transaction, which this build's wire does not
/// surface — a client that wants the trail reads its own ledger, whose ids are the same ids.
pub(crate) async fn handle_entitlements(
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
    let _request: EntitlementsReq = from_frame(frame).map_err(fault::from_wire)?;
    let owned = svc.entitlements(&caller).await?;
    ctx.reply(&EntitlementsResponse {
        items: owned
            .into_iter()
            .map(|entitlement| Entitlement {
                sku: entitlement.sku,
                acquired_at: entitlement.acquired_at,
            })
            .collect(),
    })
}
