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
//! as the `u32` the wire enum encodes. A request that names a `kind` instead walks
//! that one kind with the cursor of the previous page, for graphs longer than the
//! per-kind snapshot can hold; the answer then carries `next_cursor`, absent on the
//! last page.
//!
//! # Who hears that the graph moved
//!
//! Every mutation leaves somebody holding a stale copy of the graph, and the
//! `FRIEND_EVENT` hint (opcode 115) is how they find out without a manual refresh.
//! The hint is not the truth — the client re-reads `RELATIONSHIP_LIST` to draw the
//! right buttons — so it is published generously, to every account whose view moved:
//!
//! * The **other party**, on their own topic, whenever an edge they can observe was
//!   written or removed. A request and an acceptance arrive with the bell and the
//!   inbox row (the [`Notice`] the service returns); a decline, a block's teardown,
//!   and the request-side echo of a request carry the hint alone, because none of
//!   them is worth waking anybody for.
//! * The **caller's other devices**, on the caller's own topic, excluding the session
//!   that performed the mutation (section 156: the fan-out skips the device of
//!   origin, and the origin was already answered by the `Acknowledged`). Without
//!   this half, accepting a request on the phone leaves the tablet's friends list
//!   showing a stranger until somebody refreshes by hand.
//!
//! A block publishes to the blocked account only when an edge they could observe was
//! torn down — a friendship, a pending request, their follow — and with the same
//! `removed` state an un-friend would carry, so the two remain indistinguishable.
//! Blocking a stranger publishes nothing to them at all: a "the graph moved" hint
//! with no visible movement would tell a stranger exactly who blocked them, which is
//! a fact the wire is otherwise careful never to hand over.
//!
//! A notice's delivery is two calls, and only one of them is ours: a `FRIEND_EVENT`
//! on the recipient's own topic (the semantic event the friends UI reacts to,
//! published here because the connection context is here), and one hand-off to the
//! notifier, which finishes the rest itself — the bell on the same topic (coalesced
//! per recipient by the one seam every notification in the process rings through) and
//! the inbox row (which is what survives the recipient being offline). A row-store
//! failure is logged and swallowed — the friendship is already recorded, and failing
//! the request over a bell that did not ring would tell the caller their friend
//! request failed when it did not.

use migo_core::{Error, Id};
use migo_gateway::ClientContext;
use migo_notify::{Event, SharedNotifier};
use migo_protocol::{
    fault, from_frame, Acknowledged, Frame, FriendEvent, FriendRespond, FriendTarget, MuteSet,
    Opcode, RelationshipEntry, RelationshipList, RelationshipListReq, Topic, TopicKind,
};
use migo_social::model::{FriendOutcome, RespondOutcome, MAX_PAGE};
use migo_social::notice::Notice;
use migo_social::{BlockOutcome, Caller as SocialCaller, SharedSocial};

/// The state strings `FRIEND_EVENT` carries. A closed vocabulary the client matches on;
/// the graph's own standing is what the client fetches afterwards to draw the right
/// button, so the string is a hint, not a source of truth.
const STATE_REQUESTED: &str = "request";
const STATE_ACCEPTED: &str = "accepted";
/// An edge is gone: a declined request, an un-friend, or the teardown a block performs.
///
/// Deliberately one word for all three. The reader cannot tell *which* removal happened
/// from the event, only that the graph moved, and a decline that carried its own word
/// would be the "delivery of embarrassment" the service's decline path exists to avoid.
const STATE_REMOVED: &str = "removed";
/// The caller blocked somebody. Only ever published on the blocker's own topic, where
/// nobody but the blocker's devices are listening.
const STATE_BLOCKED: &str = "blocked";

/// Publishes one `FRIEND_EVENT` hint on `audience`'s own topic: `other` is the far end
/// of the edge that moved, `state` the hint for what moved.
///
/// This is the whole of the realtime half for the moves that carry no bell. A failure is
/// logged and swallowed: the graph is already written, and the hint is a convenience, not
/// a contract — the client that misses one catches up on its next listing.
fn graph_moved(ctx: &ClientContext<'_>, audience: Id, other: Id, state: &str) {
    let topic = Topic {
        kind: TopicKind::User,
        id: audience,
    };
    if let Err(error) = ctx.publish(
        &topic,
        Opcode::FriendEvent,
        &FriendEvent {
            user_id: other,
            state: state.to_string(),
        },
        None,
    ) {
        tracing::warn!(%error, "friend event publication failed");
    }
}

/// The caller's own echo: the same hint on the caller's topic, skipping the session that
/// performed the mutation.
///
/// Section 156 excludes the device of origin from every fan-out, and this is that rule
/// applied to the caller's own topic: the acting connection was answered by the
/// `Acknowledged` (and refreshed itself from it), while the caller's *other* devices
/// learn the graph moved and re-read. Publishing without the exclusion would hand the
/// acting device a second, redundant hint for a change it just made.
fn echo_caller(ctx: &ClientContext<'_>, other: Id, state: &str) {
    let topic = Topic {
        kind: TopicKind::User,
        id: ctx.identity().account_id(),
    };
    if let Err(error) = ctx.publish_excluding_self(
        &topic,
        Opcode::FriendEvent,
        &FriendEvent {
            user_id: other,
            state: state.to_string(),
        },
        None,
    ) {
        tracing::warn!(%error, "friend event echo publication failed");
    }
}

/// Delivers one social notice: the semantic event here, the bell and the inbox row
/// through the notifier's seam.
///
/// The `FRIEND_EVENT` needs the connection context, so it is published in this module;
/// the notification does not, and hand-rolling its frame here — as this path once did —
/// is how the rest of the process ends up with bell-less notifications. One seam rings
/// every bell with one per-recipient coalescing rule; this handler only decides that a
/// friend event is worth one.
async fn deliver_notice(
    ctx: &ClientContext<'_>,
    notifier: &SharedNotifier,
    notice: &Notice,
    state: &'static str,
) {
    let audience = notice.audience;
    let actor = notice.event.actor_id.unwrap_or(audience);
    // The semantic event: who did what. The requester is not subscribed to the
    // recipient's topic, so there is no echo to exclude.
    graph_moved(ctx, audience, actor, state);
    // The bell and the row. The notifier rings the recipient's topic and stores the
    // inbox half; a failure there costs a buzz, not a friendship.
    let event = Event {
        account_id: audience,
        kind: notice.event.kind,
        actor_id: notice.event.actor_id,
        room_id: None,
        subject_id: None,
        conversation_id: None,
        at: notice.event.at,
    };
    if let Err(error) = notifier.notify(event).await {
        tracing::warn!(code = error.code(), "friend notification dropped");
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
        // The asker's other devices: the request they just watched leave is an edge in
        // their own graph too, and a tablet that never hears about it shows no outgoing
        // request until somebody refreshes by hand. The acting session is excluded; it
        // was answered above.
        echo_caller(ctx, request.user_id, state);
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
    let (outcome, notice) = svc
        .respond_friend(&caller, request.user_id, request.accept)
        .await?;
    ctx.reply(&Acknowledged { ok: true })?;
    match outcome {
        // An acceptance is news on both sides of the edge. The requester hears it through
        // the notice — the hint, the bell, and the inbox row — while the acceptor's other
        // devices get the hint alone: they hold a friends list that just grew, and the
        // session that answered was already told.
        RespondOutcome::Accepted => {
            if let Some(notice) = notice {
                deliver_notice(ctx, notifier, &notice, STATE_ACCEPTED).await;
            }
            echo_caller(ctx, request.user_id, STATE_ACCEPTED);
        }
        // A decline is the responder's own business, and no bell rings for it on either
        // side. But the graph moved for both parties all the same: the asker's outgoing
        // list and the decliner's incoming list each hold a row that is gone. The hint
        // carries no direction and no verdict, so the asker learns only that the edge
        // they were watching is no longer there — which their own next listing would
        // have told them anyway.
        RespondOutcome::Declined => {
            graph_moved(ctx, request.user_id, caller.account_id, STATE_REMOVED);
            echo_caller(ctx, request.user_id, STATE_REMOVED);
        }
    }
    Ok(())
}

/// Blocks `user_id` and acknowledges.
///
/// Blocking is one of the few social writes that is also a read of everything it must undo
/// — the service drops any friendship, follow, and pending edge in both directions so a
/// block actually stops contact. The handler is therefore the forward, the reply, and the
/// fan-out the service's [`BlockOutcome`] describes: the blocked account hears the graph
/// moved when an edge they could observe was torn down (with the same `removed` hint an
/// un-friend carries, so the two stay indistinguishable), and the blocker's other devices
/// hear it when the blocker's own graph changed. Nothing is published to a stranger the
/// block removed nothing from — a hint with no visible movement would name the blocker to
/// somebody the wire otherwise tells nothing.
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
    let outcome: BlockOutcome = svc.block(&caller, request.user_id).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if outcome.severed {
        graph_moved(ctx, request.user_id, caller.account_id, STATE_REMOVED);
    }
    if outcome.moved {
        echo_caller(ctx, request.user_id, STATE_BLOCKED);
    }
    Ok(())
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
    //
    // Two shapes share this opcode. Without a `kind`, the caller wants the combined
    // snapshot — every kind, bounded per kind, no cursor. With one, the caller is
    // walking a list longer than a page and holds the cursor of the last page; the
    // answer carries the next one, and `None` means the walk is over.
    if let Some(kind) = request.kind {
        let kind = migo_protocol::RelationshipKind::from_wire(kind);
        let (edges, next_cursor) = svc
            .page_relationships(&caller, kind, limit, request.cursor)
            .await?;
        let entries: Vec<RelationshipEntry> = edges
            .into_iter()
            .map(|edge| RelationshipEntry {
                user_id: edge.other_id,
                kind: edge.kind.to_wire(),
            })
            .collect();
        ctx.reply(&RelationshipList {
            entries,
            next_cursor,
        })
    } else {
        let edges = svc.list_relationships(&caller, limit).await?;
        let entries: Vec<RelationshipEntry> = edges
            .into_iter()
            .map(|edge| RelationshipEntry {
                user_id: edge.other_id,
                kind: edge.kind.to_wire(),
            })
            .collect();
        ctx.reply(&RelationshipList {
            entries,
            next_cursor: None,
        })
    }
}
