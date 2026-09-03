//! The messaging contract.
//!
//! One trait, for the same reason `migo_auth::Authenticator` is one trait (spelled
//! without a link, because this crate must not depend on that one): the
//! operations are not independent. Sending assigns a sequence *and* moves the
//! sender's own cursor; a receipt has to know the conversation's high-water mark
//! before it can clamp to it; sync has to agree with both about what a sequence
//! means. Handing out a narrow `Sender` that could append but not advance a cursor
//! would be handing out the half of the operation without the invariant.
//!
//! # Why every method returns a plan instead of performing one
//!
//! The three broadcasting operations return a [`Fanout`] rather than delivering
//! it. See the [`fanout`](crate::fanout) module: the short version is that the
//! gateway owns connections, encodes once, and sends N times, and a domain crate
//! that delivered its own frames would be that gateway with a database in the
//! middle.
//!
//! An `Option<Fanout>` is not an accident of style. Brief section 156 forbids
//! sending a frame when nothing changed, and the `Option` is that rule made
//! visible in the type: a duplicate send, a receipt for a sequence already
//! acknowledged, and a delete of something already tombstoned all return `None`,
//! and a caller cannot forget to check.

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::{
    ConversationCreateRequest, ConversationInviteRequest, ConversationKickRequest,
    ConversationLeaveRequest, ConversationListRequest, ConversationListResponse,
    ConversationMuteRequest, ConversationRosterRequest, ConversationRosterResponse,
    ConversationSummary, ConversationUpdateRequest, ConversationVoteKickRequest,
    ConversationVoteKickResponse, MessageAccepted, MessageDelete, MessageReceipt, MessageSend,
    SyncRequest, SyncResponse, TypingEvent,
};

use crate::fanout::Fanout;
use crate::model::Caller;

/// Conversations, messages, receipts, sync, and typing.
#[async_trait]
pub trait Messaging: Send + Sync {
    /// Accepts a message, assigns its sequence, and describes its delivery.
    ///
    /// Idempotent by `message_id` (brief section 68): a retry of a send that
    /// already landed is a **success** carrying `duplicate: true` and no fanout,
    /// because the recipients already have it and a second copy would be a second
    /// notification for one message. The same id with a different payload is not a
    /// retry — it is two different messages claiming one identity — and fails with
    /// `IDEMPOTENCY_MISMATCH`.
    ///
    /// Fails with `NOT_FOUND` when the conversation does not exist *or* the caller
    /// is not in it — one answer for both, so that the operation cannot be used to
    /// discover which conversations exist. `BLOCKED_BY_USER`, `ROOM_ARCHIVED`,
    /// `CONFLICT`, `VALIDATION_FAILED`, `PAYLOAD_TOO_LARGE`, and `RATE_LIMITED`
    /// are the rest.
    async fn send(
        &self,
        caller: &Caller,
        request: MessageSend,
    ) -> Result<(MessageAccepted, Option<Fanout>)>;

    /// Moves a delivery or read watermark forward.
    ///
    /// Cumulative and forward-only (brief section 158): a receipt is a
    /// high-water mark, not an event about one message, so a client that reports
    /// an older sequence than it already reported changes nothing. A sequence
    /// above the conversation's own high-water mark is clamped to it rather than
    /// refused — a client that raced a send should not have to handle an error for
    /// having been fast.
    ///
    /// Reading implies delivery. A `Read` receipt advances both watermarks, and
    /// still produces exactly one frame: readers derive `delivered >= read`
    /// themselves, and a second frame saying so would double the receipt traffic
    /// of every conversation.
    ///
    /// Returns `None` when nothing moved.
    async fn receipt(&self, caller: &Caller, request: MessageReceipt) -> Result<Option<Fanout>>;

    /// Edits a message's envelope in place, preserving its sequence number.
    ///
    /// Only the sender may edit. The envelope is opaque ciphertext the server cannot
    /// read; what the server enforces is ownership, the conversation's membership, and
    /// that the edit lands atomically under the message's original seq so every client's
    /// ordering stays intact.
    async fn edit(
        &self,
        caller: &Caller,
        conversation_id: Id,
        message_id: Id,
        envelope: Vec<u8>,
    ) -> Result<(MessageAccepted, Option<Fanout>)>;

    /// Tombstones a message for everyone in the conversation.
    ///
    /// The tombstone keeps the message's sequence (brief section 67) and loses its
    /// envelope. Both halves matter: the sequence stays so the space of sequence
    /// numbers has no hole for a syncing client to mistake for lost data, and the
    /// ciphertext goes so that "delete" means deleted rather than hidden on one
    /// screen.
    ///
    /// Only the sender may delete. Moderator deletion in a room is
    /// `migo-moderation`'s to authorise, because the question it answers — who
    /// holds which role, and is this an appealable action — is a moderation
    /// question with an audit trail attached, not a messaging one.
    ///
    /// `for_everyone: false` fails with `FEATURE_DISABLED`. There is nowhere to
    /// record a per-member hide, and silently doing nothing to a user who asked
    /// for a message to disappear from their own screen is the worst of the three
    /// available behaviours.
    ///
    /// Returns `None` for a message that was already a tombstone.
    async fn delete(
        &self,
        caller: &Caller,
        request: MessageDelete,
    ) -> Result<(MessageAccepted, Option<Fanout>)>;

    /// Catches a client up, or pages older history.
    ///
    /// Forward by default: the client sends the highest contiguous sequence it
    /// holds and reads forward from there. No diffing, no timestamps, no agreement
    /// about clocks — which is why the sequence is per conversation and gapless.
    ///
    /// `backwards: true` pages *older* history instead, downward from `have_seq`.
    ///
    /// The `limit` is clamped to the server's maximum rather than refused (brief
    /// section 157), and `status` is [`SyncStatus::Truncated`] whenever the answer
    /// has a hole in it — a client that asked from before the oldest surviving
    /// message is told so, and renders a boundary, instead of being handed a
    /// shorter history that looks complete.
    ///
    /// [`SyncStatus::Truncated`]: migo_protocol::SyncStatus::Truncated
    async fn sync(&self, caller: &Caller, request: SyncRequest) -> Result<SyncResponse>;

    /// One page of the caller's conversation list, most recently active first.
    ///
    /// Paged by opaque cursor, not by offset: the list reorders itself whenever
    /// anybody sends anything, so an offset silently skips and repeats rows. A
    /// cursor comes back whenever the page was full; `None` means the caller has
    /// reached the end.
    async fn conversations(
        &self,
        caller: &Caller,
        request: ConversationListRequest,
    ) -> Result<ConversationListResponse>;

    /// Creates a direct or group conversation.
    ///
    /// Direct conversations are idempotent in their member set: two devices
    /// tapping "message Bob" at the same moment must not produce two
    /// conversations, so the pair is a key and the second caller reads the first
    /// one's row.
    ///
    /// `ConversationKind::Room` is refused. A room has an owner, a home region, a
    /// join policy, a member count, and a moderation surface, and creating one
    /// through the conversation endpoint would create the conversation without any
    /// of them.
    async fn create(
        &self,
        caller: &Caller,
        request: ConversationCreateRequest,
    ) -> Result<ConversationSummary>;

    /// Seats new members in a group.
    ///
    /// Any member may invite — a group nobody could grow except its founders
    /// would be a group that could only shrink. The checks are the create call's:
    /// each invitee is refused if either side has blocked the other, and the
    /// roster may not pass the group ceiling (`GROUP_FULL`).
    ///
    /// One `ConversationMemberEvent(Joined)` per person actually seated, so
    /// clients can rotate sender keys member by member rather than diffing the
    /// roster. A name that is already in the group is skipped, not an error: two
    /// members inviting the same friend in the same second is a race both won,
    /// not a conflict either needs to see.
    async fn invite(
        &self,
        caller: &Caller,
        request: ConversationInviteRequest,
    ) -> Result<(ConversationSummary, Vec<Fanout>)>;

    /// The caller's own departure from a group.
    ///
    /// Leaving is a right, not a permission: no founder gates it, because a
    /// group somebody could not leave would be a detention. Direct conversations
    /// are refused — a two-party conversation is not left, it is deleted or
    /// blocked, and a room's membership belongs to the room service.
    ///
    /// When the last founder walks out, the longest-standing member inherits the
    /// role, so the group never reaches a state where nobody can rename it or
    /// answer a report. The succession is a write, not an announcement: roles
    /// travel in the roster, and the member event that says "they left" does not
    /// also get to say who was promoted.
    ///
    /// A kick vote aimed at the leaver closes as they go — the question it asked
    /// has answered itself — and every remaining tally is recounted against the
    /// smaller roster on its next voice.
    async fn leave(
        &self,
        caller: &Caller,
        request: ConversationLeaveRequest,
    ) -> Result<Vec<Fanout>>;

    /// The full membership of one conversation, as the caller's roster sees it.
    ///
    /// Active members first, by join time, then the departed: a member row is
    /// tombstoned rather than deleted so history stays attributable, and the
    /// roster is where "who used to be here" is legible. The caller must be in
    /// the conversation — a roster is the membership list, and a membership list
    /// is not a public fact.
    async fn roster(
        &self,
        caller: &Caller,
        request: ConversationRosterRequest,
    ) -> Result<ConversationRosterResponse>;

    /// A founder gags one member, or lifts the gag.
    ///
    /// `until` is an absolute moment; `None` clears a mute early. A mute silences
    /// what a member says, not who they are: they stay in the roster, they keep
    /// their vote, and the moment passes they may speak again without anybody
    /// readmitting them.
    ///
    /// No frame. A mute is between a founder and one member, and announcing it
    /// to the group turns a correction into a spectacle — the same call the room
    /// service makes, for the same reason.
    async fn mute(&self, caller: &Caller, request: ConversationMuteRequest) -> Result<()>;

    /// A founder removes one member outright, no vote.
    ///
    /// The vote exists for members who must act together; the founders are the
    /// group's memory of who built it, and the memory does not need permission
    /// to protect itself. The other founder is beyond this reach — the pair
    /// exists so that neither alone is the group.
    async fn kick(&self, caller: &Caller, request: ConversationKickRequest) -> Result<Vec<Fanout>>;

    /// Starts a group kick vote, or adds the caller's voice to one running.
    ///
    /// A strict majority of the roster — half rounded up, floor of two, the same
    /// arithmetic a room's vote uses — carries the kick, and the member event
    /// that follows *is* the closing: no separate frame says the vote ended,
    /// because the removal says it in the words every client already renders.
    ///
    /// One question at a time per group (`VOTE_ALREADY_OPEN`). The founders are
    /// beyond a vote (`VOTE_TARGET_IMMUNE`), and a muted member keeps their
    /// voice — a mute silences speech, not citizenship.
    async fn vote_kick(
        &self,
        caller: &Caller,
        request: ConversationVoteKickRequest,
    ) -> Result<(ConversationVoteKickResponse, Vec<Fanout>)>;

    /// A founder renames a group.
    ///
    /// The title is the group's name in every member's list, and this is the one
    /// write that changes it — the create call is the other. Same limit as a
    /// room's name, in characters a person typed.
    ///
    /// Returns the renamed summary for the founder, and a state event carrying
    /// the new title for everyone else — deltas only, coalesced per
    /// conversation, exactly the way a room's rename travels.
    async fn update(
        &self,
        caller: &Caller,
        request: ConversationUpdateRequest,
    ) -> Result<(ConversationSummary, Option<Fanout>)>;

    /// Records that the caller started or stopped typing.
    ///
    /// The mark lives in the cache with a TTL, so losing the cache loses typing
    /// indicators and nothing else (brief section 158/5241). Brief section 728
    /// forbids a typing event from entering an offline queue at all — a typing
    /// indicator delivered late is not late information, it is wrong information —
    /// and the opcode's `Coalescable` delivery class is what tells the gateway so.
    ///
    /// Returns `None` when the mark did not change, which is section 156 again.
    async fn typing(&self, caller: &Caller, request: TypingEvent) -> Result<Option<Fanout>>;

    /// Whether the caller is currently in this conversation.
    ///
    /// For the transport, which has to decide whether a session may subscribe to a
    /// conversation's topic before it will deliver a single event from it. That
    /// decision cannot live in the gateway: a topic id is just an id there, and
    /// nothing in a transport crate knows what a conversation is (brief section
    /// 177).
    ///
    /// One boolean, and deliberately not the `NOT_FOUND` that every other read on
    /// this trait raises. The caller asks about a batch of topics at once and is
    /// told which it may have, without being told which of "there is no such
    /// conversation" and "you are not in it" applied to the rest -- the same
    /// conflation `conversation_for` makes internally, in the only shape a batch
    /// answer can take. A caller who could tell those apart would have a probe for
    /// which conversations exist, 512 ids at a time.
    ///
    /// Not rate limited, for the reason `migo_social`'s `may_interact` is not: it is
    /// called inside an operation the transport has already charged for.
    async fn is_participant(&self, caller: &Caller, conversation_id: Id) -> Result<bool>;

    /// Deletes messages whose disappearing-message deadline has passed.
    ///
    /// For the background sweeper, not for a request handler: it takes no caller
    /// because there is none, and a `limit` because a sweep that tried to catch up
    /// on a month of expiries in one statement would hold locks across a table
    /// every send needs.
    ///
    /// No fanout. A client learns that an expired message is gone by expiring it
    /// locally at the deadline it was given — every client has the same deadline,
    /// so telling them all again would be a broadcast to say what they already
    /// knew, timed to arrive after they acted on it.
    async fn purge_expired(&self, now: Timestamp, limit: u16) -> Result<u64>;
}
