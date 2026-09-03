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
use migo_notify::SharedNotifier;
use migo_presence::{Caller as PresenceCaller, SharedPresence};
use migo_protocol::{
    fault, from_frame, Acknowledged, BandwidthMode, ConversationCreateRequest,
    ConversationInviteRequest, ConversationKickRequest, ConversationLeaveRequest,
    ConversationListRequest, ConversationMuteRequest, ConversationRosterRequest,
    ConversationUpdateRequest, ConversationVoteKickRequest, Encode, Frame, GameAction, GameEvent,
    KeyBundle as WireBundle, KeyBundleRequest, KeyBundleResponse, KeyPublish, KeyPublishResult,
    MessageDelete, MessageEdit, MessageKind, MessageReceipt, MessageSend, Opcode, PresenceUpdate,
    ProfileRequest, ProfileResponse, ReactionSet, RoomJoinRequest, RoomLeaveRequest,
    RoomListRequest, SyncRequest, Topic, TopicKind, TypingEvent, UserProfile,
};
use migo_rooms::{
    Broadcast as RoomBroadcast, Caller as RoomCaller, Fanout as RoomFanout, SharedRooms,
};
use migo_social::{Caller as SocialCaller, Interaction, ProfileCard, SharedSocial};

use crate::room_presence::{GatewayHandle, GatewayPublisher, RoomPresence};

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
    /// here rather than passed in, because it is glue this dispatcher owns and nothing else
    /// holds — it reads the same store and rooms handle the dispatcher already has, and it
    /// publishes through the same late-bound gateway.
    room_presence: Arc<RoomPresence>,
    /// The late-bound gateway, filled by the composition root once the gateway is open. Used to
    /// publish presence and room lifecycle events out of band — with no client request in hand —
    /// on the connection edges the gateway reports through [`Dispatcher::session_started`] and
    /// [`Dispatcher::session_ended`].
    gateway: Arc<GatewayHandle>,
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
        gateway: Arc<GatewayHandle>,
    ) -> Self {
        // The room-presence component reads the same store and rooms handle and publishes through
        // the same gateway handle, so it is assembled from what this call already holds rather
        // than threaded through as one more argument.
        let room_presence = Arc::new(RoomPresence::new(
            migo_store::SharedStore::clone(&store),
            SharedRooms::clone(&rooms),
            Arc::new(GatewayPublisher::new(Arc::clone(&gateway))),
        ));
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
                let caller = MessageCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: MessageSend = from_frame(frame).map_err(fault::from_wire)?;
                let (accepted, fanout) = self.messaging.send(&caller, request).await?;
                context.reply(&accepted)?;
                if let Some(fanout) = fanout {
                    publish_messaging(context, caller.account_id, fanout)?;
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
                    publish_messaging(context, caller.account_id, fanout)?;
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
                let send = MessageSend {
                    message_id: migo_core::Id::generate_at(now, &mut migo_core::OsRandom),
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
                    publish_messaging(context, caller.account_id, fanout)?;
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
                    publish_messaging(context, caller.account_id, fanout)?;
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
                let (accepted, fanout) = self.messaging.delete(&caller, request).await?;
                context.reply(&accepted)?;
                if let Some(fanout) = fanout {
                    publish_messaging(context, caller.account_id, fanout)?;
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
                let summary = self.messaging.create(&caller, request).await?;
                context.reply(&summary)
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
                    publish_messaging(context, caller.account_id, fanout)?;
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
                for fanout in fanouts {
                    publish_messaging(context, caller.account_id, fanout)?;
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
                context.reply(&Acknowledged { ok: true })?;
                for fanout in fanouts {
                    publish_messaging(context, caller.account_id, fanout)?;
                }
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
                    publish_messaging(context, caller.account_id, fanout)?;
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
                    publish_messaging(context, caller.account_id, fanout)?;
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
                    publish_messaging(context, caller.account_id, fanout)?;
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
                let caller = RoomCaller::new(
                    identity.account_id(),
                    identity.device_id(),
                    identity.tier,
                    now,
                );
                let request: RoomJoinRequest = from_frame(frame).map_err(fault::from_wire)?;
                let (mut response, fanout) = self.rooms.join(&caller, request).await?;
                // The rooms crate leaves `online_count` at `view::ONLINE_COUNT_UNSET`; fill it from
                // the in-memory session tally — the joiner themselves is now online and a member, so
                // the count they are handed already includes them.
                response.room.online_count =
                    self.room_presence.online_count(response.room.room_id).await;
                context.reply(&response)?;
                if let Some(fanout) = fanout {
                    publish_rooms(context, fanout)?;
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
                if let Some(fanout) = self.rooms.leave(&caller, request).await? {
                    publish_rooms(context, fanout)?;
                }
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
            Opcode::RoomRoleSet => rooms_admin::handle_role_set(context, frame, &self.rooms).await,
            Opcode::RoomUpdate => {
                rooms_admin::handle_room_update(context, frame, &self.rooms).await
            }
            Opcode::RoomArchive => {
                rooms_admin::handle_room_archive(context, frame, &self.rooms).await
            }
            Opcode::RoomSanction => rooms_admin::handle_sanction(context, frame, &self.rooms).await,
            Opcode::RoomVoteKick => {
                rooms_admin::handle_vote_kick(context, frame, &self.rooms).await
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
                publish_game(context, &result.view, &result.events)
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
                social::handle_friend_request(context, frame, &self.social, &self.notify).await
            }
            Opcode::FriendRespond => {
                social::handle_friend_respond(context, frame, &self.social, &self.notify).await
            }
            Opcode::BlockSet => social::handle_block_set(context, frame, &self.social).await,
            Opcode::MuteSet => social::handle_mute_set(context, frame, &self.social).await,
            Opcode::RelationshipList => {
                social::handle_relationship_list(context, frame, &self.social).await
            }

            // --- notify ---
            Opcode::NotificationAck => notify::handle_ack(context, frame, &self.notify).await,
            Opcode::NotificationList => notify::handle_list(context, frame, &self.notify).await,

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

            // --- calls ---
            // Each handler replies to the sender and publishes the returned
            // event to the other party's user topic; the service owns every
            // rule and never sends a frame itself.
            Opcode::CallInvite => {
                calls::handle_invite(context, frame, &self.calls, &self.notify).await
            }
            Opcode::CallAnswer => calls::handle_answer(context, frame, &self.calls).await,
            Opcode::CallDecline => calls::handle_decline(context, frame, &self.calls).await,
            Opcode::CallCancel => calls::handle_cancel(context, frame, &self.calls).await,
            Opcode::CallEnd => calls::handle_end(context, frame, &self.calls).await,
            Opcode::CallSdp => calls::handle_sdp(context, frame, &self.calls).await,
            Opcode::CallIce => calls::handle_ice(context, frame, &self.calls).await,
            Opcode::CallRenegotiate => calls::handle_renegotiate(context, frame, &self.calls).await,
            Opcode::CallKeyUpdate => calls::handle_key_update(context, frame, &self.calls).await,
            Opcode::CallStats => calls::handle_stats(context, frame).await,
            Opcode::CallTurnFetch => calls::handle_turn_fetch(context, frame, &self.calls).await,
            Opcode::CallSfuJoin => calls::refuse_sfu_join(),

            // Every other opcode is one this node speaks the transport for but does not route.
            other => Err(fault::feature_disabled(other.name())),
        }
    }

    async fn authorize_topics(&self, request: &TopicRequest<'_>, topics: &[Topic]) -> Vec<bool> {
        let identity = request.identity();
        let now = request.now();
        let mut verdicts = Vec::with_capacity(topics.len());
        for topic in topics {
            verdicts.push(self.authorize_topic(identity, now, topic).await);
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
    async fn session_ended(&self, identity: &Identity, mode: BandwidthMode, now: Timestamp) {
        let caller = PresenceCaller::new(
            identity.account_id(),
            identity.device_id(),
            identity.tier,
            mode,
            now,
        );
        if let Ok(Some(fanout)) = self.presence.disconnected(&caller).await {
            self.publish_presence(&fanout, now);
        }
        self.room_presence
            .on_session_ended(identity.account_id(), now)
            .await;
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
    async fn authorize_topic(&self, identity: &Identity, now: Timestamp, topic: &Topic) -> bool {
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
                self.messaging
                    .is_participant(&caller, topic.id)
                    .await
                    .unwrap_or(false)
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
                self.rooms.authorize(&caller, topic.id, 0).await.is_ok()
            }
            // A user topic is a presence stream. The caller's own presence is theirs by right; a
            // peer's is theirs only when the peer's own `show_last_seen` rule says so — the very
            // gate the presence read path already consults, so the subscribe door cannot show a
            // user who the read door hides (section 180).
            TopicKind::User => {
                if topic.id == identity.account_id() {
                    true
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
                }
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
        MessageBroadcast::Member(event) => publish_event(
            context,
            &topic,
            opcode,
            event,
            None,
            fanout.exclude_device.is_some(),
        ),
        MessageBroadcast::Vote(event) => publish_event(
            context,
            &topic,
            opcode,
            event,
            Some(stream_key(&fanout.conversation_id)),
            fanout.exclude_device.is_some(),
        ),
        MessageBroadcast::State(event) => publish_event(
            context,
            &topic,
            opcode,
            event,
            Some(stream_key(&fanout.conversation_id)),
            fanout.exclude_device.is_some(),
        ),
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

/// Publishes a rooms [`Fanout`](RoomFanout) to its room topic, excluding the actor.
///
/// A membership event (join, leave, role change) is not coalesced: collapsing two joins would lose
/// one arrival. A state event (a counter or a setting moving) is Coalescable, keyed by room, so
/// three counter updates about one room collapse to the last one for a backed-up consumer.
///
/// Shared with the rooms dispatch module, whose role and settings handlers fan out through the
/// same match so a member event and a state event keep one encoder and one exclusion rule.
pub(crate) fn publish_rooms(context: &ClientContext<'_>, fanout: RoomFanout) -> Result<(), Error> {
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
/// (`MGO-XXXXXXXX`), which is why nothing persists it.
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
        custom_status: None,
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
/// **Including the mover's own connection**, which is the one place in this file that does not use
/// [`publish_excluding_self`](ClientContext::publish_excluding_self). The house rule holds elsewhere
/// because the reply carries the outcome; here the IDL's response to `GAME_ACTION` is
/// `Acknowledged`, which carries nothing, so a mover excluded from its own fan-out would never learn
/// whose turn it now is or that the game just ended. The deltas are safe to send to every player by
/// construction — section 39's `Moved` says only *that* somebody moved, never what the move was — so
/// there is nothing in them the mover may not see.
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
        context.publish(&topic, Opcode::GameEvent, &wire, None)?;
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
fn stream_key(identity: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    hasher.finish()
}
