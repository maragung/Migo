//! The SOCIAL application opcodes: friendship, blocking, and the relationship list.
//!
//! Four opcodes, each one a thin translation from a wire frame onto one
//! [`Graph`](migo_social::traits::Graph) method. The service owns every rule — the
//! symmetric block, the "a pending request is not a friendship" test, the rate charge —
//! so these handlers only decode, call, and reply. The shape follows the other dispatch
//! modules exactly: build the [`Caller`](migo_social::Caller), decode the body with
//! [`from_frame`], await the single service method, and [`reply`](ClientContext::reply)
//! with the named response.
//!
//! # Opcode → method map
//!
//! | Opcode            | Wire payload     | Service method            | Response           |
//! |-------------------|------------------|---------------------------|--------------------|
//! | `FRIEND_REQUEST`  | `FriendTarget`   | `Graph::request_friend`   | `Acknowledged`     |
//! | `FRIEND_RESPOND`  | `FriendRespond`  | `Graph::respond_friend`   | `Acknowledged`     |
//! | `BLOCK_SET`       | `FriendTarget`   | `Graph::block`            | `Acknowledged`     |
//! | `RELATIONSHIP_LIST` | `RelationshipListReq` | `Graph::list_relationships` | `RelationshipList` |
//!
//! `RELATIONSHIP_LIST` is one read of the whole graph the caller owns: the service
//! gathers every kind — requests, friends, follows, followers, blocks, mutes,
//! favourites — under a single charge, applying the limit per kind so a full friends
//! list can never starve the requests waiting behind it. The handler projects each
//! [`Edge`](migo_social::model::Edge) onto a [`RelationshipEntry`], carrying the kind
//! as the `u32` the wire enum encodes.
//!
//! # The other side of a friendship event
//!
//! A request and an acceptance each have an audience of exactly one other account, and
//! the service hands the handler a [`Notice`] for that account. Delivery is three
//! things, and all three happen here, where a connection context exists: a
//! `FRIEND_EVENT` on the recipient's own topic (the semantic event the friends UI
//! reacts to), a `NOTIFICATION_EVENT` on the same topic (the bell, coalesced per
//! recipient exactly as the out-of-band path coalesces it), and a notification row
//! through the notifier (the inbox half, which is what survives the recipient being
//! offline). A row-store failure is logged and swallowed — the friendship is already
//! recorded, and failing the request over a bell that did not ring would tell the
//! caller their friend request failed when it did not.

use migo_core::Error;
use migo_gateway::ClientContext;
use migo_notify::{Event, SharedNotifier};
use migo_protocol::{
    fault, from_frame, Acknowledged, Frame, FriendEvent, FriendRespond, FriendTarget, MuteSet,
    Opcode, RelationshipEntry, RelationshipList, RelationshipListReq, Topic, TopicKind,
};
use migo_social::model::{FriendOutcome, MAX_PAGE};
use migo_social::notice::Notice;
use migo_social::Caller as SocialCaller;
use migo_social::SharedSocial;

/// The state strings `FRIEND_EVENT` carries. A closed vocabulary the client matches on;
/// the graph's own standing is what the client fetches afterwards to draw the right
/// button, so the string is a hint, not a source of truth.
const STATE_REQUESTED: &str = "request";
const STATE_ACCEPTED: &str = "accepted";

/// Delivers one social notice: the semantic event, the bell, and the inbox row.
async fn deliver_notice(
    ctx: &ClientContext<'_>,
    notifier: &SharedNotifier,
    notice: &Notice,
    state: &'static str,
) {
    let audience = notice.audience;
    let actor = notice.event.actor_id.unwrap_or(audience);
    let topic = Topic {
        kind: TopicKind::User,
        id: audience,
    };
    // The semantic event: who did what. The requester is not subscribed to the
    // recipient's topic, so there is no echo to exclude.
    if let Err(error) = ctx.publish(
        &topic,
        Opcode::FriendEvent,
        &FriendEvent {
            user_id: actor,
            state: state.to_string(),
        },
        None,
    ) {
        tracing::warn!(%error, "friend event publication failed");
    }
    // The bell, coalesced per recipient: a burst of requests collapses to the latest
    // for a subscriber whose mailbox is backed up.
    if let Err(error) = ctx.publish(
        &topic,
        Opcode::NotificationEvent,
        &notice.event,
        Some(crate::dispatch::coalesce_key_of(&audience)),
    ) {
        tracing::warn!(%error, "friend notification publication failed");
    }
    // The row. Offline delivery is the inbox's whole job, and a failure here costs a
    // bell, not a friendship.
    let event = Event {
        account_id: audience,
        kind: notice.event.kind,
        actor_id: notice.event.actor_id,
        room_id: None,
        subject_id: None,
        at: notice.event.at,
    };
    if let Err(error) = notifier.notify(event).await {
        tracing::warn!(code = error.code(), "friend notification row dropped");
    }
}

/// Asks `user_id` to be a friend and acknowledges.
///
/// `ctx.identity` is the asker; the wire names the asked. The service decides whether the
/// request may be sent, so the handler never inspects the recipient — it forwards the id
/// and replies `Acknowledged { ok: true }` on success, letting a refusal surface as the
/// fault the service returns.
pub(crate) async fn handle_friend_request(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedSocial,
    notifier: &SharedNotifier,
) -> Result<(), Error> {
    let caller = SocialCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: FriendTarget = from_frame(frame).map_err(fault::from_wire)?;
    let (outcome, notice) = svc.request_friend(&caller, request.user_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(notice) = notice {
        // A crossing request accepts the one already waiting, so the audience is
        // holding an acceptance, not a second request — and the hint must say which,
        // or a client that starts trusting it draws the wrong screen.
        let state = if matches!(outcome, FriendOutcome::Accepted) {
            STATE_ACCEPTED
        } else {
            STATE_REQUESTED
        };
        deliver_notice(ctx, notifier, &notice, state).await;
    }
    Ok(())
}

/// Accepts or declines a request the caller received, and acknowledges.
pub(crate) async fn handle_friend_respond(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedSocial,
    notifier: &SharedNotifier,
) -> Result<(), Error> {
    let caller = SocialCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: FriendRespond = from_frame(frame).map_err(fault::from_wire)?;
    let (_outcome, notice) = svc
        .respond_friend(&caller, request.user_id, request.accept)
        .await?;
    ctx.reply(&Acknowledged { ok: true })?;
    // Only an acceptance produces a notice: a decline is the responder's own business,
    // and telling the asker about it would be a delivery of embarrassment, not news.
    if let Some(notice) = notice {
        deliver_notice(ctx, notifier, &notice, STATE_ACCEPTED).await;
    }
    Ok(())
}

/// Blocks `user_id` and acknowledges.
///
/// Blocking is one of the few social writes that is also a read of everything it must undo
/// — the service drops any friendship, follow, and pending edge in both directions so a
/// block actually stops contact. The handler is therefore just the forward and the reply.
pub(crate) async fn handle_block_set(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedSocial,
) -> Result<(), Error> {
    let caller = SocialCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: FriendTarget = from_frame(frame).map_err(fault::from_wire)?;
    svc.block(&caller, request.user_id).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Mutes or unmutes `user_id` for the caller, and acknowledges.
///
/// The personal mute: the caller's clients stop rendering what the muted account
/// says, in every room the two share. Unlike a block it tears nothing down — no
/// friendship, no follow — and the muted account is not told, because a volume
/// control is not a verdict. The handler is the forward and the reply; the wire
/// carries the switch and the service owns the edge.
pub(crate) async fn handle_mute_set(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedSocial,
) -> Result<(), Error> {
    let caller = SocialCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: MuteSet = from_frame(frame).map_err(fault::from_wire)?;
    svc.mute(&caller, request.user_id, request.on).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Lists the caller's relationships and replies with them.
///
/// One read of the whole graph the caller owns: the service gathers every kind under
/// a single charge and applies the limit **per kind**, so a caller whose friends fill
/// the page still sees the requests waiting behind them. The handler projects each
/// [`Edge`] onto a [`RelationshipEntry`]; `kind` is the `u32` encoding of
/// [`RelationshipKind`], and an unknown edge kind collapses to `0`
/// (`RelationshipKind::Unknown`) by the protocol's own `from_wire` contract, so the
/// projection is total without a fallback branch.
pub(crate) async fn handle_relationship_list(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedSocial,
) -> Result<(), Error> {
    let caller = SocialCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: RelationshipListReq = from_frame(frame).map_err(fault::from_wire)?;
    let limit = if request.limit == 0 {
        None
    } else {
        Some(request.limit.clamp(1, u32::from(MAX_PAGE)) as u16)
    };
    // A failed or rate-limited read is an error, not a silently shorter graph: a
    // caller that swallowed it would render "no pending requests" over a list it
    // never saw, and there is no way back from telling a user nobody asked.
    let edges = svc.list_relationships(&caller, limit).await?;
    let entries: Vec<RelationshipEntry> = edges
        .into_iter()
        .map(|edge| RelationshipEntry {
            user_id: edge.other_id,
            kind: edge.kind.to_wire(),
        })
        .collect();
    ctx.reply(&RelationshipList { entries })
}
