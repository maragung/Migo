//! A queued federation event crossing a real TCP link between two independent nodes.
//!
//! What this test proves end to end: an event queued in node B's durable outbox reaches
//! node A over a real loopback socket — the handshake (both `FED_HELLO`s, both `FED_AUTH`
//! proofs over the transcript), one sequence-numbered `FED_FORWARD`, the server's
//! cumulative `FED_ACK` watermark, and node B's outbox settling so the event never comes
//! due again. It drives the real [`MeshTransport`] tasks — node A's listener and node B's
//! runner — not a mirror of them, so a regression in the transport's wire behavior lands
//! here and not in a copy that still passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use migo_core::metrics::Registry;
use migo_core::{Id, SystemClock, Timestamp};
use migo_crypto::NodeSecret;
use migo_federation::{FederatedEvent, MeshConfig, MeshService, NewPeerSpec, SharedMesh};
use migo_protocol::{to_frame, Opcode};
use migo_store::MemoryStore;
use tokio::time::sleep;

/// How long the test waits for the link to carry the event before failing.
const WAIT_LIMIT: Duration = Duration::from_secs(10);

/// How often the test re-checks a condition it is waiting for.
const POLL: Duration = Duration::from_millis(100);

/// Builds a mesh service for one node, with nothing in its allow-list yet.
async fn bare_mesh(name: u8, region: &str) -> SharedMesh {
    let mesh = MeshService::new(
        Arc::new(MemoryStore::new()),
        MeshConfig::default(),
        Id::from(u128::from(name) * 0x0101),
        region.to_string(),
        NodeSecret::from_seed(&[name; 32]).expect("a 32-byte seed builds a key"),
        Box::new(migo_core::random::SeededRandom::new(u64::from(name) * 7919)),
        &Registry::new(),
    )
    .expect("the mesh configuration is valid");
    Arc::new(mesh)
}

/// Admits `peer` to the allow-list, naming where its listener is and which key signs for it.
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

/// The presence digest the test ships across the link, encoded as the inner MWP frame a
/// `FED_FORWARD` carries.
fn digest_frame(region: &str, note: &[u8]) -> bytes::Bytes {
    to_frame(
        Opcode::FedPresenceDigest.to_wire(),
        0,
        &migo_protocol::FedPresenceDigest {
            region: region.to_string(),
            digest: note.to_vec(),
        },
    )
    .expect("the digest encodes")
    .encode()
    .expect("the frame encodes")
}

/// The whole product, over a real socket: node B's runner delivers its queued digest to
/// node A's listener, node A ingests it, and node B's outbox goes quiet.
#[tokio::test]
async fn an_outbox_event_flows_from_one_node_to_another_over_real_tcp() {
    // Node A listens; its transport carries the ingest side.
    let mesh_a = bare_mesh(1, "region-a").await;
    let transport_a = Arc::new(migod::mesh::MeshTransport::new(
        mesh_a.clone(),
        None,
        &Registry::new(),
        Arc::new(SystemClock) as Arc<dyn migo_core::Clock>,
    ));
    let bound = transport_a
        .spawn_listener("127.0.0.1:0")
        .await
        .expect("the listener binds");
    assert_ne!(bound.port(), 0, "port zero binds an ephemeral port");

    // Node B dials node A: its allow-list entry carries the bound address, so the runner
    // knows where the queue drains to.
    let mesh_b = bare_mesh(2, "region-b").await;
    let a_id = Id::from(0x0101);
    let b_id = Id::from(0x0202);
    let key_a = NodeSecret::from_seed(&[1u8; 32])
        .expect("a 32-byte seed builds a key")
        .public()
        .to_bytes();
    let key_b = NodeSecret::from_seed(&[2u8; 32])
        .expect("a 32-byte seed builds a key")
        .public()
        .to_bytes();
    admit(
        &mesh_a,
        b_id,
        &key_b,
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    admit(&mesh_b, a_id, &key_a, format!("wss://{bound}"), "region-a").await;
    let transport_b = Arc::new(migod::mesh::MeshTransport::new(
        mesh_b.clone(),
        None,
        &Registry::new(),
        Arc::new(SystemClock) as Arc<dyn migo_core::Clock>,
    ));
    transport_b.spawn_runner(Arc::new(SystemClock) as Arc<dyn migo_core::Clock>);

    // Queue one digest for node A, then wait for it to land on the other side. The stamp
    // is the node's own clock scale, not a wall-clock epoch: `Timestamp` counts from the
    // protocol's custom epoch, so a fixed 2023 millisecond value would land in 2077 and
    // the event would sit "not yet due" for fifty years.
    let now = Timestamp::now();
    mesh_b
        .enqueue(
            FederatedEvent {
                target_node: a_id,
                opcode: Opcode::FedPresenceDigest.to_wire() as i32,
                payload: digest_frame("region-b", b"presence across the mesh").to_vec(),
            },
            now,
        )
        .await
        .expect("a federation-band event enqueues");

    // The peer the runner will dial must be there and allowed, or nothing below matters.
    let peer_view = mesh_b
        .peer(a_id)
        .await
        .expect("node B resolves node A in its allow-list");
    assert_eq!(peer_view.status, migo_federation::PeerStatus::Allowed);

    // One deterministic pass first — a drain the test drives itself — so a failure here
    // is the transport's, not the tick's timing.
    transport_b
        .drain_once(Timestamp::now())
        .await
        .expect("the outbox drain completes");

    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let seen = transport_a.ingested();
        if seen
            .iter()
            .any(|(opcode, _)| *opcode == Opcode::FedPresenceDigest.to_wire())
        {
            break;
        }
        let queue = mesh_b
            .due(Timestamp::from_millis(i64::MAX / 2))
            .await
            .unwrap_or_default();
        assert!(
            Instant::now() < deadline,
            "the digest never reached node A over the link; ingested so far: {seen:?}; \
             queue still holding: {queue:?}"
        );
        sleep(POLL).await;
    }

    // And the sender's queue settled: the watermark covered the batch, so the event never
    // comes due again.
    let later = now.saturating_add_millis(60_000);
    assert!(
        mesh_b.due(later).await.expect("the queue reads").is_empty(),
        "a delivered event never comes due again"
    );
}
