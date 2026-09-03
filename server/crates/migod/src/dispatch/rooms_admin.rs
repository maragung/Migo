//! The ROOMS administration opcodes: creating a room, reading its roster, and the
//! operator actions on an existing one.
//!
//! Seven opcodes, each a thin translation from a wire frame onto one
//! [`Roomkeeper`](migo_rooms::traits::Roomkeeper) method. The service owns every rule —
//! the slug namespace, the capacity ceiling, the role algebra, the "only the owner
//! archives" check, the rate charge — so these handlers only build the
//! [`Caller`](migo_rooms::Caller), decode the body with [`from_frame`], await the
//! service, and [`reply`](ClientContext::reply). The shape follows the other dispatch
//! modules exactly.
//!
//! # Opcode → method map
//!
//! | Opcode          | Wire payload   | Service method         | Response           |
//! |-----------------|----------------|------------------------|--------------------|
//! | `ROOM_CREATE`   | `RoomCreate`   | `Roomkeeper::create`   | `RoomJoinResponse` |
//! | `ROOM_ROSTER`   | `RosterReq`    | `Roomkeeper::roster`   | `RosterResponse`   |
//! | `ROOM_ROLE_SET` | `RoomRoleSet`  | `Roomkeeper::set_role` | `Acknowledged`     |
//! | `ROOM_UPDATE`   | `RoomUpdate`   | `Roomkeeper::update`   | `Acknowledged`     |
//! | `ROOM_ARCHIVE`  | `RoomArchive`  | `Roomkeeper::archive`  | `Acknowledged`     |
//! | `ROOM_SANCTION` | `RoomSanction` | `Roomkeeper::sanction` | `Acknowledged`     |
//! | `ROOM_VOTE_KICK`| `RoomVoteKick` | `Roomkeeper::vote_kick`| `RoomVoteKickResponse` |
//!
//! `ROOM_JOIN`, `ROOM_LEAVE` and `ROOM_LIST` stay inline in `dispatch.rs`, where they
//! began; these seven live here because each carries a projection (a `NewRoomRequest`
//! mapping, a roster page, a settings patch, a sanction mapping) that would otherwise
//! fatten the match.
//!
//! `set_role` and `update` return one [`Option<Fanout>`](migo_rooms::Fanout);
//! `sanction` and `vote_kick` return a `Vec` — one action can touch many rooms, because
//! the kick that trips the network-ban escalation sweeps the account out of every room
//! it still holds. An empty `Vec`, like a `None`, means nothing changed and nothing is
//! sent (section 156). The publishing itself is
//! [`publish_rooms`](super::publish_rooms) — the same helper the inline room handlers
//! use, so a member event, a state event, and a vote tally keep their one encoder.

use migo_core::Error;
use migo_gateway::ClientContext;
use migo_protocol::{
    fault, from_frame, Acknowledged, Frame, RoomArchive, RoomCreate, RoomJoinResponse, RoomKind,
    RoomRole, RoomRoleSet, RoomSanction, RoomUpdate, RoomVoteKick, RosterEntry, RosterReq,
    RosterResponse, SanctionAction,
};
use migo_rooms::view::encryption_for;
use migo_rooms::{
    Caller as RoomCaller, NewRoomRequest, Sanction, Settings, SharedRooms, TopicChange,
    MAX_MUTE_MS, MAX_ROSTER_PAGE,
};

/// The `last_seq` a room's conversation is born with.
///
/// `create` makes the room and its conversation in one store unit, so the conversation
/// the reply names has never carried a message and its high-water mark is zero — the
/// same value `ROOM_JOIN` computes for it by reading the row. A named constant rather
/// than a bare `0`, so a reader who finds the zero knows it is an empty conversation
/// and not an unread one.
const FRESH_CONVERSATION_LAST_SEQ: u64 = 0;

/// Creates a room and answers with the same shape `ROOM_JOIN` answers with.
///
/// `create` returns the room as its new owner sees it, but the registry names
/// `RoomJoinResponse` as this opcode's reply, and that shape carries what a client
/// needs before it can talk: the conversation to subscribe to and the encryption it
/// runs under. Neither is on the summary, so a second, free call — `authorize` with an
/// empty mask, the same "may this account be here at all" question the subscribe path
/// asks — reads them from the row the first call just wrote. `authorize` charges
/// nothing by its own contract, because it is called from inside an operation that has
/// already paid.
///
/// The wire's `kind` is decoded with the protocol's own `from_wire`, so a number this
/// build does not know arrives as `RoomKind::Unknown` and is refused by the service's
/// validation naming the field — the handler adds no rule of its own. `max_members` is
/// narrowed saturating rather than wrapping: a capacity above `i32::MAX` cannot be
/// honored, and the honest answer is the service's "above this deployment's ceiling",
/// not a wrapped negative.
pub(crate) async fn handle_room_create(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedRooms,
) -> Result<(), Error> {
    let caller = RoomCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: RoomCreate = from_frame(frame).map_err(fault::from_wire)?;
    let summary = svc
        .create(
            &caller,
            NewRoomRequest {
                slug: request.slug,
                name: request.name,
                topic: request.topic,
                kind: RoomKind::from_wire(request.kind),
                max_members: request
                    .max_members
                    .map(|max| i32::try_from(max).unwrap_or(i32::MAX)),
            },
        )
        .await?;
    let standing = svc.authorize(&caller, summary.room_id, 0).await?;
    ctx.reply(&RoomJoinResponse {
        room: summary,
        conversation_id: standing.conversation_id,
        encryption: encryption_for(standing.kind),
        last_seq: FRESH_CONVERSATION_LAST_SEQ,
    })
}

/// Reads a page of a room's roster and replies with it.
///
/// The service hides departed members and refuses anyone who is not in the room; the
/// handler maps what survives onto the wire's three fields per member. `limit` is
/// narrowed here only because the wire carries a `u32` and the method takes a `u16` —
/// a narrowing that must not wrap — and the service's own clamp to
/// [`MAX_ROSTER_PAGE`] runs inside the call regardless, so the real page ceiling stays
/// where the rule lives. An absent limit asks for the largest page the service allows,
/// which is the honest default for a list a client renders by scrolling.
pub(crate) async fn handle_roster(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedRooms,
) -> Result<(), Error> {
    let caller = RoomCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: RosterReq = from_frame(frame).map_err(fault::from_wire)?;
    let limit = request.limit.map_or(MAX_ROSTER_PAGE, |limit| {
        limit.clamp(1, u32::from(MAX_ROSTER_PAGE)) as u16
    });
    let members = svc
        .roster(&caller, request.room_id, limit, request.after)
        .await?;
    ctx.reply(&RosterResponse {
        members: members
            .into_iter()
            .map(|member| RosterEntry {
                account_id: member.account_id,
                role: member.role.to_wire(),
                joined_at: member.joined_at,
            })
            .collect(),
    })
}

/// Changes a member's role and acknowledges, publishing the member event.
///
/// The wire's `role` number is decoded with the protocol's own `from_wire`; a number
/// this build does not know arrives as `RoomRole::Unknown` and the service refuses it,
/// as it refuses a grant of `Owner`, by its own rules. `Some(fanout)` reaches everyone
/// else on the room topic through [`publish_rooms`](super::publish_rooms), which
/// excludes the actor's own socket: the actor has the acknowledgement, and a member
/// event the service can return as `None` — the role already held — sends nothing.
pub(crate) async fn handle_role_set(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedRooms,
) -> Result<(), Error> {
    let caller = RoomCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: RoomRoleSet = from_frame(frame).map_err(fault::from_wire)?;
    let fanout = svc
        .set_role(
            &caller,
            request.room_id,
            request.member,
            RoomRole::from_wire(request.role),
        )
        .await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(fanout) = fanout {
        super::publish_rooms(ctx, fanout)?;
    }
    Ok(())
}

/// Applies a settings patch and acknowledges, publishing the state delta.
///
/// The wire's `topic` is `Option<String>` and the domain's is a three-way
/// [`TopicChange`]: `None` means "leave it alone" on both sides, and a `Some` —
/// including an empty or all-whitespace string, which the service reads as a removal,
/// exactly as creation does — becomes a `Set`. The wire cannot name a join policy, so
/// the patch leaves that field untouched rather than inventing a value for it.
///
/// `slow_mode_ms` is milliseconds on the wire and whole seconds in the store; the
/// division truncates, so a sub-second interval asks for the zero the store can hold,
/// which is slow mode off. The service's own bound — no more than an hour — refuses
/// anything above it. The summary `update` returns goes nowhere: the registry's answer
/// here is `Acknowledged`, and the room hears what moved through the state event.
pub(crate) async fn handle_room_update(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedRooms,
) -> Result<(), Error> {
    let caller = RoomCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: RoomUpdate = from_frame(frame).map_err(fault::from_wire)?;
    let settings = Settings {
        name: request.name,
        topic: request.topic.map_or(TopicChange::Keep, TopicChange::Set),
        slow_mode_seconds: request
            .slow_mode_ms
            .map(|ms| i32::try_from(ms / 1_000).unwrap_or(i32::MAX)),
        join_policy: None,
    };
    let (_summary, fanout) = svc.update(&caller, request.room_id, settings).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    if let Some(fanout) = fanout {
        super::publish_rooms(ctx, fanout)?;
    }
    Ok(())
}

/// Archives a room and acknowledges.
///
/// `archive` returns no fanout and this handler publishes none: the service's decision
/// is that an archived room announces nothing, and a second press of the button is not
/// an error worth showing anybody — the store keeps links resolving and history
/// readable, which is why archive exists instead of delete.
pub(crate) async fn handle_room_archive(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedRooms,
) -> Result<(), Error> {
    let caller = RoomCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: RoomArchive = from_frame(frame).map_err(fault::from_wire)?;
    svc.archive(&caller, request.room_id).await?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Applies a moderation action and acknowledges, publishing what the room hears.
///
/// The wire carries one `SanctionAction` number and an optional reason; the domain
/// carries a five-way enum whose mute and ban take parameters the wire cannot name.
/// The durations are supplied here, not read from the frame: a mute is the maximum the
/// crate allows (thirty days — a longer silence is a ban and belongs on the audit
/// trail a ban gets), and a ban is permanent, which matches the escalation posture of
/// the network ban the service may impose on the same action.
///
/// A kick carries no reason on the domain side — the store has no column for one, and
/// accepting text only to drop it would document a feature the schema cannot hold — so
/// a `reason` sent with a kick is refused rather than ignored: a client that typed one
/// should learn it went nowhere. `Unknown` action numbers are refused the same way, by
/// name, before any store call.
///
/// One action can reach many rooms: a global admin's fourth kick of one account sweeps
/// them out of every room they still hold, and each emptied room hears its own member
/// event. All of it publishes through [`publish_rooms`](super::publish_rooms).
pub(crate) async fn handle_sanction(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedRooms,
) -> Result<(), Error> {
    let caller = RoomCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: RoomSanction = from_frame(frame).map_err(fault::from_wire)?;
    let sanction = match request.action {
        SanctionAction::Mute => Sanction::Mute {
            duration_ms: MAX_MUTE_MS,
            reason: request.reason,
        },
        SanctionAction::Unmute => Sanction::Unmute,
        SanctionAction::Kick => {
            if request.reason.is_some() {
                return Err(fault::validation(
                    "reason",
                    "a kick stores no reason; ban instead if the account must be told why",
                ));
            }
            Sanction::Kick
        }
        SanctionAction::Ban => Sanction::Ban {
            duration_ms: None,
            reason: request.reason,
        },
        SanctionAction::Unban => Sanction::Unban,
        SanctionAction::Unknown => {
            return Err(fault::validation("action", "a known action is required"))
        }
    };
    let fanouts = svc
        .sanction(&caller, request.room_id, request.target_id, sanction)
        .await?;
    ctx.reply(&Acknowledged { ok: true })?;
    for fanout in fanouts {
        super::publish_rooms(ctx, fanout)?;
    }
    Ok(())
}

/// Voices a kick vote and replies with the tally, publishing what the room hears.
///
/// The reply is the whole state of the vote as the voter's socket needs it — voices,
/// needed, member count, open or landed — and every fanout the service returns is for
/// everybody else: the running tally on a new voice, the closing of an expired vote,
/// and the member event when the tally reaches its number and the kick lands.
pub(crate) async fn handle_vote_kick(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedRooms,
) -> Result<(), Error> {
    let caller = RoomCaller::new(
        ctx.identity().account_id(),
        ctx.identity().device_id(),
        ctx.identity().tier,
        ctx.now(),
    );
    let request: RoomVoteKick = from_frame(frame).map_err(fault::from_wire)?;
    let (response, fanouts) = svc
        .vote_kick(&caller, request.room_id, request.target_id)
        .await?;
    ctx.reply(&response)?;
    for fanout in fanouts {
        super::publish_rooms(ctx, fanout)?;
    }
    Ok(())
}
