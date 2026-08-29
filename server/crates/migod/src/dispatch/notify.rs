//! NOTIFY domain dispatch: the push-inbox opcodes.
//!
//! Two opcodes route here, both answered (section 139):
//!
//! - `NOTIFICATION_ACK` (145) — the client has opened its bell and wants everything up to one
//!   notification marked read. The watermark is the acked id's own embedded time, which is exactly
//!   the "I have opened the bell" gesture the service already understands: [`Notifier::acknowledge`]
//!   marks every row at or before that instant, so the one id the client named and anything that
//!   arrived before it both go quiet in one call. The body carries a single `id`, not a list, so
//!   there is no per-id loop to race with a notification that lands mid-flight.
//! - `NOTIFICATION_LIST` (146) — one page of the inbox, newest first. The domain owns no cursor, so
//!   the requested `cursor` is accepted and then dropped: the service returns a single watermark page
//!   and the client pages by re-asking with a higher limit, not by handing the server a bookmark it
//!   would have to store. The reply's `next_cursor` is therefore always absent.
//!
//! Both handlers build the notify [`Caller`] from the gateway-proven [`Identity`], decode the body
//! (a bad body is the client's fault and comes back as a wire fault, never a panic), call exactly one
//! service method, and answer with [`ClientContext::reply`], reusing the request's opcode and
//! correlation (section 139).

use migo_core::Error;
use migo_gateway::ClientContext;
use migo_notify::SharedNotifier;
use migo_notify::Caller as NotifyCaller;
use migo_protocol::{
    from_frame, fault, Acknowledged, Frame, InboxItem, InboxReq, InboxResponse, NotificationAck,
};

/// Marks every notification up to the one the client named as read.
///
/// The acked `id` is time-ordered, so its embedded timestamp is the watermark
/// [`Notifier::acknowledge`] wants: one call clears the named row and everything older. A list could
/// not do this safely — a notification arriving while the request is in flight would be missed — and
/// the wire already sends a single id for exactly that reason.
pub(crate) async fn handle_ack(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedNotifier,
) -> Result<(), Error> {
    let caller = NotifyCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: NotificationAck = from_frame(frame).map_err(fault::from_wire)?;
    let through = request.id.timestamp();
    svc.acknowledge(&caller, through).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Returns one page of the caller's inbox, newest first.
///
/// The domain has no pagination, so `cursor` is read and ignored and `next_cursor` is always
/// `None`. The `limit` is clamped to the service's own page ceiling inside
/// [`Notifier::inbox`]; here it is only narrowed to `u16` to match that method's signature.
pub(crate) async fn handle_list(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedNotifier,
) -> Result<(), Error> {
    let caller = NotifyCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: InboxReq = from_frame(frame).map_err(fault::from_wire)?;
    let inbox = svc.inbox(&caller, request.limit as u16).await?;
    let items = inbox
        .items
        .iter()
        .map(|item| InboxItem {
            id: item.notification_id,
            kind: item.kind.to_wire().to_string(),
            at: item.at,
            title: None,
            body: None,
            conversation_id: None,
            room_id: item.room_id,
            actor_id: item.actor_id,
        })
        .collect();
    ctx.reply(&InboxResponse {
        items,
        next_cursor: None,
    })
}

