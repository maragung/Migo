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
//! 1. **Build the caller.** The authenticated [`Identity`](migo_auth::Identity) the gateway proved
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
//! # Reply-or-fan-out follows the return type, not a table
//!
//! There is no per-opcode configuration of "does this reply". The domain trait already encodes it:
//! `send`, `delete`, `sync`, `conversations`, `create`, `join`, and `list` return a payload and are
//! answered; `receipt`, `typing`, `set`, and `leave` return only an `Option<Fanout>` and are not.
//! `send`, `delete`, and `join` do both — the caller gets the authoritative reply, and everyone
//! else on the topic gets the fan-out — so the reply goes first.
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

use async_trait::async_trait;

use migo_core::{Error, Id};
use migo_gateway::{ClientContext, Dispatcher};
use migo_messaging::{
    Broadcast as MessageBroadcast, Caller as MessageCaller, Fanout as MessageFanout,
    SharedMessaging,
};
use migo_presence::{Caller as PresenceCaller, SharedPresence};
use migo_protocol::{
    fault, from_frame, BandwidthMode, ConversationCreateRequest, ConversationListRequest, Frame,
    MessageDelete, MessageReceipt, MessageSend, Opcode, PresenceUpdate, RoomJoinRequest,
    RoomLeaveRequest, RoomListRequest, SyncRequest, Topic, TopicKind, TypingEvent,
};
use migo_rooms::{
    Broadcast as RoomBroadcast, Caller as RoomCaller, Fanout as RoomFanout, SharedRooms,
};

/// The dispatcher that routes the client-facing application opcodes into the domain services.
///
/// Holds a handle to each domain it speaks for. The handles are `Arc<dyn Trait>`, so the dispatcher
/// is cheap to clone conceptually and is shared as `Arc<dyn Dispatcher>` by the gateway; it adds no
/// state of its own beyond the three services.
pub struct AppDispatcher {
    messaging: SharedMessaging,
    presence: SharedPresence,
    rooms: SharedRooms,
}

impl AppDispatcher {
    /// Wires the dispatcher to the three domains whose opcodes it routes.
    #[must_use]
    pub fn new(messaging: SharedMessaging, presence: SharedPresence, rooms: SharedRooms) -> Self {
        Self {
            messaging,
            presence,
            rooms,
        }
    }
}

#[async_trait]
impl Dispatcher for AppDispatcher {
    async fn dispatch(&self, context: &ClientContext<'_>, frame: &Frame) -> Result<(), Error> {
        let identity = context.identity();
        let now = context.now();

        match context.opcode() {
            // --- messaging ---
            Opcode::MessageSend => {
                let caller =
                    MessageCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);
                let request: MessageSend = from_frame(frame).map_err(fault::from_wire)?;
                let (accepted, fanout) = self.messaging.send(&caller, request).await?;
                context.reply(&accepted)?;
                if let Some(fanout) = fanout {
                    publish_messaging(context, caller.account_id, fanout)?;
                }
                Ok(())
            }
            Opcode::MessageReceipt => {
                let caller =
                    MessageCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);
                let request: MessageReceipt = from_frame(frame).map_err(fault::from_wire)?;
                if let Some(fanout) = self.messaging.receipt(&caller, request).await? {
                    publish_messaging(context, caller.account_id, fanout)?;
                }
                Ok(())
            }
            Opcode::MessageDelete => {
                let caller =
                    MessageCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);
                let request: MessageDelete = from_frame(frame).map_err(fault::from_wire)?;
                let (accepted, fanout) = self.messaging.delete(&caller, request).await?;
                context.reply(&accepted)?;
                if let Some(fanout) = fanout {
                    publish_messaging(context, caller.account_id, fanout)?;
                }
                Ok(())
            }
            Opcode::Sync => {
                let caller =
                    MessageCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);
                let request: SyncRequest = from_frame(frame).map_err(fault::from_wire)?;
                let response = self.messaging.sync(&caller, request).await?;
                context.reply(&response)
            }
            Opcode::ConversationList => {
                let caller =
                    MessageCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);
                let request: ConversationListRequest = from_frame(frame).map_err(fault::from_wire)?;
                let response = self.messaging.conversations(&caller, request).await?;
                context.reply(&response)
            }
            Opcode::ConversationCreate => {
                let caller =
                    MessageCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);
                let request: ConversationCreateRequest =
                    from_frame(frame).map_err(fault::from_wire)?;
                let summary = self.messaging.create(&caller, request).await?;
                context.reply(&summary)
            }
            Opcode::Typing => {
                let caller =
                    MessageCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);
                let request: TypingEvent = from_frame(frame).map_err(fault::from_wire)?;
                if let Some(fanout) = self.messaging.typing(&caller, request).await? {
                    publish_messaging(context, caller.account_id, fanout)?;
                }
                Ok(())
            }

            // --- presence ---
            Opcode::PresenceSet => {
                // The bandwidth mode only shapes heartbeat cadence, which `set` does not consult;
                // the request itself carries the state being set. The default is fine here.
                let caller = PresenceCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    BandwidthMode::default(),
                    now,
                );
                let request: PresenceUpdate = from_frame(frame).map_err(fault::from_wire)?;
                if let Some(fanout) = self.presence.set(&caller, request).await? {
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
                }
                Ok(())
            }

            // --- rooms ---
            Opcode::RoomJoin => {
                let caller =
                    RoomCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);
                let request: RoomJoinRequest = from_frame(frame).map_err(fault::from_wire)?;
                let (response, fanout) = self.rooms.join(&caller, request).await?;
                context.reply(&response)?;
                if let Some(fanout) = fanout {
                    publish_rooms(context, fanout)?;
                }
                Ok(())
            }
            Opcode::RoomLeave => {
                let caller =
                    RoomCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);
                let request: RoomLeaveRequest = from_frame(frame).map_err(fault::from_wire)?;
                if let Some(fanout) = self.rooms.leave(&caller, request).await? {
                    publish_rooms(context, fanout)?;
                }
                Ok(())
            }
            Opcode::RoomList => {
                let caller =
                    RoomCaller::new(identity.account_id(), identity.device_id(), identity.tier, now);
                let request: RoomListRequest = from_frame(frame).map_err(fault::from_wire)?;
                let response = self.rooms.list(&caller, request).await?;
                context.reply(&response)
            }

            // Every other opcode is one this node speaks the transport for but does not route.
            other => Err(fault::feature_disabled(other.name())),
        }
    }
}

/// Publishes a messaging [`Fanout`](MessageFanout) to its conversation topic, excluding the sender.
///
/// The message and receipt frames are not coalesced — each is a distinct fact a subscriber must
/// see. A typing frame is Coalescable, keyed by conversation and user (section 154), so a burst of
/// start/stop marks from one author collapses to the latest for a consumer whose queue is backed
/// up, and two different authors typing in the same conversation never collapse into one.
fn publish_messaging(
    context: &ClientContext<'_>,
    user: Id,
    fanout: MessageFanout,
) -> Result<(), Error> {
    let topic = Topic {
        kind: TopicKind::Conversation,
        id: fanout.conversation_id,
    };
    let opcode = fanout.event.opcode();
    match &fanout.event {
        MessageBroadcast::Message(event) => {
            context.publish_excluding_self(&topic, opcode, event, None)
        }
        MessageBroadcast::Receipt(event) => {
            context.publish_excluding_self(&topic, opcode, event, None)
        }
        MessageBroadcast::Typing(event) => context.publish_excluding_self(
            &topic,
            opcode,
            event,
            Some(stream_key(&(fanout.conversation_id, user))),
        ),
    }
}

/// Publishes a rooms [`Fanout`](RoomFanout) to its room topic, excluding the actor.
///
/// A membership event (join, leave, role change) is not coalesced: collapsing two joins would lose
/// one arrival. A state event (a counter or a setting moving) is Coalescable, keyed by room, so
/// three counter updates about one room collapse to the last one for a backed-up consumer.
fn publish_rooms(context: &ClientContext<'_>, fanout: RoomFanout) -> Result<(), Error> {
    let topic = Topic {
        kind: TopicKind::Room,
        id: fanout.room_id,
    };
    let opcode = fanout.opcode();
    match &fanout.event {
        RoomBroadcast::Member(event) => context.publish_excluding_self(&topic, opcode, event, None),
        RoomBroadcast::State(event) => {
            context.publish_excluding_self(&topic, opcode, event, Some(stream_key(&fanout.room_id)))
        }
    }
}

/// A stable per-process key that groups the frames of one Coalescable stream.
///
/// Coalescing compares keys only within a single subscriber's queue and only among frames of the
/// same delivery class, so the key needs to be stable for the life of the process and equal for
/// frames that should supersede one another — which a hash of the stream's identity (a subject, a
/// room, or a conversation-and-author pair) gives. [`DefaultHasher`] is seeded deterministically,
/// so the same identity yields the same key every time within a run.
fn stream_key(identity: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    hasher.finish()
}
