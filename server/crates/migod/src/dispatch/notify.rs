//! NOTIFY domain dispatch: the push-inbox and push-registration opcodes.
//!
//! Four opcodes route here, all answered (section 139):
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
//! - `PUSH_REGISTER` (147) — the calling device hands over its push token. The token is sealed and
//!   hashed inside the service and never seen by anything else, and re-registration replaces whatever
//!   the device had, so a client can send this on every cold start without ceremony.
//! - `PUSH_UNREGISTER` (148) — the calling device withdraws its token. Sign-out and
//!   "turn notifications off" both land here; the service refuses nothing on this path, because a
//!   phone that cannot stop buzzing is worse than one that re-registers.
//!
//! All handlers build the notify [`Caller`] from the gateway-proven [`Identity`], decode the body
//! (a bad body is the client's fault and comes back as a wire fault, never a panic), call exactly one
//! service method, and answer with [`ClientContext::reply`], reusing the request's opcode and
//! correlation (section 139).

use migo_core::Error;
use migo_gateway::ClientContext;
use migo_notify::{Caller as NotifyCaller, RawToken, SharedNotifier};
use migo_protocol::{
    fault, from_frame, Acknowledged, Frame, InboxItem, InboxReq, InboxResponse, NotificationAck,
    PushRegister, PushUnregister,
};
use migo_store::model::{Device, PushProvider};
use migo_store::SharedStore;

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

/// Records the calling device's push registration.
///
/// The wire's `provider` field is read and deliberately dropped. Which push service carries a
/// wake-up is a deployment fact — [`PushProvider`] is not in the protocol schema on purpose — and
/// a client that could name its provider could name one this deployment does not run. The platform
/// is read from the device row recorded at sign-in (never from the frame, per
/// [`Notifier::register`]'s own rule), and the provider is derived from it the same way the notify
/// suite's client stand-in does: Apple platforms go to APNs, the web to Web Push, everything else
/// to FCM.
///
/// The device row is read rather than assumed for a second reason: `set_push_registration` refuses
/// an unknown or revoked device with `NOT_FOUND`, but reading it here keeps the lookup to one store
/// call and gives the refusal a chance to happen before the token is sealed. The token itself is
/// sealed and hashed inside the service; nothing here sees it as anything but the wire string the
/// client sent.
pub(crate) async fn handle_register(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    store: &SharedStore,
    svc: &SharedNotifier,
) -> Result<(), Error> {
    let caller = NotifyCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: PushRegister = from_frame(frame).map_err(fault::from_wire)?;
    let device = device_of(store, caller.device_id).await?;
    let token = RawToken::new(
        request.token,
        provider_for(device.platform),
        device.platform,
    );
    svc.register(&caller, token).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Forgets the calling device's push registration.
///
/// The body is empty on purpose: the registration affected is the calling device's own, taken from
/// the session's identity, because a request that named another device's id would be a request to
/// silence somebody else's phone. The service deliberately does not charge the rate limiter on
/// this path — a sign-out that could be refused for spending is a phone that keeps buzzing for an
/// account somebody deliberately left — but the opcode itself carries cost 1 rather than 0,
/// because the free-opcode flood exemption is reserved for frames the wire depends on, and a
/// client that cannot afford a priced withdrawal can simply drop the registration and let the
/// staleness sweep retire it.
pub(crate) async fn handle_unregister(
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
    // Decoded and dropped: the struct is the wire's shape, not a source of facts. A frame that
    // will not decode is still the client's fault and still a wire fault.
    let _request: PushUnregister = from_frame(frame).map_err(fault::from_wire)?;
    svc.unregister(&caller).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Reads one device row, refusing the request when it is gone.
///
/// A missing row and a revoked row are the same answer here: the device the session claims is not
/// one the store will hold a registration for, and the honest reply names neither which nor why.
async fn device_of(store: &SharedStore, device_id: migo_core::Id) -> Result<Device, Error> {
    store
        .device_by_id(device_id)
        .await?
        .ok_or_else(|| fault::not_found("device"))
}

/// Which push service serves a platform.
///
/// The same mapping the notify suite's test client uses, so the dispatcher and the suite agree on
/// what a platform means. `Unknown` maps to FCM but never gets that far: the platform is carried
/// through to [`RawToken`] unchanged, and the service refuses an `Unknown` platform before the
/// provider is ever used — a device row that never recorded a platform is a claim the notify
/// service is the right place to reject, not this mapping.
const fn provider_for(platform: migo_protocol::Platform) -> i16 {
    use migo_protocol::Platform;
    match platform {
        Platform::Ios => PushProvider::Apns.to_i16(),
        Platform::Web => PushProvider::WebPush.to_i16(),
        Platform::Android
        | Platform::Desktop
        | Platform::Bot
        | Platform::LoadTest
        | Platform::Unknown => PushProvider::Fcm.to_i16(),
    }
}
