//! The application dispatcher: transport opcodes translated into domain calls.
//!
//! The gateway owns the connection — the handshake, the heartbeat, backpressure, resume, and the
//! subscription registry — and knows nothing about what an application request *means*. Every
//! application opcode on a `Ready` session is handed to a [`Dispatcher`], the one trait the
//! composition root implements to wire the domain crates in behind the transport (brief section
//! 177). [`AppDispatcher`] is that implementation for `migod`.
//!
//! # The shape of every handler
//!
//! One request becomes four steps, always in this order:
//!
//! 1. **Build the caller.** The authenticated [`Identity`] the gateway proved
//!    becomes the domain's `Caller` — account, device, trust tier, and the single sampled `now`.
//!    Each domain has its own `Caller` type on purpose: they are not interchangeable, and the
//!    composition root is the one place that holds all of them at once.
//! 2. **Decode the body.** [`from_frame`] against the type the opcode names. A body that will not
//!    decode is the client's fault and comes back as a wire fault, never a panic.
//! 3. **Call the service.** Exactly one method, awaited. Its return type decides step 4.
//! 4. **Answer and fan out.** A method that returns a payload is answered with
//!    [`reply`](ClientContext::reply) (reusing the request's opcode and correlation, section 139).
//!    A method that returns an `Option<Fanout>` describes a change to publish to a topic; `None`
//!    means nothing changed and section 156 forbids a frame, so nothing is sent.
//!
//! # Reply-or-fan-out follows the caller, not the return type
//!
//! There is no per-opcode configuration of "does this reply" — but the deciding fact is what the
//! *client* awaits, not what the service returns. The SDK sends an opcode as either an RPC
//! (`rpc.call`, with a request timer) or a notification (`rpc.notify`, fire-and-forget). An RPC
//! opcode must get exactly one `reply` on every success path — including the ones where the
//! service returns `Ok(None)` and there is no fan-out to ride on; a handler that replies only
//! inside `if let Some(fanout)` leaves the no-op path hanging until the client's timer fires.
//! `MessageReceipt` and `Typing` are notifications and correctly never reply. `PresenceSet` and
//! `RoomLeave` are RPCs the SDK awaits, so they always reply exactly once; `RoomLeave` replies
//! *after* its fan-out and revocation so no room frame can be delivered past the acknowledgement
//! (the removal must stop the frames at the ack, not after it), while `PresenceSet` replies first
//! and then publishes. The
//! regression tests in `tests/dispatch_replies.rs` drive both over a real socket, because this
//! silence is invisible from inside the process.
//!
//! # Excluding the sender
//!
//! A domain [`Fanout`](migo_messaging::Fanout) names the device that caused the change; every
//! handler here that publishes one uses [`publish_excluding_self`](ClientContext::publish_excluding_self),
//! which skips the origin connection. The caller already has the outcome from its `reply` (or, for
//! a fire-and-forget mark, from having performed it), and the sender's *other* devices and every
//! other subscriber still receive the event. This is section 156's "exclude the originating device"
//! mapped onto "skip this session".
//!
//! # Anything else
//!
//! An opcode with no handler here is answered `FEATURE_DISABLED`, naming the opcode. That is the
//! honest reply for a build that speaks the transport but has not wired a given feature in — the
//! same posture as [`migo_gateway::NoopDispatcher`], but for the specific opcodes this node does
//! not yet route rather than all of them.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_trait::async_trait;

use migo_auth::Identity;
use migo_bots::SharedBots;
use migo_calls::SharedCallkeeper;
use migo_core::{Error, Id, PublicId, Timestamp};
use migo_economy::SharedTreasurer;
use migo_federation::SharedMesh;
use migo_games::{
    Caller as GameCaller, Event as GameDelta, GameView, Hand, Move, Outcome, SharedReferee,
};
use migo_gateway::{ClientContext, Dispatcher, TopicRequest};
use migo_keys::{Bundle, Caller as KeyCaller, SharedKeyring, SIGNED_PREKEY_LIFETIME_MS};
use migo_media::SharedLibrary;
use migo_messaging::{
    Broadcast as MessageBroadcast, Caller as MessageCaller, Fanout as MessageFanout,
    SharedMessaging,
};
use migo_moderation::SharedWarden;
use migo_notify::{Event as NotificationEvent, SharedNotifier};
use migo_presence::{Caller as PresenceCaller, SharedPresence};
use migo_protocol::{
    fault, from_frame, Acknowledged, BandwidthMode, ConversationCreateRequest,
    ConversationInviteRequest, ConversationKickRequest, ConversationLeaveRequest,
    ConversationListRequest, ConversationMemberEvent, ConversationMuteRequest,
    ConversationRosterRequest, ConversationUpdateRequest, ConversationVoteKickRequest, Encode,
    Frame, GameAction, GameEvent, GroupKeyDistribution, KeyBundle as WireBundle, KeyBundleRequest,
    KeyBundleResponse, KeyPublish, KeyPublishResult, MemberChange, MessageDelete, MessageEdit,
    MessageKind, MessageReceipt, MessageSend, NotificationKind, Opcode, PresenceScope,
    PresenceUpdate, ProfileRequest, ProfileResponse, ReactionSet, RoomJoinRequest,
    RoomLeaveRequest, RoomListRequest, SyncRequest, Topic, TopicKind, TypingEvent, UserProfile,
};
use migo_rooms::{
    Broadcast as RoomBroadcast, Caller as RoomCaller, Fanout as RoomFanout, SharedRooms,
};
use migo_social::{Caller as SocialCaller, Interaction, ProfileCard, SharedSocial};

use crate::conversation_relay::ConversationRelay;
use crate::presence_relay::PresenceRelay;
use crate::replication::ReplicationRelay;
use crate::room_presence::{GatewayHandle, RoomPresence};
use crate::room_relay::RoomRelay;

/// The dispatcher that routes the client-facing application opcodes into the domain services.
///
/// Holds a handle to each domain it speaks for. The handles are `Arc<dyn Trait>`, so the dispatcher
/// is cheap to clone conceptually and is shared as `Arc<dyn Dispatcher>` by the gateway; it adds no
/// state of its own beyond the three services.
// Per-domain dispatch handlers. Each module owns the application opcodes for one domain and
// is written against the domain's own `Shared` handle, keeping `AppDispatcher` free of
// per-feature detail. See each module's header for the exact opcode-to-method map.
pub(crate) mod bots;
pub(crate) mod calls;
pub(crate) mod economy;
pub(crate) mod economy_read;
pub(crate) mod federation;
pub(crate) mod games_admin;
pub(crate) mod media;
pub(crate) mod moderation;
pub(crate) mod notify;
pub(crate) mod profile;
pub(crate) mod rooms_admin;
pub(crate) mod social;

/// A stable key that groups the frames of one Coalescable stream by an id.
///
/// The gateway's out-of-band notification path derives its key the same way, so an
/// in-band notification published here and an out-of-band one published there coalesce
/// into the same stream for a subscriber watching both.
pub(crate) fn coalesce_key_of(id: &Id) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

pub struct AppDispatcher {
    store: migo_store::SharedStore,
    messaging: SharedMessaging,
    presence: SharedPresence,
    rooms: SharedRooms,
    keys: SharedKeyring,
    social: SharedSocial,
    games: SharedReferee,
    media: SharedLibrary,
    economy: SharedTreasurer,
    moderation: SharedWarden,
    notify: SharedNotifier,
    federation: SharedMesh,
    bots: SharedBots,
    calls: SharedCallkeeper,
    /// The per-account session tally behind room online counts and the reconnect grace. Built
    /// by the composition root rather than here, because the mesh transport's ingest path
    /// shares the same tally — a member event that crossed the mesh is the only word this
    /// node gets about who is online on the nodes it came from — and neither the dispatcher
    /// nor the transport may own the other.
    room_presence: Arc<RoomPresence>,
    /// The late-bound gateway, filled by the composition root once the gateway is open. Used to
    /// publish presence and room lifecycle events out of band — with no client request in hand —
    /// on the connection edges the gateway reports through [`Dispatcher::session_started`] and
    /// [`Dispatcher::session_ended`].
    gateway: Arc<GatewayHandle>,
    /// The tiered-fanout relay (section 170): which peer nodes hold subscribers of which
    /// rooms, and the one federated copy per node that a room publish owes them. Built by
    /// the composition root rather than here, because the mesh transport's ingest path
    /// shares the same table — the home node's watchers and the subscriber's asks are two
    /// halves of one bookkeeping, not two components that happen to talk.
    room_relay: Arc<RoomRelay>,
    /// The conversation half of the same tier (section 170): which peer nodes hold
    /// subscribers of which direct or group conversations, and the one federated copy per
    /// node such a conversation's publish owes them. A sibling of the room relay rather
    /// than a field of it, because the two tiers answer different questions of the store
    /// (which room owns a conversation, and which node homes it) and share nothing but
    /// the shape.
    conversation_relay: Arc<ConversationRelay>,
    /// The user-topic tier of the same fanout: which peer nodes hold subscribers of which
    /// users' presence streams, and the one federated copy per node that a presence change
    /// owes them. Built by the composition root for the same reason `room_relay` is — the
    /// mesh transport's ingest path registers watchers into the same table.
    presence_relay: Arc<PresenceRelay>,
    /// The row-replication tier (section 170's account-to-node routing map): the pull a
    /// fail-closed read triggers when the row it missed may live on another node. Held by
    /// the dispatcher for the one read that is a subscription door — a conversation topic
    /// a far member asks for — and by the mesh transport's ingest path for the answers.
    replication: Arc<ReplicationRelay>,
}

impl AppDispatcher {
    /// Wires the dispatcher to every domain whose opcodes it routes.
    ///
    /// One argument per domain is the honest shape: a composition root that bundles the
    /// services into a struct here would hide which dispatcher actually holds which
    /// handle, and the count only grows when a new domain earns an opcode.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: migo_store::SharedStore,
        messaging: SharedMessaging,
        presence: SharedPresence,
        rooms: SharedRooms,
        keys: SharedKeyring,
        social: SharedSocial,
        games: SharedReferee,
        media: SharedLibrary,
        economy: SharedTreasurer,
        moderation: SharedWarden,
        notify: SharedNotifier,
        federation: SharedMesh,
        bots: SharedBots,
        calls: SharedCallkeeper,
        room_presence: Arc<RoomPresence>,
        gateway: Arc<GatewayHandle>,
        room_relay: Arc<RoomRelay>,
        conversation_relay: Arc<ConversationRelay>,
        presence_relay: Arc<PresenceRelay>,
        replication: Arc<ReplicationRelay>,
    ) -> Self {
        Self {
            store,
            messaging,
            presence,
            rooms,
            keys,
            social,
            games,
            media,
            economy,
            moderation,
            notify,
            federation,
            bots,
            calls,
            room_presence,
            gateway,
            room_relay,
            conversation_relay,
            presence_relay,
            replication,
        }
    }

    /// Publishes a presence [`Fanout`](migo_presence::Fanout) to the subject's user topic, out of
    /// band through the late-bound gateway.
    ///
    /// The connection edge that triggers this carries no [`ClientContext`] — there is no request
    /// in flight — so it cannot take the in-band publish path the request handlers use. It goes
    /// through the gateway handle instead, exactly as the mesh publishes an ingested event, and is
    /// a no-op in the startup window before the gateway is bound. Coalescable, keyed by the subject
    /// (section 154): a fresh presence supersedes a stale one still queued for a slow consumer.
    ///
    /// The fanout's `exclude_device` is not honoured — an out-of-band broadcast has no per-device
    /// exclusion — so the connecting device's *other* sessions also hear it. Harmless: it is a
    /// Coalescable state they already agree with, and the device that caused it learns nothing it
    /// did not already know.
    fn publish_presence(&self, fanout: &migo_presence::Fanout, now: Timestamp) {
        let Some(gateway) = self.gateway.get() else {
            return;
        };
        let topic = Topic {
            kind: TopicKind::User,
            id: fanout.subject_id,
        };
        gateway.broadcast_to_topic_coalesced(
            &topic,
            fanout.opcode(),
            &fanout.event,
            coalesce_key_of(&fanout.subject_id),
            now,
        );
        // The federated half, spawned for the same reason the room tier's out-of-band
        // publisher spawns its copy: this publish is sync — it happens on the connection
        // edge, with no request in hand — while the outbox enqueue it now also owes is a
        // durable write. At least once by design, so a moment's delay between the two
        // halves costs ordering headroom the per-link sequence already preserves.
        let relay = Arc::clone(&self.presence_relay);
        let federated = fanout.clone();
        tokio::spawn(async move {
            if let Err(error) = relay.forward(&federated, now).await {
                tracing::warn!(
                    %error,
                    subject = %federated.subject_id.to_text(),
                    "cannot enqueue the federated half of a presence change"
                );
            }
        });
    }

    /// The read-only gate of section 173's scenario 2, for the handlers that
    /// mutate an existing room from the admin module.
    ///
    /// A room whose home node is partitioned away is read-only on this node:
    /// the write would need a second sequencer that must not exist (section
    /// 170). The inline handlers call the relay directly; this thin wrapper
    /// exists so the admin handlers do not reach into the relay field
    /// themselves, keeping the one-call-per-opcode shape the module promises.
    pub(crate) async fn ensure_room_writable(&self, room_id: Id) -> Result<(), Error> {
        self.room_relay.ensure_writable(room_id).await
    }

    /// Publishes a rooms [`Fanout`](RoomFanout) and then, when it removed a
    /// member, takes the room away from the removed account's live sockets.
    ///
    /// Publish first, revoke second: the removed member's last frame from the
    /// room must be the one that tells them they were removed, and every frame
    /// after it is the room's business without them. Both of the room's topics
    /// go — the `Room` topic that carries roster and state events, and the
    /// `Conversation` topic its messages fan out on — because a member who
    /// lost the first but kept the second would still hear every word said in
    /// a room they can no longer be in. Getting the topic back requires a
    /// `SUBSCRIBE`, and authorisation asks the membership question again.
    async fn publish_rooms(
        &self,
        context: &ClientContext<'_>,
        fanout: RoomFanout,
    ) -> Result<(), Error> {
        // Kicked and Banned are removals done to the member, Left one they did
        // themselves; every other change keeps the member and their topics.
        let removed = match &fanout.event {
            RoomBroadcast::Member(event)
                if matches!(
                    event.change,
                    Some(MemberChange::Kicked | MemberChange::Banned | MemberChange::Left)
                ) =>
            {
                Some(event.user_id)
            }
            _ => None,
        };
        let room_id = fanout.room_id;
        // The federated half is captured before the local publish takes the fanout by
        // value: the home node owes each watching node one copy of the same event
        // (section 170), and a failure to enqueue it is logged rather than failed —
        // the local delivery already happened, and retrying the request would
        // publish the event twice.
        let federated = fanout.clone();
        let now = context.now();
        publish_room_fanout(context, fanout)?;
        if let Err(error) = self.room_relay.forward(&federated, now).await {
            tracing::warn!(
                %error,
                room = %room_id.to_text(),
                "cannot enqueue the federated half of a room fanout"
            );
        }
        if let Some(account_id) = removed {
            // The room's conversation loses the member with the room, and the
            // typing mark they may hold there ends the same moment. Order is the
            // point: the Stop publishes before the revocation takes the topics
            // away, so the members who remain hear it on a topic they still
            // hold, and the removed member's last frames are the departure and
            // the end of their own indicator.
            self.stop_room_typing(room_id, account_id, now).await;
            self.revoke_room_audience(room_id, account_id).await;
        }
        Ok(())
    }

    /// Ends a removed room member's typing mark on the room's conversation and
    /// publishes the `Stop`, out of band.
    ///
    /// The rooms crate cannot do this itself: it holds no cache by design
    /// (nothing in a room is cached), and the typing mark lives in the
    /// messaging cache keyed by the conversation the room's chat runs on. The
    /// dispatcher is the one place both facts meet — the same seam that revokes
    /// the topics a line below its caller. No frame when the member was not
    /// typing, which is section 156: a removal that ends no indicator owes no
    /// frame about one.
    async fn stop_room_typing(&self, room_id: Id, account_id: Id, now: Timestamp) {
        let Ok(Some(room)) = self.store.room(room_id).await else {
            return;
        };
        match self
            .messaging
            .stop_typing(room.conversation_id, account_id, now)
            .await
        {
            Ok(Some(fanout)) => {
                let MessageBroadcast::Typing(event) = fanout.event else {
                    return;
                };
                let Some(typer) = event.user_id else {
                    return;
                };
                if let Some(gateway) = self.gateway.get() {
                    // The same topic, opcode, and coalescing key the member's
                    // own `Start` was published under, so the pair collapses to
                    // the latest for any subscriber whose queue is backed up.
                    gateway.broadcast_to_topic_coalesced(
                        &Topic {
                            kind: TopicKind::Conversation,
                            id: fanout.conversation_id,
                        },
                        Opcode::Typing,
                        &event,
                        stream_key(&(fanout.conversation_id, typer)),
                        now,
                    );
                }
            }
            // Not typing: nothing owed, nothing sent.
            Ok(None) => {}
            Err(error) => tracing::warn!(
                %error,
                "cannot stop a removed room member's typing mark; the sweeper is the backstop"
            ),
        }
    }

    /// Publishes a messaging [`Fanout`](MessageFanout) and then, when it
    /// removed a member from a group conversation, takes the conversation away
    /// from that account's live sockets.
    ///
    /// The room path revokes two topics; this one has only the conversation
    /// topic to lose. A `Left` reaches the leaver's own other devices too —
    /// the event is published to everyone but the acting socket — so a parked
    /// tab that never unsubscribes is caught the same way a kicked member is.
    async fn publish_messaging(
        &self,
        context: &ClientContext<'_>,
        user: Id,
        fanout: MessageFanout,
    ) -> Result<(), Error> {
        let removed = match &fanout.event {
            MessageBroadcast::Member(
                event @ ConversationMemberEvent {
                    change: MemberChange::Kicked | MemberChange::Left,
                    ..
                },
            ) => Some(event.user_id),
            _ => None,
        };
        let conversation_id = fanout.conversation_id;
        // The federated half is captured before the local publish takes the fanout by
        // value. A room's chat is its conversation, and every node keeps its own store:
        // a message that stopped at this hub is not late on the other nodes, it is
        // absent, because there is no row there for a member to sync (section 170).
        let federated = fanout.clone();
        publish_message_fanout(context, user, fanout)?;
        match self.store.room_by_conversation(conversation_id).await {
            Ok(Some(room)) => {
                if let Err(error) = self
                    .room_relay
                    .forward_message(&room, &federated, context.now())
                    .await
                {
                    tracing::warn!(
                        %error,
                        room = %room.room_id.to_text(),
                        "cannot enqueue the federated half of a room message"
                    );
                }
            }
            // Not a room's conversation: a direct or group chat, whose home node the
            // conversation row itself names (section 170's conversation tier). The
            // publish that just reached this node's hub is owed to the far members
            // through the same tiered fanout a room's chat rides — one copy to the
            // home node from here, or one per watching node from there — and the
            // sealed envelope crosses every node on the way unread.
            Ok(None) => {
                if let Err(error) = self
                    .conversation_relay
                    .forward(&federated, context.now())
                    .await
                {
                    tracing::warn!(
                        %error,
                        conversation = %conversation_id.to_text(),
                        "cannot enqueue the federated half of a conversation fanout"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(%error, "cannot tell whether a conversation belongs to a room");
            }
        }
        if let Some(account_id) = removed {
            // The same re-join race the room path guards against: a member who
            // left a group and was re-invited before this fanout reached their
            // other devices holds a subscription the fresh invite granted, and
            // the removal that triggered this is no longer the last word on
            // their standing. One membership read at the last moment decides;
            // a read that fails revokes anyway, because the removal already
            // happened and a store fault is not evidence of a re-join.
            let still_removed = !self
                .store
                .is_member(conversation_id, account_id)
                .await
                .unwrap_or(false);
            if still_removed {
                if let Some(gateway) = self.gateway.get() {
                    gateway.revoke_subscriptions(
                        account_id,
                        &[Topic {
                            kind: TopicKind::Conversation,
                            id: conversation_id,
                        }],
                    );
                }
            }
        }
        Ok(())
    }

    /// Takes a room's topics away from one account's live sessions.
    ///
    /// The conversation id is read from the room row rather than carried on
    /// the wire event, because the fanout names the room and the row is the
    /// one place that knows which conversation speaks for it. A missing row —
    /// archived mid-flight, or a store fault — still revokes the room topic;
    /// the message topic is the bonus, not the floor.
    ///
    /// The membership is re-read here, at the last moment before the
    /// revocation, because the removal that called this can be overtaken by a
    /// re-join: a member who leaves and comes back — on another socket, or on
    /// another node whose copy of the departure arrives late — holds a
    /// subscription the fresh join granted, and revoking it would cut an
    /// active member off from a room they are entitled to. Every room
    /// membership write (join, leave, kick, vote, ban) mirrors the account's
    /// standing into the room conversation's member rows, so one `is_member`
    /// read answers for both topics at once. A read that fails revokes
    /// anyway: the removal already happened, and a store fault is not
    /// evidence of a re-join.
    async fn revoke_room_audience(&self, room_id: Id, account_id: Id) {
        let Some(gateway) = self.gateway.get() else {
            return;
        };
        let mut topics = vec![Topic {
            kind: TopicKind::Room,
            id: room_id,
        }];
        if let Ok(Some(room)) = self.store.room(room_id).await {
            if self
                .store
                .is_member(room.conversation_id, account_id)
                .await
                .unwrap_or(false)
            {
                return;
            }
            topics.push(Topic {
                kind: TopicKind::Conversation,
                id: room.conversation_id,
            });
        }
        gateway.revoke_subscriptions(account_id, &topics);
    }
}

#[async_trait]
impl Dispatcher for AppDispatcher {
    async fn dispatch(&self, context: &ClientContext<'_>, frame: &Frame) -> Result<(), Error> {
        let identity = context.identity();
        let now = context.now();

        match context.opcode() {
            // Section 146 invariant: the reserved span 247-255 is never-allocated, and
            // the gateway refuses it before a frame can reach this dispatcher (see the
            // range gate in migo-gateway's connection.rs). A variant generated into that
            // span therefore must not be routable here — the migo-protocol registry test
            // (`no_opcode_lives_in_the_never_allocated_span_of_the_reserved_range`) fails
            // the build first, and the allocation needs a written decision per section
            // 145's precedent before any number is taken from the reserved head. The
            // conversation-federation pair at 241-242 and the row-replication tier at
            // 243-246 are two such decisions; the span they left never-allocated begins
            // at 247.
            // --- messaging ---
            Opcode::MessageSend => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: MessageSend = from_frame(frame).map_err(fault::from_wire)?;
                // Section 173's scenario 2: a room whose home node is partitioned away is
                // read-only on this node, because the write would need a second sequencer
                // that must not exist. A direct or group conversation passes: it has no
                // home node to be partitioned from.
                self.room_relay
                    .ensure_conversation_writable(request.conversation_id)
                    .await?;
                let (accepted, fanout) = self.messaging.send(&caller, request).await?;
                context.reply(&accepted)?;
                if let Some(fanout) = fanout {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                Ok(())
            }
            Opcode::MessageEdit => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: MessageEdit = from_frame(frame).map_err(fault::from_wire)?;
                // The same read-only gate as the send: an edit reorders nothing, but it
                // still writes the room's conversation, whose order belongs to the home
                // node's sequencer.
                self.room_relay
                    .ensure_conversation_writable(request.conversation_id)
                    .await?;
                // The envelope is ciphertext the client sealed; the server never sees the
                // text. What the service enforces is ownership and membership, and the
                // edit lands under the message's original seq.
                let (accepted, fanout) = self
                    .messaging
                    .edit(
                        &caller,
                        request.conversation_id,
                        request.message_id,
                        request.envelope,
                    )
                    .await?;
                context.reply(&accepted)?;
                if let Some(fanout) = fanout {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                Ok(())
            }
            Opcode::ReactionSet => {
                // A reaction is a message: kind Reaction, sealed like any other content,
                // sent through the ordinary path. The handler's translation is exactly
                // the composition a client would otherwise do itself.
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ReactionSet = from_frame(frame).map_err(fault::from_wire)?;
                // A reaction rides the send path, so it rides the send gate too.
                self.room_relay
                    .ensure_conversation_writable(request.conversation_id)
                    .await?;
                // The message id is derived, not minted. A client that retries a
                // REACTION_SET after a timeout re-sends byte-identical fields, and a
                // fresh random id would store the reaction twice — the send path's
                // idempotency is client-chosen ids, so the translation has to choose
                // one deterministically. Hashing the account and the whole request
                // gives that: a retry maps to the same id and converges on the
                // stored row, while a genuinely different reaction (another emoji,
                // or the same one on a different message) differs in the hashed
                // bytes and gets its own id. Every id bit is derived — a timestamp
                // prefix would defeat the point, since a retry is sampled at a new
                // `now`. Nothing reads a message id's embedded time; ids are
                // identity here, not clock. The device is deliberately absent
                // from the hash: a reaction belongs to the account, not the
                // device it was tapped on, so the same reaction re-sent from a
                // second device of one account is the same reaction — one row,
                // one fan-out — exactly as a retry from the first device is.
                let mut hasher = DefaultHasher::new();
                identity.account_id().hash(&mut hasher);
                request.conversation_id.hash(&mut hasher);
                request.target_message_id.hash(&mut hasher);
                request.envelope.hash(&mut hasher);
                let first = hasher.finish();
                // A second round over the first: 16 derived bytes from a 64-bit
                // core, so the full id is set without trusting one hash's width.
                let mut second = DefaultHasher::new();
                first.hash(&mut second);
                let mut derived = [0u8; 16];
                derived[..8].copy_from_slice(&first.to_le_bytes());
                derived[8..].copy_from_slice(&second.finish().to_le_bytes());
                let message_id = Id::from_bytes(derived);
                let send = MessageSend {
                    message_id,
                    conversation_id: request.conversation_id,
                    kind: MessageKind::Text, // reactions ride a Text envelope; the Reaction discriminator is inside the ciphertext (SDK kindForContent)
                    envelope: request.envelope,
                    sender_key_id: None,
                    reply_to: Some(request.target_message_id),
                    expires_in_ms: None,
                };
                let (_accepted, fanout) = self.messaging.send(&caller, send).await?;
                context.reply(&Acknowledged { ok: true })?;
                if let Some(fanout) = fanout {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                Ok(())
            }
            Opcode::MessageReceipt => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: MessageReceipt = from_frame(frame).map_err(fault::from_wire)?;
                if let Some(fanout) = self.messaging.receipt(&caller, request).await? {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                Ok(())
            }
            Opcode::MessageDelete => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: MessageDelete = from_frame(frame).map_err(fault::from_wire)?;
                // And the delete, for the same reason as the edit: it mutates the
                // conversation a partitioned home node owns the order of.
                self.room_relay
                    .ensure_conversation_writable(request.conversation_id)
                    .await?;
                let (accepted, fanout) = self.messaging.delete(&caller, request).await?;
                context.reply(&accepted)?;
                if let Some(fanout) = fanout {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                Ok(())
            }
            Opcode::Sync => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: SyncRequest = from_frame(frame).map_err(fault::from_wire)?;
                let response = self.messaging.sync(&caller, request).await?;
                context.reply(&response)
            }
            Opcode::ConversationList => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ConversationListRequest =
                    from_frame(frame).map_err(fault::from_wire)?;
                let response = self.messaging.conversations(&caller, request).await?;
                context.reply(&response)
            }
            Opcode::ConversationCreate => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ConversationCreateRequest =
                    from_frame(frame).map_err(fault::from_wire)?;
                // The reply carries the summary the creator's list needs; the fanouts carry
                // one arrival each for everyone the create seated — on the user topic the
                // dispatcher adds, since a brand-new member cannot be a subscriber of a
                // conversation they have never heard of.
                let (summary, fanouts) = self.messaging.create(&caller, request).await?;
                context.reply(&summary)?;
                for fanout in fanouts {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                Ok(())
            }
            Opcode::Typing => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: TypingEvent = from_frame(frame).map_err(fault::from_wire)?;
                if let Some(fanout) = self.messaging.typing(&caller, request).await? {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                Ok(())
            }
            Opcode::ConversationInvite => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ConversationInviteRequest =
                    from_frame(frame).map_err(fault::from_wire)?;
                // The reply carries the summary the inviter's list needs; the fanouts carry
                // one arrival each for everyone else, because a client rotates sender keys
                // per person, not per batch.
                let (summary, fanouts) = self.messaging.invite(&caller, request).await?;
                context.reply(&summary)?;
                // One GroupInvite notification per account actually seated. The invite
                // path is group-only (the service refuses a direct conversation, whose
                // two members arrive through the create call, and a room, whose
                // membership belongs to the room service), and the freshly seated are
                // exactly the Member-Joined fanouts it returns — an already-seated name
                // the service dropped silently gets no bell for a seat it already holds.
                // The row follows the FriendRequest notice shape: the actor is the
                // inviter, the sentence is the client's to write, and the conversation
                // id is the tap target.
                let invited: Vec<Id> = fanouts
                    .iter()
                    .filter_map(|fanout| match &fanout.event {
                        MessageBroadcast::Member(ConversationMemberEvent {
                            change: MemberChange::Joined,
                            user_id,
                            ..
                        }) => Some(*user_id),
                        _ => None,
                    })
                    .collect();
                for fanout in fanouts {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                for member in invited {
                    let notification = NotificationEvent {
                        account_id: member,
                        kind: NotificationKind::GroupInvite,
                        actor_id: Some(caller.account_id),
                        room_id: None,
                        subject_id: None,
                        conversation_id: Some(summary.conversation_id),
                        at: now,
                    };
                    // Best-effort, like every other bell this process rings: the seat
                    // is already written and the Joined fanout already published, and
                    // failing the invite over a row in an inbox would un-invite
                    // somebody the group already holds.
                    if let Err(error) = self.notify.notify(notification).await {
                        tracing::warn!(code = error.code(), "group invite notification dropped");
                    }
                }
                Ok(())
            }
            Opcode::ConversationLeave => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ConversationLeaveRequest =
                    from_frame(frame).map_err(fault::from_wire)?;
                let fanouts = self.messaging.leave(&caller, request).await?;
                // Publish (and revoke the leaver's sockets) before the reply, for the same
                // reason `RoomLeave` does: the acknowledgement must be the last frame the
                // leaver can attribute to the conversation, and a reply sent first opens a
                // window where a raced message is still delivered after "you are out".
                for fanout in fanouts {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                context.reply(&Acknowledged { ok: true })?;
                Ok(())
            }
            Opcode::ConversationRoster => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ConversationRosterRequest =
                    from_frame(frame).map_err(fault::from_wire)?;
                let response = self.messaging.roster(&caller, request).await?;
                context.reply(&response)
            }
            Opcode::ConversationMute => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ConversationMuteRequest =
                    from_frame(frame).map_err(fault::from_wire)?;
                // No fanout: a mute is between a founder and one member, and the member
                // learns of it from the send that refuses them.
                self.messaging.mute(&caller, request).await?;
                context.reply(&Acknowledged { ok: true })
            }
            Opcode::ConversationKick => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ConversationKickRequest =
                    from_frame(frame).map_err(fault::from_wire)?;
                let fanouts = self.messaging.kick(&caller, request).await?;
                context.reply(&Acknowledged { ok: true })?;
                for fanout in fanouts {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                Ok(())
            }
            Opcode::ConversationVoteKick => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ConversationVoteKickRequest =
                    from_frame(frame).map_err(fault::from_wire)?;
                // The reply is this voice's tally; the fanout is the same tally for
                // everyone else, and — when the vote carried — the removal itself.
                let (response, fanouts) = self.messaging.vote_kick(&caller, request).await?;
                context.reply(&response)?;
                for fanout in fanouts {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                Ok(())
            }
            Opcode::GroupKeyDistribute => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: GroupKeyDistribution = from_frame(frame).map_err(fault::from_wire)?;
                // The service's half is authorization only — the distributor is
                // a member, the target still is one, the sealed blob is within
                // the envelope bound — and the relay is the other half: the
                // same frame, still sealed, published to the target member's
                // own user topic, which reaches every device that account has
                // connected. One frame per target device is the client's job to
                // send; a member with two devices is owed two differently-sealed
                // envelopes, and the server has no way to make one out of the
                // other (section 163).
                self.messaging
                    .distribute_key(&caller, request.clone())
                    .await?;
                context.reply(&Acknowledged { ok: true })?;
                let topic = Topic {
                    kind: TopicKind::User,
                    id: request.to_account,
                };
                if let Err(error) = context.publish_excluding_self(
                    &topic,
                    Opcode::GroupKeyDistribute,
                    &request,
                    None,
                ) {
                    // The acknowledgement above already promised the sender the
                    // frame was accepted, and a delivery hiccup is not grounds
                    // to unring it: the client retries a distribution the
                    // target reports missing, which is cheaper than lying about
                    // either half.
                    tracing::warn!(
                        %error,
                        target = %request.to_account.to_text(),
                        "cannot publish a sealed key distribution to its member"
                    );
                }
                // The federated half of the same distribution: the target
                // member's devices may hold their sessions on another node,
                // and the sealed envelope reaches their user topic there the
                // same way it reached this node's hub — through the user-topic
                // tier (section 170), sealed exactly as the local publish
                // sealed it, because the sender already holds an
                // acknowledgement that promises the distribution was taken. A
                // failure to enqueue is logged rather than failed, for the
                // same reason the local publish above is.
                if let Err(error) = self
                    .presence_relay
                    .forward_frame(
                        request.to_account,
                        Opcode::GroupKeyDistribute,
                        &request,
                        now,
                    )
                    .await
                {
                    tracing::warn!(
                        %error,
                        target = %request.to_account.to_text(),
                        "cannot enqueue the federated half of a sealed key distribution"
                    );
                }
                Ok(())
            }
            Opcode::ConversationUpdate => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ConversationUpdateRequest =
                    from_frame(frame).map_err(fault::from_wire)?;
                let (summary, fanout) = self.messaging.update(&caller, request).await?;
                context.reply(&summary)?;
                if let Some(fanout) = fanout {
                    self.publish_messaging(context, caller.account_id, fanout)
                        .await?;
                }
                Ok(())
            }

            // --- presence ---
            Opcode::PresenceSet => {
                // The mode the session negotiated in its HELLO, at last reaching the
                // crate it was meant for: presence stores it per device, because a
                // LowData session's punctual heartbeats must not expire against a
                // Normal cadence it never runs (section 75).
                let caller = PresenceCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    context.bandwidth_mode(),
                    now,
                );
                let request: PresenceUpdate = from_frame(frame).map_err(fault::from_wire)?;
                let fanout = self.presence.set(&caller, request).await?;
                // The caller awaits this acknowledgement — an SDK `presence.set` is an RPC, not a
                // fire-and-forget publish — so it must land even when no fanout follows (an
                // unchanged state publishes nothing but still succeeded).
                context.reply(&Acknowledged { ok: true })?;
                if let Some(fanout) = fanout {
                    let topic = Topic {
                        kind: TopicKind::User,
                        id: fanout.subject_id,
                    };
                    // Presence is Coalescable, keyed by the subject (section 154): a fresh state
                    // supersedes a stale one still queued for a slow consumer.
                    context.publish_excluding_self(
                        &topic,
                        fanout.opcode(),
                        &fanout.event,
                        Some(stream_key(&fanout.subject_id)),
                    )?;
                    // The federated half of the same change: the peers whose sessions watch
                    // this subject's user topic are owed one copy per node. The origin of a
                    // presence change is always the node whose session caused it, so this
                    // node is the fan-out authority and there is no home-node detour to
                    // consult. A failure to enqueue is logged rather than failed — the local
                    // publish already happened, and the ack above already promised success.
                    if let Err(error) = self.presence_relay.forward(&fanout, now).await {
                        tracing::warn!(
                            %error,
                            subject = %fanout.subject_id.to_text(),
                            "cannot enqueue the federated half of a presence change"
                        );
                    }
                }
                Ok(())
            }

            // --- rooms ---
            Opcode::RoomJoin => {
                let caller = RoomCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: RoomJoinRequest = from_frame(frame).map_err(fault::from_wire)?;
                // Membership is room state, and room state is ordered by the home node's
                // sequencer: joining a room whose home node is partitioned away would
                // fork that order, so the join is refused while the partition stands.
                self.room_relay.ensure_writable(request.room_id).await?;
                let (mut response, fanout) = self.rooms.join(&caller, request).await?;
                // The rooms crate leaves `online_count` at `view::ONLINE_COUNT_UNSET`; fill it from
                // the in-memory session tally — the joiner themselves is now online and a member, so
                // the count they are handed already includes them.
                response.room.online_count =
                    self.room_presence.online_count(response.room.room_id).await;
                context.reply(&response)?;
                if let Some(fanout) = fanout {
                    self.publish_rooms(context, fanout).await?;
                }
                Ok(())
            }
            Opcode::RoomLeave => {
                let caller = RoomCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: RoomLeaveRequest = from_frame(frame).map_err(fault::from_wire)?;
                let room_id = request.room_id;
                // Leaving is room state too: the member list the home node sequences is
                // the one every node must agree on, partition or not.
                self.room_relay.ensure_writable(room_id).await?;
                let fanout = self.rooms.leave(&caller, request).await?;
                // Publish and revoke before the reply, and reply unconditionally. The service
                // returns Ok(None) for an idempotent no-op leave (not a member, or already gone),
                // which is a success the caller still awaits an acknowledgement for — gating the
                // reply on the fanout left those paths hanging until the client's request timer
                // fired. But when a real leave did happen, the acknowledgement must not overtake
                // the teardown: a reply sent first lets a message that raced the leave be enqueued
                // and delivered after the client saw "you are out" — the frames stop at, not after,
                // the acknowledgement. A no-op leave still revokes defensively: a session that
                // somehow holds the topics against an already-emptied seat loses them here.
                if let Some(fanout) = fanout {
                    self.publish_rooms(context, fanout).await?;
                } else {
                    self.revoke_room_audience(room_id, caller.account_id).await;
                }
                context.reply(&Acknowledged { ok: true })?;
                Ok(())
            }
            Opcode::RoomList => {
                let caller = RoomCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: RoomListRequest = from_frame(frame).map_err(fault::from_wire)?;
                let mut response = self.rooms.list(&caller, request).await?;
                // Fill each summary's online count from the tally, as the join path does. Section
                // 14's tension is real here — one tally read per listed room — but each is an
                // in-memory roster intersection with no presence query, and a listing is bounded by
                // `MAX_LIST_LIMIT`.
                for summary in &mut response.rooms {
                    summary.online_count = self.room_presence.online_count(summary.room_id).await;
                }
                context.reply(&response)
            }
            Opcode::RoomCreate => {
                rooms_admin::handle_room_create(context, frame, &self.rooms).await
            }
            Opcode::RoomRoster => rooms_admin::handle_roster(context, frame, &self.rooms).await,
            Opcode::RoomRoleSet => {
                rooms_admin::handle_role_set(context, frame, &self.rooms, self).await
            }
            Opcode::RoomUpdate => {
                rooms_admin::handle_room_update(context, frame, &self.rooms, self).await
            }
            Opcode::RoomArchive => {
                rooms_admin::handle_room_archive(context, frame, &self.rooms, self).await
            }
            Opcode::RoomSanction => {
                rooms_admin::handle_sanction(context, frame, &self.rooms, self).await
            }
            Opcode::RoomVoteKick => {
                rooms_admin::handle_vote_kick(context, frame, &self.rooms, self).await
            }

            // --- key material ---
            Opcode::KeyPublish => {
                let caller = KeyCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: KeyPublish = from_frame(frame).map_err(fault::from_wire)?;
                // The expiry section 163 requires is not on the wire: the IDL and its golden
                // vectors are frozen and neither `KeyPublish` nor `KeyBundle` carries the field.
                // The server supplies it, which is the safer half of the disagreement — a client
                // that chose its own expiry could choose one ten years out.
                let outcome = self
                    .keys
                    .publish(
                        &caller,
                        migo_keys::PublishRequest {
                            identity_key: request.identity_key,
                            signed_prekey_id: request.signed_prekey_id,
                            signed_prekey: request.signed_prekey,
                            signed_prekey_signature: request.signed_prekey_signature,
                            signed_prekey_expires_at: now
                                .saturating_add_millis(SIGNED_PREKEY_LIFETIME_MS),
                            one_time_prekeys: request
                                .one_time_prekeys
                                .into_iter()
                                .map(|entry| (entry.key_id, entry.public_key))
                                .collect(),
                        },
                    )
                    .await?;
                // `one_time_prekeys_remaining` has no field on `KeyPublishResult` and is dropped
                // here rather than smuggled into another one. A client learns the count from its
                // own bookkeeping — it knows what it just published — and the server's number
                // only diverges from that after fetches it will see the effect of anyway.
                context.reply(&KeyPublishResult {
                    accepted_prekeys: outcome.accepted_prekeys,
                    identity_fingerprint: outcome.identity_fingerprint,
                })
            }
            Opcode::KeyBundleFetch => {
                let caller = KeyCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: KeyBundleRequest = from_frame(frame).map_err(fault::from_wire)?;
                let fetched = self
                    .keys
                    .bundles(&caller, request.user_id, request.device_id)
                    .await?;
                context.reply(&KeyBundleResponse {
                    bundles: fetched.bundles.into_iter().map(wire_bundle).collect(),
                })
            }

            // --- social ---
            Opcode::ProfileFetch => {
                let caller = SocialCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: ProfileRequest = from_frame(frame).map_err(fault::from_wire)?;
                let cards = self.social.profiles(&caller, &request.user_ids).await?;
                // Possibly shorter than what was asked for, and deliberately unordered: a
                // profile the caller may not see is omitted rather than reported, so that
                // "blocked you", "deleted their account", and "never existed" are one
                // observation (section 180). A client matches on `user_id`.
                context.reply(&ProfileResponse {
                    profiles: cards.into_iter().map(wire_profile).collect(),
                })
            }

            // --- profile ---
            Opcode::ProfileUpdate => {
                profile::handle_profile_update(context, frame, &self.social, &self.store).await
            }
            Opcode::Suggestions => profile::handle_suggestions(context, frame, &self.social).await,
            Opcode::Search => profile::handle_search(context, frame, &self.social).await,

            // --- games ---
            Opcode::GameAction => {
                let caller = GameCaller {
                    account_id: identity.account_id(),
                    device_id: identity.device_id(),
                    tier: identity.tier,
                    now,
                    request_id: None,
                };
                let request: GameAction = from_frame(frame).map_err(fault::from_wire)?;
                // `room_id` and `action_id` arrive and are not trusted. The conversation a game
                // belongs to comes from the game itself, so a client cannot fan its move out
                // onto a topic the game is not in; replays are beaten by the store's
                // compare-and-set, which sees a board that already reflects the move and rejects
                // it, so a client-supplied counter would be a second, weaker defence that a
                // client controls.
                let mv = domain_move(&request)?;
                let result = self.games.play(&caller, request.game_id, mv).await?;
                context.reply(&Acknowledged { ok: true })?;
                publish_game(context, &result.view, &result.events, true)
            }
            Opcode::GameStart => games_admin::handle_game_start(context, frame, &self.games).await,
            Opcode::GameView => games_admin::handle_game_view(context, frame, &self.games).await,
            Opcode::GameAbandon => {
                games_admin::handle_game_abandon(context, frame, &self.games).await
            }
            Opcode::GameCatalogue => {
                games_admin::handle_game_catalogue(context, frame, &self.games).await
            }

            // --- media ---
            Opcode::MediaUploadBegin => {
                media::handle_upload_begin(context, frame, &self.media).await
            }
            Opcode::MediaUploadStatus => {
                media::handle_upload_status(context, frame, &self.media).await
            }
            Opcode::MediaUploadCommit => {
                media::handle_upload_commit(context, frame, &self.media).await
            }
            Opcode::MediaUploadAbort => {
                media::handle_upload_abort(context, frame, &self.media).await
            }
            Opcode::MediaFetchUrl => media::handle_fetch_url(context, frame, &self.media).await,

            // --- social ---
            Opcode::FriendRequest => {
                social::handle_friend_request(
                    context,
                    frame,
                    &self.social,
                    &self.presence_relay,
                    &self.notify,
                )
                .await
            }
            Opcode::FriendRespond => {
                social::handle_friend_respond(
                    context,
                    frame,
                    &self.social,
                    &self.presence_relay,
                    &self.notify,
                )
                .await
            }
            Opcode::FriendRemove => {
                social::handle_friend_remove(context, frame, &self.social, &self.presence_relay)
                    .await
            }
            Opcode::BlockSet => {
                social::handle_block_set(context, frame, &self.social, &self.presence_relay).await
            }
            Opcode::BlockClear => {
                social::handle_block_clear(context, frame, &self.social, &self.presence_relay).await
            }
            Opcode::MuteSet => {
                social::handle_mute_set(context, frame, &self.social, &self.presence_relay).await
            }
            Opcode::RelationshipList => {
                social::handle_relationship_list(context, frame, &self.social).await
            }

            // --- notify ---
            Opcode::NotificationAck => notify::handle_ack(context, frame, &self.notify).await,
            Opcode::NotificationList => notify::handle_list(context, frame, &self.notify).await,
            Opcode::PushRegister => {
                notify::handle_register(context, frame, &self.store, &self.notify).await
            }
            Opcode::PushUnregister => notify::handle_unregister(context, frame, &self.notify).await,

            // --- economy ---
            Opcode::GiftSend => economy::handle_gift_send(context, frame, &self.economy).await,
            Opcode::BalanceFetch => {
                economy::handle_balance_fetch(context, frame, &self.economy).await
            }
            Opcode::GiftCatalogue => {
                economy_read::handle_gift_catalogue(context, frame, &self.economy).await
            }
            Opcode::LedgerHistory => {
                economy_read::handle_ledger_history(context, frame, &self.economy).await
            }
            Opcode::Progression => {
                economy_read::handle_progression(context, frame, &self.economy).await
            }
            Opcode::Badges => economy_read::handle_badges(context, frame, &self.economy).await,
            Opcode::Leaderboard => {
                economy_read::handle_leaderboard(context, frame, &self.economy).await
            }
            Opcode::StorePurchase => {
                economy::handle_store_purchase(context, frame, &self.economy).await
            }
            Opcode::Entitlements => {
                economy::handle_entitlements(context, frame, &self.economy).await
            }
            Opcode::KickPointsBuy => {
                economy::handle_kick_points_buy(context, frame, &self.economy).await
            }

            // --- bots ---
            Opcode::BotCommand => bots::handle_command(context, frame, &self.bots).await,
            Opcode::BotRegister => bots::handle_register(context, frame, &self.bots).await,

            // --- moderation ---
            Opcode::ReportCreate => {
                moderation::handle_report(context, frame, &self.moderation).await
            }
            Opcode::ModerationAction => {
                moderation::handle_action(context, frame, &self.moderation).await
            }

            // --- federation (server-to-server mesh) ---
            Opcode::FedHello => federation::handle_hello(context, frame, &self.federation).await,
            Opcode::FedAuth => federation::handle_auth(context, frame, &self.federation).await,
            Opcode::FedPing => federation::handle_ping(context, frame, &self.federation).await,
            Opcode::FedForward => {
                federation::handle_forward(context, frame, &self.federation).await
            }
            Opcode::FedAck => federation::handle_ack(context, frame, &self.federation).await,
            Opcode::FedRoomSubscribe => {
                federation::handle_room_subscribe(context, frame, &self.federation).await
            }
            Opcode::FedRoomEvent => {
                federation::handle_room_event(context, frame, &self.federation).await
            }
            Opcode::FedPresenceDigest => {
                federation::handle_presence_digest(context, frame, &self.federation).await
            }
            Opcode::FedKeyRotate => {
                federation::handle_key_rotate(context, frame, &self.federation).await
            }
            Opcode::FedHealth => federation::handle_health(context, frame, &self.federation).await,
            Opcode::FedShardMap => {
                federation::handle_shard_map(context, frame, &self.federation).await
            }
            Opcode::FedError => federation::handle_error(context, frame, &self.federation).await,
            Opcode::FedCallRelay => {
                federation::handle_call_relay(context, frame, &self.federation).await
            }
            Opcode::FedDirectory => {
                federation::handle_directory(context, frame, &self.federation).await
            }
            Opcode::FedUserSubscribe => {
                federation::handle_user_subscribe(context, frame, &self.federation).await
            }
            Opcode::FedUserEvent => {
                federation::handle_user_event(context, frame, &self.federation).await
            }

            // --- calls ---
            // Each handler replies to the sender and publishes the returned
            // event to the other party's user topic; the service owns every
            // rule and never sends a frame itself.
            Opcode::CallInvite => {
                calls::handle_invite(
                    context,
                    frame,
                    &self.calls,
                    &self.notify,
                    &self.presence_relay,
                )
                .await
            }
            Opcode::CallAnswer => {
                calls::handle_answer(context, frame, &self.calls, &self.presence_relay).await
            }
            Opcode::CallDecline => {
                calls::handle_decline(context, frame, &self.calls, &self.presence_relay).await
            }
            Opcode::CallCancel => {
                calls::handle_cancel(context, frame, &self.calls, &self.presence_relay).await
            }
            Opcode::CallEnd => {
                // The id may name a 1:1 call or a group call; the group
                // handler answers the frame when it does, and the 1:1 handler
                // takes it otherwise. NOT_FOUND from the group service is the
                // handoff, not an error the caller sees.
                match calls::handle_group_end(context, frame, &self.calls).await {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        calls::handle_end(context, frame, &self.calls, &self.presence_relay).await
                    }
                    Err(error) if error.code() == migo_protocol::codes::NOT_FOUND => {
                        calls::handle_end(context, frame, &self.calls, &self.presence_relay).await
                    }
                    Err(error) => Err(error),
                }
            }
            Opcode::CallSdp => {
                calls::handle_sdp(context, frame, &self.calls, &self.presence_relay).await
            }
            Opcode::CallIce => {
                calls::handle_ice(context, frame, &self.calls, &self.presence_relay).await
            }
            Opcode::CallRenegotiate => {
                calls::handle_renegotiate(context, frame, &self.calls, &self.presence_relay).await
            }
            Opcode::CallKeyUpdate => {
                calls::handle_key_update(context, frame, &self.calls, &self.presence_relay).await
            }
            Opcode::CallStats => calls::handle_stats(context, frame).await,
            Opcode::CallTurnFetch => calls::handle_turn_fetch(context, frame, &self.calls).await,
            Opcode::CallSfuJoin => {
                calls::handle_sfu_join(context, frame, &self.calls, &self.presence_relay).await
            }

            // Every other opcode is one this node speaks the transport for but does not route.
            other => Err(fault::feature_disabled(other.name())),
        }
    }

    async fn authorize_topics(&self, request: &TopicRequest<'_>, topics: &[Topic]) -> Vec<bool> {
        let identity = request.identity();
        let now = request.now();
        let mode = request.bandwidth_mode();
        let mut verdicts = Vec::with_capacity(topics.len());
        for topic in topics {
            verdicts.push(self.authorize_topic(identity, now, mode, topic).await);
        }
        verdicts
    }

    /// A session finished authenticating: mark the account online and tell its rooms.
    ///
    /// Two things follow the first socket of an account coming up. Presence records the device as
    /// connected — the wiring brief section 183 asks for, and which was until now a component
    /// nobody called — and any presence change that produces is published to the account's user
    /// topic. And the room-presence tally learns of the session, which on a 0 → 1 edge raises each
    /// of the account's rooms' online counts and pays back any `Reconnected` owed from an earlier
    /// disconnect.
    ///
    /// Best-effort: a store error is dropped rather than failing a handshake that already
    /// succeeded. The account is connected either way; the worst case is a contact who learns it a
    /// moment late from the next edge.
    async fn session_started(&self, identity: &Identity, mode: BandwidthMode, now: Timestamp) {
        let caller = PresenceCaller::new(
            identity.account_id(),
            identity.device_id(),
            identity.tier,
            mode,
            now,
        );
        if let Ok(Some(fanout)) = self.presence.connected(&caller).await {
            self.publish_presence(&fanout, now);
        }
        self.room_presence
            .on_session_started(identity.account_id(), now)
            .await;
    }

    /// A session ended: mark the account's device offline and start its rooms' grace clock.
    ///
    /// The mirror of [`session_started`](Self::session_started). Presence drops the device, and on
    /// the account's *last* device dropping, the room-presence tally tells each of its rooms the
    /// member went dark and arms the two-minute reconnect grace (section 184). Membership is not
    /// touched here; only the grace expiring with the account still gone removes it.
    ///
    /// Two call facts ride the same edge. A group seat held by the departing device is stamped
    /// gone — the seat sweep retires it once the grace window passes without a re-join, so a
    /// roster never renders a participant whose session died (audit area 6, section 166). And an
    /// account whose last session *anywhere* went down mid-call cannot end the call itself —
    /// nothing is running to send the end — so the node ends its answered calls as `Network` and
    /// tells the survivor now, rather than leaving them to a client-side media timeout (section
    /// 180). "Anywhere" is the tally's to answer: a member whose last socket *here* dropped but
    /// who is online on another node (section 170) is reachable still, and their calls are not
    /// this node's to end. Both are best-effort, logged rather than failed, for the same reason
    /// the presence fan-out is: the socket is already gone, and there is no request to fail.
    async fn session_ended(&self, identity: &Identity, mode: BandwidthMode, now: Timestamp) {
        let account_id = identity.account_id();
        let caller =
            PresenceCaller::new(account_id, identity.device_id(), identity.tier, mode, now);
        if let Ok(Some(fanout)) = self.presence.disconnected(&caller).await {
            self.publish_presence(&fanout, now);
        }
        self.room_presence.on_session_ended(account_id, now).await;
        if let Err(error) = self
            .calls
            .group_session_ended(account_id, identity.device_id(), now)
            .await
        {
            tracing::warn!(%error, "cannot tell the call service a group seat's session ended");
        }
        // The last socket of the account, here or anywhere: the calls above are calls
        // nobody on this account can speak for anymore. The tally is read after the
        // decrement above — and a session that comes up a moment later is the reconnect
        // this cannot see, exactly as one that is already up on another node is. The call
        // it lost is a call its client must re-dial, which is what a network death already
        // meant.
        if !self.room_presence.reachable(account_id) {
            match self.calls.end_disconnected(account_id, now).await {
                Ok(retired) => self.publish_network_ends(account_id, &retired, now).await,
                Err(error) => {
                    tracing::warn!(%error, "cannot end the calls of a departing last session")
                }
            }
        }
    }
}

impl AppDispatcher {
    /// Publishes the `Ended(Network)` events for calls a last session left behind.
    ///
    /// Out of band, through the late-bound gateway, exactly as the ring sweeper publishes
    /// its `NoAnswer` ends: there is no request in hand, and the one party who still
    /// cares — the survivor — is reached on their user topic. The departed account's own
    /// topic gets nothing; it holds no session to receive it, and its next client learns
    /// the call's state from the row the end wrote.
    ///
    /// The survivor's sessions may sit on another node, so the federated half rides the
    /// same publish (section 170's user-topic watch table, in the `FED_CALL_RELAY`
    /// envelope) — warn-not-fail for the same reason the local half is: the socket is
    /// already gone, and there is no request to retry into a second end.
    async fn publish_network_ends(
        &self,
        departed: Id,
        retired: &[migo_calls::Call],
        now: Timestamp,
    ) {
        let Some(gateway) = self.gateway.get() else {
            return;
        };
        for call in retired {
            let Some(other) = call.other_party(departed) else {
                continue;
            };
            let event = call.ended_event();
            gateway.broadcast_to_topic(
                &Topic {
                    kind: TopicKind::User,
                    id: other,
                },
                Opcode::CallStateEvent,
                &event,
                now,
            );
            if let Err(error) = self
                .presence_relay
                .forward_call(other, Opcode::CallStateEvent, &event, now)
                .await
            {
                tracing::warn!(
                    %error,
                    "cannot enqueue the federated half of a network-ended call"
                );
            }
            tracing::info!(call_id = %call.call_id, "a last session died mid-call; the survivor told");
        }
    }
}

impl AppDispatcher {
    /// Whether this caller may receive the fan-out of one [`Topic`], asked ahead of subscription.
    ///
    /// `SUBSCRIBE` is the one place the gateway would otherwise let a frame's own contents decide
    /// what the server sends back, so the decision is read from the domain rather than trusted from
    /// the frame. A `false` here is the same refusal whether the subject is another account, a room
    /// the caller is not in, or a conversation that does not exist: nothing in the answer names why
    /// (section 48), so the batch answer doubles as a probe for which ids are real, and therefore
    /// carries nothing that could be one.
    ///
    /// Refusal rather than error on every lookup failure. The alternative — a `Result` that fails
    /// the whole batch for one bad topic, or leaks which topic was bad — is exactly the probe this
    /// path must not become. `unwrap_or(false)` is that posture: a domain lookup that cannot answer
    /// must answer "no".
    async fn authorize_topic(
        &self,
        identity: &Identity,
        now: Timestamp,
        mode: BandwidthMode,
        topic: &Topic,
    ) -> bool {
        match topic.kind {
            // A conversation's topic is its private stream — message, receipt, typing and game
            // events — and only its members may hold it. Membership is the question, and it is
            // read from the row, never assumed from the frame (section 48).
            TopicKind::Conversation => {
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let mut granted = self
                    .messaging
                    .is_participant(&caller, topic.id)
                    .await
                    .unwrap_or(false);
                if !granted {
                    // The empty membership read may only be the mesh's honest
                    // gap: the conversation's row lives on another node — its
                    // home, stamped at creation — and this node has never had
                    // a reason to hold a copy. Pull the row over the mesh and
                    // ask once more, fail-closed all the way: a home node that
                    // cannot be reached leaves the refusal exactly as it was,
                    // because a topic this node cannot vouch for is a topic it
                    // does not grant.
                    granted = self.replication.ensure_conversation(topic.id, now).await
                        && self
                            .messaging
                            .is_participant(&caller, topic.id)
                            .await
                            .unwrap_or(false);
                }
                // The granted subscription is the moment this node first has a reason to
                // hear the conversation's federated stream: the same tiered fanout a room
                // rides (section 170), asking the conversation's home node to watch it,
                // once per conversation. A room's own conversation needs no ask — its
                // events ride the room's tier — and the relay reads the row to tell the
                // two apart. Best-effort for the same reason every other half here is:
                // the local subscription already succeeded, and the ask is retried by the
                // next granted `SUBSCRIBE` if it could not be made.
                let watch = if granted {
                    self.conversation_relay.subscribe_to(topic.id, now).await
                } else {
                    Ok(())
                };
                if let Err(error) = watch {
                    tracing::warn!(
                        %error,
                        conversation = %topic.id.to_text(),
                        "cannot ask the home node to watch this conversation"
                    );
                }
                granted
            }
            // A room topic carries membership and state events — and, for a room that does not
            // claim end-to-end, the messages themselves. An empty mask to `authorize` asks only
            // "may this account be here at all": membership is the gate, and not-a-member and
            // banned and muted are all the same "no" once the answer is collapsed to a boolean.
            TopicKind::Room => {
                let caller = RoomCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let granted = self.rooms.authorize(&caller, topic.id, 0).await.is_ok();
                // The granted subscription is the moment this node first has a reason to
                // hear the room's federated stream: tiered fanout (section 170) asks the
                // room's home node to watch it, once per room. Best-effort for the same
                // reason every other half here is — the local subscription already
                // succeeded, and the ask is retried by the next granted `SUBSCRIBE` if it
                // could not be made.
                let watch = if granted {
                    self.room_relay.subscribe_to(topic.id, now).await
                } else {
                    Ok(())
                };
                if let Err(error) = watch {
                    tracing::warn!(
                        %error,
                        room = %topic.id.to_text(),
                        "cannot ask the home node to watch this room"
                    );
                }
                granted
            }
            // A user topic is a presence stream. The caller's own presence is theirs by right; a
            // peer's is theirs only when the peer's own `show_last_seen` rule says so — the very
            // gate the presence read path already consults, so the subscribe door cannot show a
            // user who the read door hides (section 180). And a session that narrowed its
            // presence scope (section 159) may not add a peer's stream at all: on LowData and
            // UltraLowData the cadence table limits a session to the streams of the
            // conversations it has open, and refusing the subscription here — rather than
            // filtering every broadcast per subscriber — is how that narrowing is enforced,
            // because a session that cannot subscribe cannot be delivered to. The same refusal
            // as every other "no" here: it names nothing, and is indistinguishable from a topic
            // that does not exist.
            TopicKind::User => {
                let granted = if topic.id == identity.account_id() {
                    true
                } else if self.presence.cadence(mode).scope != PresenceScope::Everything {
                    false
                } else {
                    let caller = SocialCaller::new(
                        identity.account_id(),
                        identity.device_id(),
                        identity.tier,
                        now,
                    );
                    self.social
                        .may_interact(&caller, topic.id, Interaction::LastSeen)
                        .await
                        .is_ok()
                };
                // The granted subscription is the moment this node first has a reason to
                // hear the subject's federated presence stream: the user-topic tier of
                // section 170 asks the peers to watch her, once per subject. This includes
                // the subject's own topic — her devices may sit on several nodes, and the
                // phone's Busy is news to the laptop. Best-effort for the same reason every
                // other half here is: the local subscription already succeeded, and the ask
                // is retried by the next granted `SUBSCRIBE` if it could not be made.
                if granted {
                    if let Err(error) = self.presence_relay.subscribe_to(topic.id, now).await {
                        tracing::warn!(
                            %error,
                            subject = %topic.id.to_text(),
                            "cannot ask the peers to watch this user topic"
                        );
                    }
                }
                granted
            }
            // Nothing on this node ever broadcasts to either, so subscribing is refused outright.
            // Granting them would be granting a topic that produces no events but still costs a
            // held subscription slot.
            TopicKind::Unknown | TopicKind::Game => false,
        }
    }
}

/// Publishes a messaging [`Fanout`](MessageFanout) to its conversation topic, excluding the sender.
///
/// The message and receipt frames are not coalesced — each is a distinct fact a subscriber must
/// see. A typing frame is Coalescable, keyed by conversation and user (section 154), so a burst of
/// start/stop marks from one author collapses to the latest for a consumer whose queue is backed
/// up, and two different authors typing in the same conversation never collapse into one.
fn publish_message_fanout(
    context: &ClientContext<'_>,
    user: Id,
    fanout: MessageFanout,
) -> Result<(), Error> {
    let topic = Topic {
        kind: TopicKind::Conversation,
        id: fanout.conversation_id,
    };
    let opcode = fanout.event.opcode();
    // A member event is never coalesced: *who* joined is the fact, and two joins collapsed
    // into one count is one arrival lost. A vote tally and a state change are Coalescable,
    // keyed by conversation, so a backed-up consumer sees the latest tally and the latest
    // title rather than every intermediate one.
    match &fanout.event {
        MessageBroadcast::Message(event) => publish_event(
            context,
            &topic,
            opcode,
            event,
            None,
            fanout.exclude_device.is_some(),
        ),
        MessageBroadcast::Receipt(event) => publish_event(
            context,
            &topic,
            opcode,
            event,
            None,
            fanout.exclude_device.is_some(),
        ),
        MessageBroadcast::Typing(event) => publish_event(
            context,
            &topic,
            opcode,
            event,
            Some(stream_key(&(fanout.conversation_id, user))),
            fanout.exclude_device.is_some(),
        ),
        MessageBroadcast::Member(event) => {
            publish_event(
                context,
                &topic,
                opcode,
                event,
                None,
                fanout.exclude_device.is_some(),
            )?;
            // The invite's other half. A member event's audience is the
            // conversation's subscribers, and a member who has just been added
            // is not one yet — their client cannot be subscribed to a topic it
            // has never heard of. The same event, published to the joined
            // account's user topic, is the one frame that reaches the invited:
            // every session subscribed to its own topic hears it, and a client
            // that does not know the conversation fetches its list, and the
            // group appears without a refresh. Not coalesced, for the same
            // reason the conversation copy is not: *who* joined is the fact.
            if event.change == MemberChange::Joined {
                context.publish(
                    &Topic {
                        kind: TopicKind::User,
                        id: event.user_id,
                    },
                    opcode,
                    event,
                    None,
                )?;
            }
            Ok(())
        }
        MessageBroadcast::Vote(event) => publish_event(
            context,
            &topic,
            opcode,
            event,
            Some(stream_key(&fanout.conversation_id)),
            fanout.exclude_device.is_some(),
        ),
        MessageBroadcast::State(event) => {
            publish_event(
                context,
                &topic,
                opcode,
                event,
                Some(stream_key(&fanout.conversation_id)),
                fanout.exclude_device.is_some(),
            )?;
            // The renamer's own screen. The conversation copy excludes the
            // device that asked for the change — its request was answered by
            // the reply — so a client that applies nothing until the server
            // says so is left rendering the title it renamed away. The actor's
            // user-topic copy includes that device by design: the same delta,
            // idempotent to apply twice, and the one that keeps the renamer's
            // screen honest.
            context.publish(
                &Topic {
                    kind: TopicKind::User,
                    id: user,
                },
                opcode,
                event,
                Some(stream_key(&fanout.conversation_id)),
            )?;
            Ok(())
        }
    }
}

/// Publishes one event, skipping the actor's connection only when an actor caused it.
///
/// The service says which: a fanout with an `exclude_device` was caused by that device, whose
/// connection was answered by the reply and should not also render the echo. A fanout without one
/// — a kick vote that expired under the next caller's request — reaches every subscriber, the
/// caller included: they are a member too, and this is the only frame that will tell them the old
/// tally closed.
fn publish_event<T: Encode>(
    context: &ClientContext<'_>,
    topic: &Topic,
    opcode: Opcode,
    event: &T,
    coalesce_key: Option<u64>,
    exclude_actor: bool,
) -> Result<(), Error> {
    if exclude_actor {
        context.publish_excluding_self(topic, opcode, event, coalesce_key)
    } else {
        context.publish(topic, opcode, event, coalesce_key)
    }
}

/// Encodes and publishes one rooms [`Fanout`](RoomFanout) to its room topic, excluding the actor.
///
/// A membership event (join, leave, role change) is not coalesced: collapsing two joins would lose
/// one arrival. A state event (a counter or a setting moving) is Coalescable, keyed by room, so
/// three counter updates about one room collapse to the last one for a backed-up consumer.
///
/// This is the delivery half only. Callers on the request path go through
/// [`AppDispatcher::publish_rooms`], which also takes the room's topics away
/// from a member the event removed; this bare helper is for fanouts that
/// change nobody's standing.
pub(crate) fn publish_room_fanout(
    context: &ClientContext<'_>,
    fanout: RoomFanout,
) -> Result<(), Error> {
    let topic = Topic {
        kind: TopicKind::Room,
        id: fanout.room_id,
    };
    let opcode = fanout.opcode();
    match &fanout.event {
        RoomBroadcast::Member(event) => {
            context.publish_excluding_self(&topic, opcode, event, None)?;
            // The join's other half, and rooms owe conversations the same doorbell. A member
            // event's audience is the room's subscribers, and a member who has just joined is
            // not one yet on any device but the one that asked — their other sessions cannot
            // be subscribed to a room they have never heard of. The same event, published to
            // the joiner's user topic, is the one frame that reaches them: every session
            // listening to its own topic hears it, and a client that does not know the room
            // reacts by re-deriving its subscriptions (a join is idempotent, section 156), so
            // a member becomes a member everywhere they hold a session. Not coalesced, for
            // the same reason the room copy is not: *who* joined is the fact.
            if matches!(event.change, Some(MemberChange::Joined)) {
                context.publish(
                    &Topic {
                        kind: TopicKind::User,
                        id: event.user_id,
                    },
                    opcode,
                    event,
                    None,
                )?;
            }
            Ok(())
        }
        RoomBroadcast::State(event) => {
            context.publish_excluding_self(&topic, opcode, event, Some(stream_key(&fanout.room_id)))
        }
        RoomBroadcast::Vote(event) => {
            context.publish_excluding_self(&topic, opcode, event, Some(stream_key(&fanout.room_id)))
        }
    }
}

/// Projects a domain [`Bundle`] onto the wire struct.
///
/// The domain's `one_time_prekey: Option<(u32, Vec<u8>)>` becomes the wire's two independent
/// `Option`s, which can in principle disagree; they never do here, because they are filled from one
/// `Option` in one expression. `signed_prekey_expires_at` has no wire field and is dropped — the
/// receiving client learns nothing about when the prekey it just fetched dies, which is a real gap
/// in the frozen IDL and not a decision taken here.
fn wire_bundle(bundle: Bundle) -> WireBundle {
    let (one_time_prekey_id, one_time_prekey) = match bundle.one_time_prekey {
        Some((key_id, public_key)) => (Some(key_id), Some(public_key)),
        None => (None, None),
    };
    WireBundle {
        user_id: bundle.account_id,
        device_id: bundle.device_id,
        identity_key: bundle.identity_key,
        signed_prekey_id: bundle.signed_prekey_id,
        signed_prekey: bundle.signed_prekey,
        signed_prekey_signature: bundle.signed_prekey_signature,
        one_time_prekey_id,
        one_time_prekey,
    }
}

/// Projects a [`ProfileCard`] onto the wire struct.
///
/// Six of the fourteen wire fields are left absent, and absent is not the same as false. `level`
/// belongs to progression, `presence` to presence, `badges` and `verified` to moderation, and
/// `custom_status` to a column the data model does not have; a defaulted `verified: false` on a
/// verified account would be a wrong answer wearing the shape of an answer. `avatar_url` is absent
/// while `avatar_media_id` is carried: section 168 forbids the server from proxying media bytes, so
/// the URL is a signed one the media service mints on request, and minting it here would put an
/// expiring credential inside a response a client may cache — the id is the durable fact the client
/// resolves at render time.
///
/// `public_id` is derived rather than stored: it is a lossy display projection of the account id
/// (`MGO-XXXXXXXXXXXX`), which is why nothing persists it.
///
/// `birth_year` is carried when its owner disclosed it; the store keeps it as an `i16` year and
/// the wire as a `u32`, and the projection is the one widening that cannot lose a year.
fn wire_profile(card: ProfileCard) -> UserProfile {
    UserProfile {
        user_id: card.account_id,
        public_id: card.account_id.public_id(PublicId::User),
        username: card.username,
        display_name: card.display_name,
        avatar_url: None,
        avatar_media_id: card.avatar_media_id,
        bio: card.bio,
        country: card.country,
        language: Some(card.locale),
        level: None,
        presence: None,
        badges: None,
        verified: None,
        custom_status: card.custom_status,
        birth_year: card.birth_year.map(|year| year as u32),
    }
}

/// Reads a [`Move`] out of a [`GameAction`]'s `action` name and its one argument.
///
/// The wire carries a string and a list of strings; the domain carries a closed enum. The mapping is
/// deliberately narrow — three names, one argument each, nothing optional — because every string the
/// server accepts here is a string every client must produce identically, and a permissive parser
/// would make "which spellings work" a property of this function rather than of the protocol.
///
/// A name this build does not know is `VALIDATION_FAILED` on `action`, and a missing or unparsable
/// argument is `VALIDATION_FAILED` on `args`. Neither is `FEATURE_DISABLED`: the feature is wired,
/// the request is wrong.
fn domain_move(request: &GameAction) -> Result<Move, Error> {
    let arg = |index: usize| -> Result<&str, Error> {
        request
            .args
            .as_ref()
            .and_then(|args| args.get(index))
            .map(String::as_str)
            .ok_or_else(|| fault::validation("args", "this action needs an argument"))
    };
    match request.action.as_str() {
        "place" => {
            let cell: u8 = arg(0)?
                .parse()
                .map_err(|_| fault::validation("args", "cell must be a number"))?;
            Ok(Move::Place { cell })
        }
        "throw" => {
            let hand = match arg(0)? {
                "rock" => Hand::Rock,
                "paper" => Hand::Paper,
                "scissors" => Hand::Scissors,
                _ => {
                    return Err(fault::validation(
                        "args",
                        "hand must be rock, paper or scissors",
                    ))
                }
            };
            Ok(Move::Throw { hand })
        }
        "guess" => {
            let value: u16 = arg(0)?
                .parse()
                .map_err(|_| fault::validation("args", "guess must be a number"))?;
            Ok(Move::Guess { value })
        }
        _ => Err(fault::validation("action", "unknown game action")),
    }
}

/// Publishes one move's deltas to the conversation the game is played in.
///
/// Who hears the fan-out is decided by the caller's own reply, which is why [`publish_game`]
/// takes `to_caller` rather than choosing on its own:
///
/// * `GAME_ACTION` passes `true` (**including** the mover's own connection), which is the one
///   place in this file that does not use
///   [`publish_excluding_self`](ClientContext::publish_excluding_self). The house rule holds
///   elsewhere because the reply carries the outcome; here the IDL's response to `GAME_ACTION`
///   is `Acknowledged`, which carries nothing, so a mover excluded from its own fan-out would
///   never learn whose turn it now is or that the game just ended. `GAME_ABANDON` passes `true`
///   for exactly the same reason: its reply is the same bare `Acknowledged`, so the abandoner's
///   own devices learn the game ended only through this fan-out.
/// * `GAME_START` passes `false` (the house rule): its reply *is* the opening view, so the
///   starting connection already holds fresher state than the delta could carry, and the other
///   members — and the starter's other devices — hear the `started` event through
///   [`publish_excluding_self`](ClientContext::publish_excluding_self).
///
/// The deltas are safe to send to every player by construction — section 39's `Moved` says only
/// *that* somebody moved, never what the move was — so there is nothing in them the mover may
/// not see.
///
/// The topic comes from [`GameView::conversation_id`], never from the request. A client that could
/// name the topic could publish a game event into a conversation it is not playing in.
///
/// `payload` and `text` are absent throughout. There is no delta to put in `payload`: the domain's
/// events carry no board content on purpose, and a full snapshot is exactly what the field forbids.
/// `text` would need display names to render a line, which this dispatcher does not have and would
/// have to fetch per event; a client that already holds the profiles renders it better.
fn publish_game(
    context: &ClientContext<'_>,
    view: &GameView,
    events: &[GameDelta],
    to_caller: bool,
) -> Result<(), Error> {
    let topic = Topic {
        kind: TopicKind::Conversation,
        id: view.conversation_id,
    };
    for event in events {
        let (name, subject) = match event {
            // The account each event is *about*, which for a turn change is whose turn it now is
            // rather than who caused it: the wire has one id field and that is the id a client
            // needs in order to highlight a seat.
            GameDelta::Started { .. } => ("started", None),
            GameDelta::Moved { by, .. } => ("moved", Some(*by)),
            GameDelta::TurnChanged { turn_of, .. } => ("turn_changed", Some(*turn_of)),
            GameDelta::Finished { outcome, .. } => (
                "finished",
                match outcome {
                    Outcome::Win { winner } => Some(*winner),
                    Outcome::Draw | Outcome::NoContest => None,
                },
            ),
        };
        let wire = GameEvent {
            game_id: view.game_id,
            // The IDL calls it `room_id`; a game is played in a conversation, and this is that
            // conversation. One subject, two names, and the domain's is the authoritative one.
            room_id: view.conversation_id,
            // Every event of one move describes the same resulting state, so they share a version.
            // A client receiving them out of order can still tell which board they describe.
            state_version: view.state_version,
            event: name.to_string(),
            payload: None,
            actor_id: subject,
            text: None,
        };
        // Coalescing is not offered: `GAME_EVENT` is Critical, so a queued event is never
        // superseded, and collapsing two moves would lose one.
        if to_caller {
            context.publish(&topic, Opcode::GameEvent, &wire, None)?;
        } else {
            context.publish_excluding_self(&topic, Opcode::GameEvent, &wire, None)?;
        }
    }
    Ok(())
}

/// A stable per-process key that groups the frames of one Coalescable stream.
///
/// Coalescing compares keys only within a single subscriber's queue and only among frames of the
/// same delivery class, so the key needs to be stable for the life of the process and equal for
/// frames that should supersede one another — which a hash of the stream's identity (a subject, a
/// room, or a conversation-and-author pair) gives. [`DefaultHasher`] is seeded deterministically,
/// so the same identity yields the same key every time within a run.
///
/// The mesh ingest path derives its keys the same way, for the same reason: a copy of a stream
/// that arrives from a peer must collapse into the one local stream, or a subscriber watching
/// both this node's publishers and the mesh would keep two. The keys need only agree within one
/// node — coalescing never compares across nodes — so the ingest side hashing an `Option` where
/// the request path hashes the plain id is the same stream, not a second one.
pub(crate) fn stream_key(identity: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    hasher.finish()
}
