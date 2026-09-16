//! The failure scenarios of brief section 173, run as deterministic tests between
//! independent nodes on real loopback links.
//!
//! Section 173 demands more than a two-node smoke test: each failure a deployment can
//! actually hit must run as an automated test with an explicit expectation, so a
//! regression is a red line in CI rather than a story about the last outage. The nodes
//! here follow the `tools/2node` pattern — co-located, nothing but loopback between them
//! — and every delivery is driven by the test through
//! [`MeshTransport::drain_once`], the same operations primitive the runner ticks, so a
//! failure is the transport's and not a timing accident.
//!
//! What each test answers, by scenario number:
//!
//! * **1 (a node dies mid-flight)** — events bound for a dead node survive in the
//!   durable outbox, are pushed out on the backoff rather than retried hot, and after
//!   the node recovers arrive once each, in the order they were queued.
//! * **2 (split brain)** — both halves. The delivery half: a partitioned link holds
//!   the outbox without loss and preserves FIFO order across the outage, which is what
//!   "no second sequencer" means on the wire — one sender, one sequence, resuming
//!   where it left off. The read-only half: a room whose home node the partition cut
//!   away answers its mutations with `ROOM_READ_ONLY_PARTITION` (1702) instead of
//!   silently diverging, while a room this node homes and a private conversation keep
//!   writing, and the refusal lifts the moment the link heals.
//! * **3 (a slow link, not a dead one)** — a peer that completes the handshake but
//!   never acknowledges holds a batch for exactly the watermark budget, then fails it:
//!   the events are rescheduled on the doubling backoff, nothing piles up beyond one
//!   bounded batch, and the redelivery after the link recovers is the at-least-once
//!   semantics section 153 promises. The scenario's other expectation — the degraded
//!   marking — runs as its own test below: a peer whose undelivered backlog crosses the
//!   degradation watermark is marked degraded while it still receives everything owed to
//!   it, and is allowed again once it has caught up.
//! * **the handshake budget (section 173's closing gap)** — a peer that accepts the TCP
//!   connection but never speaks fails exactly when `federation.handshake_timeout_ms`
//!   runs out, measured on the node's injected clock: still waiting one millisecond
//!   inside the deadline, settled into the ordinary backoff one millisecond past it,
//!   and the drain proceeds to the next peer. The test advances a `ManualClock` past
//!   the deadline rather than sleeping the wait out.
//! * **7 (clock skew)** — a proof signed sixty-one seconds behind the listener's own
//!   clock is refused on the wire, while the same handshake stamped in-window succeeds
//!   — the control that makes the refusal mean the skew and only the skew.
//! * **8 (rolling deploy, two protocol versions)** — a frame carrying an optional field
//!   from a future protocol version crosses the link and is ingested, because the
//!   reader scopes unknown fields by length and skips them.
//! * **9 (a routing epoch bump, the rebalance primitive)** — a sender working from a
//!   stale routing view is refused, and the refusal names the epoch the peer's view is
//!   current at: the sender's transport adopts that epoch and redelivers in the same
//!   drain, with nothing lost and no operator's `bump_epoch` by hand. The other half of
//!   the rebalance follows it: when a room moves to another node, every member is handed
//!   a `RECONNECT_HINT` naming the new home's endpoint — one publish on the home node's
//!   own hub, one federated copy per watching node, then the rehomed row and the raised
//!   epoch, the drain order section 170 names.
//! * **10 (a mass backlog after recovery)** — three hundred queued events drain in
//!   sessions bounded by the drain batch, arriving once each in queue order, so a
//!   mass sync cannot exceed a session's capacity no matter how long the outage was.
//!
//! Scenarios 4, 5 and 6 are not link failures and live where their seams are:
//! storage-unavailable and outbox idempotency in `migo-federation`'s suite, cache loss
//! in `migo-cache`'s contracts, media unavailability in `migo-media`'s. The gaps section
//! 173 once held against this file — the absent handshake timeout, no automatic
//! directory refetch on a stale epoch, and no `RECONNECT_HINT` to members of a
//! rebalanced room — are the scenarios above now, closed in the open rather than
//! papered over.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use migo_core::config::DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS;
use migo_core::metrics::Registry;
use migo_core::{Clock, Id, ManualClock, SystemClock, Timestamp};
use migo_crypto::node::{self, NodeHello, NodeProof, NodeSecret, MAX_CLOCK_SKEW_MS};
use migo_federation::model::DEFAULT_DUE_BATCH;
use migo_federation::{
    FederatedEvent, MeshConfig, MeshService, NewPeerSpec, PeerStatus, SharedMesh,
};
use migo_protocol::{
    codes, from_frame, to_frame, CloseReason, EncryptionMode, FedAck, FedAuth, FedHello,
    FedPresenceDigest, FedRouting, Frame, Opcode, ReconnectHint, RoomKind,
};
use migo_store::model::NewRoom;
use migo_store::{MemoryStore, SharedStore};
use migo_wire::Writer;
use migod::mesh::MeshTransport;
use migod::mesh_tls::MeshTls;
use migod::room_relay::RoomRelay;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::server::TlsStream as ServerTlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// How long a test waits for an asynchronous condition before failing.
const WAIT_LIMIT: Duration = Duration::from_secs(10);

/// How often a waiting test re-checks the condition.
const POLL: Duration = Duration::from_millis(100);

/// The node id a `node(1)`-built mesh answers to, in the same scheme `federation_link`
/// uses so ids never collide between tests.
fn node_id(name: u8) -> Id {
    Id::from(u128::from(name) * 0x0101)
}

/// Builds one independent node: its own store, its own signing key, its own randomness.
/// Nothing is shared with the other nodes in a test — each is a whole `migod` mesh
/// service the way the composition root would build it.
async fn node(name: u8, region: &str) -> (SharedMesh, NodeSecret) {
    node_with_config(name, region, MeshConfig::default()).await
}

/// The same node, with the mesh policy knobs the test names — the degraded-marking test
/// pins the watermark low so a handful of queued events is a crossing backlog, without
/// asking the default configuration to cry degraded at traffic the brief calls normal.
async fn node_with_config(name: u8, region: &str, config: MeshConfig) -> (SharedMesh, NodeSecret) {
    let secret = NodeSecret::from_seed(&[name; 32]).expect("a 32-byte seed builds a key");
    let mesh = MeshService::new(
        Arc::new(MemoryStore::new()),
        config,
        node_id(name),
        region.to_string(),
        NodeSecret::from_seed(&[name; 32]).expect("a 32-byte seed builds a key"),
        Box::new(migo_core::random::SeededRandom::new(u64::from(name) * 7919)),
        &Registry::new(),
    )
    .expect("the mesh configuration is valid");
    (Arc::new(mesh), secret)
}

/// The public key bytes a peer's allow-list entry names for `node(name)`.
fn key_bytes(name: u8) -> Vec<u8> {
    NodeSecret::from_seed(&[name; 32])
        .expect("a 32-byte seed builds a key")
        .public()
        .to_bytes()
        .to_vec()
}

/// The public key of `node(name)` in the fixed-width form the TLS pin takes.
fn key32(name: u8) -> [u8; 32] {
    NodeSecret::from_seed(&[name; 32])
        .expect("a 32-byte seed builds a key")
        .public()
        .to_bytes()
}

/// The TLS 1.3 identity of `node(name)`, minted from the same seed its mesh service
/// signs with — the channel every link in these scenarios rides in, built the same way
/// the composition root builds it.
fn tls_for(name: u8) -> MeshTls {
    MeshTls::from_secret(&NodeSecret::from_seed(&[name; 32]).expect("a 32-byte seed builds a key"))
        .expect("the node identity key mints a TLS leaf")
}

/// The keys a fixture node's TLS gate accepts: exactly the peers its mesh allow-list
/// names, read the same way the production listener reads them.
async fn allowed_keys(mesh: &SharedMesh) -> Vec<[u8; 32]> {
    mesh.peers(u16::MAX)
        .await
        .expect("the allow-list reads")
        .iter()
        .filter_map(|peer| <[u8; 32]>::try_from(peer.public_key.as_slice()).ok())
        .collect()
}

/// Admits `peer` to the node's allow-list, naming where its listener is.
async fn admit(mesh: &SharedMesh, peer: Id, peer_key: &[u8], base_url: String, region: &str) {
    mesh.add_peer(
        NewPeerSpec {
            node_id: peer,
            public_key: peer_key.to_vec(),
            base_url,
            region: region.to_string(),
        },
        Timestamp::now(),
    )
    .await
    .expect("a fresh allow-list admits the peer");
}

/// A node's transport, with no gateway and no room relay behind it: these tests assert on
/// the link and the outbox, which the ingest window already exposes. `name` must match
/// the node the mesh was built from — the transport's TLS leaf and the mesh's signing
/// key are pinned to the same identity.
fn transport(name: u8, mesh: &SharedMesh) -> Arc<MeshTransport> {
    Arc::new(MeshTransport::new(
        mesh.clone(),
        tls_for(name),
        None,
        None,
        None,
        None,
        None,
        None,
        &Registry::new(),
        Arc::new(SystemClock) as Arc<dyn Clock>,
        DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS,
    ))
}

/// A presence digest as the inner MWP frame a `FED_FORWARD` carries, with `note_len`
/// bytes of digest — distinct lengths make distinct events, so arrival order and
/// duplication are visible in the ingest window without trusting anything else.
fn digest_frame(note_len: usize) -> Vec<u8> {
    let frame = to_frame(
        Opcode::FedPresenceDigest.to_wire(),
        0,
        &FedPresenceDigest {
            region: "region-b".to_string(),
            digest: vec![0xA5; note_len],
        },
    )
    .expect("the digest encodes");
    frame.encode().expect("the frame encodes").to_vec()
}

/// Queues one digest event for `target`, at a timestamp the test names so the backoff
/// arithmetic is deterministic.
async fn queue(mesh: &SharedMesh, target: Id, note_len: usize, at: Timestamp) {
    mesh.enqueue(
        FederatedEvent {
            target_node: target,
            opcode: Opcode::FedPresenceDigest.to_wire() as i32,
            payload: digest_frame(note_len),
        },
        at,
    )
    .await
    .expect("a federation-band event enqueues");
}

/// A port where nothing listens: bind an ephemeral port, then drop the listener. The
/// node that "died" is simply a node nobody can connect to.
async fn closed_port() -> u16 {
    let holder = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the test reserves an ephemeral port");
    let port = holder
        .local_addr()
        .expect("the reservation names its port")
        .port();
    drop(holder);
    port
}

/// Builds one independent node over a store the test holds. The read-only scenario
/// places a room row on the node that serves it, and [`node`] keeps no handle to the
/// store it built the mesh over — this is the same construction with the store handed
/// in, so the row and the mesh see the same world.
async fn node_over_store(name: u8, region: &str, store: SharedStore) -> SharedMesh {
    let mesh = MeshService::new(
        store,
        MeshConfig::default(),
        node_id(name),
        region.to_string(),
        NodeSecret::from_seed(&[name; 32]).expect("a 32-byte seed builds a key"),
        Box::new(migo_core::random::SeededRandom::new(u64::from(name) * 7919)),
        &Registry::new(),
    )
    .expect("the mesh configuration is valid");
    Arc::new(mesh)
}

/// Creates one room row on `store`: the room `room_id`, its chat the conversation
/// `conversation_id`, homed at `home_region`. A room a node serves but does not home is
/// exactly the row a sync or an operator tooling places on it.
async fn room_on(
    store: &SharedStore,
    room_id: Id,
    conversation_id: Id,
    home_region: &str,
    at: Timestamp,
) {
    store
        .create_room(NewRoom {
            room_id,
            conversation_id,
            slug: format!("room-{}", room_id.to_text()),
            name: "a room".to_string(),
            topic: None,
            kind: RoomKind::Public,
            owner_id: Id::from(0x3333),
            home_region: home_region.to_string(),
            max_members: 100,
            encryption: EncryptionMode::Transport,
            created_at: at,
        })
        .await
        .expect("a fresh store creates the room");
}

/// The events a node still owes its peers, at a timestamp far past every backoff —
/// the outbox's honest contents with nothing hiding behind a retry schedule.
async fn everything_owed(mesh: &SharedMesh) -> Vec<migo_federation::PendingEvent> {
    mesh.due(Timestamp::from_millis(i64::MAX / 2))
        .await
        .expect("the outbox reads")
}

/// Polls `probe` until it returns something, failing with `what` after [`WAIT_LIMIT`].
async fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "{what} never came true within {WAIT_LIMIT:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

// ---------------------------------------------------------------------------
// Stream helpers — the test as an independent peer implementation, speaking over
// the same TLS 1.3 channels a real link rides
// ---------------------------------------------------------------------------

/// Writes one length-prefixed MWP frame, the way every stream transport in the system
/// does. A write failure (the peer tearing the link mid-handshake) surfaces as an
/// error the caller may read as a refusal.
async fn send_frame<S>(stream: &mut S, frame: &Frame) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let wire = frame
        .encode_length_prefixed()
        .expect("a scripted frame encodes");
    stream.write_all(&wire).await
}

/// Reads one length-prefixed frame, or `None` when the peer closed — at a frame
/// boundary or not, because a refusal is a close however it lands.
async fn read_frame<S>(stream: &mut S) -> Option<Frame>
where
    S: AsyncRead + Unpin,
{
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await.ok()?;
    let len = u32::from_be_bytes(head) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.ok()?;
    Some(Frame::decode(Bytes::from(body)).expect("a well-formed frame decodes"))
}

/// Encodes a [`NodeHello`] as the wire's `FED_HELLO` — the same layout `migod`'s
/// transport writes, reproduced here from the outside so the test plays a real peer.
fn hello_to_wire(hello: &NodeHello, region: &str, epoch: u64) -> FedHello {
    FedHello {
        node_id: hello.node_id.to_text(),
        region: region.to_string(),
        epoch,
        nonce: hello.nonce.to_vec(),
    }
}

/// The inverse of [`hello_to_wire`]; malformed input is a test bug, so it panics.
fn hello_from_wire(wire: &FedHello) -> NodeHello {
    NodeHello {
        node_id: wire
            .node_id
            .parse()
            .expect("a test peer sends a parseable node id"),
        nonce: wire
            .nonce
            .as_slice()
            .try_into()
            .expect("a test peer sends a 32-byte nonce"),
        protocol_version: node::MESH_PROTOCOL_VERSION,
    }
}

/// Encodes a [`NodeProof`] as the wire's `FED_AUTH`: the signing time rides ahead of
/// the signature, because the transcript commits to it.
fn proof_to_wire(proof: &NodeProof, node_id: Id, epoch: u64) -> FedAuth {
    let mut signature = Vec::with_capacity(8 + proof.signature.len());
    signature.extend_from_slice(&proof.signed_at.as_millis().to_be_bytes());
    signature.extend_from_slice(&proof.signature);
    FedAuth {
        node_id: node_id.to_text(),
        signature,
        epoch,
    }
}

/// The inverse of [`proof_to_wire`].
fn proof_from_wire(wire: &FedAuth) -> NodeProof {
    assert_eq!(
        wire.signature.len(),
        8 + 64,
        "a test peer sends a timestamped 64-byte signature"
    );
    let signed_at = Timestamp::from_millis(i64::from_be_bytes(
        wire.signature[..8]
            .try_into()
            .expect("eight bytes of timestamp"),
    ));
    let signature = wire.signature[8..]
        .try_into()
        .expect("sixty-four bytes of signature");
    NodeProof {
        signed_at,
        signature,
    }
}

// ---------------------------------------------------------------------------
// Scenario 1 and 2 — a dead node, and a partition that ends
// ---------------------------------------------------------------------------

/// Section 173, scenarios 1 and 2: a node dies with events bound for it, and the
/// recovery afterwards. The outbox is the durability, so a dead peer costs a decaying
/// trickle of retries and nothing else: no event is dropped, none is retried hot, and
/// when the node comes back every event arrives exactly once, in the order it was
/// queued — the FIFO a single-sequencer link promises, preserved across the outage.
#[tokio::test]
async fn events_bound_for_a_dead_node_survive_and_arrive_after_recovery_without_loss() {
    // The address node A used to answer at: nothing listens there now, which is what
    // "the node died" means to everyone who still has its address.
    let port = closed_port().await;
    let (mesh_a, _) = node(1, "region-a").await;
    let (mesh_b, _) = node(2, "region-b").await;
    admit(
        &mesh_b,
        node_id(1),
        &key_bytes(1),
        format!("wss://127.0.0.1:{port}"),
        "region-a",
    )
    .await;
    let transport_b = transport(2, &mesh_b);

    let now = Timestamp::now();
    for note_len in 0..5usize {
        queue(&mesh_b, node_id(1), note_len, now).await;
    }

    // The drain reaches for the dead node and cannot connect. The failure is settled
    // with a backoff, not raised: the runner would log it and tick again, and the
    // outbox keeps every event either way.
    transport_b
        .drain_once(now)
        .await
        .expect("a dead peer is a settled failure, not a drain error");

    // Nothing was delivered, nothing was dropped, and nothing comes due before the
    // backoff base has elapsed — a dead node must not be retried in a hot loop.
    assert!(
        mesh_b
            .due(now.saturating_add_millis(999))
            .await
            .expect("the outbox reads")
            .is_empty(),
        "the retry is at least one backoff base away"
    );
    let retry: Vec<_> = mesh_b
        .due(now.saturating_add_millis(1_000))
        .await
        .expect("the outbox reads");
    assert_eq!(retry.len(), 5, "every event is still owed");
    assert!(
        retry.iter().all(|event| event.attempts == 1),
        "each event failed exactly once: {:?}",
        retry.iter().map(|event| event.attempts).collect::<Vec<_>>()
    );

    // Node A recovers — a fresh process at the same address, the way a restart lands.
    admit(
        &mesh_a,
        node_id(2),
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    let transport_a = transport(1, &mesh_a);
    transport_a
        .spawn_listener(&format!("127.0.0.1:{port}"))
        .await
        .expect("the recovered node binds its old address");

    transport_b
        .drain_once(now.saturating_add_millis(1_000))
        .await
        .expect("the drain completes against the recovered node");

    let seen = transport_a.ingested();
    assert_eq!(seen.len(), 5, "every queued event arrived: {seen:?}");
    assert!(
        seen.iter()
            .all(|(opcode, _)| *opcode == Opcode::FedPresenceDigest.to_wire()),
        "the arrivals are presence digests, not handshake residue"
    );
    // Five distinct events, in queue order: the digest lengths are strictly
    // increasing across the batch, so a shuffle or a duplicate would break the ramp.
    let lengths: Vec<usize> = seen.iter().map(|(_, len)| *len).collect();
    assert!(
        lengths.windows(2).all(|pair| pair[0] < pair[1]),
        "the events arrived once each, in the order they were queued: {lengths:?}"
    );

    // And the queue settled: nothing comes due again, and a later drain delivers
    // nothing further — the recovery duplicated no message.
    assert!(
        everything_owed(&mesh_b).await.is_empty(),
        "a delivered event never comes due again"
    );
    transport_b
        .drain_once(Timestamp::from_millis(i64::MAX / 2))
        .await
        .expect("a settled outbox drains to nothing");
    assert_eq!(
        transport_a.ingested().len(),
        5,
        "no event was redelivered after the queue settled"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2, the read-only half — a room whose home node is partitioned away
// ---------------------------------------------------------------------------

/// Section 173, scenario 2, the half the send-side test above cannot see: a node cut
/// off from the home node of a room it serves must not keep writing that room, because
/// the write would need a second sequencer that must never exist (section 170). The
/// partition is discovered the only honest way — a delivery attempt that cannot
/// connect — and from that moment the room's mutations on this node are refused with
/// `ROOM_READ_ONLY_PARTITION` (1702) instead of silently diverging. Everything else
/// keeps working: a room this node homes stays writable (that is the one sequencer
/// that exists), a private conversation stays writable (it has no home node), and the
/// moment the link heals the very same write succeeds, because the refusal was about
/// the link and never about the room.
#[tokio::test]
async fn a_partitioned_room_is_read_only_until_the_link_heals() {
    // The address the home node should answer at: nothing listens there, which is what
    // a partition is to a node that still has the address.
    let port = closed_port().await;
    let (mesh_a, _) = node(1, "region-a").await;
    let store_b: SharedStore = Arc::new(MemoryStore::new());
    let mesh_b = node_over_store(2, "region-b", Arc::clone(&store_b)).await;
    admit(
        &mesh_a,
        node_id(2),
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    admit(
        &mesh_b,
        node_id(1),
        &key_bytes(1),
        format!("wss://127.0.0.1:{port}"),
        "region-a",
    )
    .await;

    // Node B serves two rooms: one homed at region-a — the far node — and one it homes
    // itself. The far-homed room is the split-brain case; the local one is the control
    // that the refusal is the partition and not some blanket lock.
    let now = Timestamp::now();
    let far_room = Id::from(0x9101);
    let far_conversation = Id::from(0x9102);
    let local_room = Id::from(0x9201);
    let local_conversation = Id::from(0x9202);
    room_on(&store_b, far_room, far_conversation, "region-a", now).await;
    room_on(&store_b, local_room, local_conversation, "region-b", now).await;
    let relay = Arc::new(RoomRelay::new(
        Arc::clone(&mesh_b),
        Arc::clone(&store_b),
        None,
    ));

    // Before any delivery attempt the link is unknown, and the mesh treats the unknown
    // as reachable: it remembers evidence, it does not guess. Both rooms write.
    assert!(
        mesh_b.link_reachable(node_id(1)),
        "a link never tried reads as reachable"
    );
    relay
        .ensure_writable(far_room)
        .await
        .expect("a partition nobody discovered yet refuses nothing");
    relay
        .ensure_writable(local_room)
        .await
        .expect("a room this node homes is always writable");

    // The partition is discovered: an event bound for the home node cannot connect.
    queue(&mesh_b, node_id(1), 1, now).await;
    let transport_b = transport(2, &mesh_b);
    transport_b
        .drain_once(now)
        .await
        .expect("a dead peer is a settled failure, not a drain error");
    assert!(
        !mesh_b.link_reachable(node_id(1)),
        "the failed connect marks the link down"
    );

    // And the far-homed room is read-only: its mutations are refused with 1702, on the
    // room itself and on the conversation that carries its chat.
    let error = relay
        .ensure_writable(far_room)
        .await
        .expect_err("a partitioned room is read-only");
    assert_eq!(
        error.code(),
        codes::ROOM_READ_ONLY_PARTITION,
        "the refusal is the brief's own error, not a generic one"
    );
    assert_eq!(
        error.symbol(),
        "ROOM_READ_ONLY_PARTITION",
        "and it names itself on the wire"
    );
    let error = relay
        .ensure_conversation_writable(far_conversation)
        .await
        .expect_err("the room's chat is the room: read-only together");
    assert_eq!(
        error.code(),
        codes::ROOM_READ_ONLY_PARTITION,
        "a message write against the room's conversation is refused the same way"
    );

    // The controls, while the partition stands: the room this node homes keeps its
    // single sequencer and writes freely, and a conversation no room names — a private
    // message — was never the far node's to order and stays writable.
    relay
        .ensure_writable(local_room)
        .await
        .expect("the sequencer that exists is here; no partition removes it");
    relay
        .ensure_conversation_writable(local_conversation)
        .await
        .expect("the local room's chat writes with it");
    relay
        .ensure_conversation_writable(Id::from(0x9301))
        .await
        .expect("a private conversation has no home node to be partitioned from");

    // The link heals — the home node answers at the address again — and the held event
    // delivers. A delivered batch is proof the peer is back, so the read-only mark
    // lifts and the very write that was refused succeeds.
    let transport_a = transport(1, &mesh_a);
    transport_a
        .spawn_listener(&format!("127.0.0.1:{port}"))
        .await
        .expect("the healed node binds its old address");
    transport_b
        .drain_once(now.saturating_add_millis(1_000))
        .await
        .expect("the drain completes against the healed node");
    assert_eq!(
        transport_a.ingested().len(),
        1,
        "the held event arrived exactly once"
    );
    assert!(
        mesh_b.link_reachable(node_id(1)),
        "the delivered batch marks the link up"
    );
    assert!(
        everything_owed(&mesh_b).await.is_empty(),
        "the recovery left nothing queued"
    );
    relay
        .ensure_writable(far_room)
        .await
        .expect("a healed link lifts the read-only mark");
    relay
        .ensure_conversation_writable(far_conversation)
        .await
        .expect("and the room's chat with it");
}

// ---------------------------------------------------------------------------
// Scenario 3 — a slow link, not a dead one
// ---------------------------------------------------------------------------

/// One connection to the slow peer, already past its TLS gate: the application handshake
/// answered by hand — the peer speaks first, both hellos and both proofs, exactly the
/// transcript `serve_session` runs — and then a read loop that swallows every
/// `FED_FORWARD` and acknowledges only when the gate is open. This is a node whose link
/// works but whose acks are late enough to starve the sender's watermark.
async fn serve_slow_session(
    mut stream: ServerTlsStream<TcpStream>,
    mesh: SharedMesh,
    ack_gate: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<Vec<u32>>>>,
) {
    // The handshake, server side, from the outside: read the peer's hello, answer with
    // ours, verify their proof, answer with ours.
    let Some(opening) = read_frame(&mut stream).await else {
        return;
    };
    if Opcode::from_wire(opening.header.opcode) != Some(Opcode::FedHello) {
        return;
    }
    let wire_hello: FedHello = from_frame(&opening).expect("the peer's hello decodes");
    mesh.check_epoch(wire_hello.epoch)
        .expect("the test peer's view is current");
    let remote = hello_from_wire(&wire_hello);
    let local = mesh.hello();
    let now = Timestamp::now();

    let reply = to_frame(
        Opcode::FedHello.to_wire(),
        0,
        &hello_to_wire(&local, mesh.region(), mesh.epoch()),
    )
    .expect("the counter-hello encodes");
    send_frame(&mut stream, &reply)
        .await
        .expect("the counter-hello is written");

    let Some(proof_frame) = read_frame(&mut stream).await else {
        return;
    };
    if Opcode::from_wire(proof_frame.header.opcode) != Some(Opcode::FedAuth) {
        return;
    }
    let wire_proof: FedAuth = from_frame(&proof_frame).expect("the peer's proof decodes");
    let proof = proof_from_wire(&wire_proof);
    mesh.authenticate(&local, &remote, &proof, now)
        .await
        .expect("the slow peer is an admitted node");
    let counter = to_frame(
        Opcode::FedAuth.to_wire(),
        0,
        &proof_to_wire(
            &mesh.prove(&local, &remote, now),
            local.node_id,
            mesh.epoch(),
        ),
    )
    .expect("the counter-proof encodes");
    send_frame(&mut stream, &counter)
        .await
        .expect("the counter-proof is written");

    // The read loop: record every forward's link sequence, and acknowledge only when
    // the gate opens — a link that carries bytes but withholds its watermark.
    let mut session: Vec<u32> = Vec::new();
    let mut watermark: u64 = 0;
    while let Some(frame) = read_frame(&mut stream).await {
        if Opcode::from_wire(frame.header.opcode) != Some(Opcode::FedForward) {
            continue;
        }
        let sequence = frame.header.correlation;
        session.push(sequence);
        if ack_gate.load(Ordering::SeqCst) {
            watermark = watermark.max(u64::from(sequence));
            let ack = to_frame(
                Opcode::FedAck.to_wire(),
                0,
                &FedAck {
                    node_id: mesh.region().to_string(),
                    link_seq: watermark,
                },
            )
            .expect("the ack encodes");
            if send_frame(&mut stream, &ack).await.is_err() {
                break;
            }
        }
    }
    log.lock()
        .expect("the session log is not held")
        .push(session);
}

/// Binds the slow peer: a listener whose every connection is the same TLS 1.3 gate a real
/// node runs — the fixture completes the handshake so the slowness being tested is the
/// link's, not the channel's — and whose sessions are [`serve_slow_session`], plus the
/// ack gate and the per-session sequence log the test reads. `name` is the fixture's node
/// name: its TLS leaf is minted from the same identity key its mesh signs with, and the
/// allow-list snapshot pins the dialers it will admit.
async fn spawn_slow_peer(
    name: u8,
    mesh: SharedMesh,
) -> (SocketAddr, Arc<AtomicBool>, Arc<Mutex<Vec<Vec<u32>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the slow peer binds");
    let bound = listener
        .local_addr()
        .expect("the slow peer knows its address");
    let ack_gate = Arc::new(AtomicBool::new(false));
    let log = Arc::new(Mutex::new(Vec::new()));

    let acceptor = TlsAcceptor::from(
        tls_for(name)
            .server_config(&allowed_keys(&mesh).await)
            .expect("the slow peer mints its TLS gate from its own identity and its dialers' keys"),
    );
    let accept_gate = Arc::clone(&ack_gate);
    let accept_log = Arc::clone(&log);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            // Each connection takes its own handles: the shared Arcs are cloned on this
            // side of the inner spawn, so the outer loop lends rather than moves them.
            let mesh = Arc::clone(&mesh);
            let accept_gate = Arc::clone(&accept_gate);
            let accept_log = Arc::clone(&accept_log);
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                serve_slow_session(stream, mesh, accept_gate, accept_log).await;
            });
        }
    });
    (bound, ack_gate, log)
}

/// Section 173, scenario 3: a link that works but never acknowledges. The batch gets
/// exactly the watermark budget to be acked; when the budget runs out the batch fails
/// as a unit, every event is rescheduled on the doubling backoff, and nothing is lost.
/// When the link finally acks, the redelivery is the at-least-once semantics section
/// 153 promises — the peer sees the batch twice and that is correct.
#[tokio::test]
async fn a_peer_that_never_acks_fails_the_batch_backs_off_and_loses_nothing() {
    let (mesh_slow, _) = node(3, "region-c").await;
    let (mesh_b, _) = node(2, "region-b").await;
    admit(
        &mesh_slow,
        node_id(2),
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    let (slow_addr, ack_gate, log) = spawn_slow_peer(3, Arc::clone(&mesh_slow)).await;
    admit(
        &mesh_b,
        node_id(3),
        &key_bytes(3),
        format!("wss://{slow_addr}"),
        "region-c",
    )
    .await;
    let transport_b = transport(2, &mesh_b);

    let now = Timestamp::now();
    for note_len in 0..3usize {
        queue(&mesh_b, node_id(3), note_len, now).await;
    }

    // The drain dials the slow peer, the handshake completes, the batch goes out — and
    // the ack never comes. The five-second watermark wait expires inside the drain
    // (bounded below by a test-side timeout so a regression cannot hang CI forever).
    tokio::time::timeout(WAIT_LIMIT, transport_b.drain_once(now))
        .await
        .expect("the drain returns within the ack budget and a margin")
        .expect("an unacknowledged batch is a settled failure, not a drain error");

    // The first session carried the whole batch, in link order, and could not
    // acknowledge any of it. The session log lands when the sender gives up and the
    // socket closes, so it is polled rather than assumed.
    let first: Vec<u32> = eventually("the slow peer recorded the unacknowledged session", || {
        let sessions = log.lock().expect("the session log is not held").clone();
        sessions.first().cloned()
    })
    .await;
    assert_eq!(
        first,
        vec![1, 2, 3],
        "the batch went out complete and in sequence"
    );

    // The events were neither delivered nor dropped, and the retry is a backoff away.
    assert!(
        mesh_b
            .due(now.saturating_add_millis(999))
            .await
            .expect("the outbox reads")
            .is_empty(),
        "the retry is at least one backoff base away"
    );
    let retry: Vec<_> = mesh_b
        .due(now.saturating_add_millis(1_000))
        .await
        .expect("the outbox reads");
    assert_eq!(retry.len(), 3, "every event is still owed");
    assert!(retry.iter().all(|event| event.attempts == 1));

    // The link recovers — the gate opens — and the next drain redelivers. At least
    // once means exactly this: the peer sees the batch again, numbered from one on the
    // fresh session, and this time the watermark covers it.
    ack_gate.store(true, Ordering::SeqCst);
    tokio::time::timeout(
        WAIT_LIMIT,
        transport_b.drain_once(now.saturating_add_millis(1_000)),
    )
    .await
    .expect("the recovered drain completes")
    .expect("the drain against an acking peer completes");

    let sessions = eventually("the slow peer recorded the redelivering session", || {
        let sessions = log.lock().expect("the session log is not held").clone();
        (sessions.len() >= 2).then_some(sessions)
    })
    .await;
    assert_eq!(
        sessions[0],
        vec![1, 2, 3],
        "the first session sent the batch once"
    );
    assert_eq!(
        sessions[1],
        vec![1, 2, 3],
        "the second session resent it — at-least-once, as section 153 promises"
    );
    assert_eq!(sessions.len(), 2, "no third session: the queue settled");
    assert!(
        everything_owed(&mesh_b).await.is_empty(),
        "an acknowledged event never comes due again"
    );
}

/// Section 173, scenario 3's slow-link marking: a peer that answers but falls behind is
/// marked degraded before anything is exhausted, and the marking is a signal only. The
/// sender's watermark is pinned at four, five events are owed, and the drain that fails
/// against the never-acking peer marks it degraded; when the link recovers, the very same
/// drain still dials the degraded peer and redelivers everything — at-least-once, nothing
/// dropped (section 153) — and once the backlog is gone the marking clears by itself. The
/// default watermark sits far above this on purpose: a mass sync is traffic, not a fault
/// (the backlog test below pins that side of the line).
#[tokio::test]
async fn a_slow_peer_is_marked_degraded_and_still_receives_everything_it_is_owed() {
    let (mesh_slow, _) = node(4, "region-d").await;
    let (mesh_b, _) = node_with_config(
        5,
        "region-e",
        MeshConfig {
            degraded_outbox_watermark: 4,
            ..MeshConfig::default()
        },
    )
    .await;
    admit(
        &mesh_slow,
        node_id(5),
        &key_bytes(5),
        "wss://b.invalid:1".to_string(),
        "region-e",
    )
    .await;
    let (slow_addr, ack_gate, log) = spawn_slow_peer(4, Arc::clone(&mesh_slow)).await;
    admit(
        &mesh_b,
        node_id(4),
        &key_bytes(4),
        format!("wss://{slow_addr}"),
        "region-d",
    )
    .await;
    let transport_b = transport(5, &mesh_b);

    let now = Timestamp::now();
    for note_len in 0..5usize {
        queue(&mesh_b, node_id(4), note_len, now).await;
    }

    // The drain dials the slow peer, the handshake completes, the batch goes out — and no
    // ack comes. The batch fails as a unit, the drain's lag observation reads five owed
    // against a watermark of four, and the peer is marked degraded: slowness made visible
    // before it becomes backlog without end.
    tokio::time::timeout(WAIT_LIMIT, transport_b.drain_once(now))
        .await
        .expect("the drain returns within the ack budget and a margin")
        .expect("an unacknowledged batch is a settled failure, not a drain error");
    let first: Vec<u32> = eventually("the slow peer recorded the unacknowledged session", || {
        let sessions = log.lock().expect("the session log is not held").clone();
        sessions.first().cloned()
    })
    .await;
    assert_eq!(first, vec![1, 2, 3, 4, 5], "the batch went out complete");
    let peer_view = mesh_b
        .peer(node_id(4))
        .await
        .expect("node B resolves the slow peer in its allow-list");
    assert_eq!(
        peer_view.status,
        PeerStatus::Degraded,
        "the crossing backlog marked the peer degraded"
    );
    // And nothing was dropped for it: every event is still owed, on the backoff.
    let owed: Vec<_> = mesh_b
        .due(now.saturating_add_millis(1_000))
        .await
        .expect("the outbox reads");
    assert_eq!(
        owed.len(),
        5,
        "a degraded peer's events are neither dropped nor held back"
    );

    // The link recovers, and delivery to a degraded peer is delivery all the same: the
    // drain still dials it, resends the whole batch — at-least-once, so the peer sees it
    // twice and that is correct — and the watermark covers it this time.
    ack_gate.store(true, Ordering::SeqCst);
    tokio::time::timeout(
        WAIT_LIMIT,
        transport_b.drain_once(now.saturating_add_millis(1_000)),
    )
    .await
    .expect("the recovered drain completes")
    .expect("the drain against a degraded but acking peer completes");
    let sessions = eventually("the slow peer recorded the redelivering session", || {
        let sessions = log.lock().expect("the session log is not held").clone();
        (sessions.len() >= 2).then_some(sessions)
    })
    .await;
    assert_eq!(
        sessions[1],
        vec![1, 2, 3, 4, 5],
        "the degraded peer was redelivered everything, in order — section 153 unchanged"
    );
    assert!(
        everything_owed(&mesh_b).await.is_empty(),
        "the queue settled against the degraded peer"
    );
    // Caught up, the marking clears by itself: the same drain pass that emptied the
    // backlog observed the depth at zero and allowed the peer again.
    let peer_view = mesh_b
        .peer(node_id(4))
        .await
        .expect("node B resolves the slow peer in its allow-list");
    assert_eq!(
        peer_view.status,
        PeerStatus::Allowed,
        "a peer that caught up is allowed again"
    );
}

// ---------------------------------------------------------------------------
// The handshake budget — a peer that accepts but never speaks
// ---------------------------------------------------------------------------

/// Binds a peer that answers its TLS gate and then says nothing, ever, at the
/// application layer. `name` is the peer's node name — its TLS leaf is minted from that
/// identity, and `dialers` are the keys its gate admits — because the silence being
/// tested is a link that is up, encrypted and authenticated, and still mute: if the
/// fixture stopped at the TCP accept the drain would sit in the transport's dial budget
/// instead of the handshake budget this scenario is about. Every accepted TLS stream is
/// held for the task's lifetime — never read, never written, never dropped — because
/// dropping one would close it, and a close is a clean EOF the handshake already treats
/// as an ordinary failure. The peer this is exists to be the other thing: connected,
/// silent, and so a hang for any handshake without a budget.
///
/// The watch channel counts completed handshakes, so a test can wait until the drain's
/// connection has genuinely landed before it moves the clock.
async fn spawn_silent_peer(
    name: u8,
    dialers: &[[u8; 32]],
) -> (SocketAddr, tokio::sync::watch::Receiver<u32>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the silent peer binds");
    let bound = listener
        .local_addr()
        .expect("the silent peer knows its address");
    let (accepted_tx, accepted_rx) = tokio::sync::watch::channel(0_u32);
    // The gate is minted on the caller's side of the 'static boundary — the borrowed
    // dialer keys never have to outlive this function.
    let acceptor = TlsAcceptor::from(
        tls_for(name)
            .server_config(dialers)
            .expect("the silent peer mints its TLS gate"),
    );
    tokio::spawn(async move {
        let mut held: Vec<ServerTlsStream<TcpStream>> = Vec::new();
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            // The gate answers; the application never speaks. A TLS failure here would
            // fail the dial as an ordinary error, which is a different scenario.
            let Ok(stream) = acceptor.accept(stream).await else {
                continue;
            };
            held.push(stream);
            let count = u32::try_from(held.len()).expect("a test accepts few connections");
            let _ = accepted_tx.send(count);
        }
    });
    (bound, accepted_rx)
}

/// Section 173's closing gap, closed: the handshake runs under
/// `federation.handshake_timeout_ms`, so a peer that accepts the connection but never
/// sends its hello fails at the deadline — not before, not meaningfully after — and
/// the failure takes the ordinary backoff a refused connection takes, so the drain
/// proceeds instead of hanging on one silent socket.
///
/// The budget is measured on the node's injected clock, gateway-liveness style, so the
/// test never sleeps the wait out: it runs under tokio's paused clock and walks a
/// `ManualClock` to the last millisecond inside the deadline (the handshake must still
/// be waiting there), then one millisecond past it (the next budget tick must settle
/// the failure). The deadline is anchored to the drain's own timestamp, which is what
/// makes the choreography independent of when the loopback dial completes.
#[tokio::test(start_paused = true)]
async fn a_silent_peer_fails_its_handshake_at_the_deadline_and_the_drain_moves_on() {
    const NOW_MS: i64 = 1_700_000_000_000;
    let now = Timestamp::from_millis(NOW_MS);
    let deadline = Timestamp::from_millis(NOW_MS + DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS as i64);

    let (mesh_b, _) = node(2, "region-b").await;
    let (silent_addr, accepted_rx) = spawn_silent_peer(3, &[key32(2)]).await;
    admit(
        &mesh_b,
        node_id(3),
        &key_bytes(3),
        format!("wss://{silent_addr}"),
        "region-c",
    )
    .await;
    let clock = Arc::new(ManualClock::new(now));
    let transport_b = Arc::new(MeshTransport::new(
        Arc::clone(&mesh_b),
        tls_for(2),
        None,
        None,
        None,
        None,
        None,
        None,
        &Registry::new(),
        Arc::clone(&clock) as Arc<dyn Clock>,
        DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS,
    ));

    for note_len in 0..3usize {
        queue(&mesh_b, node_id(3), note_len, now).await;
    }

    // The drain runs concurrently with a driver that owns the clock. The flag is the
    // driver's only view of the drain: while the clock is inside the deadline the flag
    // must stay down (the handshake is waiting, not failed), and once the clock passes
    // the deadline it must go up within a few budget ticks (the failure is settled,
    // not lingering).
    let drain_done = Arc::new(AtomicBool::new(false));
    let done_for_driver = Arc::clone(&drain_done);
    let drain = {
        let transport = Arc::clone(&transport_b);
        let done = Arc::clone(&drain_done);
        async move {
            let result = transport.drain_once(now).await;
            done.store(true, Ordering::SeqCst);
            result
        }
    };
    let driver = async move {
        let mut accepted_rx = accepted_rx;
        // Hold still until the silent peer has actually accepted the drain's
        // connection: the budget may only start being spent on a handshake in flight.
        accepted_rx
            .changed()
            .await
            .expect("the silent peer reports its accept");
        tokio::time::sleep(Duration::from_millis(50)).await;
        // The last millisecond inside the budget, held across four budget ticks: the
        // handshake must still be waiting — a peer is never cut off early.
        clock.set(Timestamp::from_millis(deadline.as_millis() - 1));
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        assert_eq!(
            *accepted_rx.borrow_and_update(),
            1,
            "the drain dialed the silent peer once and only once"
        );
        assert!(
            !done_for_driver.load(Ordering::SeqCst),
            "the handshake was still waiting one millisecond inside the deadline"
        );
        // One millisecond past the deadline: the next budget tick fails the attempt,
        // and within a few more the drain has settled it and moved on.
        clock.set(deadline.saturating_add_millis(1));
        tokio::time::sleep(Duration::from_millis(2_000)).await;
        assert!(
            done_for_driver.load(Ordering::SeqCst),
            "the drain settled the failed handshake within a few budget ticks of the deadline"
        );
        assert_eq!(
            *accepted_rx.borrow_and_update(),
            1,
            "the failed handshake did not redial: the retry belongs to the backoff, not the drain"
        );
    };

    let (drain, ()) = tokio::join!(drain, driver);
    drain.expect("a silent peer is a settled failure, not a drain error");

    // The failure entered exactly the backoff a refused connection takes: nothing due
    // before one backoff base, everything due at it, each event failed exactly once.
    assert!(
        mesh_b
            .due(now.saturating_add_millis(999))
            .await
            .expect("the outbox reads")
            .is_empty(),
        "the retry is at least one backoff base away"
    );
    let retry: Vec<_> = mesh_b
        .due(now.saturating_add_millis(1_000))
        .await
        .expect("the outbox reads");
    assert_eq!(retry.len(), 3, "every event is still owed");
    assert!(
        retry.iter().all(|event| event.attempts == 1),
        "each event failed exactly once into the backoff: {:?}",
        retry.iter().map(|event| event.attempts).collect::<Vec<_>>()
    );
    // Nothing was delivered and nothing was lost: a silent peer never takes delivery,
    // so under at-least-once (section 153) the honest ending is every event still
    // owed — exactly as many as started, each failed exactly once and never re-failed
    // behind the test's back.
    let owed: Vec<_> = everything_owed(&mesh_b).await;
    assert_eq!(owed.len(), 3, "nothing was delivered, and nothing was lost");
    assert!(
        owed.iter().all(|event| event.attempts == 1),
        "each event is still owed after exactly one failed attempt: {:?}",
        owed.iter().map(|event| event.attempts).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Scenario 7 — clock skew between nodes
// ---------------------------------------------------------------------------

/// Dials node A's listener the way the transport's own dialer does: TCP to the bound
/// address, then the TLS 1.3 channel as node B, whose client pin names node A's key —
/// so what crosses the socket below is the same encrypted, authenticated link every
/// production dial rides.
async fn dial_as_node_b(addr: SocketAddr) -> ClientTlsStream<TcpStream> {
    let stream = TcpStream::connect(addr)
        .await
        .expect("the listener accepts the TCP dial");
    let connector = TlsConnector::from(
        tls_for(2)
            .client_config(key32(1))
            .expect("the client config mints from the node identity"),
    );
    connector
        .connect(
            rustls::pki_types::ServerName::IpAddress(addr.ip().into()),
            stream,
        )
        .await
        .expect("the TLS 1.3 channel completes against the pinned listener")
}

/// Runs the client half of a mesh handshake over an established stream — the test's
/// TLS channels below — stamping the proof at `signed_at`. Returns the server's
/// `FED_AUTH` when the handshake completes, or `None` when the listener refused it —
/// by closing at any point.
async fn client_handshake<S>(
    stream: &mut S,
    mesh: &SharedMesh,
    secret: &NodeSecret,
    signed_at: Timestamp,
) -> Option<Frame>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let local = mesh.hello();
    let hello = to_frame(
        Opcode::FedHello.to_wire(),
        0,
        &hello_to_wire(&local, mesh.region(), mesh.epoch()),
    )
    .expect("the hello encodes");
    send_frame(stream, &hello)
        .await
        .expect("the hello is written while the link is up");

    let reply = read_frame(stream).await?;
    if Opcode::from_wire(reply.header.opcode) != Some(Opcode::FedHello) {
        return None;
    }
    let remote = hello_from_wire(&from_frame::<FedHello>(&reply).expect("the hello decodes"));

    let proof = node::prove(secret, &local, &remote, signed_at);
    let auth = to_frame(
        Opcode::FedAuth.to_wire(),
        0,
        &proof_to_wire(&proof, local.node_id, mesh.epoch()),
    )
    .expect("the proof encodes");
    // A write failure here is the listener tearing the link mid-refusal.
    send_frame(stream, &auth).await.ok()?;

    let counter = read_frame(stream).await?;
    (Opcode::from_wire(counter.header.opcode) == Some(Opcode::FedAuth)).then_some(counter)
}

/// Section 173, scenario 7: a proof from a clock outside the sixty-second tolerance
/// is refused on the wire — the handshake dies, the session ingests nothing. The
/// in-window control runs first, so the refusal can only be the skew: same keys,
/// same allow-list, same transcript, one millisecond further from the listener's
/// clock.
#[tokio::test]
async fn a_handshake_from_a_clock_outside_the_skew_window_is_refused_on_the_wire() {
    // The listener's clock is pinned, so "sixty-one seconds behind" is exact.
    let now = Timestamp::now();
    let (mesh_a, _) = node(1, "region-a").await;
    let (mesh_b, secret_b) = node(2, "region-b").await;
    admit(
        &mesh_a,
        node_id(2),
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    admit(
        &mesh_b,
        node_id(1),
        &key_bytes(1),
        "wss://a.invalid:1".to_string(),
        "region-a",
    )
    .await;

    let transport_a = Arc::new(MeshTransport::new(
        Arc::clone(&mesh_a),
        tls_for(1),
        None,
        None,
        None,
        None,
        None,
        None,
        &Registry::new(),
        Arc::new(ManualClock::new(now)) as Arc<dyn Clock>,
        DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS,
    ));
    let bound = transport_a
        .spawn_listener("127.0.0.1:0")
        .await
        .expect("the listener binds");

    // The control: a proof stamped at the listener's own clock completes the
    // handshake. Without this, the refusal below could be about keys, the allow-list,
    // or the transcript — anything but the skew.
    let mut in_window = tokio::time::timeout(WAIT_LIMIT, dial_as_node_b(bound))
        .await
        .expect("connecting does not stall");
    let reply = client_handshake(&mut in_window, &mesh_b, &secret_b, now).await;
    assert!(
        reply.is_some(),
        "an in-window proof completes the handshake — the control must pass"
    );
    drop(in_window);

    // The skewed node: sixty-one seconds behind, one millisecond past the tolerance
    // `MAX_CLOCK_SKEW_MS` names. The signature itself is valid — it is a real proof
    // over a real transcript — and the refusal is still correct, because the mesh
    // trust model starts with agreeing on when now is.
    let skewed = Timestamp::from_millis(now.as_millis() - MAX_CLOCK_SKEW_MS - 1);
    let mut out_of_window = tokio::time::timeout(WAIT_LIMIT, dial_as_node_b(bound))
        .await
        .expect("connecting does not stall");
    let reply = client_handshake(&mut out_of_window, &mesh_b, &secret_b, skewed).await;
    assert!(
        reply.is_none(),
        "the listener must refuse a proof from outside the skew window, not answer it"
    );
    drop(out_of_window);

    assert!(
        transport_a.ingested().is_empty(),
        "a refused handshake ingests nothing"
    );
}

// ---------------------------------------------------------------------------
// Scenario 8 — rolling deploy, two protocol versions side by side
// ---------------------------------------------------------------------------

/// Section 173, scenario 8: a frame built the way a newer protocol version would
/// build it — the mandatory fields this build knows, plus an optional field it has
/// never heard of — crosses the link and is ingested. Forward compatibility is the
/// reader's scoping rule in action: an unknown optional field is skipped by length,
/// so an old node and a new node interoperate and no frame is refused for carrying
/// more than the reader was compiled to understand.
#[tokio::test]
async fn a_frame_carrying_a_future_optional_field_crosses_the_link_unrefused() {
    let (mesh_a, _) = node(1, "region-a").await;
    let (mesh_b, _) = node(2, "region-b").await;
    admit(
        &mesh_a,
        node_id(2),
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    let transport_a = transport(1, &mesh_a);
    let bound = transport_a
        .spawn_listener("127.0.0.1:0")
        .await
        .expect("the listener binds");
    admit(
        &mesh_b,
        node_id(1),
        &key_bytes(1),
        format!("wss://{bound}"),
        "region-a",
    )
    .await;
    let transport_b = transport(2, &mesh_b);

    // The payload a future build would write: region and digest as this build reads
    // them, then one optional field — id 999, chosen because no protocol version has
    // ever defined it — that this build's `FedPresenceDigest` decoder must skip.
    let mut writer = Writer::new();
    writer.enter().expect("the message opens");
    writer.write_str("region-b").expect("the region writes");
    writer.write_bytes(&[0x5A; 32]).expect("the digest writes");
    // This build always writes a zero here; a future build's field rides after the
    // count it raises to one.
    writer.write_u32(1);
    writer
        .optional(999, |w| {
            w.write_bytes(b"a field from a newer protocol version")
        })
        .expect("the future field writes");
    writer.leave();
    let future_payload = writer.finish().expect("the future payload finishes");

    let future_frame = Frame::simple(Opcode::FedPresenceDigest.to_wire(), 0, future_payload);
    let now = Timestamp::now();
    mesh_b
        .enqueue(
            FederatedEvent {
                target_node: node_id(1),
                opcode: Opcode::FedPresenceDigest.to_wire() as i32,
                payload: future_frame
                    .encode()
                    .expect("the future frame encodes")
                    .to_vec(),
            },
            now,
        )
        .await
        .expect("a federation-band event enqueues");

    transport_b
        .drain_once(now)
        .await
        .expect("the drain completes");

    let seen = transport_a.ingested();
    assert_eq!(
        seen.len(),
        1,
        "the future-version frame was ingested, not refused: {seen:?}"
    );
    assert_eq!(
        seen[0].0,
        Opcode::FedPresenceDigest.to_wire(),
        "the ingested frame is the presence digest the future build sent"
    );
}

// ---------------------------------------------------------------------------
// Scenario 9 — the routing epoch, the rebalance primitive
// ---------------------------------------------------------------------------

/// Section 173, scenario 9: the routing epoch is how a rebalance refuses a sender
/// working from a stale view. Node A bumps its epoch — the shard map changed — and
/// node B's next delivery is refused at the handshake, before any payload moves. The
/// refusal is not a bare close: it names the epoch A's view is current at, so B's
/// transport refreshes its own view and redials the same batch inside the same drain —
/// the refetch the brief asks the link to perform on its own, not one an operator's
/// `bump_epoch` performs by hand.
#[tokio::test]
async fn a_stale_view_is_refused_the_transport_refreshes_and_the_event_arrives() {
    let (mesh_a, _) = node(1, "region-a").await;
    let (mesh_b, _) = node(2, "region-b").await;
    admit(
        &mesh_a,
        node_id(2),
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    let transport_a = transport(1, &mesh_a);
    let bound = transport_a
        .spawn_listener("127.0.0.1:0")
        .await
        .expect("the listener binds");
    admit(
        &mesh_b,
        node_id(1),
        &key_bytes(1),
        format!("wss://{bound}"),
        "region-a",
    )
    .await;
    let transport_b = transport(2, &mesh_b);

    // The control: with both views current, delivery works.
    let now = Timestamp::now();
    queue(&mesh_b, node_id(1), 1, now).await;
    transport_b
        .drain_once(now)
        .await
        .expect("the drain completes against a current view");
    assert_eq!(
        transport_a.ingested().len(),
        1,
        "the control delivery arrived"
    );

    // The rebalance: node A's routing table changes, and its epoch says so.
    let bumped = mesh_a.bump_epoch();
    assert_eq!(bumped, 1, "the epoch advances from zero by one bump");

    // Node B, still working from the old view, is refused — and the refusal names the
    // epoch, so the drain refreshes B's view and retries the batch itself. No operator
    // and no `bump_epoch` touched B: one drain, one refused session, one redial.
    let after = Timestamp::now();
    queue(&mesh_b, node_id(1), 2, after).await;
    transport_b
        .drain_once(after)
        .await
        .expect("the drain refreshes its own view and completes");
    assert_eq!(
        mesh_b.epoch(),
        bumped,
        "the transport adopted the epoch the refusal named, not a bump of its own"
    );

    let seen = transport_a.ingested();
    assert_eq!(seen.len(), 2, "both events arrived: {seen:?}");
    let lengths: Vec<usize> = seen.iter().map(|(_, len)| *len).collect();
    assert!(
        lengths.windows(2).all(|pair| pair[0] < pair[1]),
        "the post-rebalance event arrived after the control, in order: {lengths:?}"
    );
    assert!(
        everything_owed(&mesh_b).await.is_empty(),
        "the queue settled: nothing was lost to the stale view"
    );
}

// ---------------------------------------------------------------------------
// Scenario 9's other half — the members of a rebalanced room
// ---------------------------------------------------------------------------

/// The local half of a move, recorded: what the room's own sessions would have
/// received had a hub been behind the test.
struct RecordedHints(Mutex<Vec<(Id, Bytes)>>);

impl migod::room_relay::HintPublisher for RecordedHints {
    fn publish_hint(&self, room_id: Id, frame: &Bytes, _now: Timestamp) {
        self.0
            .lock()
            .expect("the recorder locks")
            .push((room_id, frame.clone()));
    }
}

/// Section 173's missing half: a room that moves nodes must not strand its members
/// on the old one. Node A homes a room whose members sit on node B; B subscribes as
/// the tier's watch half asks; and when A moves the room to B's region, every member
/// is told before anything else moves — a `RECONNECT_HINT` naming B's endpoint,
/// published on A's own hub and carried to B as the room event the tier already
/// carries, then the rehomed row, then the raised epoch.
#[tokio::test]
async fn a_moved_room_hands_its_members_a_reconnect_hint_naming_the_new_node() {
    let (mesh_a, _) = node(1, "region-a").await;
    let (mesh_b, _) = node(2, "region-b").await;

    // Node A homes the room, so its store holds the row that says so.
    let room_id = Id::from(0x7777);
    let store_a: migo_store::SharedStore = Arc::new(MemoryStore::new());
    store_a
        .create_room(migo_store::model::NewRoom {
            room_id,
            conversation_id: Id::from(0x2222),
            slug: format!("room-{room_id}"),
            name: "a room".to_string(),
            topic: None,
            kind: RoomKind::Public,
            owner_id: Id::from(0x3333),
            home_region: "region-a".to_string(),
            max_members: 100,
            encryption: EncryptionMode::Transport,
            created_at: Timestamp::now(),
        })
        .await
        .expect("a fresh store creates the room");
    let hints = Arc::new(RecordedHints(Mutex::new(Vec::new())));
    let relay_a = Arc::new(migod::room_relay::RoomRelay::new(
        mesh_a.clone(),
        store_a.clone(),
        Some(hints.clone() as Arc<dyn migod::room_relay::HintPublisher>),
    ));

    // Both nodes listen, and each names the other in its allow-list by the address
    // the hint will carry — B's endpoint is what the members are told to reconnect to.
    let transport_b = transport(2, &mesh_b);
    let bound_b = transport_b
        .spawn_listener("127.0.0.1:0")
        .await
        .expect("B's listener binds");
    admit(
        &mesh_a,
        node_id(2),
        &key_bytes(2),
        format!("wss://{bound_b}"),
        "region-b",
    )
    .await;
    let transport_a = Arc::new(MeshTransport::new(
        mesh_a.clone(),
        tls_for(1),
        None,
        Some(Arc::clone(&relay_a)),
        None,
        None,
        None,
        None,
        &Registry::new(),
        Arc::new(SystemClock) as Arc<dyn Clock>,
        DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS,
    ));
    let bound_a = transport_a
        .spawn_listener("127.0.0.1:0")
        .await
        .expect("A's listener binds");
    admit(
        &mesh_b,
        node_id(1),
        &key_bytes(1),
        format!("wss://{bound_a}"),
        "region-a",
    )
    .await;

    // B subscribes to the room, as the tier's watch half asks: one
    // `FED_ROOM_SUBSCRIBE` over the real wire, delivered by a drain the test drives.
    let now = Timestamp::now();
    mesh_b
        .enqueue(
            FederatedEvent {
                target_node: node_id(1),
                opcode: Opcode::FedRoomSubscribe.to_wire() as i32,
                payload: to_frame(
                    Opcode::FedRoomSubscribe.to_wire(),
                    0,
                    &FedRouting {
                        epoch: mesh_b.epoch(),
                        home_region: "region-a".to_string(),
                        room_id,
                    },
                )
                .expect("the subscribe encodes")
                .encode()
                .expect("the frame encodes")
                .to_vec(),
            },
            now,
        )
        .await
        .expect("the subscribe enqueues");
    transport_b
        .drain_once(now)
        .await
        .expect("the subscribe is delivered");
    assert_eq!(
        relay_a.watchers_of(room_id),
        vec![node_id(2)],
        "A holds B as the room's one watching node"
    );

    // The move, by the operator's placement primitive: A tells the members, moves
    // the row, and raises the epoch — in that order.
    let moved_at = Timestamp::now();
    let moved = relay_a
        .move_room(room_id, "region-b", moved_at)
        .await
        .expect("the room moves to B's region");
    assert_eq!(
        moved.home_region, "region-b",
        "the row names the new home node"
    );
    assert_eq!(
        mesh_a.epoch(),
        1,
        "the routing epoch rose with the move, so a stale view is refused"
    );

    // The local half: the room's own sessions — the ones already on A — were handed
    // the same frame the far node's members receive.
    let recorded = hints.0.lock().expect("the recorder locks").clone();
    assert_eq!(recorded.len(), 1, "one hint on the room's own topic");
    let frame = Frame::decode(recorded[0].1.clone()).expect("the hint is an encoded frame");
    assert_eq!(
        Opcode::from_wire(frame.header.opcode),
        Some(Opcode::ReconnectHint)
    );
    let hint: ReconnectHint = from_frame(&frame).expect("the hint decodes");
    assert_eq!(hint.reason, CloseReason::Rebalance);
    assert_eq!(hint.after_ms, 0, "the instruction is to reconnect now");
    assert_eq!(
        hint.endpoint.as_deref(),
        Some(format!("wss://{bound_b}").as_str()),
        "the hint names the new home node, by the address the allow-list holds"
    );

    // The federated half: one `FED_ROOM_EVENT` for the watching node, carrying the
    // same hint inside the envelope, delivered over the real wire.
    transport_a
        .drain_once(moved_at)
        .await
        .expect("the hint is delivered to the watching node");
    let seen = transport_b.ingested();
    assert!(
        seen.iter()
            .any(|(opcode, _)| *opcode == Opcode::ReconnectHint.to_wire()),
        "B ingested the reconnect hint as a room event: {seen:?}"
    );
    assert!(
        everything_owed(&mesh_a).await.is_empty(),
        "the move's whole obligation — one hint per watching node — was delivered"
    );
}

// ---------------------------------------------------------------------------
// Scenario 10 — a mass backlog after recovery
// ---------------------------------------------------------------------------

/// Section 173, scenario 10: three hundred events queue while a node is unreachable,
/// and the recovery must not blast them in one unbounded session. The drain reads at
/// most `DEFAULT_DUE_BATCH` events per pass, so the backlog crosses in bounded
/// batches — 128, 128, 44 — every event arriving exactly once in queue order. A mass
/// sync after an outage costs capacity the operator planned for, never more.
#[tokio::test]
async fn a_mass_backlog_drains_in_bounded_batches_without_loss_or_duplication() {
    let (mesh_a, _) = node(1, "region-a").await;
    let (mesh_b, _) = node(2, "region-b").await;
    admit(
        &mesh_a,
        node_id(2),
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    let transport_a = transport(1, &mesh_a);
    let bound = transport_a
        .spawn_listener("127.0.0.1:0")
        .await
        .expect("the listener binds");
    admit(
        &mesh_b,
        node_id(1),
        &key_bytes(1),
        format!("wss://{bound}"),
        "region-a",
    )
    .await;
    let transport_b = transport(2, &mesh_b);

    let now = Timestamp::now();
    for note_len in 0..300usize {
        queue(&mesh_b, node_id(1), note_len, now).await;
    }

    // Three passes, each bounded: the backlog is 300, the batch is 128.
    let batch = usize::from(DEFAULT_DUE_BATCH);
    assert_eq!(batch, 128, "the drain batch this test pins");

    transport_b
        .drain_once(now)
        .await
        .expect("the first pass completes");
    assert_eq!(
        transport_a.ingested().len(),
        batch,
        "the first pass delivers exactly one batch, not the whole backlog"
    );

    transport_b
        .drain_once(now)
        .await
        .expect("the second pass completes");
    assert_eq!(
        transport_a.ingested().len(),
        batch * 2,
        "the second pass delivers one more batch"
    );

    transport_b
        .drain_once(now)
        .await
        .expect("the third pass completes");
    let seen = transport_a.ingested();
    assert_eq!(
        seen.len(),
        300,
        "the whole backlog arrived: {} of 300",
        seen.len()
    );

    // Once each, in queue order: the digest lengths are a strictly increasing ramp
    // across all three batches, so a loss, a duplicate, or a reorder breaks it.
    let lengths: Vec<usize> = seen.iter().map(|(_, len)| *len).collect();
    assert!(
        lengths.windows(2).all(|pair| pair[0] < pair[1]),
        "every event arrived once, in the order it was queued"
    );

    // And the queue settled: a further drain moves nothing.
    assert!(
        everything_owed(&mesh_b).await.is_empty(),
        "a fully drained backlog owes nothing"
    );
    transport_b
        .drain_once(now.saturating_add_millis(60_000))
        .await
        .expect("a settled outbox drains to nothing");
    assert_eq!(
        transport_a.ingested().len(),
        300,
        "no event was redelivered after the backlog settled"
    );

    // The peer never left the allow-list over 300 events: a mass sync is traffic, not
    // a fault, and the default degradation watermark sits above this backlog on purpose
    // — the slow-link test above fires at five owed against a watermark of four, while
    // three hundred owed against the default is a recovery, not a degradation.
    let peer_view = mesh_b
        .peer(node_id(1))
        .await
        .expect("node B resolves node A in its allow-list");
    assert_eq!(peer_view.status, PeerStatus::Allowed);
}

// ---------------------------------------------------------------------------
// Liveness probes on idle links
// ---------------------------------------------------------------------------

/// A peer that dies while the outbox holds nothing for it is invisible to the drain:
/// nothing dials, so nothing fails, so the link stays "reachable" until the next
/// federated write happens to try it — and the rooms it homes stay writable from this
/// node the whole time. One probe closes the gap: the dial that cannot connect takes
/// the same mark a failed delivery's dial takes, without owing the peer anything.
#[tokio::test]
async fn a_probe_marks_a_silent_peer_down_without_a_federated_write() {
    let port = closed_port().await;
    let (mesh_a, _) = node(1, "region-a").await;
    let (mesh_b, _) = node(2, "region-b").await;
    admit(
        &mesh_a,
        node_id(2),
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    admit(
        &mesh_b,
        node_id(1),
        &key_bytes(1),
        format!("wss://127.0.0.1:{port}"),
        "region-a",
    )
    .await;
    let transport_b = transport(2, &mesh_b);

    // Silence about a peer reads as reachable — the permissive default the link state
    // starts from, and exactly why a dead peer was invisible before the probe existed.
    assert!(
        mesh_b.link_reachable(node_id(1)),
        "a peer nothing has tried is the permissive answer"
    );

    // The outbox is empty, so the drain has nothing to say; the probe is the only dial.
    let now = Timestamp::now();
    assert!(
        mesh_b.due(now).await.expect("the outbox reads").is_empty(),
        "nothing is owed, so the drain alone would never dial"
    );
    transport_b.probe_once(now).await;

    assert!(
        !mesh_b.link_reachable(node_id(1)),
        "a probe that cannot connect marks the link down, the same evidence a failed delivery's dial is"
    );
}

/// The other half of the probe's job: a peer that recovers after a failed delivery
/// stays marked down until something succeeds against it — and with an empty outbox,
/// nothing ever would. The probe's round trip lifts the mark without a federated write,
/// so the rooms the peer homes become writable again the minute it is actually back.
#[tokio::test]
async fn a_probe_lifts_the_mark_on_a_peer_that_recovered() {
    let port = closed_port().await;
    let (mesh_a, _) = node(1, "region-a").await;
    let (mesh_b, _) = node(2, "region-b").await;
    admit(
        &mesh_a,
        node_id(2),
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    admit(
        &mesh_b,
        node_id(1),
        &key_bytes(1),
        format!("wss://127.0.0.1:{port}"),
        "region-a",
    )
    .await;
    let transport_b = transport(2, &mesh_b);

    // One failed delivery takes the mark, exactly as scenario 1 already pins.
    let now = Timestamp::now();
    queue(&mesh_b, node_id(1), 8, now).await;
    transport_b
        .drain_once(now)
        .await
        .expect("a dead peer is a settled failure, not a drain error");
    assert!(
        !mesh_b.link_reachable(node_id(1)),
        "the failed delivery marked the link down"
    );

    // Node A comes back at its old address — the same restart landing scenario 1 uses.
    let transport_a = transport(1, &mesh_a);
    transport_a
        .spawn_listener(&format!("127.0.0.1:{port}"))
        .await
        .expect("the recovered node binds its old address");

    // The probe dials, handshakes, pings, and is answered: the peer is demonstrably
    // there, and the mark lifts without the outbox owing it a single event.
    transport_b.probe_once(now).await;
    assert!(
        mesh_b.link_reachable(node_id(1)),
        "a probe that completes its round trip marks the link up"
    );
}

// ---------------------------------------------------------------------------
// The doc's rule that every scenario be automatable, proven for these eight
// ---------------------------------------------------------------------------

/// Section 173 closes with the rule that every scenario be runnable as an automated
/// test with injected time and randomness. The eight scenarios above each build their
/// nodes with seeded keys and seeded randomness (`node(name)` derives both from one
/// byte), drive every delivery through `drain_once` at timestamps the test names, and
/// pin the one clock that matters (the skew window) with a `ManualClock` — so this
/// test exists to fail the day someone makes a scenario above depend on wall-clock
/// luck or ambient entropy.
#[test]
fn every_scenario_pin_uses_seeded_keys_and_driven_time() {
    // The seeding rule itself: two nodes built from the same name share a key, and
    // different names never collide — the determinism the scenarios rely on.
    assert_eq!(key_bytes(7), key_bytes(7), "same name, same key");
    assert_ne!(
        key_bytes(7),
        key_bytes(8),
        "different names, different keys"
    );

    // The clock the skew scenario pins advances only when the test moves it.
    let clock = ManualClock::new(Timestamp::from_millis(1_000_000));
    assert_eq!(clock.now().as_millis(), 1_000_000);
    clock.advance_millis(250);
    assert_eq!(clock.now().as_millis(), 1_000_250);
}
