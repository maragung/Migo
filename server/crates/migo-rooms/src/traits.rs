//! The rooms contract.
//!
//! One trait, because every method here shares one question — "what is this account's
//! standing in this room right now" — and that question is answered from a membership
//! row plus a role table plus two override masks. A narrower trait that could read the
//! roster but not resolve a permission would be handing out the half of the operation
//! without the rule, and the half without the rule is the half that gets called from a
//! path nobody reviewed.
//!
//! # Why [`Roomkeeper::authorize`] is on this trait and not inlined elsewhere
//!
//! Because a room permission is needed by crates that must not depend on this one.
//! `docs/01-architecture.md` forbids two layer-3 crates from depending on each other,
//! and messaging genuinely needs to know whether an account may send into a room. So
//! the check is published here as one method returning [`Authorized`], and the
//! composition root wires it: the gateway asks rooms, then asks messaging. Two domain
//! crates never see each other, and the permission logic still exists exactly once.
//!
//! The alternative — copying the role table into messaging — is the version where a
//! bit added to `USER_BAN` next quarter is enforced in one crate and not the other.
//!
//! # Why the mutating methods return a plan instead of performing one
//!
//! See the [`fanout`](crate::fanout) module. One join in a large room is thousands of
//! deliveries; the gateway owns the sockets, encodes once, and sends N times, and a
//! domain crate that delivered its own frames would be that gateway with a store in
//! the middle.
//!
//! # What is deliberately not here
//!
//! **No online count.** `RoomSummary::online_count` is filled by the dispatcher from a
//! tally of live sessions it keeps in memory. Computing it here means intersecting the
//! roster with the presence cache on every listing, which is the query brief section 14
//! exists to prevent; `migo_presence` refuses the same request for the same reason.
//!
//! **No sequencing.** Brief section 54 gives every room exactly one sequencer in its
//! home region. This crate records which region that is and never assigns a `seq`;
//! message ordering belongs to the crate that owns the conversation.
//!
//! **No slow-mode enforcement.** [`Authorized::slow_mode_seconds`] carries the
//! interval and this crate never applies it, because applying it needs the author's
//! last message time in that conversation — which is messaging's row, not this
//! crate's. A check here would be a second read of a tail this build already has in
//! hand somewhere else, and two enforcers of one rule eventually disagree.
//!
//! **No approval queue and no invite codes.** Brief sections 20 and 21 both list
//! them, and neither has a table in `docs/04-data-model.md` yet. A join into a room
//! whose policy requires either is refused with `FEATURE_DISABLED` rather than
//! admitted, because admitting somebody a policy meant to hold back is the failure
//! that cannot be undone by shipping the table later.

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::{
    RoomJoinRequest, RoomJoinResponse, RoomLeaveRequest, RoomListRequest, RoomListResponse,
    RoomRole, RoomSummary, RoomVoteKickResponse,
};
use migo_store::model::RoomMember;

use crate::fanout::Fanout;
use crate::model::{Authorized, Caller, NewRoomRequest, Sanction, Settings};

/// Everything rooms do.
#[async_trait]
pub trait Roomkeeper: Send + Sync {
    /// Creates a room, its conversation, and the creator's ownership.
    ///
    /// The three are one unit in the store, which is what stops a room existing
    /// without somewhere to talk or without anybody able to moderate it.
    ///
    /// The room id and the conversation id are minted here rather than accepted from
    /// the client. Every link, every message row, and every audit entry will point at
    /// them for the life of the room, and a client-chosen id is a client-chosen
    /// collision — or a client-chosen *guess* at somebody else's id.
    async fn create(&self, caller: &Caller, request: NewRoomRequest) -> Result<RoomSummary>;

    /// Joins a room, or rejoins one this account left.
    ///
    /// Returns what the client needs to start participating in one round trip: the
    /// summary, the conversation to subscribe to, whether the server can read it, and
    /// the sequence number to sync from.
    ///
    /// A ban survives leaving and rejoining. It has to: a sanction that could be shed
    /// by pressing Leave and then Join would not be a sanction.
    async fn join(
        &self,
        caller: &Caller,
        request: RoomJoinRequest,
    ) -> Result<(RoomJoinResponse, Option<Fanout>)>;

    /// Leaves a room.
    ///
    /// Idempotent, and it returns `None` when the caller was not in the room. Leaving
    /// something you already left is not an error worth showing a person, and the
    /// client that asked twice is usually a client that retried a request whose reply
    /// it never saw.
    async fn leave(&self, caller: &Caller, request: RoomLeaveRequest) -> Result<Option<Fanout>>;

    /// Removes a member whose reconnect grace expired, on the server's own initiative.
    ///
    /// Not a request. Brief section 184 keeps a member in their rooms for two minutes after
    /// their last session drops, so a dropped socket or a backgrounded tab does not read as
    /// leaving; when that window closes with the account still offline, the composition root
    /// calls this to make the departure real. There is no `caller`, because no account asked for
    /// it — the actor is the timer — which is exactly why this is a separate method and not a
    /// parameter on [`leave`](Roomkeeper::leave): a caller-less removal must never travel a path
    /// that could be reached from a frame, or a client would have found a way to evict another
    /// account by naming it.
    ///
    /// It enforces the two invariants a timer cannot be trusted to have re-checked. The member
    /// must still be active — a `Left` already sent, or a rejoin in the meantime, makes this a
    /// no-op returning `None`, so a late timer cannot remove somebody twice or remove somebody who
    /// came back. And the owner is exempt: a room's creator does not lose the room by closing a
    /// laptop, so a timeout against the owner returns `None` and changes nothing, leaving them
    /// marked offline but still in place.
    ///
    /// Returns the fanout that tells the room the member is gone — a `Left`, the same shape
    /// [`leave`](Roomkeeper::leave) produces — attributed to no device, because no device caused
    /// it. `None` means nothing changed, on the section 156 rule the whole crate follows.
    ///
    /// `now` stamps the departure: this crate takes its time from whoever calls it and never
    /// reads a clock of its own, and a timer is just another caller, so the moment the grace
    /// expired arrives as an argument like every other `now` here.
    async fn timeout_member(
        &self,
        room_id: Id,
        account_id: Id,
        now: Timestamp,
    ) -> Result<Option<Fanout>>;

    /// Browses rooms.
    ///
    /// Ordered by member count, which brief section 83 says is not good enough — and
    /// it is right. Ranking by activity, retention, report rate, and spam rate needs
    /// signals no table here records, so this build orders by the one number it
    /// actually has and does not pretend the result is a trending list.
    ///
    /// Filters this build cannot apply are refused rather than ignored. A client that
    /// asks for Indonesian-language rooms and silently receives all rooms would show a
    /// filtered heading over unfiltered content, which is worse than an error.
    async fn list(&self, caller: &Caller, request: RoomListRequest) -> Result<RoomListResponse>;

    /// One room, as this caller sees it.
    ///
    /// `NOT_FOUND` for an archived room is deliberate on the join path and deliberate
    /// *not* here: an archived room still resolves, because brief section 85 archives
    /// rather than deletes so that links and history keep working.
    async fn summary(&self, caller: &Caller, room_id: Id) -> Result<RoomSummary>;

    /// Resolves a deep link (brief section 82).
    ///
    /// Accepts a slug or the canonical text form of a room id, which is what
    /// `migo://room/<id>` can carry. It does **not** accept the `MGO-ROOM-XXXXXXXXXX`
    /// alias: that alias is a lossy display projection of the id — `Id::public_id`
    /// says so — and resolving a link by it would mean resolving it to whichever room
    /// happened to collide.
    async fn resolve(&self, caller: &Caller, reference: &str) -> Result<RoomSummary>;

    /// Rooms this account is currently in.
    async fn mine(&self, caller: &Caller) -> Result<Vec<RoomSummary>>;

    /// A page of the roster, highest role first.
    ///
    /// Members only, and only to members. A roster is the membership list of a
    /// community, and handing it to anyone who knows the room id would make every
    /// public room a directory of the people in it.
    async fn roster(
        &self,
        caller: &Caller,
        room_id: Id,
        limit: u16,
        after: Option<Id>,
    ) -> Result<Vec<RoomMember>>;

    /// Changes name, topic, slow mode, or join policy.
    ///
    /// Returns `None` alongside the summary when the request asked for the values the
    /// room already had, which is brief section 156 applied to a settings screen that
    /// submits every field whether or not the operator touched it.
    async fn update(
        &self,
        caller: &Caller,
        room_id: Id,
        settings: Settings,
    ) -> Result<(RoomSummary, Option<Fanout>)>;

    /// Archives a room.
    ///
    /// Not a delete. Brief section 85 lists both, and this build implements the one
    /// that keeps links resolving and history readable; a hard delete of a room with
    /// two years of messages in it is a request that cannot be taken back and that
    /// nobody has yet said what to do about the messages.
    async fn archive(&self, caller: &Caller, room_id: Id) -> Result<()>;

    /// Sets a member's role.
    ///
    /// The actor must outrank both the target's current role and the role being
    /// granted. The second half is what stops a Moderator from minting an Admin and
    /// then being outranked by their own appointee.
    async fn set_role(
        &self,
        caller: &Caller,
        room_id: Id,
        subject_id: Id,
        role: RoomRole,
    ) -> Result<Option<Fanout>>;

    /// Sets per-member permission overrides.
    ///
    /// A caller cannot grant a bit they do not themselves hold. Without that rule the
    /// override mask is a privilege-escalation primitive: grant yourself `ROOM_MANAGE`,
    /// then everything else.
    ///
    /// No fanout. The masks are not on the wire — `RoomMemberEvent` carries a role and
    /// not a permission set — so there is no frame that could describe this change,
    /// and inventing one would put a moderation detail on a room-wide topic.
    async fn set_permissions(
        &self,
        caller: &Caller,
        room_id: Id,
        subject_id: Id,
        grant: u64,
        deny: u64,
    ) -> Result<()>;

    /// Mutes, unmutes, kicks, bans, or unbans a member.
    ///
    /// The owner cannot be sanctioned, by anybody, at any rank. There is no rank above
    /// the owner, so the only way to allow it would be a special case, and a special
    /// case here is how a room gets taken from the person who made it.
    ///
    /// Every action is written to the moderation audit log, with the actor and the
    /// wire's action numbering. A **global admin** sanctions in any room — public or
    /// managed — without membership, a permission bit, or a rank, because their
    /// standing came from the deployment and not the room; the owner protection is
    /// the one thing their override does not cross. A global admin's `Unban` also
    /// lifts the network-wide ban the escalation below imposes.
    ///
    /// The escalation: a global admin's kicks of one account are counted across every
    /// room, from the audit rows. Past the third, the next kick bars the account from
    /// every room on the service and sweeps them out of the rooms they still hold —
    /// rooms they own excepted, on the same owner rule above. Room staff's kicks
    /// never count: the rule is about the deployment's authority, not a room's.
    ///
    /// A `Vec` and not an `Option` because one sanction can touch many rooms: the
    /// kick that trips the escalation produces a fanout for the room it happened in
    /// and one for every room the sweep emptied. Empty is brief section 156's
    /// "nothing changed" — a mute nobody needed to hear about, an unban of a ban
    /// that was not there.
    async fn sanction(
        &self,
        caller: &Caller,
        room_id: Id,
        subject_id: Id,
        sanction: Sanction,
    ) -> Result<Vec<Fanout>>;

    /// Starts a kick vote, or adds the caller's voice to one already running.
    ///
    /// The members' own lever: no permission bit, no rank — any member may open a
    /// vote, and a voice per account. The vote passes when half the room's members
    /// (rounded up, two at minimum) have spoken, and passing removes the target the
    /// way a kick does: the membership is marked left, the door stays open, and the
    /// room hears a `Kicked` member event.
    ///
    /// The owner and global admins are immune — `VOTE_TARGET_IMMUNE` — for the same
    /// reason the owner is beyond every sanction. One vote runs per room; a voice
    /// for a different target while one is open is `VOTE_ALREADY_OPEN`. The same
    /// account speaking again changes nothing and returns the standing tally. A
    /// vote nobody finishes expires after a minute, lazily: the next vote this room
    /// sees closes the old one and carries its closing to the room.
    ///
    /// Returns the tally as the caller's socket needs it, plus the fanouts the room
    /// does: the running tally on every new voice, and the member event on a pass.
    /// The caller's own socket is excluded from both — the reply carries them.
    async fn vote_kick(
        &self,
        caller: &Caller,
        room_id: Id,
        target_id: Id,
    ) -> Result<(RoomVoteKickResponse, Vec<Fanout>)>;

    /// Hands the room to another member.
    ///
    /// Requires a session that proved a factor recently — brief section 85 — and
    /// demotes the outgoing owner to Manager rather than removing them. A transfer
    /// that ejected the previous owner would make a mistaken transfer unrecoverable by
    /// the only person who could explain it.
    async fn transfer_ownership(
        &self,
        caller: &Caller,
        room_id: Id,
        to: Id,
    ) -> Result<Option<Fanout>>;

    /// Whether this account may do `needed` in this room, and what it needs next.
    ///
    /// The method other domains call. `needed` is a mask from
    /// [`crate::permission`] and it is an all-of check: brief section 48 says one
    /// opcode can require more than one permission, and an any-of reading would let
    /// half a permission through.
    ///
    /// Refused with `NOT_A_MEMBER` when the account is not in the room, `BANNED` when
    /// it is banned, `MUTED` when a mute forbids the specific action, and
    /// `PERMISSION_DENIED` when it is a member in good standing without the bit. Four
    /// codes and not one, because a client that cannot tell "you were banned" from
    /// "you cannot pin messages" cannot say anything useful to the person holding the
    /// phone.
    async fn authorize(&self, caller: &Caller, room_id: Id, needed: u64) -> Result<Authorized>;
}
