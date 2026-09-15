//! Tiered room fanout across nodes (brief sections 170, 169).
//!
//! A room with tens of thousands of members must not be served by one node
//! writing to every socket: section 170 asks for fan-out bertingkat, where the
//! room's home node hands *one* copy to each node that holds a slice of the
//! members, and that node fans out through its own hub. This module is the
//! bookkeeping that makes the two halves meet:
//!
//! - the **subscribe half**: when a local session is granted a room topic
//!   (`authorize_topics`, the same gate every `SUBSCRIBE` already passes), the
//!   node asks the room's home node to watch the room — once per room, for the
//!   life of the process — with a `FED_ROOM_SUBSCRIBE` carrying the routing
//!   epoch and the home region the room row names;
//! - the **watch half**: the home node records which peer nodes hold
//!   subscribers of which rooms, so a publish becomes one federated copy per
//!   *node*, not per session and not per member;
//! - the **forward half**: all of a room's publish paths — the request path
//!   through `publish_room_fanout`, the out-of-band room-presence publisher,
//!   and the messaging path that carries the room's own *chat*, because a
//!   room's conversation is what its members actually talk in — hand their
//!   fanout here, and the node that homes the room enqueues one
//!   `FED_ROOM_EVENT` per watching node, the inner event frame sealed exactly
//!   as a local session would have received it. The receiving node's ingest
//!   path (`mesh::route_room_event`) publishes it into its own hub, which is
//!   the fan-out the tier asks for, and passes it on in turn if the watch
//!   table is there.
//! - the **move half**: a room whose home node changes is moved by
//!   [`move_room`](RoomRelay::move_room), and its members are told before
//!   anything else is — one `RECONNECT_HINT` naming the new home's endpoint,
//!   published locally for the sessions already here and carried to the other
//!   nodes through the same tier every room event rides.
//!
//! Only the home node holds a watch table, so a publish on any other node is
//! not a fan-out at all: it is one copy addressed to the home node, which does
//! the tiering. `RoomRelay::to_owner` is that decision in one place, on all
//! three publish paths and on the ingest side.
//!
//! # Ordering, redelivery, and the missing unsubscribe
//!
//! Per-link sequence numbers are strictly increasing, so the events one node
//! enqueues for another arrive in the order they were published. Delivery is
//! at least once: a torn link means the batch is resent, and a re-delivered
//! room event is re-published into a hub whose consumers are idempotent
//! (section 153) — a subscriber that already applied an event applies it
//! again to no effect.
//!
//! The wire has no `FED_ROOM_UNSUBSCRIBE` (the IDL is frozen), so a watcher
//! set only grows: a node that no longer holds subscribers of a room keeps
//! receiving its events, and the receiving hub's fan-out to an empty topic is
//! a cheap no-op. When the protocol gains the opcode, the entry can be
//! dropped here and nothing else changes. The subscribe cache is also why a
//! home node restart needs more than luck: the cache says every ask was
//! answered, the home node's replacement holds an empty table, and no client
//! `SUBSCRIBE` is coming to re-ask — so the composition root re-anchors the
//! cache on a timer, re-sending the
//! watches a fresh process would have sent and letting a restarted home node
//! rebuild its table from them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bytes::Bytes;
use migo_core::{Id, Result, Timestamp};
use migo_federation::model::{FederatedEvent, PeerView};
use migo_federation::SharedMesh;
use migo_messaging::{Broadcast as MessageBroadcast, Fanout as MessageFanout};
use migo_protocol::{
    fault, to_frame, CloseReason, Encode, FedRoomEvent, FedRouting, Frame, Opcode, ReconnectHint,
    RoomMemberEvent, RoomStateEvent, RoomVoteEvent, Topic, TopicKind,
};
use migo_rooms::{Broadcast as RoomBroadcast, Fanout as RoomFanout};
use migo_store::model::Room;
use migo_store::SharedStore;

use crate::room_presence::RoomPublisher;

/// How many allow-list rows one home-node lookup reads. The page clamp the
/// directory answers with, reused because a home-region scan is the same
/// bounded read.
const PEER_SCAN_LIMIT: u16 = 256;

/// Which peer nodes hold subscribers of which rooms, and the subscribe half
/// that keeps the table honest from the other side.
pub struct RoomRelay {
    mesh: SharedMesh,
    store: SharedStore,
    /// The local half of a rebalance hint: the gateway's hub, once the gateway
    /// exists. `None` in a test that asserts on the federated half alone.
    hints: Option<Arc<dyn HintPublisher>>,
    /// The watch table, home-node side: room → the peer nodes watching it.
    watchers: parking_lot::Mutex<HashMap<Id, HashSet<Id>>>,
    /// The rooms this process has already asked a home node to watch. One
    /// `FED_ROOM_SUBSCRIBE` per room per process; a second would only be
    /// re-registered on the other side (the insert is idempotent), but the
    /// outbox owes the peer one copy, not one per subscribing session.
    subscribed: parking_lot::Mutex<HashSet<Id>>,
}

impl RoomRelay {
    /// Wraps the mesh, the store, and the local hint surface the two halves read.
    #[must_use]
    pub fn new(
        mesh: SharedMesh,
        store: SharedStore,
        hints: Option<Arc<dyn HintPublisher>>,
    ) -> Self {
        Self {
            mesh,
            store,
            hints,
            watchers: parking_lot::Mutex::new(HashMap::new()),
            subscribed: parking_lot::Mutex::new(HashSet::new()),
        }
    }

    /// The peer nodes watching one room, sorted so a test reads a stable
    /// answer and an operator's log reads a deterministic line.
    pub fn watchers_of(&self, room_id: Id) -> Vec<Id> {
        let mut nodes = self
            .watchers
            .lock()
            .get(&room_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        nodes.sort();
        nodes
    }

    /// The subscribe half: ask the room's home node to watch this room.
    ///
    /// Called when a local session is granted a room topic, which is the
    /// moment this node first has a reason to care. A room this node itself
    /// homes needs no ask — its sessions are served from here — and a room
    /// whose home node is not in the allow-list is left unmarked, so the next
    /// granted `SUBSCRIBE` retries the ask rather than the room silently
    /// staying local forever. Errors are the caller's to log: a refused
    /// subscription must not refuse the client's `SUBSCRIBE`, whose local
    /// half already succeeded.
    pub(crate) async fn subscribe_to(&self, room_id: Id, now: Timestamp) -> Result<()> {
        if self.subscribed.lock().contains(&room_id) {
            return Ok(());
        }
        let Some(room) = self.store.room(room_id).await? else {
            // No row, nothing to home: the authorization that reached here
            // already collapsed this to a refusal elsewhere.
            return Ok(());
        };
        if room.home_region == self.mesh.region() {
            // This node is the home node: the members' sessions are served
            // from this hub, and the watchers arrive as peers subscribe.
            return Ok(());
        }
        let Some(home) = self.home_node(&room.home_region).await? else {
            // No allowed peer answers the region the room names. Unmarked, so
            // a later SUBSCRIBE — perhaps after the operator admits the home
            // node — asks again.
            tracing::warn!(
                room = %room_id.to_text(),
                home = %room.home_region,
                "no allowed mesh peer homes this room; its events stay local"
            );
            return Ok(());
        };
        let routing = FedRouting {
            epoch: self.mesh.epoch(),
            home_region: room.home_region,
            room_id,
        };
        let payload = encode_envelope(Opcode::FedRoomSubscribe, &routing)?;
        self.mesh
            .enqueue(
                FederatedEvent {
                    target_node: home,
                    opcode: Opcode::FedRoomSubscribe.to_wire() as i32,
                    payload,
                },
                now,
            )
            .await?;
        self.subscribed.lock().insert(room_id);
        Ok(())
    }

    /// The watch half: record that `peer` holds subscribers of a room this
    /// node serves.
    ///
    /// Idempotent — a re-delivered subscribe (at least once, section 153) or
    /// a racing pair of `SUBSCRIBE`s inserts the same node twice and the set
    /// keeps one entry, which is the whole point of the tier: one copy per
    /// node, however many sessions on it care. The routing epoch must be
    /// current: a peer working from a stale view is told so, and the link
    /// teardown that carries the error back forces the re-handshake that
    /// refreshes it.
    pub(crate) fn register_watcher(&self, peer: Id, routing: &FedRouting) -> Result<()> {
        self.mesh.check_epoch(routing.epoch)?;
        self.watchers
            .lock()
            .entry(routing.room_id)
            .or_default()
            .insert(peer);
        Ok(())
    }

    /// The re-anchor: re-send every ask this process owes a room's home node.
    ///
    /// The watch table is memory on the home node's side, so a home node that
    /// restarts comes back holding an empty table while this node's
    /// `subscribed` set still says every ask was answered — and the tier is
    /// then down for every room whose members all sit here, silently, because
    /// no client `SUBSCRIBE` is coming to re-ask: the sessions are already
    /// granted. The composition root calls this on a timer
    /// (`federation.reanchor_interval_ms`) for exactly that gap.
    ///
    /// Each id is unmarked before its ask so `subscribe_to` runs its skips
    /// again rather than finding this cache in its own way — the same rules a
    /// fresh process applies, which is the point: the re-anchor is a fresh
    /// process's first second of life, repeated. A home node that never
    /// restarted simply re-inserts what it already holds
    /// ([`register_watcher`](Self::register_watcher) is idempotent, and its
    /// epoch check admits the ask: a restarted home node's fresh epoch is
    /// never ahead of this one's). An ask that fails outright re-marks the
    /// id so the next interval tries again rather than retiring the anchor;
    /// the skips that return `Ok` unmarked — no row, a home node not yet
    /// admitted — stay unmarked, exactly as a client-driven ask leaves them.
    pub(crate) async fn reanchor(&self, now: Timestamp) {
        // Bound to a `let` so the guard the take borrows dies at the semicolon:
        // a temporary in a `for` head would live for the whole loop, and a
        // parking-lot guard held across the ask's await is a future tokio
        // refuses to send between threads.
        let owed = std::mem::take(&mut *self.subscribed.lock());
        for room_id in owed {
            if let Err(error) = self.subscribe_to(room_id, now).await {
                tracing::warn!(
                    room = %room_id.to_text(),
                    %error,
                    "the room re-anchor failed; the next interval tries again"
                );
                self.subscribed.lock().insert(room_id);
            }
        }
    }

    /// The forward half: one federated copy of a room event per watching node.
    ///
    /// The event frame is encoded once and the same bytes are enqueued for
    /// every watcher — the outbox's per-link batching carries them in publish
    /// order — and a room with no watchers is a plain no-op, which is the
    /// common case on every node that is not the home node of the room.
    /// `exclude_device` is deliberately not honoured: the excluded socket is
    /// on *this* node, and the far node's sessions — including the actor's
    /// other devices — are the audience the tier exists to reach.
    ///
    /// The event reaches the watch table only if this node holds it, which is
    /// to say only if this node homes the room; anywhere else the copy goes to
    /// the home node instead. See [`forward_message`](Self::forward_message).
    pub(crate) async fn forward(&self, fanout: &RoomFanout, now: Timestamp) -> Result<()> {
        let opcode = fanout.opcode();
        let inner = match &fanout.event {
            RoomBroadcast::Member(event) => to_frame(opcode.to_wire(), 0, event),
            RoomBroadcast::State(event) => to_frame(opcode.to_wire(), 0, event),
            RoomBroadcast::Vote(event) => to_frame(opcode.to_wire(), 0, event),
        }
        .map_err(fault::from_wire)?;
        match self.store.room(fanout.room_id).await? {
            Some(room) => self.to_owner(&room, inner, now).await,
            // No row, so nothing names a home node. The event was published to
            // this node's hub regardless, which is the whole of what a room
            // with no row can be owed.
            None => Ok(()),
        }
    }

    /// The forward half's second producer: a room's *messages*.
    ///
    /// A room's chat is a conversation — the row the room names in
    /// `conversation_id` — so its fanout arrives as a messaging [`Fanout`],
    /// not a rooms one, and it used to stop at this node's own hub. That made
    /// a room's text reach the members whose sockets happen to be here and
    /// nobody else: with a store per node there is no row on the far node to
    /// sync, so the message was not late for the rest of the room, it was
    /// absent.
    ///
    /// Two hops, because the home node is the only node holding the watch
    /// table — the division [`to_owner`](Self::to_owner) carries out, and the
    /// same one `subscribe_to` already relies on. The room row is passed in
    /// rather than read here because the caller had to look it up anyway: it
    /// is `conversation_id` on the row that made the message a room's at all.
    ///
    /// # Errors
    ///
    /// Propagates an encode or outbox failure. The caller logs rather than
    /// fails: the local publish already happened, and refusing the send would
    /// only cost the sender their message without undoing anything.
    pub(crate) async fn forward_message(
        &self,
        room: &Room,
        fanout: &MessageFanout,
        now: Timestamp,
    ) -> Result<()> {
        let opcode = fanout.event.opcode();
        let inner = match &fanout.event {
            MessageBroadcast::Message(event) => to_frame(opcode.to_wire(), 0, event),
            MessageBroadcast::Receipt(event) => to_frame(opcode.to_wire(), 0, event),
            MessageBroadcast::Typing(event) => to_frame(opcode.to_wire(), 0, event),
            MessageBroadcast::Member(event) => to_frame(opcode.to_wire(), 0, event),
            MessageBroadcast::Vote(event) => to_frame(opcode.to_wire(), 0, event),
            MessageBroadcast::State(event) => to_frame(opcode.to_wire(), 0, event),
        }
        .map_err(fault::from_wire)?;
        self.to_owner(room, inner, now).await
    }

    /// The forward half's third producer: a group call's membership
    /// announcements, for the conversations a room owns.
    ///
    /// A room's conversation is the one the conversation tier refuses to
    /// watch — two envelopes would deliver every member two copies of
    /// everything — so its announcements ride the room's own envelope here,
    /// the same way its chat does. The conversation id is looked up rather
    /// than passed as a row because the caller cannot know which tier owns
    /// the conversation without asking this same question: a `None` row is
    /// the conversation tier's turn and a plain no-op here.
    ///
    /// The event is encoded whole and never read: the joiner's sealed offer
    /// rides inside it, the same mail-slot rule every sealed blob the tier
    /// carries keeps.
    ///
    /// # Errors
    ///
    /// Propagates an encode or outbox failure. The caller logs rather than
    /// fails, for the same reason `forward_message`'s caller does.
    pub(crate) async fn forward_call_event(
        &self,
        conversation_id: Id,
        event: &migo_protocol::CallStateEvent,
        now: Timestamp,
    ) -> Result<()> {
        match self.store.room_by_conversation(conversation_id).await? {
            Some(room) => {
                let inner =
                    to_frame(Opcode::CallSfuEvent.to_wire(), 0, event).map_err(fault::from_wire)?;
                self.to_owner(&room, inner, now).await
            }
            // Not a room's conversation: the conversation tier carries it,
            // and this call is the cheaper half of asking both.
            None => Ok(()),
        }
    }

    /// One copy of an *ingested* event to every watching node but the one it
    /// arrived from.
    ///
    /// The home node is the tier's only fan-out authority, so a room event a
    /// peer forwarded here is owed onward to the other watchers. The origin is
    /// excluded because it has already published the event to its own hub:
    /// sending it back would deliver every local subscriber the same frame
    /// twice, and the second copy is indistinguishable from a real one.
    ///
    /// The bytes are re-sealed as-is rather than re-encoded from the arriving
    /// frame, so what the receiving node's clients see is byte-for-byte what
    /// the origin's clients saw (section 145).
    pub(crate) async fn fan_out_inbound(
        &self,
        room_id: Id,
        origin: Id,
        payload: &[u8],
        now: Timestamp,
    ) -> Result<()> {
        let inner = Frame::decode(Bytes::copy_from_slice(payload)).map_err(fault::from_wire)?;
        self.fan_out(room_id, inner, Some(origin), now).await
    }

    /// Whether this node homes the room, and so owns its fan-out.
    ///
    /// The ingest path's question. A `FED_ROOM_EVENT` arrives at a node
    /// because some node put it there, and the two reasons are opposite: the
    /// home node holds the watch table and owes the event onward, while a
    /// watching node is the end of the line and owes nothing. Only the store
    /// can tell them apart, and the row is read per event rather than cached
    /// because a room's home can be moved by an operator and a cache here
    /// would keep fanning a moved room out from the node that no longer owns
    /// it.
    pub(crate) async fn homes(&self, room_id: Id) -> bool {
        matches!(
            self.store.room(room_id).await,
            Ok(Some(room)) if room.home_region == self.mesh.region()
        )
    }

    /// Whether a room's writes may proceed on this node — the read-only half of
    /// section 173's scenario 2.
    ///
    /// The sequencer rule of section 170 is why this gate exists: one sequencer
    /// per room, at the home node, and a second one must never be appointed,
    /// because two sequencers mean two orders that cannot be merged without
    /// losing messages. So when this node does *not* home the room and the home
    /// node has proven unreachable — a delivery attempt that could not connect —
    /// a write here could only be ordered by a sequencer that must not exist.
    /// The write is refused with
    /// [`room_read_only_partition`](migo_protocol::fault::room_read_only_partition)
    /// instead: the room is read-only on this node for as long as the partition
    /// stands, and the answer is retryable, because a link that heals — a
    /// delivered batch or an inbound handshake — makes the very same write
    /// succeed.
    ///
    /// Everything else stays writable, by the same rule read the other way. A
    /// room this node homes keeps its single sequencer here and writes
    /// freely — that is the sequencer that exists, not a second one. A room
    /// whose home region no allowed peer answers is a configuration gap, not a
    /// partition, and refusing on it would break deployments where the row
    /// arrived without the peer; it warns on the publish path as before. And a
    /// link never tried reads as reachable, because the mesh remembers
    /// evidence, it does not guess.
    ///
    /// # Errors
    ///
    /// [`room_read_only_partition`](migo_protocol::fault::room_read_only_partition)
    /// — and nothing else — when the room is homed elsewhere and that home node
    /// is marked down. Store failures propagate.
    pub async fn ensure_writable(&self, room_id: Id) -> Result<()> {
        match self.store.room(room_id).await? {
            Some(room) => self.room_writable(&room).await,
            // No row, so nothing names a home node: the write is this node's
            // alone and there is no second sequencer to refuse.
            None => Ok(()),
        }
    }

    /// The same gate for a conversation: a room's chat is its conversation, so
    /// a message write names a conversation, not a room.
    ///
    /// The store tells the two kinds of conversation apart: a room's resolves
    /// to its row and is gated by where that room is homed; a direct or group
    /// conversation resolves to nothing and passes. It passes even though the
    /// conversation now has a home node for fan-out (section 170's
    /// conversation tier), because that home node is a fan-out authority and
    /// not a sequencer — a private message never needed the far node's order
    /// (section 173), so a partition makes its delivery wait, never its
    /// write.
    pub async fn ensure_conversation_writable(&self, conversation_id: Id) -> Result<()> {
        match self.store.room_by_conversation(conversation_id).await? {
            Some(room) => self.room_writable(&room).await,
            None => Ok(()),
        }
    }

    /// The gate's decision for one already-read room row.
    async fn room_writable(&self, room: &Room) -> Result<()> {
        if room.home_region == self.mesh.region() {
            // This node is the home node: the sequencer that exists is here.
            return Ok(());
        }
        let Some(home) = self.home_node(&room.home_region).await? else {
            // A configuration gap, not a partition: no peer to have proven
            // unreachable. The publish path already warns; refusing here would
            // break shared-store deployments where the row arrived without the
            // peer ever being admitted.
            return Ok(());
        };
        if self.mesh.link_reachable(home) {
            return Ok(());
        }
        tracing::warn!(
            room = %room.room_id.to_text(),
            home = %room.home_region,
            "room is read-only on this node: its home node is unreachable, so a write here would need a second sequencer"
        );
        Err(fault::room_read_only_partition())
    }

    /// Hands an event to the node that owns its fan-out: this one, or the home
    /// node.
    ///
    /// The tier's division of labour in one place. A node that homes the room
    /// holds the watch table, so it fans out directly — one copy per watching
    /// node. A node that does not home it holds no table and cannot know who
    /// is watching, so it owes the home node exactly one copy and lets the
    /// table do the tiering. Routing a non-home node's publish anywhere else
    /// would either drop it or duplicate it.
    async fn to_owner(&self, room: &Room, inner: Frame, now: Timestamp) -> Result<()> {
        if room.home_region == self.mesh.region() {
            return self.fan_out(room.room_id, inner, None, now).await;
        }
        self.send_to_home(room.room_id, room.home_region.as_str(), inner, now)
            .await
    }

    /// Moves a room to another node, telling every member where to reconnect.
    ///
    /// The placement primitive the missing half of section 173 needed: a room
    /// whose home node changes must not strand its members on the old one. The
    /// order is the drain order section 170 names — tell the members
    /// first, then move the row, then raise the epoch — and each step is the
    /// one that makes the next safe:
    ///
    /// 1. every member is handed a `RECONNECT_HINT` naming the new home's
    ///    endpoint, `Rebalance` the reason and `after_ms` zero, because the
    ///    instruction is "reconnect now, there" — the local half on this
    ///    node's hub for the sessions already here, the federated half as one
    ///    `FED_ROOM_EVENT` per watching node, riding the same tier every room
    ///    event rides;
    /// 2. the store row is rehomed, which is the fact every publish path reads
    ///    to decide who tiers the room — after this, this node's own `homes()`
    ///    answers false and the new node's answers true;
    /// 3. the routing epoch is bumped, so a peer still routing on the old view
    ///    is refused — and, because the transport now refreshes on that
    ///    refusal, converges on the new view rather than waiting for an
    ///    operator's `bump_epoch`.
    ///
    /// The hint is emitted before the row moves, so a failure partway through
    /// costs a premature hint — a member reconnects to the node that is about
    /// to own the room — rather than the alternative, a moved room whose
    /// members were never told. The watch table keeps its entries: they are
    /// read only through `homes()`, which the moved row has already answered
    /// false, so a stale entry is inert until the room's subscribers
    /// re-subscribe and the new home node builds its own table.
    ///
    /// # Errors
    ///
    /// [`not_found`](migo_protocol::fault::not_found) if the room does not
    /// exist; [`conflict`](migo_protocol::fault::conflict) if this node is not
    /// the room's home — only the node holding the watch table knows who must
    /// be told, so only it may move the room;
    /// [`validation`](migo_protocol::fault::validation) if no allowed mesh peer
    /// answers the target region, because a hint with no endpoint in it would
    /// strand the members it means to rescue.
    pub async fn move_room(
        &self,
        room_id: Id,
        new_home_region: &str,
        now: Timestamp,
    ) -> Result<Room> {
        let Some(room) = self.store.room(room_id).await? else {
            return Err(fault::not_found("room"));
        };
        if room.home_region != self.mesh.region() {
            return Err(fault::conflict("only the room's home node may move it"));
        }
        if room.home_region == new_home_region {
            // Already homed there: no hint, no write, no epoch. A move that
            // changes nothing is not a move, and bumping the epoch for it
            // would tell every peer its view is stale when nothing moved.
            return Ok(room);
        }
        let Some(home) = self.home_peer(new_home_region).await? else {
            return Err(fault::validation(
                "home_region",
                "no allowed mesh peer homes the target region",
            ));
        };

        // 1. The hint, one frame for both halves: the local hub gets the same
        //    bytes the far nodes' members receive, sealed as a session would.
        let hint = ReconnectHint {
            reason: CloseReason::Rebalance,
            after_ms: 0,
            endpoint: Some(home.base_url.clone()),
        };
        let inner =
            to_frame(Opcode::ReconnectHint.to_wire(), 0, &hint).map_err(fault::from_wire)?;
        let bytes = inner.encode().map_err(fault::from_wire)?;
        if let Some(hints) = &self.hints {
            hints.publish_hint(room_id, &bytes, now);
        }
        self.fan_out(room_id, inner, None, now).await?;

        // 2. The row moves, and with it the fan-out authority.
        let moved = self
            .store
            .rehome_room(room_id, new_home_region, now)
            .await?;
        // 3. The epoch rises, and every peer holding the old view is told so.
        let epoch = self.mesh.bump_epoch();
        tracing::info!(
            room = %room_id.to_text(),
            from = %room.home_region,
            to = %new_home_region,
            endpoint = %home.base_url,
            epoch,
            "a room moved to another node; its members were told to reconnect there"
        );
        Ok(moved)
    }

    /// The fan-out itself: encode once, enqueue one copy per watching node.
    ///
    /// A room with no watchers is a plain no-op, which is the common case on
    /// every node that is not the room's home node.
    async fn fan_out(
        &self,
        room_id: Id,
        inner: Frame,
        exclude: Option<Id>,
        now: Timestamp,
    ) -> Result<()> {
        let targets = self
            .watchers
            .lock()
            .get(&room_id)
            .cloned()
            .unwrap_or_default();
        if targets.is_empty() {
            return Ok(());
        }
        let envelope = FedRoomEvent {
            room_id,
            payload: inner.encode().map_err(fault::from_wire)?.to_vec(),
        };
        let payload = encode_envelope(Opcode::FedRoomEvent, &envelope)?;
        for node in targets {
            if Some(node) == exclude {
                continue;
            }
            self.mesh
                .enqueue(
                    FederatedEvent {
                        target_node: node,
                        opcode: Opcode::FedRoomEvent.to_wire() as i32,
                        payload: payload.clone(),
                    },
                    now,
                )
                .await?;
        }
        Ok(())
    }

    /// Enqueues one copy of an event for the node that homes it.
    ///
    /// Used by a node that does not home the room and so holds no watch table:
    /// the home node is the only node that can tier the event, and one copy to
    /// it is what makes a room's traffic cross node boundaries at all.
    async fn send_to_home(
        &self,
        room_id: Id,
        home_region: &str,
        inner: Frame,
        now: Timestamp,
    ) -> Result<()> {
        let Some(home) = self.home_node(home_region).await? else {
            tracing::warn!(
                room = %room_id.to_text(),
                home = %home_region,
                "no allowed mesh peer homes this room; its events stay local"
            );
            return Ok(());
        };
        let envelope = FedRoomEvent {
            room_id,
            payload: inner.encode().map_err(fault::from_wire)?.to_vec(),
        };
        let payload = encode_envelope(Opcode::FedRoomEvent, &envelope)?;
        // The pending handle is the outbox's business, not this tier's: what
        // the caller needs to know is whether the copy is queued.
        self.mesh
            .enqueue(
                FederatedEvent {
                    target_node: home,
                    opcode: Opcode::FedRoomEvent.to_wire() as i32,
                    payload,
                },
                now,
            )
            .await
            .map(|_queued| ())
    }

    /// The federating peer that homes a region, if the allow-list names one.
    ///
    /// A degraded peer still counts: degraded is a signal about the link's health, not a
    /// suspension, so a room's events keep flowing to a slow home node exactly as to a
    /// fast one (section 153). Only the operator's paused and blocked are excluded.
    async fn home_node(&self, home_region: &str) -> Result<Option<Id>> {
        Ok(self.home_peer(home_region).await?.map(|peer| peer.node_id))
    }

    /// The allowed peer that homes a region, with the endpoint its allow-list
    /// entry names.
    ///
    /// The move path needs the whole view — a reconnect hint without the new
    /// home's address is an instruction with no destination — while the
    /// subscribe and forward paths need only the id, so both read this one
    /// scan and take what they need.
    async fn home_peer(&self, home_region: &str) -> Result<Option<PeerView>> {
        Ok(self
            .mesh
            .peers(PEER_SCAN_LIMIT)
            .await?
            .into_iter()
            .find(|peer| peer.region == home_region && peer.status.is_allowed()))
    }
}

/// Frames one wire struct as the encoded inner frame an outbox event carries.
///
/// The outbox's payload is a whole encoded MWP frame (the transport wraps it
/// in a `FED_FORWARD` without opening it), so both halves build theirs the
/// same way and the shape lives in one place. Shared with the conversation
/// relay, whose envelopes are built the same way around a different inner
/// event.
pub(crate) fn encode_envelope<T: Encode>(opcode: Opcode, value: &T) -> Result<Vec<u8>> {
    let frame: Frame = to_frame(opcode.to_wire(), 0, value).map_err(fault::from_wire)?;
    frame
        .encode()
        .map_err(fault::from_wire)
        .map(|bytes| bytes.to_vec())
}

/// The local half of a rebalance hint: the same frame the far nodes' members
/// receive, published to this node's own hub for the members whose sockets are
/// here.
///
/// A port rather than the gateway itself, for the same reason the room
/// publisher port in `room_presence` is one: the move's
/// logic — who is told, in which order, with what endpoint — is exercised by
/// handing it a recorder and reading the frame back, with no hub and no
/// runtime behind it. The frame is the whole interface, raw bytes and nothing
/// decoded, because the far nodes' sessions receive exactly these bytes and
/// the local half must not be a re-encoding that could drift from them.
pub trait HintPublisher: Send + Sync {
    /// Publishes one reconnect hint frame to a room's topic.
    fn publish_hint(&self, room_id: Id, frame: &Bytes, now: Timestamp);
}

/// The production [`HintPublisher`]: the gateway's hub, once the gateway exists.
pub struct GatewayHintPublisher {
    gateway: Arc<crate::room_presence::GatewayHandle>,
}

impl GatewayHintPublisher {
    /// Wraps the late-bound gateway handle — the same one the dispatcher's
    /// out-of-band publishes ride, filled the moment the gateway opens.
    pub fn new(gateway: Arc<crate::room_presence::GatewayHandle>) -> Self {
        Self { gateway }
    }

    /// The topic every room event fans out to.
    fn room_topic(room_id: Id) -> Topic {
        Topic {
            kind: TopicKind::Room,
            id: room_id,
        }
    }
}

impl HintPublisher for GatewayHintPublisher {
    fn publish_hint(&self, room_id: Id, frame: &Bytes, now: Timestamp) {
        if let Some(gateway) = self.gateway.get() {
            // Never coalesced: a hint is an instruction, and the survivor of two
            // collapsed instructions would send its members to whichever node
            // the second move named — which may be the one they just left.
            gateway.broadcast_frame_to_topic(
                &Self::room_topic(room_id),
                Opcode::ReconnectHint,
                frame,
                None,
                now,
            );
        }
    }
}

/// The out-of-band room publisher with the federated half attached.
///
/// [`RoomPublisher`](crate::room_presence::RoomPublisher) is sync because the
/// gateway's hub publish is — but the outbox enqueue it now also owes is a
/// durable write, which is async. So the local publish happens inline, on the
/// caller's thread, exactly as before, and the federated copy is spawned: it
/// is at least once by design, so a moment's delay between the two halves
/// costs ordering headroom the per-link sequence already preserves, and a
/// failure to enqueue is logged rather than swallowed by the connection edge
/// that triggered it.
pub(crate) struct FederatedPublisher {
    inner: Arc<dyn RoomPublisher>,
    relay: Arc<RoomRelay>,
}

impl FederatedPublisher {
    /// Wraps the local publisher and the relay that owes the federated copy.
    pub(crate) fn new(inner: Arc<dyn RoomPublisher>, relay: Arc<RoomRelay>) -> Self {
        Self { inner, relay }
    }

    /// Local publish first, federated copy after.
    fn forward(&self, room_id: Id, event: RoomBroadcast, now: Timestamp) {
        let relay = Arc::clone(&self.relay);
        let fanout = RoomFanout {
            room_id,
            exclude_device: None,
            event,
        };
        tokio::spawn(async move {
            if let Err(error) = relay.forward(&fanout, now).await {
                tracing::warn!(
                    %error,
                    room = %fanout.room_id.to_text(),
                    "cannot enqueue the federated half of a room fanout"
                );
            }
        });
    }
}

impl RoomPublisher for FederatedPublisher {
    fn publish_member(&self, room_id: Id, event: &RoomMemberEvent, now: Timestamp) {
        self.inner.publish_member(room_id, event, now);
        self.forward(room_id, RoomBroadcast::Member(event.clone()), now);
    }

    fn publish_state(&self, room_id: Id, event: &RoomStateEvent, now: Timestamp) {
        self.inner.publish_state(room_id, event, now);
        self.forward(room_id, RoomBroadcast::State(event.clone()), now);
    }

    fn publish_vote(&self, room_id: Id, event: &RoomVoteEvent, now: Timestamp) {
        self.inner.publish_vote(room_id, event, now);
        self.forward(room_id, RoomBroadcast::Vote(event.clone()), now);
    }
}

#[cfg(test)]
mod tests {
    //! A relay over a real mesh service and a memory store: the subscribe
    //! half's home-node resolution and once-per-room promise, and the watch
    //! half's idempotence and epoch honesty, with no wire at all — the mesh
    //! tests in `mesh.rs` carry the two-node paths.

    use super::*;
    use migo_core::random::SeededRandom;
    use migo_crypto::NodeSecret;
    use migo_federation::{MeshService, NewPeerSpec};
    use migo_protocol::{EncryptionMode, RoomKind};
    use migo_store::model::NewRoom;
    use migo_store::MemoryStore;

    const NOW: i64 = 1_700_000_000_000;

    /// A mesh whose region is `region`, with `peer` admitted at `peer_region`.
    async fn mesh_in(region: &str, peer: Id, peer_region: &str) -> SharedMesh {
        let registry = migo_core::metrics::Registry::new();
        let mut seed = [0u8; 32];
        seed[..region.len()].copy_from_slice(region.as_bytes());
        let secret = NodeSecret::from_seed(&seed).expect("a 32-byte seed builds a key");
        let mesh = MeshService::new(
            Arc::new(MemoryStore::new()),
            migo_federation::MeshConfig::default(),
            Id::from(0xABCD),
            region.to_string(),
            secret,
            Box::new(SeededRandom::new(42)),
            &registry,
        )
        .expect("the mesh configuration is valid");
        let mesh: SharedMesh = Arc::new(mesh);
        mesh.add_peer(
            NewPeerSpec {
                node_id: peer,
                public_key: NodeSecret::from_seed(&[9u8; 32])
                    .expect("a seed builds a key")
                    .public()
                    .to_bytes()
                    .to_vec(),
                base_url: "wss://peer.test:9999".to_string(),
                region: peer_region.to_string(),
            },
            Timestamp::from_millis(NOW),
        )
        .await
        .expect("a fresh allow-list admits the peer");
        mesh
    }

    /// A store holding one room homed at `home_region`.
    async fn store_with_room(room_id: Id, home_region: &str) -> SharedStore {
        let store = MemoryStore::new();
        let store: SharedStore = Arc::new(store);
        store
            .create_room(NewRoom {
                room_id,
                conversation_id: Id::from(0x2222),
                slug: format!("room-{home_region}"),
                name: "a room".to_string(),
                topic: None,
                kind: RoomKind::Public,
                owner_id: Id::from(0x3333),
                home_region: home_region.to_string(),
                max_members: 100,
                encryption: EncryptionMode::Transport,
                created_at: Timestamp::from_millis(NOW),
            })
            .await
            .expect("a fresh store creates the room");
        store
    }

    /// A room this node homes needs no ask: no event is enqueued, and the
    /// room is not marked, because there is no home node to tell.
    #[tokio::test]
    async fn a_room_this_node_homes_is_never_subscribed_away() {
        let mesh = mesh_in("region-1", Id::from(0x4444), "region-2").await;
        let room_id = Id::from(0x1111);
        let store = store_with_room(room_id, "region-1").await;
        let relay = RoomRelay::new(Arc::clone(&mesh), store, None);

        relay
            .subscribe_to(room_id, Timestamp::from_millis(NOW))
            .await
            .expect("the home node needs no ask");
        assert!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .is_empty(),
            "nothing was enqueued for a room this node homes"
        );
    }

    /// A room homed elsewhere is asked for once: the second call finds the
    /// room marked and enqueues nothing.
    #[tokio::test]
    async fn a_remote_room_is_subscribed_once_per_process() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let room_id = Id::from(0x1111);
        let store = store_with_room(room_id, "region-2").await;
        let relay = RoomRelay::new(Arc::clone(&mesh), store, None);

        for _ in 0..2 {
            relay
                .subscribe_to(room_id, Timestamp::from_millis(NOW))
                .await
                .expect("the home node resolves and the ask enqueues");
        }
        let due = mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads");
        assert_eq!(due.len(), 1, "one ask, not one per subscribing session");
        assert_eq!(due[0].target_node, peer, "the ask names the home node");
    }

    /// A room whose home region no allowed peer answers is left unmarked, so
    /// a later ask — once the operator admits the home node — can happen.
    #[tokio::test]
    async fn a_room_without_a_home_peer_stays_askable() {
        let mesh = mesh_in("region-1", Id::from(0x4444), "region-2").await;
        let room_id = Id::from(0x1111);
        let store = store_with_room(room_id, "region-3").await;
        let relay = RoomRelay::new(Arc::clone(&mesh), store, None);

        relay
            .subscribe_to(room_id, Timestamp::from_millis(NOW))
            .await
            .expect("an unanswerable region is a warning, not an error");
        assert!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .is_empty(),
            "no peer to ask, so nothing was enqueued"
        );
    }

    /// A watch is idempotent and a stale routing epoch is refused: the peer
    /// must re-handshake onto the current view before it may watch.
    #[tokio::test]
    async fn a_watch_is_idempotent_and_a_stale_epoch_is_refused() {
        let mesh = mesh_in("region-1", Id::from(0x4444), "region-2").await;
        let store = Arc::new(MemoryStore::new()) as SharedStore;
        let relay = RoomRelay::new(Arc::clone(&mesh), store, None);
        let room_id = Id::from(0x1111);
        let peer = Id::from(0x5555);
        let routing = FedRouting {
            epoch: mesh.epoch(),
            home_region: "region-1".to_string(),
            room_id,
        };

        relay
            .register_watcher(peer, &routing)
            .expect("a current epoch admits the watch");
        relay
            .register_watcher(peer, &routing)
            .expect("a re-delivered subscribe is idempotent");
        assert_eq!(
            relay.watchers_of(room_id),
            vec![peer],
            "one entry per node, not per subscribe"
        );

        mesh.bump_epoch();
        let stale = FedRouting { ..routing };
        assert!(
            relay.register_watcher(peer, &stale).is_err(),
            "a peer on a stale routing view is told so"
        );
    }

    /// The forward half on a node that is *not* the room's home: one copy to the
    /// home node, because the watch table is there and this node cannot tier.
    ///
    /// This is the difference between a room whose events reach its members and
    /// one whose events reach whoever shares a node with the actor. A node with
    /// no watchers of its own has nothing to fan out to, so routing the publish
    /// through its own — empty — table would drop it in silence.
    #[tokio::test]
    async fn a_publish_on_a_non_home_node_goes_to_the_home_node_alone() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let room_id = Id::from(0x1111);
        let store = store_with_room(room_id, "region-2").await;
        let relay = RoomRelay::new(Arc::clone(&mesh), store, None);

        let member = RoomMemberEvent {
            room_id,
            user_id: Id::from(0x8888),
            joined: true,
            role: None,
            member_count: Some(2),
            change: None,
            revision: None,
        };
        relay
            .forward(
                &RoomFanout {
                    room_id,
                    exclude_device: None,
                    event: RoomBroadcast::Member(member),
                },
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("the home node resolves and the event enqueues");

        let due = mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads");
        assert_eq!(due.len(), 1, "one copy, for the only node that can tier it");
        assert_eq!(due[0].target_node, peer, "and it is the room's home node");

        let outer = Frame::decode(Bytes::from(due[0].payload.clone()))
            .expect("an outbox payload is an encoded frame");
        assert_eq!(
            Opcode::from_wire(outer.header.opcode),
            Some(Opcode::FedRoomEvent),
            "the copy is a room event envelope"
        );
        let envelope: FedRoomEvent =
            migo_protocol::from_frame(&outer).expect("the envelope decodes");
        assert_eq!(
            envelope.room_id, room_id,
            "and it names the room it speaks for, which is what the home node tiers by"
        );
        let inner = Frame::decode(Bytes::from(envelope.payload)).expect("the inner frame decodes");
        assert_eq!(
            Opcode::from_wire(inner.header.opcode),
            Some(Opcode::RoomMemberEvent),
            "carrying the member event itself, sealed as a local subscriber would have seen it"
        );
    }

    /// A hint recorder: what the local half of a move would have published, held
    /// for the test to decode — the same shape of stand-in `Recorder` is for
    /// [`RoomPublisher`](crate::room_presence::RoomPublisher).
    struct RecordedHints(parking_lot::Mutex<Vec<(Id, Bytes)>>);

    impl HintPublisher for RecordedHints {
        fn publish_hint(&self, room_id: Id, frame: &Bytes, _now: Timestamp) {
            self.0.lock().push((room_id, frame.clone()));
        }
    }

    /// A moved room tells every member where to reconnect, before anything else
    /// moves: one hint locally, one `FED_ROOM_EVENT` per watching node, then the
    /// rehomed row and the raised epoch — the drain order section 170 names.
    #[tokio::test]
    async fn a_moved_room_hints_its_members_where_to_reconnect() {
        let peer = Id::from(0x4444);
        // `mesh_in` admits the peer at region-2 with base_url wss://peer.test:9999,
        // which is the endpoint the hint must name.
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let room_id = Id::from(0x1111);
        let store = store_with_room(room_id, "region-1").await;
        let hints = Arc::new(RecordedHints(parking_lot::Mutex::new(Vec::new())));
        let relay = RoomRelay::new(
            Arc::clone(&mesh),
            store.clone(),
            Some(hints.clone() as Arc<dyn HintPublisher>),
        );
        let before = mesh.epoch();
        let routing = FedRouting {
            epoch: before,
            home_region: "region-1".to_string(),
            room_id,
        };
        relay
            .register_watcher(peer, &routing)
            .expect("a current epoch admits the watch");

        let moved = relay
            .move_room(room_id, "region-2", Timestamp::from_millis(NOW))
            .await
            .expect("the room moves to the peer's region");
        assert_eq!(
            moved.home_region, "region-2",
            "the row names the new home node"
        );

        // The local half: one hint on the room's topic, the frame a session would
        // have received.
        let recorded = hints.0.lock().clone();
        assert_eq!(recorded.len(), 1, "one hint for the room's own sessions");
        assert_eq!(recorded[0].0, room_id);
        let frame = Frame::decode(recorded[0].1.clone()).expect("the hint is an encoded frame");
        assert_eq!(
            Opcode::from_wire(frame.header.opcode),
            Some(Opcode::ReconnectHint),
            "the frame is a reconnect hint"
        );
        let hint: ReconnectHint = migo_protocol::from_frame(&frame).expect("the hint decodes");
        assert_eq!(hint.reason, CloseReason::Rebalance);
        assert_eq!(hint.after_ms, 0, "the instruction is to reconnect now");
        assert_eq!(
            hint.endpoint.as_deref(),
            Some("wss://peer.test:9999"),
            "the hint names the new home node's endpoint, read off the allow-list"
        );

        // The federated half: one FED_ROOM_EVENT for the watching node, carrying
        // the same hint sealed inside the envelope.
        let due = mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads");
        assert_eq!(
            due.len(),
            1,
            "one federated copy, for the node that holds the other members"
        );
        assert_eq!(due[0].target_node, peer);
        let outer = Frame::decode(Bytes::from(due[0].payload.clone()))
            .expect("an outbox payload is an encoded frame");
        let envelope: FedRoomEvent =
            migo_protocol::from_frame(&outer).expect("the envelope decodes");
        let inner = Frame::decode(Bytes::from(envelope.payload)).expect("the inner frame decodes");
        assert_eq!(
            Opcode::from_wire(inner.header.opcode),
            Some(Opcode::ReconnectHint),
            "the watching node's members receive the hint as a room event"
        );

        // And the two facts that make the move real: the row moved, the epoch rose.
        let row = store.room(room_id).await.expect("the store reads").unwrap();
        assert_eq!(row.home_region, "region-2");
        assert_eq!(
            mesh.epoch(),
            before + 1,
            "the routing epoch rose with the move, so stale views are refused"
        );

        // The move is real for the routing too: a post-move publish goes to the new
        // home node — the row says so — never to the watch table this node no longer
        // owns.
        relay
            .forward(
                &RoomFanout {
                    room_id,
                    exclude_device: None,
                    event: RoomBroadcast::Member(RoomMemberEvent {
                        room_id,
                        user_id: Id::from(0x8888),
                        joined: true,
                        role: None,
                        member_count: Some(2),
                        change: None,
                        revision: None,
                    }),
                },
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("the post-move publish forwards");
        let owed = mesh
            .due(Timestamp::from_millis(NOW + 120_000))
            .await
            .expect("the queue reads");
        assert_eq!(
            owed.len(),
            2,
            "the hint and the post-move member event, and nothing else"
        );
        assert!(
            owed.iter().all(|event| event.target_node == peer),
            "everything the moved room owes routes to the new home node"
        );
    }

    /// Only the home node may move a room — it holds the watch table, so it is
    /// the only node that knows who must be told — and a room that does not
    /// exist is the store's own refusal, not the mesh's.
    #[tokio::test]
    async fn a_room_is_moved_only_by_its_home_node() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let room_id = Id::from(0x1111);
        // Homed at region-2, so this region-1 node is a watcher, not the home.
        let store = store_with_room(room_id, "region-2").await;
        let relay = RoomRelay::new(Arc::clone(&mesh), store, None);

        let error = relay
            .move_room(room_id, "region-1", Timestamp::from_millis(NOW))
            .await
            .expect_err("a node that does not home the room cannot move it");
        assert_eq!(
            error.code(),
            migo_protocol::codes::CONFLICT,
            "the refusal is a conflict, not a fault of this node"
        );
        let missing = relay
            .move_room(Id::from(0x9999), "region-1", Timestamp::from_millis(NOW))
            .await
            .expect_err("a room that does not exist cannot be moved");
        assert_eq!(missing.code(), migo_protocol::codes::NOT_FOUND);
    }

    /// A move that changes nothing is not a move, and a move to a region no
    /// allowed peer answers is refused before any hint or write happens — a
    /// hint with no endpoint in it would strand the members it means to rescue.
    #[tokio::test]
    async fn a_move_nowhere_changes_nothing() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let room_id = Id::from(0x1111);
        let store = store_with_room(room_id, "region-1").await;
        let hints = Arc::new(RecordedHints(parking_lot::Mutex::new(Vec::new())));
        let relay = RoomRelay::new(
            Arc::clone(&mesh),
            store.clone(),
            Some(hints.clone() as Arc<dyn HintPublisher>),
        );
        let before = mesh.epoch();

        let same = relay
            .move_room(room_id, "region-1", Timestamp::from_millis(NOW))
            .await
            .expect("a move to the room's own home is a no-op, not an error");
        assert_eq!(same.home_region, "region-1");
        assert_eq!(
            mesh.epoch(),
            before,
            "an unchanged placement tells no peer its view is stale"
        );
        assert!(hints.0.lock().is_empty(), "nobody is told anything");

        let error = relay
            .move_room(room_id, "region-3", Timestamp::from_millis(NOW))
            .await
            .expect_err("no allowed peer answers region-3");
        assert_eq!(error.code(), migo_protocol::codes::VALIDATION_FAILED);
        assert!(
            hints.0.lock().is_empty(),
            "no hint was published for a move that could not happen"
        );
        let row = store.room(room_id).await.expect("the store reads").unwrap();
        assert_eq!(
            row.home_region, "region-1",
            "the refused move left the row exactly where it was"
        );
        assert_eq!(mesh.epoch(), before, "and the epoch unmoved");
    }
}
