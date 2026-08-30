//! The CALLS application opcodes: the ring lifecycle and the sealed relay.
//!
//! Thirteen opcodes, each one a thin translation from a wire frame onto one
//! [`Callkeeper`](migo_calls::Callkeeper) method. The service owns every rule
//! — the idempotent invite, the state machine, the relay's device checks, the
//! expiry sweep — so these handlers only build the
//! [`Caller`](migo_calls::Caller), decode the body with [`from_frame`], await
//! the service, and [`reply`](ClientContext::reply). The one thing the
//! service cannot do is send a frame, because no connection context exists
//! inside it; publishing the returned events to the right *other* party's
//! user topic is the half that lives here.
//!
//! # Opcode → method map
//!
//! | Opcode             | Wire payload       | Service method            | Response            | Published                |
//! |--------------------|--------------------|---------------------------|---------------------|--------------------------|
//! | `CALL_INVITE`      | `CallInvite`       | `invite`                  | `CallInviteResult`  | invite event → callee    |
//! | `CALL_ANSWER`      | `CallAnswer`       | `answer`                  | `Acknowledged`      | state event → caller     |
//! | `CALL_DECLINE`     | `CallDecline`      | `decline`                 | `Acknowledged`      | state event → caller     |
//! | `CALL_CANCEL`      | `CallCancel`       | `cancel`                  | `Acknowledged`      | state event → callee     |
//! | `CALL_END`         | `CallEnd`          | `end`                     | `Acknowledged`      | state event → other party|
//! | `CALL_SDP`         | `CallSdp`          | `relay_sdp`               | `Acknowledged`      | relayed frame → target   |
//! | `CALL_ICE`         | `CallIce`          | `relay_ice`               | `Acknowledged`      | relayed frame → target   |
//! | `CALL_RENEGOTIATE` | `CallRenegotiate`  | `relay_sdp` (projected)   | `Acknowledged`      | relayed frame → target   |
//! | `CALL_KEY_UPDATE`  | `CallKeyUpdate`    | `call` (authorisation)    | `Acknowledged`      | key update → other party |
//! | `CALL_STATS`       | `CallStats`        | — (metrics only)          | `Acknowledged`      | —                        |
//! | `CALL_TURN_FETCH`  | `CallTurnFetch`    | `turn_servers`            | `CallTurnResponse`  | —                        |
//! | `CALL_SFU_JOIN`    | `CallInvite`       | —                         | `FEATURE_DISABLED`  | —                        |
//!
//! `CALL_INVITE_EVENT`, `CALL_STATE_EVENT`, and `CALL_SFU_EVENT` are
//! server-originated: the gateway refuses them from a client before the
//! dispatcher is ever asked, and the dispatcher's default arm answers
//! `FEATURE_DISABLED` naming the opcode if one ever arrives by another path.
//! This module *publishes* the first two; it never receives them.
//!
//! # Who hears what
//!
//! Every call event has an audience of exactly one account — the one that
//! did not send the frame. The sender has the reply, and their own client
//! knows what it just did; the other side has only the event. The handler
//! works the audience out from the call row (via [`Callkeeper::call`], the
//! service's own participant-checked read) rather than from the frame,
//! because the frame names devices and calls, never topics.
//!
//! # The relay's target
//!
//! `CALL_SDP` and `CALL_ICE` name a target *device*. The topic that reaches
//! it belongs to the account that owns it, and the call row is the only
//! place that says whose device it is — so the handler loads the call after
//! the relay succeeds (the relay is the authority on whether the target is
//! legitimate) and maps device → account through it.
//!
//! # The sealed answer on `CALL_ANSWER`
//!
//! `CallAnswer.sealed_answer` is read by nothing here. The SDP answer
//! travels as a `CALL_SDP` relay — which is also the frame that marks the
//! call `Connected` — so the copy on the answer frame is advisory, and
//! forwarding it as well would deliver the same sealed bytes to the caller
//! twice for one answer.

use migo_calls::{Caller as CallCaller, SharedCallkeeper};
use migo_core::Error;
use migo_gateway::ClientContext;
use migo_protocol::{
    fault, from_frame, Acknowledged, CallAnswer, CallCancel, CallDecline, CallEnd, CallIce,
    CallInvite, CallInviteResult, CallKeyUpdate, CallRenegotiate, CallSdp, CallStats,
    CallTurnFetch, CallTurnResponse, Frame, Opcode, Topic, TopicKind,
};

/// Invites a callee and rings them.
///
/// The reply carries the outcome for the caller's own screen — ringing, or
/// the status a retry needs to hear (declined, expired, blocked). The event,
/// when there is one, goes to the callee's user topic: the frame names the
/// callee, so no call row is needed to route it. Not coalesced; a ring is
/// Critical, and the callee's client dedupes by `call_id` if a retry ever
/// did produce two.
pub(crate) async fn handle_invite(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallInvite = from_frame(frame).map_err(fault::from_wire)?;
    let call_id = request.call_id;
    let callee_id = request.callee_id;
    let (outcome, event) = svc.invite(&caller, request).await?;
    ctx.reply(&CallInviteResult {
        call_id,
        status: outcome.status,
        expires_at: outcome.expires_at,
    })?;
    if let Some(event) = event {
        let topic = Topic {
            kind: TopicKind::User,
            id: callee_id,
        };
        if let Err(error) =
            ctx.publish_excluding_self(&topic, Opcode::CallInviteEvent, &event, None)
        {
            tracing::warn!(%error, "call invite event publication failed");
        }
    }
    Ok(())
}

/// Answers a ringing call.
///
/// The answering device is the connection's own, not the frame's claim: a
/// client-supplied device id would let one device answer for another and
/// misroute every sealed frame after it. The caller of the call hears the
/// `Connecting` state event.
pub(crate) async fn handle_answer(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallAnswer = from_frame(frame).map_err(fault::from_wire)?;
    let event = svc
        .answer(&caller, request.call_id, ctx.identity().device_id())
        .await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(event) = event {
        publish_to_caller_of(ctx, svc, &caller, request.call_id, &event).await;
    }
    Ok(())
}

/// Declines a ringing call; the caller hears `Ended(Declined)`.
pub(crate) async fn handle_decline(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallDecline = from_frame(frame).map_err(fault::from_wire)?;
    let event = svc.decline(&caller, request.call_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(event) = event {
        publish_to_caller_of(ctx, svc, &caller, request.call_id, &event).await;
    }
    Ok(())
}

/// Cancels a call before an answer; the callee hears `Ended(ByCaller)` and
/// stops ringing.
pub(crate) async fn handle_cancel(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallCancel = from_frame(frame).map_err(fault::from_wire)?;
    let event = svc.cancel(&caller, request.call_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(event) = event {
        publish_to_callee_of(ctx, svc, &caller, request.call_id, &event).await;
    }
    Ok(())
}

/// Ends an established call; the other party hears `Ended(reason)`.
///
/// The reason is the sender's claim and is relayed as claimed — the two
/// devices know who hung up, and the server's job is to carry the message,
/// not to arbitrate it.
pub(crate) async fn handle_end(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallEnd = from_frame(frame).map_err(fault::from_wire)?;
    let event = svc.end(&caller, request.call_id, request.reason).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(event) = event {
        publish_to_other_party(ctx, svc, &caller, request.call_id, &event).await;
    }
    Ok(())
}

/// Relays sealed SDP to the other device.
///
/// One frame is both the request and the payload: the relay method validates
/// the routing headers against the call row and returns the frame unchanged,
/// and this handler publishes it — still unchanged, still sealed — to the
/// account that owns the target device. `Critical` and uncoalesced: a lost
/// answer is a call that never connects.
pub(crate) async fn handle_sdp(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallSdp = from_frame(frame).map_err(fault::from_wire)?;
    relay_sdp(ctx, svc, &caller, request).await
}

/// Relays a mid-call renegotiation. `CallRenegotiate` and `CallSdp` are the
/// same relay with different names for the client's benefit; the service
/// knows one operation.
pub(crate) async fn handle_renegotiate(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallRenegotiate = from_frame(frame).map_err(fault::from_wire)?;
    relay_sdp(
        ctx,
        svc,
        &caller,
        CallSdp {
            call_id: request.call_id,
            from_device: request.from_device,
            to_device: request.to_device,
            sealed_sdp: request.sealed_sdp,
        },
    )
    .await
}

/// Relays sealed SDP after the service has validated its routing.
async fn relay_sdp(
    ctx: &ClientContext<'_>,
    svc: &SharedCallkeeper,
    caller: &CallCaller,
    request: CallSdp,
) -> Result<(), Error> {
    let relayed = svc.relay_sdp(caller, request).await?;
    let call = svc.call(caller, relayed.call_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    // The relay succeeded, so `to_device` is one of the call's two devices;
    // the row says which account's topic reaches it.
    let target = call
        .account_of_device(relayed.to_device)
        .unwrap_or(call.caller_id);
    publish_to_user(ctx, target, Opcode::CallSdp, &relayed);
    Ok(())
}

/// Relays a batch of sealed ICE candidates to the other device. Same shape
/// as the SDP relay; never coalesced, because candidates are additive facts
/// and collapsing two batches would lose candidates.
pub(crate) async fn handle_ice(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallIce = from_frame(frame).map_err(fault::from_wire)?;
    let relayed = svc.relay_ice(&caller, request).await?;
    let call = svc.call(&caller, relayed.call_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    let target = call
        .account_of_device(relayed.to_device)
        .unwrap_or(call.caller_id);
    publish_to_user(ctx, target, Opcode::CallIce, &relayed);
    Ok(())
}

/// Re-keys a live call's media encryption.
///
/// The payload is sealed key material the devices exchange; the server's
/// whole role is to check that the sender is a party to the call (the
/// service's own participant-checked read) and to hand the frame to the
/// other party. There is no service method for this because there is no
/// state to move — the epoch inside the ciphertext is the callers' business.
pub(crate) async fn handle_key_update(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallKeyUpdate = from_frame(frame).map_err(fault::from_wire)?;
    let call = svc.call(&caller, request.call_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    // A key update for a call that is over reaches nobody who cares, and
    // sending it would imply the call is still live.
    if call.state.is_live() {
        if let Some(other) = call.other_party(caller.account_id) {
            publish_to_user(ctx, other, Opcode::CallKeyUpdate, &request);
        }
    }
    Ok(())
}

/// Accepts aggregate call quality numbers.
///
/// Metrics only: the frame is `Droppable` by declaration, nothing is
/// computed server-side, and no event follows. The decode is still done — a
/// body that will not parse is a framing violation the client should hear
/// about, not silently drop.
pub(crate) async fn handle_stats(ctx: &ClientContext<'_>, frame: &Frame) -> Result<(), Error> {
    let _request: CallStats = from_frame(frame).map_err(fault::from_wire)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Answers a TURN fetch with the configured relays.
///
/// Empty until the operator configures them — an honest empty list a client
/// can act on (direct connection only) rather than credentials that point at
/// nothing.
pub(crate) async fn handle_turn_fetch(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    // No caller is built: `turn_servers` charges nothing and reads nobody,
    // and the frame was already priced by the gateway that delivered it.
    let request: CallTurnFetch = from_frame(frame).map_err(fault::from_wire)?;
    let servers = svc.turn_servers(request.call_id).await?;
    ctx.reply(&CallTurnResponse { servers })
}

/// Refuses an SFU group-call join.
///
/// Group calls are a separate deployment (brief section 166); this node
/// signals 1:1 calls only, and the honest answer for the opcode is the
/// feature's name, so a client can render "not on this server" instead of
/// retrying a join that will never succeed.
pub(crate) fn refuse_sfu_join() -> Result<(), Error> {
    Err(fault::feature_disabled(Opcode::CallSfuJoin.name()))
}

/// The domain caller for this connection, built the same way every module
/// builds one: the identity the gateway proved, and the one sampled `now`.
fn caller_of(ctx: &ClientContext<'_>) -> CallCaller {
    CallCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    )
}

/// Publishes a state event to the call's *caller* — the account whose invite
/// this all was, the one waiting for an answer.
async fn publish_to_caller_of(
    ctx: &ClientContext<'_>,
    svc: &SharedCallkeeper,
    caller: &CallCaller,
    call_id: migo_core::Id,
    event: &migo_protocol::CallStateEvent,
) {
    match svc.call(caller, call_id).await {
        Ok(call) => publish_state(ctx, call.caller_id, event),
        Err(error) => {
            tracing::warn!(%error, "call state event dropped: routing read failed")
        }
    }
}

/// Publishes a state event to the call's *callee* — the account whose phone
/// is ringing.
async fn publish_to_callee_of(
    ctx: &ClientContext<'_>,
    svc: &SharedCallkeeper,
    caller: &CallCaller,
    call_id: migo_core::Id,
    event: &migo_protocol::CallStateEvent,
) {
    match svc.call(caller, call_id).await {
        Ok(call) => publish_state(ctx, call.callee_id, event),
        Err(error) => {
            tracing::warn!(%error, "call state event dropped: routing read failed")
        }
    }
}

/// Publishes a state event to whichever of the two parties is not the actor.
async fn publish_to_other_party(
    ctx: &ClientContext<'_>,
    svc: &SharedCallkeeper,
    caller: &CallCaller,
    call_id: migo_core::Id,
    event: &migo_protocol::CallStateEvent,
) {
    match svc.call(caller, call_id).await {
        Ok(call) => {
            if let Some(other) = call.other_party(caller.account_id) {
                publish_state(ctx, other, event);
            }
        }
        Err(error) => {
            tracing::warn!(%error, "call state event dropped: routing read failed")
        }
    }
}

/// Publishes one `CALL_STATE_EVENT` to an account's user topic.
///
/// Excluding the originating connection: the actor already has the reply,
/// and while they are not normally subscribed to the other party's topic,
/// friends may be (presence), and an echo of their own hang-up is noise.
/// Their *other* devices still receive it, which is the point — a call ended
/// from the phone should stop the laptop's ring too.
fn publish_state(
    ctx: &ClientContext<'_>,
    audience: migo_core::Id,
    event: &migo_protocol::CallStateEvent,
) {
    publish_to_user(ctx, audience, Opcode::CallStateEvent, event);
}

/// Publishes one frame to an account's user topic, logging rather than
/// failing when the mailbox cannot take it.
///
/// The reply has already gone out by the time anything is published, and a
/// publication failure must not turn a succeeded request into an error the
/// client will retry — a retried invite would be a second ring, exactly what
/// the idempotency exists to prevent.
fn publish_to_user<T: migo_protocol::Encode>(
    ctx: &ClientContext<'_>,
    audience: migo_core::Id,
    opcode: Opcode,
    frame: &T,
) {
    let topic = Topic {
        kind: TopicKind::User,
        id: audience,
    };
    if let Err(error) = ctx.publish_excluding_self(&topic, opcode, frame, None) {
        tracing::warn!(%error, "call frame publication failed");
    }
}
