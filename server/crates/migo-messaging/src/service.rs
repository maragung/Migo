//! The messaging service.
//!
//! # Shape
//!
//! [`Messages`] is generic over the store, the cache, and the limiter with `dyn`
//! defaults, so it can be held as `Messages` (all three erased, one vtable call
//! per operation) or as `Messages<MemoryStore, MemoryCache, CacheRateLimiter>`
//! (monomorphised, no vtable at all). Being generic over `?Sized` and bounded on
//! the narrow traits accepts both a concrete backend and a fully erased one, which
//! is what the tests and the composition root respectively need.
//!
//! # The invariants this file exists to hold
//!
//! Four properties are load-bearing, and every one of them is easy to break with a
//! change that looks like a simplification.
//!
//! **A sequence is assigned once, in the store, per conversation.** Nothing here
//! computes `last_seq + 1`. Reading the high-water mark and then writing one past
//! it is two statements, and two concurrent sends interleave into two messages
//! with the same sequence — which brief section 67 forbids, and which a client's
//! gap detector reports as data loss.
//!
//! **A retry is a success.** Section 68: the same `message_id` twice returns the
//! original with `duplicate: true`. The alternative — an error — is worse than
//! useless, because the client that retried did so precisely because it never saw
//! the first answer, and an error would make it show a failure for a message that
//! was delivered.
//!
//! **Nothing is sent when nothing changed.** Section 156. A receipt for a
//! sequence already acknowledged, a delete of an existing tombstone, and a typing
//! `Start` that was already set all return no fanout. In a group of forty, one
//! badly behaved client would otherwise turn its own idle loop into forty frames
//! per iteration.
//!
//! **The server never sees a plaintext and never stores a second copy of what the
//! envelope already binds.** The envelope arrives sealed and is stored as it
//! arrived. `sender_key_id` is the visible consequence: see [`Messages::send`].
//!
//! # What is deliberately not here
//!
//! *No edit.* `MESSAGE_EDIT` is a later opcode range and
//! [`MessagingStore::edit_message`] is already waiting for it, but an edit is not
//! an append with a different verb: it needs an edit history for moderation to
//! reason about, a rule for what an edit does to a quoted reply, and a decision
//! about whether an edited message re-notifies. Shipping the storage call without
//! those would be shipping the part that cannot be got wrong.
//!
//! *No push notification.* A message that reaches nobody's screen has to become a
//! notification, and deciding that is `migo-notify`'s job: it owns mute state,
//! quiet hours, per-platform token handling, and the `notified_seq` watermark that
//! keeps a second device coming online from re-notifying. The cursor field it needs
//! is written by the store and left alone here.
//!
//! *No presence.* Whether a recipient is online changes what the gateway does with
//! a [`Fanout`], not whether one is produced.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use migo_cache::traits::TypingCache;
use migo_cache::{Cache, SharedCache, Ttl};
use migo_core::metrics::Registry;
use migo_core::{Id, OsRandom, Random, Result, Timestamp};
use migo_protocol::{
    codes, fault, ConversationCreateRequest, ConversationInviteRequest, ConversationKickRequest,
    ConversationKind, ConversationLeaveRequest, ConversationListRequest, ConversationListResponse,
    ConversationMemberEvent, ConversationMuteRequest, ConversationRole, ConversationRosterEntry,
    ConversationRosterRequest, ConversationRosterResponse, ConversationStateEvent,
    ConversationSummary, ConversationUpdateRequest, ConversationVoteEvent,
    ConversationVoteKickRequest, ConversationVoteKickResponse, EncryptionMode, MemberChange,
    MessageAccepted, MessageDelete, MessageEvent, MessageKind, MessageReceipt, MessageSend, Opcode,
    ReceiptKind, SyncRequest, SyncResponse, SyncStatus, TypingEvent, TypingState,
};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter};
use migo_store::model::{
    Appended, Conversation, ConversationMember, Cursor, NewMessage, StoredMessage,
};
use migo_store::traits::{clamp_limit, MessagingStore, SocialStore};
use migo_store::{SharedStore, Store};
use migo_wire::limits::MAX_BYTES_LEN;
use parking_lot::Mutex;

use crate::cursor;
use crate::fanout::{Broadcast, Fanout};
use crate::metrics::{Meters, SendOutcome, SyncOutcome};
use crate::model::{
    Caller, DEFAULT_CONVERSATION_PAGE, MAX_EXPIRY_MS, MAX_GROUP_MEMBERS, MAX_TITLE_LEN,
    MEMBER_PREVIEW, TYPING_TTL_MS, VOTE_TTL_MS,
};
use crate::traits::Messaging;

/// A shared, fully erased messaging service.
pub type SharedMessaging = Arc<dyn Messaging>;

/// The first valid sequence in a conversation. Zero means "nothing yet".
const FIRST_SEQ: i64 = 1;

/// Conversations, messages, receipts, sync, and typing.
pub struct Messages<S: ?Sized = dyn Store, C: ?Sized = dyn Cache, L: ?Sized = dyn RateLimiter> {
    store: Arc<S>,
    cache: Arc<C>,
    limiter: Arc<L>,
    /// The randomness source, behind a lock because [`Random`] is `Send` and not
    /// `Sync`.
    ///
    /// Used for exactly one thing: the id of a conversation this service creates.
    /// Message ids come from the client, which is what makes a send idempotent.
    /// Every use is sixteen bytes and the lock is never held across an `await`,
    /// which is what keeps a mutex off the critical path of a scheduler.
    random: Mutex<Box<dyn Random>>,
    meters: Meters,
    /// The group kick votes currently open, keyed by conversation. One question
    /// at a time per group; see [`OpenVote`] for the lazy expiry that keeps this
    /// map from holding a vote nobody is answering.
    votes: Mutex<HashMap<Id, OpenVote>>,
}

/// A group kick vote in flight.
struct OpenVote {
    /// Who the vote would remove.
    target: Id,
    /// Who has voted, deduplicated by account: the same voice twice is the same
    /// voice once, and the tally must not move for a retry.
    voters: HashSet<Id>,
    /// When the vote opened, for the lazy expiry.
    opened_at: Timestamp,
}

impl OpenVote {
    /// Whether this vote has outlived [`VOTE_TTL_MS`] with no new voice.
    ///
    /// Strictly greater, so a vote is alive for its whole sixtieth second — the
    /// same edge a room's vote takes, and the one a client counting along at
    /// home expects.
    fn expired(&self, now: Timestamp) -> bool {
        now.as_millis().saturating_sub(self.opened_at.as_millis()) > VOTE_TTL_MS
    }
}

/// Builds the messaging service over the shared backends.
///
/// Infallible, and it stays that way on purpose. There is no `MessagingConfig`:
/// the one limit an operator might want to move is the page cap, and that already
/// lives in [`migo_store::MAX_PAGE`] because the store is what has to hold to it.
/// A second copy in a second config section would be one number with two sources
/// of truth, and the failure mode is a service that accepts a page the store then
/// clamps — a bug that shows up as "the client says 200, the response has 200,
/// paging still skips rows".
#[must_use]
pub fn open(
    store: SharedStore,
    cache: SharedCache,
    limiter: SharedRateLimiter,
    registry: &Registry,
) -> SharedMessaging {
    Arc::new(Messages::new(
        store,
        cache,
        limiter,
        registry,
        Box::new(OsRandom) as Box<dyn Random>,
    ))
}

impl<S, C, L> Messages<S, C, L>
where
    S: MessagingStore + SocialStore + ?Sized,
    C: TypingCache + ?Sized,
    L: RateLimiter + ?Sized,
{
    /// Builds the service over a concrete or erased set of backends.
    ///
    /// `random` is injected rather than fixed to [`OsRandom`] so a simulation can
    /// replay a run byte for byte (ADR-0009).
    pub fn new(
        store: Arc<S>,
        cache: Arc<C>,
        limiter: Arc<L>,
        registry: &Registry,
        random: Box<dyn Random>,
    ) -> Self {
        Self {
            store,
            cache,
            limiter,
            random: Mutex::new(random),
            meters: Meters::new(registry),
            votes: Mutex::new(HashMap::new()),
        }
    }

    // --- shared checks ---------------------------------------------------------

    /// Charges the caller's own surfaces for one operation.
    ///
    /// Two buckets, tightest first, because a refusal short-circuits the rest and
    /// the narrow bucket is the one that should get the chance to refuse: a client
    /// hammering one opcode should hit that opcode's limit before it consumes the
    /// account's whole allowance and starts refusing its user's other traffic.
    ///
    /// The device is deliberately not a third bucket. A per-device limit tighter
    /// than the account's would let one account's laptop starve its own phone, and
    /// a looser one would let an attacker with a stolen session mint device ids to
    /// escape the account limit.
    async fn charge(&self, caller: &Caller, opcode: Opcode) -> Result<()> {
        let keys = [
            BucketKey::endpoint_write_of_account(caller.account_id, opcode),
            BucketKey::account_write(caller.account_id),
        ];
        // `charge_opcode` and not `charge`: ADR-0006 puts the price on the opcode, so
        // naming the opcode makes it impossible to charge the wrong amount, and a
        // reprice in the IDL takes effect here without an edit.
        self.limiter
            .charge_opcode(&keys, opcode, caller.tier, caller.now)
            .await?
            .into_result()
    }

    /// The conversation, if the caller is in it.
    ///
    /// One answer — `NOT_FOUND` — for "there is no such conversation" and "you are
    /// not in it". `PERMISSION_DENIED` would be more informative and that is
    /// exactly the problem: it confirms the id names something real, which turns
    /// any messaging endpoint into a probe for which conversations exist. The
    /// caller who genuinely is a member never sees either.
    async fn conversation_for(&self, caller: &Caller, conversation_id: Id) -> Result<Conversation> {
        let conversation = self
            .store
            .conversation(conversation_id)
            .await?
            .ok_or_else(|| fault::not_found("conversation"))?;
        if !self
            .store
            .is_member(conversation_id, caller.account_id)
            .await?
        {
            return Err(fault::not_found("conversation"));
        }
        Ok(conversation)
    }

    /// The other party in a direct conversation, if this is one.
    ///
    /// `None` for a group or a room, where there is no single other party and the
    /// block question is answered by whoever manages membership.
    async fn direct_peer(&self, conversation: &Conversation, caller: Id) -> Result<Option<Id>> {
        if conversation.kind != ConversationKind::Direct {
            return Ok(None);
        }
        Ok(self
            .store
            .members(conversation.conversation_id)
            .await?
            .into_iter()
            .find(|member| member.left_at.is_none() && member.account_id != caller)
            .map(|member| member.account_id))
    }

    /// Refuses a send into a direct conversation where either party is blocked.
    ///
    /// Checked on every send rather than once when the conversation was created,
    /// because a block that only took effect on new conversations would not be a
    /// block: the person you blocked is, by definition, someone you have already
    /// been talking to.
    ///
    /// The refusal is visible to the sender, and that is a decision rather than an
    /// oversight. The alternative that hides it — accept, store, deliver to nobody
    /// — writes a message into a conversation where it can never be read and
    /// reports success for it. Brief section 180 requires the *call* path to be
    /// indistinguishable, for a good reason that does not transfer: a rejected call
    /// leaks through ring duration and answer timing whatever the server says,
    /// while a message that is accepted and dropped leaks nothing and lies to its
    /// sender indefinitely.
    ///
    /// Only direct conversations are checked. A group is a room-shaped object with
    /// its own membership: two members who block each other keep seeing each
    /// other's messages there, because the group is not a private channel between
    /// them and filtering it per pair is a rendering decision for the client.
    async fn refuse_if_blocked(&self, conversation: &Conversation, caller: Id) -> Result<()> {
        let Some(peer) = self.direct_peer(conversation, caller).await? else {
            return Ok(());
        };
        if self.store.is_blocked_either_way(caller, peer).await? {
            return Err(fault::error(
                codes::BLOCKED_BY_USER,
                "one of the two parties has blocked the other",
            ));
        }
        Ok(())
    }

    /// The gateway never produces one. It is checked anyway because a nil account
    /// id would be a membership row shared by every unauthenticated request, and a
    /// nil device id would make the fanout exclusion match somebody else's socket.
    /// Every group operation needs the pair for the same reason a room's does.
    fn require_identity(caller: &Caller) -> Result<()> {
        if caller.account_id.is_nil() || caller.device_id.is_nil() {
            return Err(fault::unauthenticated(
                "groups need an identified account and device",
            ));
        }
        Ok(())
    }

    /// Refuses a group operation on anything that is not a group.
    ///
    /// A direct conversation has exactly its two members and no way to grow; a
    /// room's membership belongs to the room service, which owns the join policy
    /// and the moderation surface that admit people. Letting either through here
    /// would write a membership row no other surface can explain.
    fn require_group(conversation: &Conversation) -> Result<()> {
        if conversation.kind == ConversationKind::Group {
            return Ok(());
        }
        Err(fault::validation(
            "kind",
            "this is a group operation; a direct conversation has no third member, and a room's membership belongs to the room service",
        ))
    }

    /// One member's active row, if they hold one.
    ///
    /// Membership is tombstoned rather than deleted, so "is in the group" and
    /// "has a row" are different questions and every permission check here must
    /// ask the first one.
    fn active_row(members: &[ConversationMember], account_id: Id) -> Option<&ConversationMember> {
        members
            .iter()
            .find(|m| m.account_id == account_id && m.left_at.is_none())
    }

    /// The conversation as the caller's list row will show it, read back from the
    /// store rather than assembled from the request.
    ///
    /// The store is the one truth for a summary: a response that could disagree
    /// with the next list read is a flicker in the UI and a bug that reproduces
    /// once. The two reads this costs are on operations a human performs one at a
    /// time — invite, rename — and never on the send path.
    async fn summary_for(
        &self,
        caller: &Caller,
        conversation: &Conversation,
    ) -> Result<ConversationSummary> {
        let cursor = self
            .store
            .cursor(conversation.conversation_id, caller.account_id)
            .await?;
        let members = self.store.members(conversation.conversation_id).await?;
        let me = Self::active_row(&members, caller.account_id);
        let mut ids: Vec<Id> = members
            .iter()
            .filter(|m| m.left_at.is_none())
            .map(|m| m.account_id)
            .collect();
        ids.sort_unstable();
        Ok(ConversationSummary {
            conversation_id: conversation.conversation_id,
            kind: conversation.kind,
            encryption: conversation.encryption,
            last_seq: conversation.last_seq.max(0) as u64,
            read_seq: cursor.read_seq.max(0) as u64,
            title: conversation.title.clone(),
            avatar_url: None,
            members: Some(ids),
            last_message: None,
            muted_until: me.and_then(|m| m.muted_until),
            pinned: me.map(|m| m.pinned).unwrap_or(false).then_some(true),
            archived: conversation.archived_at.is_some().then_some(true),
        })
    }

    /// Refuses anything that writes into an archived conversation.
    ///
    /// A room gets `ROOM_ARCHIVED`, which the client already knows how to render
    /// as a read-only banner. Anything else gets `CONFLICT`, because there is no
    /// code for an archived direct conversation and inventing one in a domain crate
    /// would put a protocol constant somewhere the schema cannot see it.
    fn refuse_if_archived(conversation: &Conversation) -> Result<()> {
        if conversation.archived_at.is_none() {
            return Ok(());
        }
        if conversation.room_id.is_some() {
            return Err(fault::error(
                codes::ROOM_ARCHIVED,
                "the room is archived and takes no more messages",
            ));
        }
        Err(fault::conflict("the conversation is archived"))
    }

    /// A fresh id stamped with `now`.
    fn new_id(&self, now: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(now, &mut **random)
    }
}

#[async_trait]
impl<S, C, L> Messaging for Messages<S, C, L>
where
    S: MessagingStore + SocialStore + ?Sized + Send + Sync,
    C: TypingCache + ?Sized + Send + Sync,
    L: RateLimiter + ?Sized + Send + Sync,
{
    async fn send(
        &self,
        caller: &Caller,
        request: MessageSend,
    ) -> Result<(MessageAccepted, Option<Fanout>)> {
        // Shape first, and before the rate limiter: a malformed frame is a client
        // bug, and charging for it would let one broken build exhaust its user's
        // allowance and take their working devices down with it.
        if request.message_id.is_nil() {
            self.meters.send(SendOutcome::Invalid);
            return Err(fault::field_required("message_id"));
        }
        if request.conversation_id.is_nil() {
            self.meters.send(SendOutcome::Invalid);
            return Err(fault::field_required("conversation_id"));
        }
        if request.kind == MessageKind::Unknown {
            self.meters.send(SendOutcome::Invalid);
            return Err(fault::validation(
                "kind",
                "not a message kind this build knows",
            ));
        }
        if request.envelope.is_empty() {
            self.meters.send(SendOutcome::Invalid);
            return Err(fault::field_required("envelope"));
        }
        if request.envelope.len() > MAX_BYTES_LEN {
            self.meters.send(SendOutcome::Invalid);
            return Err(fault::field_too_long("envelope", MAX_BYTES_LEN));
        }
        if let Some(expires_in) = request.expires_in_ms {
            if expires_in == 0 || expires_in > MAX_EXPIRY_MS {
                self.meters.send(SendOutcome::Invalid);
                return Err(fault::validation(
                    "expires_in_ms",
                    "outside the supported disappearing-message range",
                ));
            }
        }

        if let Err(error) = self.charge(caller, Opcode::MessageSend).await {
            self.meters.send(SendOutcome::RateLimited);
            return Err(error);
        }

        let conversation = match self.conversation_for(caller, request.conversation_id).await {
            Ok(conversation) => conversation,
            Err(error) => {
                self.meters.send(SendOutcome::Unknown);
                return Err(error);
            }
        };
        if let Err(error) = Self::refuse_if_archived(&conversation) {
            self.meters.send(SendOutcome::Archived);
            return Err(error);
        }
        if let Err(error) = self
            .refuse_if_blocked(&conversation, caller.account_id)
            .await
        {
            self.meters.send(SendOutcome::Blocked);
            return Err(error);
        }
        // A group's mute, enforced at the one moment it can be: the send. The
        // founder's write landed on the membership row and this is that row's
        // say. Only groups carry a mute, so the read never touches the send path
        // of a direct conversation — and the group path already reads membership
        // to answer the block question, so this is the same order of cost as a
        // room checking sanctions on speak.
        if conversation.kind == ConversationKind::Group {
            let muted = self
                .store
                .members(request.conversation_id)
                .await?
                .iter()
                .find(|m| m.account_id == caller.account_id)
                .is_some_and(|m| {
                    m.muted_until
                        .is_some_and(|until| until.as_millis() > caller.now.as_millis())
                });
            if muted {
                self.meters.send(SendOutcome::Muted);
                return Err(fault::error(
                    codes::MUTED,
                    "the caller is muted in this group",
                ));
            }
        }

        // Taken before the envelope is moved into the store, because the duplicate
        // branch below has to compare the retry against the request and the
        // request no longer exists by then. See [`SendFingerprint`].
        let fingerprint = SendFingerprint::of(caller, &request);

        let appended = self
            .store
            .append_message(NewMessage {
                message_id: request.message_id,
                conversation_id: request.conversation_id,
                sender_id: caller.account_id,
                sender_device: Some(caller.device_id),
                kind: request.kind,
                envelope: request.envelope,
                reply_to: request.reply_to,
                // Computed from the server's clock, not carried as a duration.
                // Two clients with different clocks must agree on when a
                // disappearing message disappears, and the only clock they both
                // trust is the one that sequenced the message.
                expires_at: request
                    .expires_in_ms
                    .map(|ms| caller.now.saturating_add_millis(i64::from(ms))),
                created_at: caller.now,
            })
            .await?;

        let stored = appended.message().clone();
        let accepted = MessageAccepted {
            message_id: stored.message_id,
            conversation_id: stored.conversation_id,
            seq: stored.seq.max(0) as u64,
            created_at: stored.created_at,
            duplicate: (!appended.is_new()).then_some(true),
        };

        if let Appended::Duplicate(_) = appended {
            // Same id, two different messages: whatever is returned is wrong for
            // one of them, so neither is returned. Section 68 makes a retry a
            // success, and a retry is the same message — an id reused for
            // different content is a client bug, and reporting it is how the
            // client's author finds out before their users do.
            if !fingerprint.matches(&stored) {
                self.meters.send(SendOutcome::Mismatch);
                return Err(fault::error(
                    codes::IDEMPOTENCY_MISMATCH,
                    "this message id was already used for a different message",
                ));
            }
            // No fanout. The recipients already have it, and a second copy would
            // be a second notification for one message. A recipient who somehow
            // does not have it converges through sync, which is the mechanism that
            // exists for exactly this.
            self.meters.send(SendOutcome::Duplicate);
            return Ok((accepted, None));
        }

        // The sender has, by construction, seen and read what they just sent. Not
        // advancing their own cursor would leave their unread count including
        // their own messages, which is wrong on the sending device and wrong again
        // on every other device the account has.
        self.store
            .advance_cursor(
                stored.conversation_id,
                caller.account_id,
                Some(stored.seq),
                Some(stored.seq),
                None,
                caller.now,
            )
            .await?;

        self.meters.send(SendOutcome::Accepted);
        let fanout = Fanout::to_conversation(
            stored.conversation_id,
            caller.device_id,
            Broadcast::Message(event_of(&stored)),
        );
        Ok((accepted, Some(fanout)))
    }

    async fn receipt(&self, caller: &Caller, request: MessageReceipt) -> Result<Option<Fanout>> {
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        if request.kind == ReceiptKind::Unknown {
            return Err(fault::validation(
                "kind",
                "not a receipt kind this build knows",
            ));
        }
        self.charge(caller, Opcode::MessageReceipt).await?;
        let conversation = self
            .conversation_for(caller, request.conversation_id)
            .await?;

        // Clamped, not refused. A client that read a message the same millisecond
        // a newer one arrived can legitimately report a sequence above the one the
        // server has finished writing, and turning that race into an error would
        // make being fast a failure.
        let requested = i64::try_from(request.seq).unwrap_or(i64::MAX);
        let seq = requested.min(conversation.last_seq);
        if seq < FIRST_SEQ {
            self.meters.receipt(false);
            return Ok(None);
        }

        // Read before write, so that "did anything move" has an answer. Section
        // 4528 requires no frame when nothing changed, and without the prior
        // value the only implementable rule is "always send" — which in a group
        // turns one client's redundant receipts into a broadcast per member per
        // receipt.
        let before = self
            .store
            .cursor(request.conversation_id, caller.account_id)
            .await?;
        let (delivered, read) = match request.kind {
            // Reading implies delivery. A client that reports a read without the
            // delivery that must have preceded it is not lying, it is just
            // economising on frames, and the two watermarks should not be left
            // inconsistent because of it.
            ReceiptKind::Read => (Some(seq), Some(seq)),
            _ => (Some(seq), None),
        };
        let after = self
            .store
            .advance_cursor(
                request.conversation_id,
                caller.account_id,
                delivered,
                read,
                None,
                caller.now,
            )
            .await?;
        if !moved(&before, &after) {
            self.meters.receipt(false);
            return Ok(None);
        }

        self.meters.receipt(true);
        // One frame, carrying the kind that was reported. A `Read` that also moved
        // the delivery watermark does not produce a second `Delivered` frame:
        // section 158 has readers derive `delivered >= read`, and the second frame
        // would double every conversation's receipt traffic to restate it.
        let watermark = match request.kind {
            ReceiptKind::Read => after.read_seq,
            _ => after.delivered_seq,
        };
        Ok(Some(Fanout::to_conversation(
            request.conversation_id,
            caller.device_id,
            Broadcast::Receipt(MessageReceipt {
                conversation_id: request.conversation_id,
                kind: request.kind,
                seq: watermark.max(0) as u64,
                // Filled in by the server. A client-supplied subject on a receipt
                // would let any member claim somebody else had read a message.
                user_id: Some(caller.account_id),
                at: Some(caller.now),
            }),
        )))
    }

    async fn edit(
        &self,
        caller: &Caller,
        conversation_id: Id,
        message_id: Id,
        envelope: Vec<u8>,
    ) -> Result<(MessageAccepted, Option<Fanout>)> {
        if message_id.is_nil() {
            return Err(fault::field_required("message_id"));
        }
        if conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        if envelope.is_empty() {
            return Err(fault::field_required("envelope"));
        }
        self.charge(caller, Opcode::MessageEdit).await?;
        // Same posture as delete: membership is required, and the conversation is then
        // discarded — an archived conversation still lets the sender fix what they said.
        self.conversation_for(caller, conversation_id).await?;

        let existing = self
            .store
            .message(conversation_id, message_id)
            .await?
            .ok_or_else(|| fault::not_found("message"))?;
        if existing.sender_id != caller.account_id {
            return Err(fault::permission_denied(
                "only the sender may edit a message",
            ));
        }
        if existing.deleted_at.is_some() {
            // A tombstone cannot be edited back into existence; the sender's recourse
            // is a new message, which is exactly what every other participant sees.
            return Err(fault::not_found("message"));
        }

        let edited = self
            .store
            .edit_message(conversation_id, message_id, envelope, caller.now)
            .await?
            .ok_or_else(|| fault::not_found("message"))?;
        self.meters.edited();

        let accepted = MessageAccepted {
            message_id: edited.message_id,
            conversation_id: edited.conversation_id,
            seq: edited.seq.max(0) as u64,
            created_at: edited.created_at,
            duplicate: Some(false),
        };
        Ok((
            accepted,
            Some(Fanout::to_conversation(
                conversation_id,
                caller.device_id,
                Broadcast::Message(event_of(&edited)),
            )),
        ))
    }

    async fn delete(
        &self,
        caller: &Caller,
        request: MessageDelete,
    ) -> Result<(MessageAccepted, Option<Fanout>)> {
        if request.message_id.is_nil() {
            return Err(fault::field_required("message_id"));
        }
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        if !request.for_everyone {
            // Refused rather than accepted-and-ignored. There is no per-member
            // hide table, so the only honest answers are "no" and a schema change,
            // and a success that does nothing to a user who asked for a message to
            // leave their screen is the one answer that cannot be right.
            return Err(fault::feature_disabled("message_delete_for_me"));
        }
        self.charge(caller, Opcode::MessageDelete).await?;
        // Membership is required and the conversation itself is then discarded: an
        // archived conversation still lets a sender withdraw their own message,
        // because a deletion removes content rather than adding it, and the right
        // to take back something you said should not expire when a room closes.
        self.conversation_for(caller, request.conversation_id)
            .await?;

        let existing = self
            .store
            .message(request.conversation_id, request.message_id)
            .await?
            .ok_or_else(|| fault::not_found("message"))?;
        if existing.sender_id != caller.account_id {
            // `PERMISSION_DENIED` and not `NOT_FOUND`: the caller is already
            // established as a member and the message is one they can see, so
            // there is nothing left to hide, and pretending it is missing would
            // send a client into a resync looking for it.
            return Err(fault::permission_denied(
                "only the sender may delete a message for everyone",
            ));
        }

        let accepted = MessageAccepted {
            message_id: existing.message_id,
            conversation_id: existing.conversation_id,
            // The sequence the message was given when it was sent. Section 67:
            // a tombstone keeps its sequence, so the numbering has no hole for a
            // syncing client to read as lost data.
            seq: existing.seq.max(0) as u64,
            created_at: existing.created_at,
            duplicate: existing.deleted_at.is_some().then_some(true),
        };
        if existing.deleted_at.is_some() {
            return Ok((accepted, None));
        }

        let tombstone = self
            .store
            .delete_message(
                request.conversation_id,
                request.message_id,
                caller.account_id,
                caller.now,
            )
            .await?
            // Between the read above and this write the row can only have been
            // removed by the expiry sweeper. Reporting it as missing is then the
            // truth, and it is the same answer the client would have got a moment
            // earlier.
            .ok_or_else(|| fault::not_found("message"))?;

        self.meters.deleted();
        Ok((
            accepted,
            Some(Fanout::to_conversation(
                request.conversation_id,
                caller.device_id,
                Broadcast::Message(event_of(&tombstone)),
            )),
        ))
    }

    async fn sync(&self, caller: &Caller, request: SyncRequest) -> Result<SyncResponse> {
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        self.charge(caller, Opcode::Sync).await?;
        let conversation = self
            .conversation_for(caller, request.conversation_id)
            .await?;

        let limit = page_limit(request.limit);
        let have = i64::try_from(request.have_seq).unwrap_or(i64::MAX);
        let backwards = request.backwards.unwrap_or(false);

        let (messages, more, truncated) = if backwards {
            // `history_before` is newest-first; the response is ascending, because
            // `from_seq <= to_seq` says so and because a client appending a page
            // to its own store should not have to reverse it first.
            let mut page = self
                .store
                .history_before(
                    request.conversation_id,
                    // Zero means "from the newest", which is what a client with
                    // no history at all reports when it asks for the last page.
                    (have > 0).then_some(have),
                    limit,
                )
                .await?;
            page.reverse();
            // Conservative in the only direction that is safe to be wrong in. A
            // full page might be the last one, and claiming there may be more
            // costs one empty request; claiming there is none when there is loses
            // history the user can see is missing. The exact answer would need
            // either a count or a fetch of one extra row, and neither is worth
            // paying for on every scroll.
            let more = page.len() == usize::from(limit);
            // Nothing was skipped: paging downward from a known point returns the
            // rows immediately below it, and the bottom of the conversation is the
            // bottom whether or not older messages were once there. Truncation
            // going backwards is what `more == false` already says.
            (page, more, false)
        } else {
            let mut page = self
                .store
                .history_after(request.conversation_id, have.max(0), limit)
                .await?;
            if let Some(to) = request.to_seq {
                // A ranged fetch for a gap the client detected. Trimmed here
                // rather than pushed into the store, because an upper bound is a
                // property of this one request and every backend would have to
                // grow a parameter for it.
                let ceiling = i64::try_from(to).unwrap_or(i64::MAX);
                page.retain(|message| message.seq <= ceiling);
            }
            // Exact, and free: the conversation's own high-water mark was read
            // above, so whether anything remains is a comparison rather than a
            // query.
            let highest = page.last().map_or(have, |message| message.seq);
            let more = highest < conversation.last_seq;
            // A hole at the front of the page means the messages between what the
            // client holds and what came back are gone — expired, or deleted
            // hard — and will never arrive. Section 158: the client is told, so it
            // can render a boundary, rather than handed a shorter history that
            // looks complete.
            let truncated = match page.first() {
                Some(first) => first.seq > have + 1,
                // An empty forward page with messages still above the client's
                // watermark means everything in between is gone. `more` is left
                // false because there is nothing to come back for.
                None => request.to_seq.is_none() && conversation.last_seq > have,
            };
            (page, more, truncated)
        };

        let outcome = if truncated {
            SyncOutcome::Truncated
        } else if more {
            SyncOutcome::More
        } else {
            SyncOutcome::Complete
        };
        self.meters.synced(outcome, messages.len());

        // A zero range means "nothing". Sequences start at one, so zero cannot be
        // mistaken for a real position, which `have_seq` on both ends could be.
        let from_seq = messages.first().map_or(0, |m| m.seq.max(0) as u64);
        let to_seq = messages.last().map_or(0, |m| m.seq.max(0) as u64);
        Ok(SyncResponse {
            conversation_id: request.conversation_id,
            status: if truncated {
                SyncStatus::Truncated
            } else {
                SyncStatus::Ok
            },
            from_seq,
            to_seq,
            more,
            messages: messages.iter().map(event_of).collect(),
        })
    }

    async fn conversations(
        &self,
        caller: &Caller,
        request: ConversationListRequest,
    ) -> Result<ConversationListResponse> {
        self.charge(caller, Opcode::ConversationList).await?;
        let limit = if request.limit == 0 {
            DEFAULT_CONVERSATION_PAGE
        } else {
            page_limit(request.limit)
        };
        let after = match request.cursor.as_deref() {
            Some(text) => Some(cursor::decode(text)?),
            None => None,
        };

        let rows = self
            .store
            .conversation_list(caller.account_id, limit, MEMBER_PREVIEW, after)
            .await?;
        self.meters.conversations_listed();

        // A cursor whenever the page was full. It may turn out to name the end of
        // the list, which costs the client one request that comes back empty; the
        // alternative is fetching one row past the page on every request to answer
        // a question most callers never ask, because most callers stop after the
        // first screenful.
        let next_cursor = (rows.len() == usize::from(limit))
            .then(|| rows.last().map(|row| cursor::encode(row.position())))
            .flatten();
        Ok(ConversationListResponse {
            conversations: rows.iter().map(summary_of).collect(),
            next_cursor,
        })
    }

    async fn create(
        &self,
        caller: &Caller,
        request: ConversationCreateRequest,
    ) -> Result<ConversationSummary> {
        // Dedup and drop the caller: a client that lists its own user in the
        // member set is not making a mistake worth an error, and two copies of one
        // member would either violate the primary key or silently create a group
        // with a phantom seat.
        let mut others: Vec<Id> = Vec::with_capacity(request.members.len());
        for member in &request.members {
            if member.is_nil() {
                return Err(fault::validation("members", "contains an empty identifier"));
            }
            if *member != caller.account_id && !others.contains(member) {
                others.push(*member);
            }
        }
        if others.is_empty() {
            return Err(fault::validation(
                "members",
                "a conversation needs somebody other than its creator",
            ));
        }
        // The title is a group's alone. A direct conversation's name is the other
        // person — accepting a title there would store something no client would
        // ever render — and a room's is the room's own, carried by the room.
        let title = match request.title.as_deref() {
            Some(raw) if !raw.trim().is_empty() => match request.kind {
                ConversationKind::Group => {
                    let trimmed = raw.trim();
                    // `chars().count()` and not `len()`: the limit is what a person
                    // typed, and a byte limit would give an Indonesian or Arabic
                    // title half the room of an English one for no reason a user
                    // could discover.
                    if trimmed.chars().count() > MAX_TITLE_LEN {
                        return Err(fault::field_too_long("title", MAX_TITLE_LEN));
                    }
                    Some(trimmed.to_owned())
                }
                _ => {
                    return Err(fault::validation(
                        "title",
                        "only a group conversation carries a title",
                    ));
                }
            },
            _ => None,
        };

        match request.kind {
            ConversationKind::Direct if others.len() != 1 => {
                return Err(fault::validation(
                    "members",
                    "a direct conversation is between exactly two accounts",
                ));
            }
            ConversationKind::Group if others.len() + 1 > MAX_GROUP_MEMBERS => {
                return Err(fault::validation(
                    "members",
                    "more members than a group may have",
                ));
            }
            ConversationKind::Direct | ConversationKind::Group => {}
            ConversationKind::Room => {
                // A room has an owner, a home region that sequences it, a join
                // policy, a slug, a capacity, and a moderation surface. Creating
                // the conversation here would create it without any of them, and
                // the result would be a room that cannot be joined, browsed, or
                // moderated.
                return Err(fault::validation(
                    "kind",
                    "rooms are created through the room service",
                ));
            }
            ConversationKind::Unknown => {
                return Err(fault::validation(
                    "kind",
                    "not a conversation kind this build knows",
                ));
            }
        }

        self.charge(caller, Opcode::ConversationCreate).await?;

        // Sequentially, and every member. Stopping at the first block would leak
        // which member blocked the caller through the ordering of the checks, and
        // running them concurrently would multiply a single refused create into
        // one query per member against a table the block itself is meant to keep
        // quiet.
        for member in &others {
            if self
                .store
                .is_blocked_either_way(caller.account_id, *member)
                .await?
            {
                return Err(fault::error(
                    codes::BLOCKED_BY_USER,
                    "one of the proposed members has blocked the caller, or is blocked by them",
                ));
            }
        }

        let conversation_id = self.new_id(caller.now);
        let conversation = if request.kind == ConversationKind::Direct {
            // Idempotent by member pair, resolved in the store: two devices
            // tapping "message Bob" at the same moment must converge on one
            // conversation, and the loser of that race reads the winner's row
            // rather than creating a second one.
            self.store
                .direct_conversation(
                    caller.account_id,
                    others[0],
                    conversation_id,
                    EncryptionMode::EndToEnd,
                    caller.now,
                )
                .await?
        } else {
            let mut members = Vec::with_capacity(others.len() + 1);
            members.push(caller.account_id);
            members.extend_from_slice(&others);
            let conversation = self
                .store
                .create_conversation(
                    Conversation {
                        conversation_id,
                        kind: ConversationKind::Group,
                        // Always end-to-end. A private conversation the server
                        // could read would be a different product, and there is
                        // no request field that could ask for one.
                        encryption: EncryptionMode::EndToEnd,
                        room_id: None,
                        last_seq: 0,
                        created_by: caller.account_id,
                        created_at: caller.now,
                        last_message_at: None,
                        archived_at: None,
                        title: title.clone(),
                    },
                    members,
                )
                .await?;
            // The creator and the first person they named are the group's two
            // founders: the pair who may mute, kick, and rename without a vote.
            // Everyone else the create call seated is a member, and `create`
            // seats nobody as a founder, so the promotions are additions and not
            // corrections. The store wrote the rows a moment ago; a missing row
            // here is an internal state fault and not a client error.
            for founder in [caller.account_id, others[0]] {
                if !self
                    .store
                    .set_conversation_role(conversation_id, founder, ConversationRole::Founder)
                    .await?
                {
                    return Err(fault::internal(
                        "a founder row is missing right after creation",
                    ));
                }
            }
            conversation
        };

        // Read rather than assumed zero. A direct conversation that already
        // existed has history, and reporting `last_seq: 0` for it would tell the
        // creating client its conversation was empty — which it would then render
        // as a blank thread over a real one.
        let cursor = self
            .store
            .cursor(conversation.conversation_id, caller.account_id)
            .await?;
        self.meters.conversation_created();

        let mut members = Vec::with_capacity(others.len() + 1);
        members.push(caller.account_id);
        members.extend_from_slice(&others);
        members.sort_unstable();
        Ok(ConversationSummary {
            conversation_id: conversation.conversation_id,
            kind: conversation.kind,
            encryption: conversation.encryption,
            last_seq: conversation.last_seq.max(0) as u64,
            read_seq: cursor.read_seq.max(0) as u64,
            // The stored title, not the request's: a direct conversation ignores
            // the field above, and this summary is also what the store reads back
            // for every later list — one truth, read from the row.
            title: conversation.title.clone(),
            avatar_url: None,
            members: Some(members),
            last_message: None,
            muted_until: None,
            pinned: None,
            archived: conversation.archived_at.is_some().then_some(true),
        })
    }

    async fn invite(
        &self,
        caller: &Caller,
        request: ConversationInviteRequest,
    ) -> Result<(ConversationSummary, Vec<Fanout>)> {
        Self::require_identity(caller)?;
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        if request.members.is_empty() {
            return Err(fault::field_required("members"));
        }
        for member in &request.members {
            if member.is_nil() {
                return Err(fault::validation("members", "contains an empty identifier"));
            }
        }
        self.charge(caller, Opcode::ConversationInvite).await?;
        let conversation = self
            .conversation_for(caller, request.conversation_id)
            .await?;
        Self::require_group(&conversation)?;
        let members = self.store.members(request.conversation_id).await?;

        // Already-seated names are dropped, not refused: two members inviting the
        // same friend in the same second is a race both won, and neither needs an
        // error for the other's timing. The caller's own id goes the same way —
        // inviting yourself to a group you are in is a tautology.
        let mut fresh: Vec<Id> = Vec::with_capacity(request.members.len());
        for member in &request.members {
            let seated = members
                .iter()
                .any(|m| m.account_id == *member && m.left_at.is_none());
            if *member != caller.account_id && !seated && !fresh.contains(member) {
                fresh.push(*member);
            }
        }
        let active = members
            .iter()
            .filter(|m| m.left_at.is_none())
            .count()
            .max(1); // the caller is in it, so this is never zero in practice
        if active + fresh.len() > MAX_GROUP_MEMBERS {
            return Err(fault::error(
                codes::GROUP_FULL,
                "more members than a group may have",
            ));
        }

        // Sequentially, and every name: same reason as the create call — stopping
        // at the first block would leak which invitee blocked the caller through
        // the ordering of the refusals.
        for member in &fresh {
            if self
                .store
                .is_blocked_either_way(caller.account_id, *member)
                .await?
            {
                return Err(fault::error(
                    codes::BLOCKED_BY_USER,
                    "one of the proposed members has blocked the caller, or is blocked by them",
                ));
            }
        }

        // One frame per person actually seated, so clients rotate sender keys
        // member by member. The count is the count after all the seats, because a
        // client coalescing these still has to end on the truth.
        let count = (active + fresh.len()) as u32;
        let mut fanouts = Vec::with_capacity(fresh.len());
        for member in &fresh {
            self.store
                .add_member(ConversationMember {
                    conversation_id: request.conversation_id,
                    account_id: *member,
                    // Seated by invitation, never by inheritance: founders come
                    // from the create call and the succession in `leave`, and an
                    // invited member who could arrive a founder would make the
                    // founder check a function of who happened to invite.
                    role: ConversationRole::Member,
                    joined_at: caller.now,
                    left_at: None,
                    muted_until: None,
                    pinned: false,
                })
                .await?;
            fanouts.push(Fanout::to_conversation(
                request.conversation_id,
                caller.device_id,
                Broadcast::Member(ConversationMemberEvent {
                    conversation_id: request.conversation_id,
                    user_id: *member,
                    change: MemberChange::Joined,
                    member_count: count,
                }),
            ));
        }

        let summary = self.summary_for(caller, &conversation).await?;
        Ok((summary, fanouts))
    }

    async fn leave(
        &self,
        caller: &Caller,
        request: ConversationLeaveRequest,
    ) -> Result<Vec<Fanout>> {
        Self::require_identity(caller)?;
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        self.charge(caller, Opcode::ConversationLeave).await?;
        let conversation = self
            .conversation_for(caller, request.conversation_id)
            .await?;
        Self::require_group(&conversation)?;
        let members = self.store.members(request.conversation_id).await?;
        let me = Self::active_row(&members, caller.account_id)
            .ok_or_else(|| fault::not_found("conversation member"))?;

        self.store
            .remove_member(request.conversation_id, caller.account_id, caller.now)
            .await?;

        let remaining: Vec<&ConversationMember> = members
            .iter()
            .filter(|m| m.left_at.is_none() && m.account_id != caller.account_id)
            .collect();
        let count = remaining.len() as u32;

        // The last founder out names a successor. Leaving is a right and founders
        // are people, so a group can absolutely reach a state where its founders
        // have all walked — but not a state where nobody can rename it or answer
        // a report, which is what an unpromoted remainder would be. The heir is
        // the longest-standing member, which is the closest thing a roster has to
        // a memory of the group's own founding. No announcement: roles travel in
        // the roster, and the next roster read is the truth.
        if me.role == ConversationRole::Founder
            && !remaining.is_empty()
            && !remaining
                .iter()
                .any(|m| m.role == ConversationRole::Founder)
        {
            let heir = remaining
                .iter()
                .min_by_key(|m| m.joined_at)
                .expect("non-empty by the check above");
            if !self
                .store
                .set_conversation_role(
                    request.conversation_id,
                    heir.account_id,
                    ConversationRole::Founder,
                )
                .await?
            {
                return Err(fault::internal(
                    "the heir's membership row vanished between the read and the promotion",
                ));
            }
        }

        let mut fanouts = Vec::new();
        // A vote aimed at the leaver closes as they go: the question it asked has
        // answered itself, and a tally still running against an empty seat is a
        // tally that could "pass" and remove nobody. The voters keep their record
        // of it; the registry does not.
        {
            let mut votes = self.votes.lock();
            if let Some(open) = votes.get(&request.conversation_id) {
                if open.target == caller.account_id {
                    let closed = votes
                        .remove(&request.conversation_id)
                        .expect("just checked present");
                    fanouts.push(Fanout::unattributed(
                        request.conversation_id,
                        Broadcast::Vote(ConversationVoteEvent {
                            conversation_id: request.conversation_id,
                            target_id: closed.target,
                            votes: closed.voters.len() as u32,
                            needed: votes_needed(count),
                            member_count: count,
                            closed: Some(true),
                        }),
                    ));
                }
            }
        }
        fanouts.push(Fanout::to_conversation(
            request.conversation_id,
            caller.device_id,
            Broadcast::Member(ConversationMemberEvent {
                conversation_id: request.conversation_id,
                user_id: caller.account_id,
                change: MemberChange::Left,
                member_count: count,
            }),
        ));
        Ok(fanouts)
    }

    async fn roster(
        &self,
        caller: &Caller,
        request: ConversationRosterRequest,
    ) -> Result<ConversationRosterResponse> {
        Self::require_identity(caller)?;
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        self.charge(caller, Opcode::ConversationRoster).await?;
        // `conversation_for` is the whole authorization: a roster is the
        // membership list, and a membership list is not a public fact.
        self.conversation_for(caller, request.conversation_id)
            .await?;
        let mut members = self.store.members(request.conversation_id).await?;
        // Active first, by join time, then the departed in the order they left.
        // The member a person reads a roster for is "who is here"; "who used to
        // be" stays attributable underneath it, because history is.
        members.sort_by(|a, b| {
            (a.left_at.is_some(), a.joined_at).cmp(&(b.left_at.is_some(), b.joined_at))
        });
        Ok(ConversationRosterResponse {
            entries: members
                .into_iter()
                .map(|m| ConversationRosterEntry {
                    account_id: m.account_id,
                    role: m.role,
                    joined_at: m.joined_at,
                    muted_until: m.muted_until,
                    left_at: m.left_at,
                })
                .collect(),
        })
    }

    async fn mute(&self, caller: &Caller, request: ConversationMuteRequest) -> Result<()> {
        Self::require_identity(caller)?;
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        if request.target_id.is_nil() {
            return Err(fault::field_required("target_id"));
        }
        if request.target_id == caller.account_id {
            return Err(fault::validation(
                "target_id",
                "a founder cannot mute themselves",
            ));
        }
        if request
            .until
            .is_some_and(|until| until.as_millis() <= caller.now.as_millis())
        {
            return Err(fault::validation("until", "a mute must end in the future"));
        }
        self.charge(caller, Opcode::ConversationMute).await?;
        let conversation = self
            .conversation_for(caller, request.conversation_id)
            .await?;
        Self::require_group(&conversation)?;
        let members = self.store.members(request.conversation_id).await?;
        let me = Self::active_row(&members, caller.account_id)
            .ok_or_else(|| fault::not_found("conversation member"))?;
        if me.role != ConversationRole::Founder {
            return Err(fault::permission_denied("only a founder may mute a member"));
        }
        let target = Self::active_row(&members, request.target_id)
            .ok_or_else(|| fault::not_found("conversation member"))?;
        if target.role == ConversationRole::Founder {
            return Err(fault::permission_denied(
                "the other founder is beyond a mute",
            ));
        }
        if !self
            .store
            .set_conversation_mute(request.conversation_id, request.target_id, request.until)
            .await?
        {
            return Err(fault::not_found("conversation member"));
        }
        // No frame. A mute is between a founder and one member, and announcing
        // it to the group turns a correction into a spectacle — the same call the
        // room service makes about its mutes, for the same reason. The member
        // learns of it from the send that refuses them and the roster that shows
        // the expiry.
        Ok(())
    }

    async fn kick(&self, caller: &Caller, request: ConversationKickRequest) -> Result<Vec<Fanout>> {
        Self::require_identity(caller)?;
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        if request.target_id.is_nil() {
            return Err(fault::field_required("target_id"));
        }
        if request.target_id == caller.account_id {
            return Err(fault::conflict(
                "a founder cannot be kicked by their own hand",
            ));
        }
        self.charge(caller, Opcode::ConversationKick).await?;
        let conversation = self
            .conversation_for(caller, request.conversation_id)
            .await?;
        Self::require_group(&conversation)?;
        let members = self.store.members(request.conversation_id).await?;
        let me = Self::active_row(&members, caller.account_id)
            .ok_or_else(|| fault::not_found("conversation member"))?;
        if me.role != ConversationRole::Founder {
            return Err(fault::permission_denied(
                "only a founder may remove a member outright",
            ));
        }
        let target = Self::active_row(&members, request.target_id)
            .ok_or_else(|| fault::not_found("conversation member"))?;
        // The pair exists so that neither founder alone is the group. A founder
        // the other founder could remove would be a member with extra chores.
        if target.role == ConversationRole::Founder {
            return Err(fault::permission_denied(
                "the other founder is beyond a founder's kick",
            ));
        }

        self.store
            .remove_member(request.conversation_id, request.target_id, caller.now)
            .await?;
        let count = members
            .iter()
            .filter(|m| m.left_at.is_none() && m.account_id != request.target_id)
            .count() as u32;

        let mut fanouts = Vec::new();
        // A vote running against the target is moot now, and it closes with the
        // same frame an expiry gets: a `closed` tally, not a silent drop, so a
        // client still rendering the running count stops.
        {
            let mut votes = self.votes.lock();
            if let Some(open) = votes.get(&request.conversation_id) {
                if open.target == request.target_id {
                    let closed = votes
                        .remove(&request.conversation_id)
                        .expect("just checked present");
                    fanouts.push(Fanout::unattributed(
                        request.conversation_id,
                        Broadcast::Vote(ConversationVoteEvent {
                            conversation_id: request.conversation_id,
                            target_id: closed.target,
                            votes: closed.voters.len() as u32,
                            needed: votes_needed(count),
                            member_count: count,
                            closed: Some(true),
                        }),
                    ));
                }
            }
        }
        // `Kicked` and not `Left`: a client that could not tell a removal from a
        // departure could not colour the two differently in its roster.
        fanouts.push(Fanout::to_conversation(
            request.conversation_id,
            caller.device_id,
            Broadcast::Member(ConversationMemberEvent {
                conversation_id: request.conversation_id,
                user_id: request.target_id,
                change: MemberChange::Kicked,
                member_count: count,
            }),
        ));
        Ok(fanouts)
    }

    async fn vote_kick(
        &self,
        caller: &Caller,
        request: ConversationVoteKickRequest,
    ) -> Result<(ConversationVoteKickResponse, Vec<Fanout>)> {
        Self::require_identity(caller)?;
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        if request.target_id.is_nil() {
            return Err(fault::field_required("target_id"));
        }
        if request.target_id == caller.account_id {
            return Err(fault::conflict("a vote cannot be aimed at its own voter"));
        }
        self.charge(caller, Opcode::ConversationVoteKick).await?;
        let conversation = self
            .conversation_for(caller, request.conversation_id)
            .await?;
        Self::require_group(&conversation)?;
        let members = self.store.members(request.conversation_id).await?;

        // No role check on the caller: the vote is the one lever ordinary members
        // hold, and gating it on the founder's role would reduce it to a button
        // the founders could have pressed on their own. A muted member keeps
        // their voice for the same reason — a mute silences what they say, not
        // how they may vote about the group they are in.
        let target = Self::active_row(&members, request.target_id)
            .ok_or_else(|| fault::not_found("conversation member"))?;
        if target.role == ConversationRole::Founder {
            return Err(fault::error(
                codes::VOTE_TARGET_IMMUNE,
                "a group's founders cannot be voted out",
            ));
        }
        let member_count = members.iter().filter(|m| m.left_at.is_none()).count() as u32;
        let needed = votes_needed(member_count);

        let mut fanouts = Vec::new();
        let response;
        let mut passed = false;
        {
            let mut votes = self.votes.lock();
            // The lazy expiry. A vote nobody finished is dropped the moment the
            // group sees another one, and the group is told it closed — a client
            // still rendering the old tally would otherwise show a question the
            // server has stopped asking. No timer exists and none is needed.
            if let Some(open) = votes.get(&request.conversation_id) {
                if open.expired(caller.now) {
                    let closed = votes
                        .remove(&request.conversation_id)
                        .expect("just checked present");
                    fanouts.push(Fanout::unattributed(
                        request.conversation_id,
                        Broadcast::Vote(ConversationVoteEvent {
                            conversation_id: request.conversation_id,
                            target_id: closed.target,
                            votes: closed.voters.len() as u32,
                            needed,
                            member_count,
                            closed: Some(true),
                        }),
                    ));
                }
            }
            match votes.get_mut(&request.conversation_id) {
                Some(open) if open.target == request.target_id => {
                    if !open.voters.insert(caller.account_id) {
                        // The same voice twice. The tally did not move, so nothing
                        // is published — the retry gets the same answer the first
                        // call got, and no other member's screen moves.
                        return Ok((
                            ConversationVoteKickResponse {
                                votes: open.voters.len() as u32,
                                needed,
                                member_count,
                                open: true,
                            },
                            fanouts,
                        ));
                    }
                    let count = open.voters.len() as u32;
                    if count >= needed {
                        // Passed. The registry entry is dropped here, inside the
                        // lock, so a second vote starting while the kick's writes
                        // run cannot see a tally that has already decided; the
                        // writes themselves happen outside it.
                        votes.remove(&request.conversation_id);
                        passed = true;
                        response = ConversationVoteKickResponse {
                            votes: count,
                            needed,
                            member_count,
                            open: false,
                        };
                    } else {
                        response = ConversationVoteKickResponse {
                            votes: count,
                            needed,
                            member_count,
                            open: true,
                        };
                        fanouts.push(Fanout::to_conversation(
                            request.conversation_id,
                            caller.device_id,
                            Broadcast::Vote(ConversationVoteEvent {
                                conversation_id: request.conversation_id,
                                target_id: request.target_id,
                                votes: count,
                                needed,
                                member_count,
                                closed: None,
                            }),
                        ));
                    }
                }
                Some(_) => {
                    // One question at a time per group: two interleaved tallies
                    // would let a faction split the group's attention and pass
                    // the one nobody was counting.
                    return Err(fault::error(
                        codes::VOTE_ALREADY_OPEN,
                        "another kick vote is already open in this group",
                    ));
                }
                None => {
                    let mut voters = HashSet::new();
                    voters.insert(caller.account_id);
                    votes.insert(
                        request.conversation_id,
                        OpenVote {
                            target: request.target_id,
                            voters,
                            opened_at: caller.now,
                        },
                    );
                    response = ConversationVoteKickResponse {
                        votes: 1,
                        needed,
                        member_count,
                        open: true,
                    };
                    fanouts.push(Fanout::to_conversation(
                        request.conversation_id,
                        caller.device_id,
                        Broadcast::Vote(ConversationVoteEvent {
                            conversation_id: request.conversation_id,
                            target_id: request.target_id,
                            votes: 1,
                            needed,
                            member_count,
                            closed: None,
                        }),
                    ));
                }
            }
        }
        if passed {
            // Outside the lock: this is a store write, and holding a mutex across
            // an await would put the registry on a scheduler's critical path.
            self.store
                .remove_member(request.conversation_id, request.target_id, caller.now)
                .await?;
            let count = member_count.saturating_sub(1);
            // The member event is the tally's closing: a `ConversationVoteEvent`
            // with `closed: false` would tell the group a vote ended, and the
            // removal it caused says that already, in the frame every client
            // renders. A client watching the tally sees it stop at `open: true`
            // and the member go in the same breath.
            fanouts.push(Fanout::to_conversation(
                request.conversation_id,
                caller.device_id,
                Broadcast::Member(ConversationMemberEvent {
                    conversation_id: request.conversation_id,
                    user_id: request.target_id,
                    change: MemberChange::Kicked,
                    member_count: count,
                }),
            ));
        }
        Ok((response, fanouts))
    }

    async fn update(
        &self,
        caller: &Caller,
        request: ConversationUpdateRequest,
    ) -> Result<(ConversationSummary, Option<Fanout>)> {
        Self::require_identity(caller)?;
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        let Some(raw) = request.title.as_deref() else {
            return Err(fault::field_required("title"));
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(fault::validation(
                "title",
                "a group's title cannot be empty",
            ));
        }
        if trimmed.chars().count() > MAX_TITLE_LEN {
            return Err(fault::field_too_long("title", MAX_TITLE_LEN));
        }
        self.charge(caller, Opcode::ConversationUpdate).await?;
        let conversation = self
            .conversation_for(caller, request.conversation_id)
            .await?;
        Self::require_group(&conversation)?;
        let members = self.store.members(request.conversation_id).await?;
        let me = Self::active_row(&members, caller.account_id)
            .ok_or_else(|| fault::not_found("conversation member"))?;
        if me.role != ConversationRole::Founder {
            return Err(fault::permission_denied(
                "only a founder may rename a group",
            ));
        }
        if !self
            .store
            .set_conversation_title(request.conversation_id, Some(trimmed))
            .await?
        {
            return Err(fault::not_found("conversation"));
        }

        let mut renamed = conversation.clone();
        renamed.title = Some(trimmed.to_owned());
        let summary = self.summary_for(caller, &renamed).await?;
        // The state event is the rename's delivery: deltas only, coalesced per
        // conversation, exactly the way a room's rename travels. A member who was
        // offline for it reads the title from their next list, so the event has
        // to be right for the members who are present and no more.
        let fanout = Fanout::to_conversation(
            request.conversation_id,
            caller.device_id,
            Broadcast::State(ConversationStateEvent {
                conversation_id: request.conversation_id,
                title: Some(trimmed.to_owned()),
            }),
        );
        Ok((summary, Some(fanout)))
    }

    async fn typing(&self, caller: &Caller, request: TypingEvent) -> Result<Option<Fanout>> {
        if request.conversation_id.is_nil() {
            return Err(fault::field_required("conversation_id"));
        }
        if request.state == TypingState::Unknown {
            return Err(fault::validation(
                "state",
                "not a typing state this build knows",
            ));
        }
        self.charge(caller, Opcode::Typing).await?;
        let conversation = self
            .conversation_for(caller, request.conversation_id)
            .await?;
        // An archived conversation takes no messages, so an indicator saying
        // somebody is composing one is an indicator that cannot come true.
        Self::refuse_if_archived(&conversation)?;

        // The mark lives only in the cache. Losing it loses typing indicators and
        // nothing else, which is what makes the cache legitimately ephemeral
        // (brief section 173) — and it is why a cache failure here is reported
        // rather than swallowed: silently dropping the write would leave a stale
        // mark visible for its whole TTL.
        match request.state {
            TypingState::Start => {
                self.cache
                    .set_typing(
                        request.conversation_id,
                        caller.account_id,
                        Ttl::from_millis(TYPING_TTL_MS),
                        caller.now,
                    )
                    .await?;
            }
            _ => {
                self.cache
                    .clear_typing(request.conversation_id, caller.account_id)
                    .await?;
            }
        }
        self.meters.typed();

        // Always a frame, unlike a receipt. A repeated `Start` is a *refresh*
        // (brief section 15) and its whole purpose is to reset the recipients'
        // TTL, so suppressing it as unchanged would make the indicator expire
        // under a user who never stopped typing. Section 156 is about state that
        // did not change, and a deadline that moved is a change.
        Ok(Some(Fanout::to_conversation(
            request.conversation_id,
            caller.device_id,
            Broadcast::Typing(TypingEvent {
                conversation_id: request.conversation_id,
                state: request.state,
                // Filled in by the server, for the same reason a receipt's is: a
                // client-supplied subject would let any member claim that somebody
                // else was typing.
                user_id: Some(caller.account_id),
            }),
        )))
    }

    async fn is_participant(&self, caller: &Caller, conversation_id: Id) -> Result<bool> {
        if conversation_id.is_nil() {
            return Ok(false);
        }
        // The membership row alone, not `conversation_for`: a membership row for a conversation
        // that does not exist cannot be written, so the existence read would be a second query
        // that can only agree with this one. One indexed lookup per topic is also what keeps this
        // usable on the subscribe path, where section 14 forbids reading a roster.
        self.store
            .is_member(conversation_id, caller.account_id)
            .await
    }

    async fn purge_expired(&self, now: Timestamp, limit: u16) -> Result<u64> {
        let purged = self
            .store
            .purge_expired_messages(now, clamp_limit(limit) as u16)
            .await?;
        self.meters.expired(purged);
        Ok(purged)
    }
}

/// Turns a stored row into the event that goes on the wire.
///
/// `sender_key_id` is **not** carried, and this is the one place where that
/// decision is visible, so it is written down here.
///
/// The field is accepted on [`MessageSend`] because the sender's key
/// generation is part of what a recipient needs to decrypt. It is not echoed,
/// because the authoritative copy is already inside the envelope: brief section
/// 11 makes `sender_key_id` a varint within the sealed payload *and* part of
/// the AEAD associated data, so the copy the recipient uses is authenticated
/// and cannot be tampered with in flight. A second copy in a plaintext protocol
/// field would be unauthenticated, could disagree with the bound one, and would
/// hand a client the choice of which to trust — which is how a downgrade gets
/// built by accident.
///
/// The storage side settles it in any case: [`StoredMessage`] has no column for
/// it, so echoing it on the live path and not on the sync path would make the
/// field's presence depend on whether the recipient happened to be online.
fn event_of(message: &StoredMessage) -> MessageEvent {
    MessageEvent {
        message_id: message.message_id,
        conversation_id: message.conversation_id,
        // Stored as `i64` because a database column is signed, sent as `u64`
        // because a sequence is never negative. The clamp keeps a corrupt row
        // from wrapping into an enormous sequence, which a client's gap
        // detector would read as "you are missing four billion messages".
        seq: message.seq.max(0) as u64,
        sender_id: message.sender_id,
        // A message whose sending device was not recorded reports `Id::NIL`,
        // which is the protocol's own "absent" value for a required field. The
        // alternative — an arbitrary device id — would make another device on
        // the same account believe it had sent the message itself.
        sender_device: message.sender_device.unwrap_or(Id::NIL),
        kind: message.kind,
        envelope: message.envelope.clone(),
        created_at: message.created_at,
        reply_to: message.reply_to,
        edited_at: message.edited_at,
        // `None` rather than `Some(false)`: an optional field that is always
        // present costs a byte on every message in the product to say nothing.
        deleted: message.deleted_at.is_some().then_some(true),
        sender_key_id: None,
    }
}

/// Whether a cursor moved.
///
/// `notified_seq` is not compared. It is written by `migo-notify`, not by anything
/// here, and including it would make a push notification look like a read receipt
/// to the fanout decision.
fn moved(before: &Cursor, after: &Cursor) -> bool {
    after.delivered_seq > before.delivered_seq || after.read_seq > before.read_seq
}

/// Clamps a client's page request into the server's range.
///
/// Section 157: a larger request is made smaller, not refused. The `u32` is
/// narrowed by saturation rather than by `as`, because `as` on 65_536 produces
/// zero, and a zero would then be clamped up to one — turning "give me everything"
/// into "give me one message" and a client into an infinite pager.
fn page_limit(requested: u32) -> u16 {
    let narrowed = u16::try_from(requested).unwrap_or(u16::MAX);
    clamp_limit(narrowed) as u16
}

/// What a send has to agree with for a repeat of its id to count as a retry.
///
/// # Why not the envelope itself
///
/// Comparing the bytes would be exact, and it would cost a clone of every
/// envelope on every send — up to [`MAX_BYTES_LEN`] — kept alive past the store
/// call purely so that the one send in ten thousand that is a duplicate can be
/// checked. The store consumes the envelope precisely so it does not have to be
/// copied, and undoing that to serve the rare path would be paying the whole
/// product's hot path for it.
///
/// So the length stands in for the content. It is not a proof of equality and is
/// not claimed to be one: two different plaintexts can seal to the same number of
/// bytes. It is a guard against *confusion* — a client that reuses an id for a
/// different message, which is what this check exists to catch — and combined with
/// the sender, the kind, and the quoted message it catches that case in almost
/// every real shape it takes. Anything it lets through is byte-identical in
/// length, same kind, same reply target, from the same account: indistinguishable
/// from a retry by everything the server can see without keeping the plaintext it
/// is forbidden to have.
///
/// # Why the sender is in it
///
/// A message id is scoped to a conversation, not to an account, so two members can
/// pick the same one. Without this field the second sender would be told their
/// message was delivered while what everyone actually holds is the first sender's
/// — an authorship confusion the sender could never detect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct SendFingerprint {
    sender_id: Id,
    kind: MessageKind,
    reply_to: Option<Id>,
    envelope_len: usize,
}

impl SendFingerprint {
    fn of(caller: &Caller, request: &MessageSend) -> Self {
        Self {
            sender_id: caller.account_id,
            kind: request.kind,
            reply_to: request.reply_to,
            envelope_len: request.envelope.len(),
        }
    }

    /// Whether `stored` is plausibly the same message this fingerprint describes.
    fn matches(self, stored: &StoredMessage) -> bool {
        if self.sender_id != stored.sender_id {
            return false;
        }
        // A tombstone had its payload cleared, so its length carries no
        // information and comparing it would report every resend of a deleted
        // message as a mismatch. Authorship still has to agree, which is checked
        // above and is the part that matters once the content is gone.
        if stored.deleted_at.is_some() {
            return true;
        }
        self.kind == stored.kind
            && self.reply_to == stored.reply_to
            && self.envelope_len == stored.envelope.len()
    }
}

/// Turns a store row into the protocol summary.
///
/// `title` and `avatar_url` are always absent, and that is a boundary rather than
/// an omission. For a room they live in the room aggregate, which `migo-rooms`
/// owns; reading them here would be one query per row of a two-hundred-row list,
/// against a table this crate has no business joining. An avatar additionally
/// needs a signed URL, which `migo-media` mints and which brief section 174
/// forbids from reaching a log — so it is minted at the edge, close to the
/// response, and not carried through a domain layer that logs.
/// Voices a group kick needs, from a roster of `member_count`.
///
/// Half the group rounded up, with a floor of two — the same arithmetic a room's
/// vote uses, because the user's rule is "like in room" and a group deserves the
/// same discipline. The floor keeps a group of one from voting, for the same
/// reason it keeps a room of one from voting: the only person a sole member
/// could name is themselves, and self-removal is what
/// [`Messaging::leave`] is for.
#[must_use]
pub const fn votes_needed(member_count: u32) -> u32 {
    if member_count < 2 {
        2
    } else {
        member_count.div_ceil(2)
    }
}

fn summary_of(row: &migo_store::model::ConversationSummary) -> ConversationSummary {
    ConversationSummary {
        conversation_id: row.conversation.conversation_id,
        kind: row.conversation.kind,
        encryption: row.conversation.encryption,
        last_seq: row.conversation.last_seq.max(0) as u64,
        read_seq: row.cursor.read_seq.max(0) as u64,
        title: row.conversation.title.clone(),
        avatar_url: None,
        members: Some(row.members.clone()),
        last_message: row.last_message.as_ref().map(event_of),
        muted_until: row.member.muted_until,
        // `None` rather than `Some(false)`: an optional flag that is always
        // present pays a byte per row to say nothing.
        pinned: row.member.pinned.then_some(true),
        archived: row.conversation.archived_at.is_some().then_some(true),
    }
}
