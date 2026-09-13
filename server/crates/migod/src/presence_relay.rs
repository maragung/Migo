//! User-topic presence fanout across nodes (brief sections 170, 14).
//!
//! A room is not the only thing whose audience spans nodes: a user topic is a
//! presence stream, and bob watching alice's topic from a node alice never
//! touches was the gap `tools/nnode` reported honestly — her presence change
//! was published to her own node's hub and stopped there. This module is the
//! bookkeeping that carries it the rest of the way, in the shape of the room
//! tier it sits beside ([`room_relay`](crate::room_relay)):
//!
//! - the **subscribe half**: when a local session is granted a user topic —
//!   the same `authorize_topics` gate every `SUBSCRIBE` already passes — the
//!   node asks its peers to watch that subject, once per subject for the life
//!   of the process, with a `FED_USER_SUBSCRIBE` carrying the routing epoch;
//! - the **watch half**: every node that receives the ask records which peer
//!   nodes hold subscribers of which subjects, so a presence change becomes
//!   one federated copy per *node*, not per session and not per watcher;
//! - the **forward half**: both of a presence change's publish paths — the
//!   `PRESENCE_SET` request path and the out-of-band session edges — hand
//!   their fanout here, and the node the change happened on enqueues one
//!   `FED_USER_EVENT` per watching node, the event frame sealed exactly as a
//!   local subscriber would have received it. The receiving node's ingest
//!   path (`mesh::route_user_event`) publishes it into its own hub.
//!
//! # Why the ask is a broadcast and the table is everywhere
//!
//! A room row names its home node, so a watcher asks exactly one peer and
//! only that peer holds the watch table. A user has no such row: presence is
//! not stored state with a placement, it is an edge of a session, and the
//! session lives wherever the user happened to connect. So the subscribe half
//! cannot know which peer to ask, and it asks them all — and because any node
//! may be the one a session appears on, every node that receives the ask
//! keeps the watch entry. The dead entries never fire: a node that holds no
//! session of the subject publishes no presence of hers, so an entry for it
//! is inert until her session arrives, which is the one moment it becomes
//! exactly right.
//!
//! This is also why there is no `to_owner` half here. A room publish on a
//! non-home node must detour through the home node because only it holds the
//! table; a presence change happens *only* on the node whose session caused
//! it — `PRESENCE_SET` comes from the subject's own socket, and the
//! connect/disconnect edges come from that socket's lifecycle — so the origin
//! is the fan-out authority by construction, and it can reach every watcher
//! directly because every peer holds the table.
//!
//! # Ordering, redelivery, and the missing unsubscribe
//!
//! Per-link sequence numbers are strictly increasing, so the events one node
//! enqueues for another arrive in the order they were published, and delivery
//! is at least once: a re-delivered presence event is re-published into a hub
//! whose consumers are idempotent, and presence is `Coalescable` keyed by the
//! subject (section 154) on both halves, so a backed-up queue keeps the
//! latest state and a duplicate is the state it already holds.
//!
//! Like the room tier, the wire has no unsubscribe, so a watcher set only
//! grows: a node that no longer holds subscribers of a subject keeps
//! receiving her presence, and its hub's fan-out to an empty topic is a cheap
//! no-op. Also inherited: the watch is per process, so a node restart
//! re-sends the asks on the next granted `SUBSCRIBE` and re-anchors the
//! peers' tables.
//!
//! # What does not ride this tier
//!
//! The *stored* presence the read path serves stays node-local: this tier
//! carries the stream, not the cache, so a watcher who reads a subject's
//! presence after the fact still asks the node he is on. The periodic
//! aggregated `FED_PRESENCE_DIGEST` remains the design for that aggregation;
//! what this tier adds is the demand-gated per-subject stream, which cannot
//! be a digest because a watcher must see the state, not a summary of it.

use std::collections::{HashMap, HashSet};

use migo_core::{Id, Result, Timestamp};
use migo_federation::model::FederatedEvent;
use migo_federation::SharedMesh;
use migo_protocol::{fault, to_frame, Encode, FedUserEvent, FedUserWatch, Frame, Opcode};

/// How many allow-list rows one subscribe-half scan reads. The page clamp the
/// directory answers with, reused because a peer scan is the same bounded
/// read.
const PEER_SCAN_LIMIT: u16 = 256;

/// Which peer nodes hold subscribers of which user topics, and the subscribe
/// half that keeps the table honest from the other side.
pub struct PresenceRelay {
    mesh: SharedMesh,
    /// The watch table, every-node side: subject → the peer nodes watching
    /// their user topic.
    watchers: parking_lot::Mutex<HashMap<Id, HashSet<Id>>>,
    /// The subjects this process has already asked its peers to watch. One
    /// `FED_USER_SUBSCRIBE` per subject per process; a second would only be
    /// re-registered on the other side (the insert is idempotent), but the
    /// outbox owes each peer one copy, not one per subscribing session.
    subscribed: parking_lot::Mutex<HashSet<Id>>,
}

impl PresenceRelay {
    /// Wraps the mesh the two halves read.
    #[must_use]
    pub fn new(mesh: SharedMesh) -> Self {
        Self {
            mesh,
            watchers: parking_lot::Mutex::new(HashMap::new()),
            subscribed: parking_lot::Mutex::new(HashSet::new()),
        }
    }

    /// The peer nodes watching one subject, sorted so a test reads a stable
    /// answer and an operator's log reads a deterministic line.
    pub fn watchers_of(&self, subject_id: Id) -> Vec<Id> {
        let mut nodes = self
            .watchers
            .lock()
            .get(&subject_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        nodes.sort();
        nodes
    }

    /// The subscribe half: ask every allowed peer to watch this subject.
    ///
    /// Called when a local session is granted the subject's user topic, which
    /// is the moment this node first has a reason to care. Unlike a room
    /// there is no home row to read, so the ask cannot name one peer: it goes
    /// to every allowed peer, and the one that later holds a session of the
    /// subject is the one whose forward half fires. A node with no allowed
    /// peer leaves the subject unmarked, so the next granted `SUBSCRIBE` —
    /// perhaps after the operator admits the peer — retries the ask rather
    /// than the subject silently staying local forever. Errors are the
    /// caller's to log: a refused ask must not refuse the client's
    /// `SUBSCRIBE`, whose local half already succeeded.
    pub(crate) async fn subscribe_to(&self, subject_id: Id, now: Timestamp) -> Result<()> {
        if self.subscribed.lock().contains(&subject_id) {
            return Ok(());
        }
        let peers = self.mesh.peers(PEER_SCAN_LIMIT).await?;
        let targets: Vec<_> = peers
            .into_iter()
            .filter(|peer| peer.status.is_allowed())
            .map(|peer| peer.node_id)
            .collect();
        if targets.is_empty() {
            // Nobody to ask. Unmarked, so a later SUBSCRIBE — once the
            // operator admits a peer — asks again.
            return Ok(());
        }
        let watch = FedUserWatch {
            epoch: self.mesh.epoch(),
            user_id: subject_id,
        };
        let payload = encode_envelope(Opcode::FedUserSubscribe, &watch)?;
        for node in targets {
            self.mesh
                .enqueue(
                    FederatedEvent {
                        target_node: node,
                        opcode: Opcode::FedUserSubscribe.to_wire() as i32,
                        payload: payload.clone(),
                    },
                    now,
                )
                .await?;
        }
        self.subscribed.lock().insert(subject_id);
        Ok(())
    }

    /// The watch half: record that `peer` holds subscribers of a subject this
    /// node may serve.
    ///
    /// Idempotent — a re-delivered subscribe (at least once, section 153) or
    /// a racing pair of `SUBSCRIBE`s inserts the same node twice and the set
    /// keeps one entry, which is the whole point of the tier: one copy per
    /// node, however many sessions on it care. The routing epoch must be
    /// current: a peer working from a stale view is told so, and the link
    /// teardown that carries the error back forces the re-handshake that
    /// refreshes it.
    pub(crate) fn register_watcher(&self, peer: Id, watch: &FedUserWatch) -> Result<()> {
        self.mesh.check_epoch(watch.epoch)?;
        self.watchers
            .lock()
            .entry(watch.user_id)
            .or_default()
            .insert(peer);
        Ok(())
    }

    /// The forward half: one federated copy of a presence event per watching
    /// node.
    ///
    /// The event frame is encoded once and the same bytes are enqueued for
    /// every watcher — the outbox's per-link batching carries them in publish
    /// order — and a subject with no watchers is a plain no-op, which is the
    /// common case for every subject whose watchers are all local.
    /// `exclude_device` is deliberately not honoured, for the same reason the
    /// room tier does not honour it: the excluded socket is on *this* node,
    /// and the far node's sessions — including the subject's other devices,
    /// which are part of the audience by design — are who the tier exists to
    /// reach.
    ///
    /// There is no home-node detour here, and that is not an omission: a
    /// presence change happens only on the node whose session caused it, so
    /// the origin holds every fact a fan-out needs, and because the subscribe
    /// half is a broadcast, every peer holds the table the origin reads.
    pub(crate) async fn forward(
        &self,
        fanout: &migo_presence::Fanout,
        now: Timestamp,
    ) -> Result<()> {
        let targets = self
            .watchers
            .lock()
            .get(&fanout.subject_id)
            .cloned()
            .unwrap_or_default();
        if targets.is_empty() {
            return Ok(());
        }
        let inner =
            to_frame(fanout.opcode().to_wire(), 0, &fanout.event).map_err(fault::from_wire)?;
        let envelope = FedUserEvent {
            user_id: fanout.subject_id,
            payload: inner.encode().map_err(fault::from_wire)?.to_vec(),
        };
        let payload = encode_envelope(Opcode::FedUserEvent, &envelope)?;
        for node in targets {
            self.mesh
                .enqueue(
                    FederatedEvent {
                        target_node: node,
                        opcode: Opcode::FedUserEvent.to_wire() as i32,
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

#[cfg(test)]
mod tests {
    //! A relay over a real mesh service: the subscribe half's broadcast and
    //! once-per-subject promise, and the watch half's idempotence and epoch
    //! honesty, with no wire at all — the mesh tests in `mesh.rs` carry the
    //! two-node paths.

    use super::*;
    use migo_core::random::SeededRandom;
    use migo_crypto::NodeSecret;
    use migo_federation::{MeshService, NewPeerSpec};
    use migo_protocol::{PresenceEvent, PresenceState};
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

    /// A presence fanout about `subject`, the shape both publish paths hand
    /// the relay.
    fn fanout_about(subject: Id, state: PresenceState) -> migo_presence::Fanout {
        migo_presence::Fanout::about(
            subject,
            Id::from(0x0707),
            PresenceEvent {
                user_id: subject,
                state,
                custom_status: None,
                last_seen: None,
            },
        )
    }

    /// The ask is a broadcast: one `FED_USER_SUBSCRIBE` per allowed peer,
    /// because a user topic has no home row to name the one node that serves
    /// it.
    #[tokio::test]
    async fn a_subject_is_asked_of_every_allowed_peer() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let other = Id::from(0x4545);
        mesh.add_peer(
            NewPeerSpec {
                node_id: other,
                public_key: NodeSecret::from_seed(&[8u8; 32])
                    .expect("a seed builds a key")
                    .public()
                    .to_bytes()
                    .to_vec(),
                base_url: "wss://other.test:9999".to_string(),
                region: "region-3".to_string(),
            },
            Timestamp::from_millis(NOW),
        )
        .await
        .expect("a second peer is admitted");
        let relay = PresenceRelay::new(Arc::clone(&mesh));
        let subject = Id::from(0x1111);

        relay
            .subscribe_to(subject, Timestamp::from_millis(NOW))
            .await
            .expect("the asks enqueue");
        let due = mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads");
        assert_eq!(
            due.len(),
            2,
            "one ask per allowed peer, not one per session"
        );
        let mut asked: Vec<Id> = due.iter().map(|event| event.target_node).collect();
        asked.sort();
        assert_eq!(asked, vec![other, peer]);
    }

    /// A subject is asked for once per process: the second call finds the
    /// subject marked and enqueues nothing.
    #[tokio::test]
    async fn a_subject_is_subscribed_once_per_process() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let relay = PresenceRelay::new(Arc::clone(&mesh));
        let subject = Id::from(0x1111);

        for _ in 0..2 {
            relay
                .subscribe_to(subject, Timestamp::from_millis(NOW))
                .await
                .expect("the ask enqueues");
        }
        let due = mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads");
        assert_eq!(due.len(), 1, "one ask, not one per subscribing session");
        assert_eq!(due[0].target_node, peer, "the ask names the peer");
    }

    /// A node with no allowed peer leaves the subject unmarked, so a later
    /// ask — once the operator admits a peer — can happen.
    #[tokio::test]
    async fn a_subject_without_a_peer_stays_askable() {
        let registry = migo_core::metrics::Registry::new();
        let secret = NodeSecret::from_seed(&[1u8; 32]).expect("a seed builds a key");
        let mesh: SharedMesh = Arc::new(
            MeshService::new(
                Arc::new(MemoryStore::new()),
                migo_federation::MeshConfig::default(),
                Id::from(0xABCD),
                "region-1".to_string(),
                secret,
                Box::new(SeededRandom::new(42)),
                &registry,
            )
            .expect("the mesh configuration is valid"),
        );
        let relay = PresenceRelay::new(Arc::clone(&mesh));
        let subject = Id::from(0x1111);

        relay
            .subscribe_to(subject, Timestamp::from_millis(NOW))
            .await
            .expect("a peerless mesh is a no-op, not an error");
        assert!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .is_empty(),
            "no peer to ask, so nothing was enqueued"
        );

        // A peer admitted later makes the next ask real, which is what
        // staying unmarked buys.
        mesh.add_peer(
            NewPeerSpec {
                node_id: Id::from(0x4444),
                public_key: NodeSecret::from_seed(&[9u8; 32])
                    .expect("a seed builds a key")
                    .public()
                    .to_bytes()
                    .to_vec(),
                base_url: "wss://peer.test:9999".to_string(),
                region: "region-2".to_string(),
            },
            Timestamp::from_millis(NOW),
        )
        .await
        .expect("a fresh allow-list admits the peer");
        relay
            .subscribe_to(subject, Timestamp::from_millis(NOW))
            .await
            .expect("the ask enqueues once a peer exists");
        assert_eq!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .len(),
            1
        );
    }

    /// A watch is idempotent and a stale routing epoch is refused: the peer
    /// must re-handshake onto the current view before it may watch.
    #[tokio::test]
    async fn a_watch_is_idempotent_and_a_stale_epoch_is_refused() {
        let mesh = mesh_in("region-1", Id::from(0x4444), "region-2").await;
        let relay = PresenceRelay::new(Arc::clone(&mesh));
        let subject = Id::from(0x1111);
        let peer = Id::from(0x5555);
        let watch = FedUserWatch {
            epoch: mesh.epoch(),
            user_id: subject,
        };

        relay
            .register_watcher(peer, &watch)
            .expect("a current epoch admits the watch");
        relay
            .register_watcher(peer, &watch)
            .expect("a re-delivered subscribe is idempotent");
        assert_eq!(
            relay.watchers_of(subject),
            vec![peer],
            "one entry per node, not per subscribe"
        );

        mesh.bump_epoch();
        let stale = FedUserWatch { ..watch };
        assert!(
            relay.register_watcher(peer, &stale).is_err(),
            "a peer on a stale routing view is told so"
        );
    }

    /// The forward half: one copy per watching *node*, carrying the presence
    /// event sealed exactly as a local subscriber would have received it, and
    /// nothing at all for a subject nobody watches.
    #[tokio::test]
    async fn a_presence_change_is_one_federated_copy_per_watching_node() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let relay = PresenceRelay::new(Arc::clone(&mesh));
        let subject = Id::from(0x1111);
        let other = Id::from(0x0303);
        let watch = FedUserWatch {
            epoch: mesh.epoch(),
            user_id: subject,
        };
        relay
            .register_watcher(peer, &watch)
            .expect("a current epoch admits the watch");
        relay
            .register_watcher(other, &watch)
            .expect("a second node may watch too");

        // A subject with no watchers forwards nothing: the common case on
        // every node for every subject whose watchers are all local.
        let stranger = Id::from(0x9999);
        relay
            .forward(
                &fanout_about(stranger, PresenceState::Away),
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("an unwatched subject is a no-op");

        relay
            .forward(
                &fanout_about(subject, PresenceState::Away),
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("the watched subject forwards");
        let due = mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads");
        assert_eq!(
            due.len(),
            2,
            "one copy per watching node, and none for the stranger"
        );
        let mut targets: Vec<Id> = due.iter().map(|event| event.target_node).collect();
        targets.sort();
        assert_eq!(targets, vec![other, peer]);

        let outer = Frame::decode(bytes::Bytes::from(due[0].payload.clone()))
            .expect("an outbox payload is an encoded frame");
        assert_eq!(
            Opcode::from_wire(outer.header.opcode),
            Some(Opcode::FedUserEvent),
            "the copy is a user event envelope"
        );
        let envelope: FedUserEvent =
            migo_protocol::from_frame(&outer).expect("the envelope decodes");
        assert_eq!(
            envelope.user_id, subject,
            "and it names the subject it speaks for, which is what the receiver places the topic by"
        );
        let inner =
            Frame::decode(bytes::Bytes::from(envelope.payload)).expect("the inner frame decodes");
        assert_eq!(
            Opcode::from_wire(inner.header.opcode),
            Some(Opcode::PresenceEvent),
            "carrying the presence event itself, sealed as a local subscriber would have seen it"
        );
        let event: PresenceEvent = migo_protocol::from_frame(&inner).expect("the event decodes");
        assert_eq!(event.user_id, subject);
        assert_eq!(event.state, PresenceState::Away);
    }
}
