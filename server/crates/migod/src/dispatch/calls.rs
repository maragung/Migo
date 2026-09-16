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
//! | `CALL_END`         | `CallEnd`          | `end` / `group_leave`     | `Acknowledged`      | state event → other party|
//! | `CALL_SDP`         | `CallSdp`          | `relay_sdp` / `group_relay` | `Acknowledged`    | relayed frame → target   |
//! | `CALL_ICE`         | `CallIce`          | `relay_ice` / `group_relay` | `Acknowledged`    | relayed frame → target   |
//! | `CALL_RENEGOTIATE` | `CallRenegotiate`  | `relay_sdp` (projected) / `group_relay` | `Acknowledged` | relayed frame → target |
//! | `CALL_KEY_UPDATE`  | `CallKeyUpdate`    | `group_key_audience` / `call` | `Acknowledged`  | key update → other party, or the whole roster |
//! | `CALL_STATS`       | `CallStats`        | `stats`                   | `Acknowledged`      | —                        |
//! | `CALL_TURN_FETCH`  | `CallTurnFetch`    | `turn_servers`            | `CallTurnResponse`  | —                        |
//! | `CALL_SFU_JOIN`    | `CallInvite`       | `group_join`              | `CallTurnResponse`  | roster → joiner; join → conversation |
//!
//! `CALL_INVITE_EVENT`, `CALL_STATE_EVENT`, and `CALL_SFU_EVENT` are
//! server-originated: the gateway refuses them from a client before the
//! dispatcher is ever asked, and the dispatcher's default arm answers
//! `FEATURE_DISABLED` naming the opcode if one ever arrives by another path.
//! This module *publishes* the first two, and the third for the group call.
//!
//! # Who hears what
//!
//! Every 1:1 call event has an audience of exactly one account — the one that
//! did not send the frame. The sender has the reply, and their own client
//! knows what it just did; the other side has only the event. The handler
//! works the audience out from the call row (via [`Callkeeper::call`], the
//! service's own participant-checked read) rather than from the frame,
//! because the frame names devices and calls, never topics.
//!
//! A group call has no "other side" to name: its audience is a roster, and
//! which seats are on it is the group store's answer, not the frame's. The
//! one frame that must reach all of them is `CALL_KEY_UPDATE`, and it reaches
//! them one account at a time — see the handler for why.
//!
//! # The relay's target
//!
//! `CALL_SDP` and `CALL_ICE` name a target *device*. The topic that reaches
//! it belongs to the account that owns it, and the call row is the only
//! place that says whose device it is — so the handler loads the call after
//! the relay succeeds (the relay is the authority on whether the target is
//! legitimate) and maps device → account through it.
//!
//! A group call answers the same question against its roster, and answers it
//! with the roster the *relay itself* returned: a group call and a 1:1 call
//! share no store, so every addressed relay tries the group store first and
//! reads the other's `NOT_FOUND` as the handoff between them. The device →
//! account map is then the roster's, one lookup either way, and the rest of
//! the handler is unchanged.
//!
//! # The sealed answer on `CALL_ANSWER`
//!
//! `CallAnswer.sealed_answer` is read by nothing here. The SDP answer
//! travels as a `CALL_SDP` relay — which is also the frame that marks the
//! call `Connected` — so the copy on the answer frame is advisory, and
//! forwarding it as well would deliver the same sealed bytes to the caller
//! twice for one answer.
//!
//! # The federated half
//!
//! Every audience this module publishes to is an account, and an account's
//! sessions may sit on another node: the user-topic tier's watch table
//! (section 170) says which, and the same table that carries a presence
//! change carries the call frame — inside the `FED_CALL_RELAY` envelope the
//! registry allocated for call signaling, so the stream stays separately
//! countable from presence's. The half is best-effort for the same reason
//! the local one is: the reply already went out, and a retried invite would
//! ring twice, exactly what the idempotency exists to prevent.
//!
//! What the far node can *do* with the frame is bounded by where the call
//! row lives: 1:1 calls sit in the inviting node's in-process call store,
//! so a callee on another node hears the ring and every state event the
//! inviting side publishes, but their own `CALL_ANSWER` or `CALL_DECLINE`
//! reaches the other node's store and finds no row — the completion paths
//! are the flagged remainder, not a silence to paper over.
//!
//! A group call's *announcements* are the one call frames whose audience is
//! a conversation rather than an account, so they cross on the conversation
//! tier instead: whichever of the two fan-out tiers owns the conversation
//! (the room envelope for a room's conversation, the conversation envelope
//! for a group chat's) carries the join and departure frames a local
//! publication put on the conversation's topic, one copy per watching node,
//! with the sealed offer inside read by nobody on the way — see
//! [`forward_announcement`]. The roster-to-user-topic frames above are a
//! different audience on a different topic, so a joiner's device hears each
//! frame once no matter how many tiers carry the call.

use migo_calls::{roster_wire, Caller as CallCaller, SharedCallkeeper};
use migo_core::{Error, Timestamp};
use migo_gateway::ClientContext;
use migo_notify::{Event as NotificationEvent, SharedNotifier};
use migo_protocol::{
    fault, from_frame, Acknowledged, CallAnswer, CallCancel, CallDecline, CallEnd, CallIce,
    CallInvite, CallInviteResult, CallKeyUpdate, CallListQuery, CallListResult, CallRenegotiate,
    CallSdp, CallStats, CallTurnFetch, CallTurnResponse, Frame, NotificationKind, Opcode, Topic,
    TopicKind,
};

use crate::conversation_relay::ConversationRelay;
use crate::presence_relay::PresenceRelay;
use crate::room_relay::RoomRelay;

/// Invites a callee and rings them.
///
/// The reply carries the outcome for the caller's own screen — ringing, or
/// the status a retry needs to hear (declined, expired, blocked). The event,
/// when there is one, goes to the callee's user topic: the frame names the
/// callee, so no call row is needed to route it. Not coalesced; a ring is
/// Critical, and the callee's client dedupes by `call_id` if a retry ever
/// did produce two.
///
/// A ring the gate accepted is also handed to the notifier as an
/// `IncomingCall` event, which finishes the notification's own three halves: the
/// bell (a `NOTIFICATION_EVENT` on the callee's user topic, rung by the seam every
/// notification rides), the inbox row, and the push (once a sender is wired) for a
/// callee whose every device is offline. The `CallInviteEvent` above remains the
/// semantic event the ringing screen reacts to; the notification is the bell and the
/// record, not a second copy of the ring.
pub(crate) async fn handle_invite(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
    notify: &SharedNotifier,
    relay: &PresenceRelay,
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
        publish_to_user(ctx, relay, callee_id, Opcode::CallInviteEvent, &event).await;
        // The wake-up half of the ring. The notification is a courtesy, not a
        // requirement — the call is already recorded and the realtime event
        // is already out, and failing the request over a bell that did not
        // ring would have the caller retry an invite the service would then
        // have to answer as a duplicate.
        let notification = NotificationEvent {
            account_id: callee_id,
            kind: NotificationKind::IncomingCall,
            actor_id: Some(caller.account_id),
            room_id: None,
            subject_id: Some(call_id),
            conversation_id: None,
            at: caller.now,
        };
        if let Err(error) = notify.notify(notification).await {
            tracing::warn!(code = error.code(), "incoming-call notification dropped");
        }
    }
    Ok(())
}

/// Answers a ringing call.
///
/// The answering device is the connection's own, not the frame's claim: a
/// client-supplied device id would let one device answer for another and
/// misroute every sealed frame after it. The `Connecting` state event goes to
/// *both* parties: the caller's screen leaves "ringing" for "connecting", and
/// the callee's other devices — every one of them rang — learn the call was
/// answered elsewhere and stop, which is the fan-out a multi-device ring needs
/// and the one this path never sent.
pub(crate) async fn handle_answer(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallAnswer = from_frame(frame).map_err(fault::from_wire)?;
    let event = svc
        .answer(&caller, request.call_id, ctx.identity().device_id())
        .await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(event) = event {
        publish_to_both_parties(ctx, svc, relay, &caller, request.call_id, &event).await;
    }
    Ok(())
}

/// Declines a ringing call; the caller hears `Ended(Declined)` — or
/// `Ended(Busy)`, when the callee's own reason says their devices were
/// occupied rather than unwilling.
pub(crate) async fn handle_decline(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallDecline = from_frame(frame).map_err(fault::from_wire)?;
    let event = svc
        .decline(&caller, request.call_id, request.reason)
        .await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(event) = event {
        publish_to_caller_of(ctx, svc, relay, &caller, request.call_id, &event).await;
    }
    Ok(())
}

/// Cancels a call before an answer; the callee hears `Ended(ByCaller)` and
/// stops ringing.
pub(crate) async fn handle_cancel(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallCancel = from_frame(frame).map_err(fault::from_wire)?;
    let event = svc.cancel(&caller, request.call_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(event) = event {
        publish_to_callee_of(ctx, svc, relay, &caller, request.call_id, &event).await;
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
    relay: &PresenceRelay,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallEnd = from_frame(frame).map_err(fault::from_wire)?;
    let event = svc.end(&caller, request.call_id, request.reason).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(event) = event {
        publish_to_other_party(ctx, svc, relay, &caller, request.call_id, &event).await;
    }
    Ok(())
}

/// Relays sealed SDP to the target device.
///
/// One frame is both the request and the payload: the relay method validates
/// the routing headers against the call row — or the roster, when the id names
/// a group call — and returns the frame unchanged, and this handler publishes
/// it, still unchanged and still sealed, to the account that owns the target
/// device. `Critical` and uncoalesced: a lost answer is a call that never
/// connects.
pub(crate) async fn handle_sdp(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallSdp = from_frame(frame).map_err(fault::from_wire)?;
    relay_sdp(ctx, svc, relay, &caller, Opcode::CallSdp, request).await
}

/// Relays a mid-call renegotiation. `CallRenegotiate` and `CallSdp` are the
/// same relay with different names for the client's benefit; the service
/// knows one operation.
pub(crate) async fn handle_renegotiate(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallRenegotiate = from_frame(frame).map_err(fault::from_wire)?;
    relay_sdp(
        ctx,
        svc,
        relay,
        &caller,
        Opcode::CallRenegotiate,
        CallSdp {
            call_id: request.call_id,
            from_device: request.from_device,
            to_device: request.to_device,
            sealed_sdp: request.sealed_sdp,
        },
    )
    .await
}

/// Tries the group-call relay, returning the roster when the id named one.
///
/// A group call and a 1:1 call live in distinct stores, so the group read's
/// `NOT_FOUND` is the fact that this id belongs to the other kind — the
/// handoff, not an error the caller sees. It is the same idiom the
/// dispatcher's `CALL_END` arm uses, kept here in one place for the three
/// addressed relays. Every other failure — a sender who is not seated, a
/// target who is not on the roster, the rate limit — is the caller's answer
/// and travels on unchanged.
async fn try_group_relay(
    svc: &SharedCallkeeper,
    caller: &CallCaller,
    opcode: Opcode,
    call_id: migo_core::Id,
    from_device: migo_core::Id,
    to_device: migo_core::Id,
    sealed: &[u8],
) -> Result<Option<migo_calls::GroupCall>, Error> {
    match svc
        .group_relay(caller, opcode, call_id, from_device, to_device, sealed)
        .await
    {
        Ok(group) => Ok(Some(group)),
        Err(error) if error.code() == migo_protocol::codes::NOT_FOUND => Ok(None),
        Err(error) => Err(error),
    }
}

/// Publishes a relayed frame to the account whose device the relay targeted.
///
/// The group twin of the 1:1 relay's routing read: the service has already
/// proved `to_device` is a seated device — the relay's own check is the
/// authority on that — so the roster it handed back says which account's
/// topic reaches it. A device absent from the roster would be a publication
/// to nobody, which is why the miss is logged rather than invented into a
/// target; within one frame it cannot happen, because the roster the relay
/// validated is the roster this reads.
async fn publish_to_group_device<T: migo_protocol::Encode>(
    ctx: &ClientContext<'_>,
    relay: &PresenceRelay,
    group: &migo_calls::GroupCall,
    to_device: migo_core::Id,
    opcode: Opcode,
    frame: &T,
) {
    match group.account_of_device(to_device) {
        Some(account) => publish_to_user(ctx, relay, account, opcode, frame).await,
        None => tracing::warn!(
            target_device = %to_device,
            "group relay target is not on the roster; the frame reached nobody"
        ),
    }
}

/// Relays sealed SDP after the service has validated its routing, on
/// whichever store the call id names.
///
/// When the relayed frame is the callee's first answer — the moment the call
/// turns `Connected` — the service hands back the `Connected` state event
/// alongside the frame, and it is published to *both* parties here: each
/// side's screen has a `Connecting` state to retire, and the wire's promise
/// is that the authoritative transitions arrive as events, not as side
/// effects of frames a client must infer from.
///
/// A group call knows no such transition: a roster has no second party to
/// turn `Connected`, its seats are connected by joining, and the join
/// announcement is the event that said so. So the group path publishes the
/// relayed frame and nothing else.
async fn relay_sdp(
    ctx: &ClientContext<'_>,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
    caller: &CallCaller,
    opcode: Opcode,
    request: CallSdp,
) -> Result<(), Error> {
    if let Some(group) = try_group_relay(
        svc,
        caller,
        opcode,
        request.call_id,
        request.from_device,
        request.to_device,
        &request.sealed_sdp,
    )
    .await?
    {
        ctx.reply(&Acknowledged { ok: true })?;
        // `CALL_SDP` and not `opcode`: a renegotiation is the same frame under
        // another name, and the 1:1 path already projects it to `CALL_SDP` for
        // the peer — the name the sender used is for the sender's own charge,
        // not for what the target's client has to decode.
        publish_to_group_device(
            ctx,
            relay,
            &group,
            request.to_device,
            Opcode::CallSdp,
            &request,
        )
        .await;
        return Ok(());
    }
    let (relayed, connected) = svc.relay_sdp(caller, request).await?;
    let call = svc.call(caller, relayed.call_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(event) = connected {
        publish_to_both_parties(ctx, svc, relay, caller, relayed.call_id, &event).await;
    }
    // The relay succeeded, so `to_device` is one of the call's two devices;
    // the row says which account's topic reaches it.
    let target = call
        .account_of_device(relayed.to_device)
        .unwrap_or(call.caller_id);
    publish_to_user(ctx, relay, target, Opcode::CallSdp, &relayed).await;
    Ok(())
}

/// Relays a batch of sealed ICE candidates to the other device. Same shape
/// as the SDP relay — including the group store's turn at the id — never
/// coalesced, because candidates are additive facts and collapsing two
/// batches would lose candidates.
pub(crate) async fn handle_ice(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallIce = from_frame(frame).map_err(fault::from_wire)?;
    if let Some(group) = try_group_relay(
        svc,
        &caller,
        Opcode::CallIce,
        request.call_id,
        request.from_device,
        request.to_device,
        &request.sealed_candidates,
    )
    .await?
    {
        ctx.reply(&Acknowledged { ok: true })?;
        publish_to_group_device(
            ctx,
            relay,
            &group,
            request.to_device,
            Opcode::CallIce,
            &request,
        )
        .await;
        return Ok(());
    }
    let relayed = svc.relay_ice(&caller, request).await?;
    let call = svc.call(&caller, relayed.call_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    let target = call
        .account_of_device(relayed.to_device)
        .unwrap_or(call.caller_id);
    publish_to_user(ctx, relay, target, Opcode::CallIce, &relayed).await;
    Ok(())
}

/// Re-keys a live call's media encryption.
///
/// The payload is sealed key material the devices exchange; the server's
/// whole role is to check that the sender is a party to the call and to hand
/// the frame on. There is no service method for the *material* because there
/// is no state to move — the epoch inside the ciphertext is the callers'
/// business, and this node cannot open it.
///
/// Who is a party is the one thing that differs by kind. A 1:1 call has a
/// second party, so the frame goes to the one account that is not the sender.
/// A group call has a roster, every seat of which holds the same frame key
/// and must rotate with it (section 166), and the frame names no target to
/// pick between them — so the audience is the whole roster, read from the
/// group store, and each account is published to once. The sender's own
/// account is in that audience: its other seated devices hold the key too.
/// The publication excludes the originating connection, so the sender does
/// not hear its own rotation back.
pub(crate) async fn handle_key_update(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallKeyUpdate = from_frame(frame).map_err(fault::from_wire)?;
    // The group store first, for the reason the addressed relays try it
    // first: the id names one kind of call and the other store's `NOT_FOUND`
    // is how this handler learns which. A group rotation reaches the roster;
    // a seatless caller — or an id that names no group call at all — falls
    // through to the 1:1 read, which answers for its own store.
    match svc.group_key_audience(&caller, request.call_id).await {
        Ok(audience) => {
            ctx.reply(&Acknowledged { ok: true })?;
            for account in audience {
                publish_to_user(ctx, relay, account, Opcode::CallKeyUpdate, &request).await;
            }
            return Ok(());
        }
        Err(error) if error.code() == migo_protocol::codes::NOT_FOUND => {}
        Err(error) => return Err(error),
    }
    let call = svc.call(&caller, request.call_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    // A key update for a call that is over reaches nobody who cares, and
    // sending it would imply the call is still live.
    if call.state.is_live() {
        if let Some(other) = call.other_party(caller.account_id) {
            publish_to_user(ctx, relay, other, Opcode::CallKeyUpdate, &request).await;
        }
    }
    Ok(())
}

/// Accepts aggregate call quality numbers.
///
/// The frame is `Droppable` by declaration and no event follows, but it is not
/// nothing: the setup latency and the TURN fallback it carries are the only
/// view this server ever gets of the media plane, because the media itself
/// never crosses it. The sample is handed to the service, which counts it only
/// for a party to the call — a stranger naming ids is answered the same
/// acknowledgement and moves no series. The decode is still done — a body that
/// will not parse is a framing violation the client should hear about, not
/// silently drop.
pub(crate) async fn handle_stats(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let request: CallStats = from_frame(frame).map_err(fault::from_wire)?;
    svc.stats(&caller_of(ctx), request).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Answers what calls the caller can see.
///
/// The one call frame that publishes nothing: every other handler here replies
/// *and* fans an event out to a topic, because it moved a call and somebody had
/// to hear about it. A listing moves nothing, so it answers on the connection
/// it arrived on and stops — a broadcast would be the read telling every one of
/// the account's other devices about a screen they did not ask for.
pub(crate) async fn handle_list(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
) -> Result<(), Error> {
    let request: CallListQuery = from_frame(frame).map_err(fault::from_wire)?;
    let calls = svc.list(&caller_of(ctx), request.conversation_id).await?;
    ctx.reply(&CallListResult { calls })
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

/// Carries one group-call announcement to whichever tier owns its
/// conversation's fan-out.
///
/// The audience question every announcement asks is the one
/// `publish_messaging` already answers for a chat: a conversation's
/// subscribers, whichever nodes they sit on, with the envelope chosen by who
/// owns the conversation — a room's conversation rides the room tier
/// (`FED_ROOM_EVENT`), a direct or group chat's rides the conversation tier
/// (`FED_CONVERSATION_EVENT`). Each relay answers the ownership question for
/// itself against the store and no-ops when the conversation is not its own,
/// so one call here is the whole of the routing: one federated copy per
/// watching node, the sealed offer inside the event read by nobody on the
/// way.
///
/// Warn-not-fail, exactly as every other federated half this module sends:
/// the local publish already happened, and refusing the request over the far
/// copy would only cost the roster its far members without undoing anything.
/// Public to the crate because the call sweeper owes the same crossing for
/// the departures it publishes out of band.
pub(crate) async fn forward_announcement(
    rooms: &RoomRelay,
    conversations: &ConversationRelay,
    conversation_id: migo_core::Id,
    event: &migo_protocol::CallStateEvent,
    now: Timestamp,
) {
    if let Err(error) = rooms.forward_call_event(conversation_id, event, now).await {
        tracing::warn!(
            %error,
            "cannot enqueue the federated half of a group-call announcement"
        );
    }
    if let Err(error) = conversations
        .forward_call_event(conversation_id, event, now)
        .await
    {
        tracing::warn!(
            %error,
            "cannot enqueue the federated half of a group-call announcement"
        );
    }
}

/// Joins (or re-joins) a group call on this node.
///
/// The payload is a `CallInvite` — the same frame a 1:1 ring carries, read
/// with group semantics: the `call_id` names the *group* call (the join's
/// idempotency key), the `conversation_id` is the conversation whose members
/// may join, and the `sealed_offer` is the joiner's sealed media description.
/// `callee_id`, `caller_device`, and `capabilities` are ignored — the joining
/// device is the connection's own, and a group call has no single callee.
///
/// The reply is a `CallTurnResponse`, the frame the registry froze for this
/// opcode; its `servers` list carries the configured TURN relays, exactly as
/// a 1:1 fetch would. The roster itself is *published* to the joiner's own
/// user topic as a `CALL_SFU_EVENT` carrying the full participant list — the
/// one event the registry gives this opcode a coalescing key for — and the
/// join announcement is published to the conversation's topic for the rest of
/// the roster.
pub(crate) async fn handle_sfu_join(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
    rooms: &RoomRelay,
    conversations: &ConversationRelay,
) -> Result<(), Error> {
    let caller = caller_of(ctx);
    let request: CallInvite = from_frame(frame).map_err(fault::from_wire)?;
    let (outcome, call, announcements) = svc
        .group_join(
            &caller,
            request.call_id,
            request.conversation_id,
            request.media_kind,
            request.sealed_offer,
        )
        .await?;
    let servers = svc.turn_servers(request.call_id).await?;
    ctx.reply(&CallTurnResponse { servers })?;
    // The joiner's own roster: their screen builds the call from this frame,
    // on their own user topic so it arrives whether or not their client ever
    // subscribed to the conversation. The coalescing key is `None` even
    // though the opcode's class is Coalescable: a retried join's roster is a
    // second fact the joiner's screen may still be waiting for, and the
    // reply rule is answered by the TURN list — this frame is the roster,
    // and dropping it to a collapse would leave a joiner holding a reply and
    // no call. The joiner's connection is not excluded — this is the reply
    // the frame shape could not carry.
    let roster = migo_protocol::CallStateEvent {
        call_id: call.call_id,
        state: migo_calls::group_store::GROUP_STATE_CONNECTED,
        reason: None,
        conversation_id: Some(call.conversation_id),
        user_id: Some(caller.account_id),
        device_id: Some(caller.device_id),
        participant_count: Some(call.participants.len() as u32),
        sealed_offer: None,
        participants: Some(roster_wire(&call)),
    };
    let user_topic = Topic {
        kind: TopicKind::User,
        id: caller.account_id,
    };
    if let Err(error) = ctx.publish(&user_topic, Opcode::CallSfuEvent, &roster, None) {
        tracing::warn!(%error, "group-call roster publication failed");
    }
    // The roster's federated half: the joiner's *other* devices may sit on
    // another node, and the roster is the one frame that builds their call
    // screen — the same audience question every call frame here asks, over
    // the same watch table. The conversation-topic announcements below ride
    // the conversation's own tier instead, one copy per watching node,
    // because their audience is the conversation's subscribers rather than
    // one account's devices.
    if let Err(error) = relay
        .forward_call(caller.account_id, Opcode::CallSfuEvent, &roster, caller.now)
        .await
    {
        tracing::warn!(%error, "cannot enqueue the federated half of a group-call roster");
    }
    for event in announcements {
        // The roster hears each announcement on the conversation's topic, in
        // the order the service produced them — a seat replacement is a
        // departure followed by an arrival, and the roster hears both facts.
        // `None` for the coalescing key because no two membership facts may
        // collapse into one, whatever the opcode's class allows.
        let topic = Topic {
            kind: TopicKind::Conversation,
            id: call.conversation_id,
        };
        if let Err(error) = ctx.publish_excluding_self(&topic, Opcode::CallSfuEvent, &event, None) {
            tracing::warn!(%error, "group-call join announcement failed");
        }
        // And the announcement's own federated half, so a member whose socket
        // sits on another node learns the call is running exactly as a local
        // subscriber would — the crossing the conversation tier was widened
        // to carry.
        forward_announcement(
            rooms,
            conversations,
            call.conversation_id,
            &event,
            caller.now,
        )
        .await;
    }
    let _ = outcome;
    Ok(())
}

/// Leaves a group call: the `CALL_END` frame, routed to the group service
/// when the id names a group call.
///
/// Returns `Ok(true)` when the frame was a group leave and is fully answered,
/// and `Ok(false)` when the id names no group call — the 1:1 handler's turn.
pub(crate) async fn handle_group_end(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedCallkeeper,
    rooms: &RoomRelay,
    conversations: &ConversationRelay,
) -> Result<bool, Error> {
    let caller = caller_of(ctx);
    let request: CallEnd = from_frame(frame).map_err(fault::from_wire)?;
    let Some(event) = svc.group_leave(&caller, request.call_id).await? else {
        // No group call held this id, or the caller held no seat in one that
        // did. `group_leave` answers NOT_FOUND for the first, so reaching here
        // with `Ok(None)` means the call exists and the caller was not seated:
        // a retried leave after a seat replacement, or a member who never
        // joined. A 1:1 call cannot share the id (the id spaces are distinct
        // stores), and the leave is already true — the caller is not in the
        // call — so the frame is acknowledged as the idempotent no-op it is,
        // the same answer ROOM_LEAVE gives to somebody already gone. Silence
        // here left the client hanging until its request timer fired.
        ctx.reply(&Acknowledged { ok: true })?;
        return Ok(true);
    };
    ctx.reply(&Acknowledged { ok: true })?;
    let conversation_id = event.conversation_id.unwrap_or_default();
    let topic = Topic {
        kind: TopicKind::Conversation,
        id: conversation_id,
    };
    if let Err(error) = ctx.publish_excluding_self(&topic, Opcode::CallSfuEvent, &event, None) {
        tracing::warn!(%error, "group-call departure publication failed");
    }
    // The departure's federated half, over the same tier the join crossed: a
    // roster on another node must hear the seat empty, or it renders a
    // participant who is gone.
    forward_announcement(rooms, conversations, conversation_id, &event, caller.now).await;
    Ok(true)
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
    relay: &PresenceRelay,
    caller: &CallCaller,
    call_id: migo_core::Id,
    event: &migo_protocol::CallStateEvent,
) {
    match svc.call(caller, call_id).await {
        Ok(call) => publish_state(ctx, relay, call.caller_id, event).await,
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
    relay: &PresenceRelay,
    caller: &CallCaller,
    call_id: migo_core::Id,
    event: &migo_protocol::CallStateEvent,
) {
    match svc.call(caller, call_id).await {
        Ok(call) => publish_state(ctx, relay, call.callee_id, event).await,
        Err(error) => {
            tracing::warn!(%error, "call state event dropped: routing read failed")
        }
    }
}

/// Publishes a state event to both parties of a call.
///
/// The answer is the one event both sides are waiting on, and neither can be
/// left to infer it: the caller's screen leaves "ringing" for "connecting",
/// and the callee's other devices — every one of them rang — learn the call
/// was answered elsewhere and stop on their own.
async fn publish_to_both_parties(
    ctx: &ClientContext<'_>,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
    caller: &CallCaller,
    call_id: migo_core::Id,
    event: &migo_protocol::CallStateEvent,
) {
    match svc.call(caller, call_id).await {
        Ok(call) => {
            publish_state(ctx, relay, call.caller_id, event).await;
            publish_state(ctx, relay, call.callee_id, event).await;
        }
        Err(error) => {
            tracing::warn!(%error, "call state event dropped: routing read failed")
        }
    }
}

/// Publishes a state event to whichever of the two parties is not the actor.
async fn publish_to_other_party(
    ctx: &ClientContext<'_>,
    svc: &SharedCallkeeper,
    relay: &PresenceRelay,
    caller: &CallCaller,
    call_id: migo_core::Id,
    event: &migo_protocol::CallStateEvent,
) {
    match svc.call(caller, call_id).await {
        Ok(call) => {
            if let Some(other) = call.other_party(caller.account_id) {
                publish_state(ctx, relay, other, event).await;
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
async fn publish_state(
    ctx: &ClientContext<'_>,
    relay: &PresenceRelay,
    audience: migo_core::Id,
    event: &migo_protocol::CallStateEvent,
) {
    publish_to_user(ctx, relay, audience, Opcode::CallStateEvent, event).await;
}

/// Publishes one frame to an account's user topic, logging rather than
/// failing when the mailbox cannot take it.
///
/// The reply has already gone out by the time anything is published, and a
/// publication failure must not turn a succeeded request into an error the
/// client will retry — a retried invite would be a second ring, exactly what
/// the idempotency exists to prevent.
///
/// The federated half rides the same call: the audience's sessions may sit
/// on another node, and the user-topic tier's watch table (section 170) says
/// which — one copy per watching node, inside the `FED_CALL_RELAY` envelope,
/// with the same warn-not-fail posture as the local half.
async fn publish_to_user<T: migo_protocol::Encode>(
    ctx: &ClientContext<'_>,
    relay: &PresenceRelay,
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
    if let Err(error) = relay.forward_call(audience, opcode, frame, ctx.now()).await {
        tracing::warn!(%error, "cannot enqueue the federated half of a call frame");
    }
}
