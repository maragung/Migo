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
//! - the **forward half**: both of a room's publish paths — the request path
//!   through `publish_room_fanout` and the out-of-band room-presence publisher
//!   — hand their [`Fanout`](migo_rooms::Fanout) here, and the home node
//!   enqueues one `FED_ROOM_EVENT` per watching node, the inner event frame
//!   sealed exactly as a local session would have received it. The receiving
//!   node's ingest path (`mesh::route_room_event`) publishes it into its own
//!   hub, which is the fan-out the tier asks for.
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
//! home node restart is survived in practice rather than by design: a room
//! whose last local subscriber goes away and later returns is re-subscribed
//! only after this process restarts, which re-sends the watch and re-anchors
//! the home node's table.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use migo_core::{Id, Result, Timestamp};
use migo_federation::model::{FederatedEvent, PeerStatus};
use migo_federation::SharedMesh;
use migo_protocol::{
    fault, to_frame, Encode, FedRoomEvent, FedRouting, Frame, Opcode, RoomMemberEvent,
    RoomStateEvent, RoomVoteEvent,
};
use migo_rooms::{Broadcast as RoomBroadcast, Fanout as RoomFanout};
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
    /// The watch table, home-node side: room → the peer nodes watching it.
    watchers: parking_lot::Mutex<HashMap<Id, HashSet<Id>>>,
    /// The rooms this process has already asked a home node to watch. One
    /// `FED_ROOM_SUBSCRIBE` per room per process; a second would only be
    /// re-registered on the other side (the insert is idempotent), but the
    /// outbox owes the peer one copy, not one per subscribing session.
    subscribed: parking_lot::Mutex<HashSet<Id>>,
}

impl RoomRelay {
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
        let peers = self.mesh.peers(PEER_SCAN_LIMIT).await?;
        let Some(home) = peers
            .into_iter()
            .find(|peer| peer.region == room.home_region && peer.status == PeerStatus::Allowed)
        else {
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
                    target_node: home.node_id,
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

    /// The forward half: one federated copy of a room event per watching node.
    ///
    /// The event frame is encoded once and the same bytes are enqueued for
    /// every watcher — the outbox's per-link batching carries them in publish
    /// order — and a room with no watchers is a plain no-op, which is the
    /// common case on every node that is not the home node of the room.
    /// `exclude_device` is deliberately not honoured: the excluded socket is
    /// on *this* node, and the far node's sessions — including the actor's
    /// other devices — are the audience the tier exists to reach.
    pub(crate) async fn forward(&self, fanout: &RoomFanout, now: Timestamp) -> Result<()> {
        let targets = self
            .watchers
            .lock()
            .get(&fanout.room_id)
            .cloned()
            .unwrap_or_default();
        if targets.is_empty() {
            return Ok(());
        }
        let opcode = fanout.opcode();
        let inner = match &fanout.event {
            RoomBroadcast::Member(event) => to_frame(opcode.to_wire(), 0, event),
            RoomBroadcast::State(event) => to_frame(opcode.to_wire(), 0, event),
            RoomBroadcast::Vote(event) => to_frame(opcode.to_wire(), 0, event),
        }
        .map_err(fault::from_wire)?;
        let envelope = FedRoomEvent {
            room_id: fanout.room_id,
            payload: inner.encode().map_err(fault::from_wire)?.to_vec(),
        };
        let payload = encode_envelope(Opcode::FedRoomEvent, &envelope)?;
        for node in targets {
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
}

/// Frames one wire struct as the encoded inner frame an outbox event carries.
///
/// The outbox's payload is a whole encoded MWP frame (the transport wraps it
/// in a `FED_FORWARD` without opening it), so both halves build theirs the
/// same way and the shape lives in one place.
fn encode_envelope<T: Encode>(opcode: Opcode, value: &T) -> Result<Vec<u8>> {
    let frame: Frame = to_frame(opcode.to_wire(), 0, value).map_err(fault::from_wire)?;
    frame
        .encode()
        .map_err(fault::from_wire)
        .map(|bytes| bytes.to_vec())
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
        let relay = RoomRelay::new(Arc::clone(&mesh), store);

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
        let relay = RoomRelay::new(Arc::clone(&mesh), store);

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
        let relay = RoomRelay::new(Arc::clone(&mesh), store);

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
        let relay = RoomRelay::new(Arc::clone(&mesh), store);
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
}
