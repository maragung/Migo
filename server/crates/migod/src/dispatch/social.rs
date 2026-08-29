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
//! | `RELATIONSHIP_LIST` | `RelationshipListReq` | `Graph::friends`/`pending`/… | `RelationshipList` |
//!
//! `RELATIONSHIP_LIST` has no single method behind it: the graph keeps each kind of edge
//! in its own listing, so the handler gathers them and projects each
//! [`Edge`](migo_social::model::Edge) onto a [`RelationshipEntry`], carrying the kind as
//! the `u32` the wire enum encodes.

use migo_core::Error;
use migo_gateway::ClientContext;
use migo_protocol::{
    fault, from_frame, Frame, Acknowledged, FriendRespond, FriendTarget, RelationshipEntry,
    RelationshipList, RelationshipListReq,
};
use migo_social::model::{Edge, MAX_PAGE};
use migo_social::Caller as SocialCaller;
use migo_social::SharedSocial;

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
) -> Result<(), Error> {
    let caller = SocialCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: FriendTarget = from_frame(frame).map_err(fault::from_wire)?;
    svc.request_friend(&caller, request.user_id).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Accepts or declines a request the caller received, and acknowledges.
pub(crate) async fn handle_friend_respond(
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
    let request: FriendRespond = from_frame(frame).map_err(fault::from_wire)?;
    svc.respond_friend(&caller, request.user_id, request.accept)
        .await?;
    ctx.reply(&Acknowledged { ok: true })
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

/// Lists the caller's relationships and replies with them.
///
/// There is no single `relationships` listing on the graph — friendships, pending
/// requests, follows, followers, blocks, and favourites are each their own method — so
/// the handler fans out to each, projecting every [`Edge`] onto a
/// [`RelationshipEntry`]. `kind` is the `u32` encoding of [`RelationshipKind`]; an unknown
/// edge kind collapses to `0` (`RelationshipKind::Unknown`) by the protocol's own `from_wire`
/// contract, so the projection is total without a fallback branch.
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
    let entries = collect_relationships(svc, &caller, limit).await;
    ctx.reply(&RelationshipList { entries })
}

/// Gathers every relationship edge the caller owns into wire entries.
///
/// Kind by kind, because the graph stores each as a separate listing; `kind` comes from
/// [`RelationshipKind::to_wire`], an exhaustive mapping with no unknown case to handle.
/// `limit`, when present, bounds the combined result so a large graph cannot flood a frame.
async fn collect_relationships(
    svc: &SharedSocial,
    caller: &SocialCaller,
    limit: Option<u16>,
) -> Vec<RelationshipEntry> {
    // Each relationship kind lives in its own listing on the graph, so gather them all and
    // project every `Edge` onto a wire `RelationshipEntry`. A listing that errors (for
    // example a limiter rejection) is skipped rather than aborting the whole response, so
    // one failed read cannot blank the rest of the list.
    let mut edges: Vec<Edge> = Vec::new();
    if let Ok(list) = svc.friends(caller, limit).await {
        edges.extend(list);
    }
    if let Ok(pending) = svc.pending(caller, limit).await {
        edges.extend(pending.incoming);
        edges.extend(pending.outgoing);
    }
    if let Ok(list) = svc.following(caller, limit).await {
        edges.extend(list);
    }
    if let Ok(list) = svc.followers(caller, limit).await {
        edges.extend(list);
    }
    if let Ok(list) = svc.blocked(caller, limit).await {
        edges.extend(list);
    }
    if let Ok(list) = svc.favorites(caller, limit).await {
        edges.extend(list);
    }
    let mut entries: Vec<RelationshipEntry> = edges
        .into_iter()
        .map(|edge| RelationshipEntry {
            user_id: edge.other_id,
            kind: edge.kind.to_wire(),
        })
        .collect();
    if let Some(limit) = limit {
        entries.truncate(usize::from(limit));
    }
    entries
}

