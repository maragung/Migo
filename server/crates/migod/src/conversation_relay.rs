//! Tiered conversation fanout across nodes (brief sections 170, 169).
//!
//! A room is not the only conversation whose members sit on more than one
//! node: a direct chat or a group whose participants connected to different
//! nodes has exactly the same problem section 170 solved for rooms — a
//! publish that stops at this node's hub is not late for the far members, it
//! is absent, because every node keeps its own store and there is no row
//! there to sync from. This module is the room relay's shape applied to the
//! conversations a room does *not* own:
//!
//! - the **subscribe half**: when a local session is granted a conversation
//!   topic (`authorize_topics`, the same gate every `SUBSCRIBE` already
//!   passes), the node asks the conversation's home node to watch it — once
//!   per conversation, for the life of the process — with a
//!   `FED_CONVERSATION_SUBSCRIBE` carrying the routing epoch and the home
//!   region the conversation row names;
//! - the **watch half**: the home node records which peer nodes hold
//!   subscribers of which conversations, so a publish becomes one federated
//!   copy per *node*, not per session and not per member;
//! - the **forward half**: every publish path a direct or group conversation
//!   has — send, edit, receipt, typing, invite, leave, vote, state — funnels
//!   through `publish_messaging`, which hands the fanout here when the store
//!   says the conversation is not a room's, and the call plane's group-call
//!   membership announcements reach the same half through
//!   [`forward_call_event`](Self::forward_call_event). The node that homes the
//!   conversation enqueues one `FED_CONVERSATION_EVENT` per watching node, the
//!   inner event frame sealed exactly as a local session would have received
//!   it; the receiving node's ingest path (`mesh::route_conversation_event`)
//!   publishes it into its own hub and passes it on in turn if the watch
//!   table is there.
//!
//! The home node here is *not* a sequencer. Section 170 says a private
//! message needs no global order — each node's store numbers its own sends,
//! and the seq a message carries is the sending node's, which is why
//! `ensure_conversation_writable` still passes a conversation no room owns:
//! there is no single order to duplicate, so a partition never makes a direct
//! chat read-only. The home node is the fan-out authority and nothing else,
//! which is the whole of what the tier asks of it.
//!
//! # The sealed envelope
//!
//! The relay never opens what it carries. A direct conversation's messages
//! are sealed client-side (section 170's standing rule), and the frame this
//! module forwards is the *encoded* event — envelope bytes included, read by
//! nobody on the way through, including the home node. The intermediate node
//! sees a conversation id and an opaque payload, which is everything it needs
//! to route and nothing it could read.
//!
//! # Ordering, redelivery, and the missing unsubscribe
//!
//! The same contract the room half rides: per-link sequence numbers are
//! strictly increasing, delivery is at least once, and a re-delivered event
//! is re-published into a hub whose consumers are idempotent (section 153).
//! There is no `FED_CONVERSATION_UNSUBSCRIBE` (the IDL is frozen), so a
//! watcher set only grows and a copy that arrives with no local subscriber is
//! a cheap no-op — the same honest ceiling the room half lives under.

use std::collections::{HashMap, HashSet};

use bytes::Bytes;
use migo_core::{Id, Result, Timestamp};
use migo_federation::model::{FederatedEvent, PeerView};
use migo_federation::SharedMesh;
use migo_messaging::{Broadcast as MessageBroadcast, Fanout as MessageFanout};
use migo_protocol::{fault, to_frame, FedConversationEvent, FedConversationRouting, Frame, Opcode};
use migo_store::model::Conversation;
use migo_store::SharedStore;

use crate::room_relay::encode_envelope;

/// How many allow-list rows one home-node lookup reads. The page clamp the
/// directory answers with, reused because a home-region scan is the same
/// bounded read — and the same number the room half reads, so the two tiers
/// cost one page of the allow-list each.
const PEER_SCAN_LIMIT: u16 = 256;

/// Which peer nodes hold subscribers of which conversations, and the
/// subscribe half that keeps the table honest from the other side.
pub struct ConversationRelay {
    mesh: SharedMesh,
    store: SharedStore,
    /// The watch table, home-node side: conversation → the peer nodes
    /// watching it.
    watchers: parking_lot::Mutex<HashMap<Id, HashSet<Id>>>,
    /// The conversations this process has already asked a home node to watch.
    /// One `FED_CONVERSATION_SUBSCRIBE` per conversation per process; a
    /// second would only be re-registered on the other side (the insert is
    /// idempotent), but the outbox owes the peer one copy, not one per
    /// subscribing session.
    subscribed: parking_lot::Mutex<HashSet<Id>>,
}

impl ConversationRelay {
    /// Wraps the mesh and the store the two halves read.
    #[must_use]
    pub fn new(mesh: SharedMesh, store: SharedStore) -> Self {
        Self {
            mesh,
            store,
            watchers: parking_lot::Mutex::new(HashMap::new()),
            subscribed: parking_lot::Mutex::new(HashSet::new()),
        }
    }

    /// The peer nodes watching one conversation, sorted so a test reads a
    /// stable answer and an operator's log reads a deterministic line.
    pub fn watchers_of(&self, conversation_id: Id) -> Vec<Id> {
        let mut nodes = self
            .watchers
            .lock()
            .get(&conversation_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        nodes.sort();
        nodes
    }

    /// The subscribe half: ask the conversation's home node to watch it.
    ///
    /// Called when a local session is granted the conversation topic, which is
    /// the moment this node first has a reason to care. Three conversations
    /// need no ask, each for its own reason:
    ///
    /// - one a *room* owns, because its events already ride the room's own
    ///   tier (`FED_ROOM_EVENT`) and a second envelope would deliver every
    ///   member two copies of everything;
    /// - one whose home region is empty, the pre-label state of a row created
    ///   before the label existed or seeded without one, because there is no
    ///   home node to ask and its events stay local — unmarked, so the next
    ///   granted `SUBSCRIBE` asks again if the row ever gains a label;
    /// - one this node itself homes, because its sessions are served from this
    ///   hub and the watchers arrive as peers subscribe.
    ///
    /// A home region no allowed peer answers is also left unmarked, so the
    /// next granted `SUBSCRIBE` retries the ask. Errors are the caller's to
    /// log: a refused ask must not refuse the client's `SUBSCRIBE`, whose
    /// local half already succeeded.
    pub(crate) async fn subscribe_to(&self, conversation_id: Id, now: Timestamp) -> Result<()> {
        if self.subscribed.lock().contains(&conversation_id) {
            return Ok(());
        }
        let Some(conversation) = self.store.conversation(conversation_id).await? else {
            // No row, nothing to home: the authorization that reached here
            // already collapsed this to a refusal elsewhere.
            return Ok(());
        };
        if conversation.room_id.is_some() {
            // A room's conversation rides the room's own tier. Marked, so the
            // store is read once per process for a conversation that will
            // never be asked about — a room's conversation does not stop being
            // the room's.
            self.subscribed.lock().insert(conversation_id);
            return Ok(());
        }
        if conversation.home_region.is_empty() {
            // Pre-label: no node is named, so there is nobody to ask and the
            // conversation's events stay local. Unmarked, in case the row
            // later arrives carrying a label.
            return Ok(());
        }
        if conversation.home_region == self.mesh.region() {
            // This node is the home node: the members' sessions are served
            // from this hub, and the watchers arrive as peers subscribe.
            return Ok(());
        }
        let Some(home) = self.home_node(&conversation.home_region).await? else {
            tracing::warn!(
                conversation = %conversation_id.to_text(),
                home = %conversation.home_region,
                "no allowed mesh peer homes this conversation; its events stay local"
            );
            return Ok(());
        };
        let routing = FedConversationRouting {
            epoch: self.mesh.epoch(),
            home_region: conversation.home_region.clone(),
            conversation_id,
        };
        let payload = encode_envelope(Opcode::FedConversationSubscribe, &routing)?;
        self.mesh
            .enqueue(
                FederatedEvent {
                    target_node: home,
                    opcode: Opcode::FedConversationSubscribe.to_wire() as i32,
                    payload,
                },
                now,
            )
            .await?;
        self.subscribed.lock().insert(conversation_id);
        Ok(())
    }

    /// The watch half: record that `peer` holds subscribers of a conversation
    /// this node serves.
    ///
    /// Idempotent — a re-delivered subscribe (at least once, section 153) or
    /// a racing pair of `SUBSCRIBE`s inserts the same node twice and the set
    /// keeps one entry, which is the whole point of the tier: one copy per
    /// node, however many sessions on it care. The routing epoch must be
    /// current: a peer working from a stale view is told so, and the link
    /// teardown that carries the error back forces the re-handshake that
    /// refreshes it.
    pub(crate) fn register_watcher(
        &self,
        peer: Id,
        routing: &FedConversationRouting,
    ) -> Result<()> {
        self.mesh.check_epoch(routing.epoch)?;
        self.watchers
            .lock()
            .entry(routing.conversation_id)
            .or_default()
            .insert(peer);
        Ok(())
    }

    /// The re-anchor: re-send every ask this process owes a conversation's
    /// home node.
    ///
    /// The watch table is memory on the home node's side, so a home node that
    /// restarts comes back holding an empty table while this node's
    /// `subscribed` set still says every ask was answered — and the tier is
    /// then down for every conversation whose subscribers all sit here,
    /// silently, because no client `SUBSCRIBE` is coming to re-ask: the
    /// sessions are already granted. The composition root calls this on a
    /// timer (`federation.reanchor_interval_ms`) for exactly that gap.
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
    /// the skips that return `Ok` unmarked — no row, a pre-label region, a
    /// home node not yet admitted — stay unmarked, exactly as a
    /// client-driven ask leaves them.
    pub(crate) async fn reanchor(&self, now: Timestamp) {
        // Bound to a `let` so the guard the take borrows dies at the semicolon:
        // a temporary in a `for` head would live for the whole loop, and a
        // parking-lot guard held across the ask's await is a future tokio
        // refuses to send between threads.
        let owed = std::mem::take(&mut *self.subscribed.lock());
        for conversation_id in owed {
            if let Err(error) = self.subscribe_to(conversation_id, now).await {
                tracing::warn!(
                    conversation = %conversation_id.to_text(),
                    %error,
                    "the conversation re-anchor failed; the next interval tries again"
                );
                self.subscribed.lock().insert(conversation_id);
            }
        }
    }

    /// The forward half: hand a direct or group conversation's fanout to the
    /// node that owns its fan-out.
    ///
    /// Called from `publish_messaging` once the store has said the
    /// conversation is not a room's — the one branch where a message used to
    /// stop at this node's hub and the far members never heard it. The
    /// conversation row is read here rather than passed in, because the
    /// caller's lookup answered a different question (which room, if any,
    /// owns it) and this half needs the label the row carries.
    ///
    /// A conversation with no row, or one whose home region is empty, stays
    /// local: the publish already reached this node's hub, which is the whole
    /// of what a conversation with no home node can be owed. A room's
    /// conversation never arrives here (the caller checked), and a defensive
    /// lookup that finds one anyway passes, because the room's tier already
    /// carries it and a second envelope would only duplicate.
    ///
    /// # Errors
    ///
    /// Propagates an encode or outbox failure. The caller logs rather than
    /// fails: the local publish already happened, and refusing the send would
    /// only cost the sender their message without undoing anything.
    pub(crate) async fn forward(&self, fanout: &MessageFanout, now: Timestamp) -> Result<()> {
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
        match self.store.conversation(fanout.conversation_id).await? {
            Some(conversation) => self.to_owner(&conversation, inner, now).await,
            // No row, so nothing names a home node. The event was published
            // to this node's hub regardless.
            None => Ok(()),
        }
    }

    /// The forward half's second producer: a group call's membership
    /// announcements, the frames the dispatcher publishes to a
    /// conversation's topic so every subscribed member learns a call is
    /// running.
    ///
    /// The same two hops `forward` rides, over the same watch table, because
    /// the audience question is identical: a conversation's subscribers,
    /// whichever nodes they sit on. The event is encoded whole — the joiner's
    /// sealed offer rides inside it, read by nobody on the way through, the
    /// same mail-slot rule every sealed blob the tier carries keeps.
    ///
    /// A conversation a *room* owns is a skip rather than a route: its events
    /// ride the room's own tier (`FED_ROOM_EVENT`), and a second envelope
    /// here would deliver the room's members two copies of every
    /// announcement. A conversation with no row stays local, exactly as a
    /// messaging fanout does — the publish already reached this node's hub.
    ///
    /// # Errors
    ///
    /// Propagates an encode or outbox failure. The caller logs rather than
    /// fails, for the same reason `forward`'s caller does: the local publish
    /// already happened, and refusing the join over the federated half would
    /// only cost the roster its far members without undoing anything.
    pub(crate) async fn forward_call_event(
        &self,
        conversation_id: Id,
        event: &migo_protocol::CallStateEvent,
        now: Timestamp,
    ) -> Result<()> {
        let inner = to_frame(Opcode::CallSfuEvent.to_wire(), 0, event).map_err(fault::from_wire)?;
        match self.store.conversation(conversation_id).await? {
            Some(conversation) if conversation.room_id.is_none() => {
                self.to_owner(&conversation, inner, now).await
            }
            // A room's conversation rides the room's own tier, and a row that
            // names no home at all has nobody to forward to. Either way the
            // local publish that already happened is the whole of what this
            // tier owes.
            _ => Ok(()),
        }
    }

    /// One copy of an *ingested* event to every watching node but the one it
    /// arrived from.
    ///
    /// The home node is the tier's only fan-out authority, so a conversation
    /// event a peer forwarded here is owed onward to the other watchers. The
    /// origin is excluded because it has already published the event to its
    /// own hub: sending it back would deliver every local subscriber the same
    /// frame twice, and the second copy is indistinguishable from a real one.
    ///
    /// The bytes are re-sealed as-is rather than re-encoded from the arriving
    /// frame, so what the receiving node's clients see is byte-for-byte what
    /// the origin's clients saw (section 145).
    pub(crate) async fn fan_out_inbound(
        &self,
        conversation_id: Id,
        origin: Id,
        payload: &[u8],
        now: Timestamp,
    ) -> Result<()> {
        let inner = Frame::decode(Bytes::copy_from_slice(payload)).map_err(fault::from_wire)?;
        self.fan_out(conversation_id, inner, Some(origin), now)
            .await
    }

    /// Whether this node homes the conversation, and so owns its fan-out.
    ///
    /// The ingest path's question. A `FED_CONVERSATION_EVENT` arrives at a
    /// node because some node put it there, and the two reasons are opposite:
    /// the home node holds the watch table and owes the event onward, while a
    /// watching node is the end of the line and owes nothing. Only the store
    /// can tell them apart, and the row is read per event rather than cached
    /// for the same reason the room half reads per event: the row is the one
    /// authority on where the table lives, and a cache here would keep
    /// fanning a conversation out from a node that no longer owns it.
    pub(crate) async fn homes(&self, conversation_id: Id) -> bool {
        matches!(
            self.store.conversation(conversation_id).await,
            Ok(Some(conversation)) if conversation.home_region == self.mesh.region()
        )
    }

    /// Hands an event to the node that owns its fan-out: this one, or the
    /// home node.
    ///
    /// The tier's division of labour in one place, the same one the room half
    /// carries out. A node that homes the conversation holds the watch table,
    /// so it fans out directly — one copy per watching node. A node that does
    /// not home it holds no table and cannot know who is watching, so it owes
    /// the home node exactly one copy and lets the table do the tiering.
    /// Routing a non-home node's publish anywhere else would either drop it
    /// or duplicate it.
    async fn to_owner(
        &self,
        conversation: &Conversation,
        inner: Frame,
        now: Timestamp,
    ) -> Result<()> {
        if conversation.home_region.is_empty() {
            // Pre-label: nobody is named to tier it, and the local publish
            // that already happened is the whole of what the conversation is
            // owed.
            return Ok(());
        }
        if conversation.home_region == self.mesh.region() {
            return self
                .fan_out(conversation.conversation_id, inner, None, now)
                .await;
        }
        self.send_to_home(
            conversation.conversation_id,
            conversation.home_region.as_str(),
            inner,
            now,
        )
        .await
    }

    /// The fan-out itself: encode once, enqueue one copy per watching node.
    ///
    /// A conversation with no watchers is a plain no-op, which is the common
    /// case on every node that is not the conversation's home node.
    async fn fan_out(
        &self,
        conversation_id: Id,
        inner: Frame,
        exclude: Option<Id>,
        now: Timestamp,
    ) -> Result<()> {
        let targets = self
            .watchers
            .lock()
            .get(&conversation_id)
            .cloned()
            .unwrap_or_default();
        if targets.is_empty() {
            return Ok(());
        }
        let envelope = FedConversationEvent {
            conversation_id,
            payload: inner.encode().map_err(fault::from_wire)?.to_vec(),
        };
        let payload = encode_envelope(Opcode::FedConversationEvent, &envelope)?;
        for node in targets {
            if Some(node) == exclude {
                continue;
            }
            self.mesh
                .enqueue(
                    FederatedEvent {
                        target_node: node,
                        opcode: Opcode::FedConversationEvent.to_wire() as i32,
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
    /// Used by a node that does not home the conversation and so holds no
    /// watch table: the home node is the only node that can tier the event,
    /// and one copy to it is what makes a conversation's traffic cross node
    /// boundaries at all.
    async fn send_to_home(
        &self,
        conversation_id: Id,
        home_region: &str,
        inner: Frame,
        now: Timestamp,
    ) -> Result<()> {
        let Some(home) = self.home_node(home_region).await? else {
            tracing::warn!(
                conversation = %conversation_id.to_text(),
                home = %home_region,
                "no allowed mesh peer homes this conversation; its events stay local"
            );
            return Ok(());
        };
        let envelope = FedConversationEvent {
            conversation_id,
            payload: inner.encode().map_err(fault::from_wire)?.to_vec(),
        };
        let payload = encode_envelope(Opcode::FedConversationEvent, &envelope)?;
        // The pending handle is the outbox's business, not this tier's: what
        // the caller needs to know is whether the copy is queued.
        self.mesh
            .enqueue(
                FederatedEvent {
                    target_node: home,
                    opcode: Opcode::FedConversationEvent.to_wire() as i32,
                    payload,
                },
                now,
            )
            .await
            .map(|_queued| ())
    }

    /// The federating peer that homes a region, if the allow-list names one.
    ///
    /// A degraded peer still counts: degraded is a signal about the link's
    /// health, not a suspension, so a conversation's events keep flowing to a
    /// slow home node exactly as to a fast one (section 153). Only the
    /// operator's paused and blocked are excluded.
    async fn home_node(&self, home_region: &str) -> Result<Option<Id>> {
        Ok(self.home_peer(home_region).await?.map(|peer| peer.node_id))
    }

    /// The allowed peer that homes a region. The same scan the room half
    /// reads, so both tiers agree on who a label names.
    async fn home_peer(&self, home_region: &str) -> Result<Option<PeerView>> {
        Ok(self
            .mesh
            .peers(PEER_SCAN_LIMIT)
            .await?
            .into_iter()
            .find(|peer| peer.region == home_region && peer.status.is_allowed()))
    }
}

#[cfg(test)]
mod tests {
    //! A relay over a real mesh service and a memory store: the subscribe
    //! half's home-node resolution and once-per-conversation promise, the
    //! skips a room's conversation and a pre-label row owe, and the watch
    //! half's idempotence and epoch honesty, with no wire at all — the mesh
    //! tests in `mesh.rs` carry the two-node paths.

    use super::*;
    use migo_core::random::SeededRandom;
    use migo_crypto::NodeSecret;
    use migo_federation::{MeshService, NewPeerSpec};
    use migo_messaging::Broadcast;
    use migo_protocol::{from_frame, EncryptionMode, MessageEvent, MessageKind};
    use migo_store::MemoryStore;
    use std::sync::Arc;

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

    /// A store holding one direct conversation homed at `home_region`, with
    /// two members so the row is the shape a real chat leaves behind.
    async fn store_with_conversation(conversation_id: Id, home_region: &str) -> SharedStore {
        let store = MemoryStore::new();
        let store: SharedStore = Arc::new(store);
        store
            .direct_conversation(
                Id::from(0xAAAA),
                Id::from(0xBBBB),
                conversation_id,
                EncryptionMode::EndToEnd,
                home_region.to_string(),
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("a fresh pair builds a conversation");
        store
    }

    /// One message fanout for the conversation, the shape `publish_messaging`
    /// hands over: a sealed envelope nobody on the way may open.
    fn message_fanout(conversation_id: Id) -> MessageFanout {
        MessageFanout {
            conversation_id,
            exclude_device: None,
            event: Broadcast::Message(MessageEvent {
                message_id: Id::from(0x7777),
                conversation_id,
                seq: 3,
                sender_id: Id::from(0xAAAA),
                sender_device: Id::from(0xAAAA_0001),
                kind: MessageKind::Text,
                envelope: b"sealed-from-the-client".to_vec(),
                created_at: Timestamp::from_millis(NOW),
                reply_to: None,
                edited_at: None,
                deleted: None,
                sender_key_id: None,
            }),
        }
    }

    /// A conversation this node homes needs no ask: no event is enqueued.
    #[tokio::test]
    async fn a_conversation_this_node_homes_is_never_subscribed_away() {
        let mesh = mesh_in("region-1", Id::from(0x4444), "region-2").await;
        let conversation_id = Id::from(0x1111);
        let store = store_with_conversation(conversation_id, "region-1").await;
        let relay = ConversationRelay::new(Arc::clone(&mesh), store);

        relay
            .subscribe_to(conversation_id, Timestamp::from_millis(NOW))
            .await
            .expect("the home node needs no ask");
        assert!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .is_empty(),
            "nothing was enqueued for a conversation this node homes"
        );
    }

    /// A conversation homed elsewhere is asked for once: the second call finds
    /// the conversation marked and enqueues nothing.
    #[tokio::test]
    async fn a_remote_conversation_is_subscribed_once_per_process() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let conversation_id = Id::from(0x1111);
        let store = store_with_conversation(conversation_id, "region-2").await;
        let relay = ConversationRelay::new(Arc::clone(&mesh), store);

        for _ in 0..2 {
            relay
                .subscribe_to(conversation_id, Timestamp::from_millis(NOW))
                .await
                .expect("the home node resolves and the ask enqueues");
        }
        let due = mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads");
        assert_eq!(due.len(), 1, "one ask, not one per subscribing session");
        assert_eq!(due[0].target_node, peer, "the ask names the home node");

        let outer = Frame::decode(Bytes::from(due[0].payload.clone()))
            .expect("an outbox payload is an encoded frame");
        assert_eq!(
            Opcode::from_wire(outer.header.opcode),
            Some(Opcode::FedConversationSubscribe),
            "the ask is a conversation routing envelope"
        );
        let routing: FedConversationRouting = from_frame(&outer).expect("the routing decodes");
        assert_eq!(routing.conversation_id, conversation_id);
        assert_eq!(routing.home_region, "region-2");
        assert_eq!(routing.epoch, mesh.epoch());
    }

    /// A conversation whose home region no allowed peer answers is left
    /// unmarked, so a later ask — once the operator admits the home node —
    /// can happen.
    #[tokio::test]
    async fn a_conversation_without_a_home_peer_stays_askable() {
        let mesh = mesh_in("region-1", Id::from(0x4444), "region-2").await;
        let conversation_id = Id::from(0x1111);
        let store = store_with_conversation(conversation_id, "region-3").await;
        let relay = ConversationRelay::new(Arc::clone(&mesh), store);

        relay
            .subscribe_to(conversation_id, Timestamp::from_millis(NOW))
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

    /// A pre-label conversation — the state of every row created before the
    /// label existed — is never asked about and never forwarded: its events
    /// stay local, which is what a row that names no home node can owe.
    #[tokio::test]
    async fn a_pre_label_conversation_stays_local() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let conversation_id = Id::from(0x1111);
        let store = store_with_conversation(conversation_id, "").await;
        let relay = ConversationRelay::new(Arc::clone(&mesh), store);

        relay
            .subscribe_to(conversation_id, Timestamp::from_millis(NOW))
            .await
            .expect("a row that names no home node is a skip, not an error");
        relay
            .forward(
                &message_fanout(conversation_id),
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("and its publishes stay local");
        assert!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .is_empty(),
            "no ask and no copy for a conversation no node is named to home"
        );
    }

    /// A watch is idempotent and a stale routing epoch is refused: the peer
    /// must re-handshake onto the current view before it may watch.
    #[tokio::test]
    async fn a_watch_is_idempotent_and_a_stale_epoch_is_refused() {
        let mesh = mesh_in("region-1", Id::from(0x4444), "region-2").await;
        let store = Arc::new(MemoryStore::new()) as SharedStore;
        let relay = ConversationRelay::new(Arc::clone(&mesh), store);
        let conversation_id = Id::from(0x1111);
        let peer = Id::from(0x5555);
        let routing = FedConversationRouting {
            epoch: mesh.epoch(),
            home_region: "region-1".to_string(),
            conversation_id,
        };

        relay
            .register_watcher(peer, &routing)
            .expect("a current epoch admits the watch");
        relay
            .register_watcher(peer, &routing)
            .expect("a re-delivered subscribe is idempotent");
        assert_eq!(
            relay.watchers_of(conversation_id),
            vec![peer],
            "one entry per node, not per subscribe"
        );

        mesh.bump_epoch();
        let stale = FedConversationRouting { ..routing };
        assert!(
            relay.register_watcher(peer, &stale).is_err(),
            "a peer on a stale routing view is told so"
        );
    }

    /// The forward half on a node that is *not* the conversation's home: one
    /// copy to the home node, because the watch table is there and this node
    /// cannot tier.
    ///
    /// This is the difference between a direct chat whose messages reach the
    /// other participant and one whose messages reach whoever shares a node
    /// with the sender. The copy is an opaque envelope: the sealed bytes ride
    /// through this node unread, which is the standing rule of section 170.
    #[tokio::test]
    async fn a_publish_on_a_non_home_node_goes_to_the_home_node_alone() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let conversation_id = Id::from(0x1111);
        let store = store_with_conversation(conversation_id, "region-2").await;
        let relay = ConversationRelay::new(Arc::clone(&mesh), store);

        let fanout = message_fanout(conversation_id);
        relay
            .forward(&fanout, Timestamp::from_millis(NOW))
            .await
            .expect("the home node resolves and the event enqueues");

        let due = mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads");
        assert_eq!(due.len(), 1, "one copy, for the only node that can tier it");
        assert_eq!(
            due[0].target_node, peer,
            "and it is the conversation's home node"
        );

        let outer = Frame::decode(Bytes::from(due[0].payload.clone()))
            .expect("an outbox payload is an encoded frame");
        assert_eq!(
            Opcode::from_wire(outer.header.opcode),
            Some(Opcode::FedConversationEvent),
            "the copy is a conversation event envelope"
        );
        let envelope: FedConversationEvent = from_frame(&outer).expect("the envelope decodes");
        assert_eq!(
            envelope.conversation_id, conversation_id,
            "and it names the conversation it speaks for, which is what the home node tiers by"
        );
        let inner = Frame::decode(Bytes::from(envelope.payload)).expect("the inner frame decodes");
        assert_eq!(
            Opcode::from_wire(inner.header.opcode),
            Some(Opcode::MessageEvent),
            "carrying the message itself, sealed as a local subscriber would have seen it"
        );
        let event: MessageEvent = from_frame(&inner).expect("the message decodes");
        assert_eq!(
            event.envelope,
            b"sealed-from-the-client".to_vec(),
            "the sealed envelope crossed this node byte for byte, unopened"
        );
    }

    /// The forward half on the home node: one copy per watching node, and
    /// none for a conversation nobody watches — the common case on a home
    /// node whose members all connected to it.
    #[tokio::test]
    async fn a_home_node_fans_out_one_copy_per_watcher() {
        let mesh = mesh_in("region-1", Id::from(0x4444), "region-2").await;
        let conversation_id = Id::from(0x1111);
        let store = store_with_conversation(conversation_id, "region-1").await;
        let relay = ConversationRelay::new(Arc::clone(&mesh), store);

        // No watchers: a plain no-op, nothing enqueued.
        relay
            .forward(
                &message_fanout(conversation_id),
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("a conversation with no watchers forwards nothing");
        assert!(mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads")
            .is_empty());

        // Two watchers: one copy each, and the inbound half excludes the node
        // a copy arrived from.
        let routing = FedConversationRouting {
            epoch: mesh.epoch(),
            home_region: "region-1".to_string(),
            conversation_id,
        };
        relay
            .register_watcher(Id::from(0x5001), &routing)
            .expect("the first watch lands");
        relay
            .register_watcher(Id::from(0x5002), &routing)
            .expect("the second watch lands");
        relay
            .forward(
                &message_fanout(conversation_id),
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("the home node fans out");
        let due = mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads");
        let mut targets: Vec<Id> = due.iter().map(|event| event.target_node).collect();
        targets.sort();
        assert_eq!(
            targets,
            vec![Id::from(0x5001), Id::from(0x5002)],
            "one copy per watching node, no more"
        );
        assert!(
            due.iter().all(|event| Opcode::from_wire(
                Frame::decode(Bytes::from(event.payload.clone()))
                    .expect("the payload is a frame")
                    .header
                    .opcode
            ) == Some(Opcode::FedConversationEvent)),
            "and every copy is a conversation event envelope"
        );
    }

    /// `homes` reads the row, not the watch table: a conversation homed
    /// elsewhere answers false even when watchers are registered here, which
    /// is what keeps the ingest path from passing events on from a node that
    /// is the end of the line.
    #[tokio::test]
    async fn homes_reads_the_row_not_the_table() {
        let mesh = mesh_in("region-1", Id::from(0x4444), "region-2").await;
        let homed = Id::from(0x1111);
        let remote = Id::from(0x2222);
        let store = store_with_conversation(homed, "region-1").await;
        store
            .direct_conversation(
                Id::from(0xAAAA),
                Id::from(0xBBBB),
                remote,
                EncryptionMode::EndToEnd,
                "region-2".to_string(),
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("a second pair builds a second conversation");
        let relay = ConversationRelay::new(Arc::clone(&mesh), store);
        let routing = FedConversationRouting {
            epoch: mesh.epoch(),
            home_region: "region-1".to_string(),
            conversation_id: remote,
        };
        relay
            .register_watcher(Id::from(0x5001), &routing)
            .expect("the watch lands");

        assert!(
            relay.homes(homed).await,
            "the conversation this node homes answers true"
        );
        assert!(
            !relay.homes(remote).await,
            "the conversation homed elsewhere answers false even with watchers here"
        );
        assert!(
            !relay.homes(Id::from(0x9999)).await,
            "and a conversation with no row answers false"
        );
    }
}
