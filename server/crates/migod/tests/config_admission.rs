//! Configuration-driven peer admission, proven over a real loopback mesh link.
//!
//! What this test proves end to end: the `federation.peers` entries an operator writes —
//! node id, mesh address, Ed25519 public key, region — are the whole of what it takes to
//! link two nodes. [`apply_mesh_peers`] is the same function [`App::build`](migod::App)
//! runs once per entry after the mesh opens, so what passes here is what a deployment
//! gets from configuration alone, with no programmatic `add_peer` anywhere in the flow.
//!
//! The link the entries produce is real: a listener bound on a loopback port, a runner
//! dialing it, the full `FED_HELLO`/`FED_AUTH` handshake, one sequence-numbered
//! `FED_FORWARD`, and the server's `FED_ACK` watermark — the same wire path
//! `federation_link.rs` drives, reached here through the configuration surface instead
//! of the trait method.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use migo_core::config::{MeshPeer, DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS};
use migo_core::metrics::Registry;
use migo_core::{Clock, Id, SystemClock, Timestamp};
use migo_crypto::NodeSecret;
use migo_federation::{FederatedEvent, MeshConfig, MeshService, SharedMesh};
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

/// The Ed25519 public key of node `name`, base64-encoded the way an operator would paste
/// it into the configuration document.
fn config_key(name: u8) -> String {
    base64::engine::general_purpose::STANDARD.encode(key_bytes(name))
}

/// The raw Ed25519 public key bytes of node `name`.
fn key_bytes(name: u8) -> Vec<u8> {
    NodeSecret::from_seed(&[name; 32])
        .expect("a 32-byte seed builds a key")
        .public()
        .to_bytes()
        .to_vec()
}

/// A `federation.peers` entry naming node `name`, exactly as it would be typed in the
/// configuration: the canonical text form of the id, the base64 key, the mesh address,
/// and the region the node itself claims.
fn entry(name: u8, base_url: &str) -> MeshPeer {
    MeshPeer {
        node_id: Id::from(u128::from(name) * 0x0101).to_text(),
        public_key: config_key(name),
        base_url: base_url.to_string(),
        region: format!("region-{name}"),
    }
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

/// The whole product, from the configuration document to the wire: two nodes whose only
/// knowledge of each other is what their `federation.peers` entries say, linked for real.
#[tokio::test]
async fn configured_peers_link_two_nodes_over_a_real_mesh() {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    // Node A listens first, because node B's configuration entry must name the address
    // the listener actually bound.
    let mesh_a = bare_mesh(1, "region-a").await;
    let transport_a = Arc::new(migod::mesh::MeshTransport::new(
        mesh_a.clone(),
        None,
        None,
        None,
        None,
        None,
        &Registry::new(),
        clock.clone(),
        DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS,
    ));
    let bound = transport_a
        .spawn_listener("127.0.0.1:0")
        .await
        .expect("the listener binds");

    let mesh_b = bare_mesh(2, "region-b").await;

    // Node A's configuration names node B once — the fresh admission.
    let a_id = Id::from(0x0101);
    migod::apply_mesh_peers(&mesh_a, a_id, &[entry(2, "wss://b.invalid:1")], clock.now())
        .await
        .expect("node A applies its configured peer");

    // Node B's configuration carried a stale address for node A — the entry a previous
    // deployment wrote. Applying it admits the peer; applying the corrected entry brings
    // the row to the address that now serves, which is the convergence a fixed
    // configuration must produce on the next boot.
    let b_id = Id::from(0x0202);
    let stale = vec![entry(1, "wss://old-a.invalid:1")];
    migod::apply_mesh_peers(&mesh_b, b_id, &stale, clock.now())
        .await
        .expect("node B applies the stale entry it booted with");
    let corrected = vec![entry(1, &format!("wss://{bound}"))];
    migod::apply_mesh_peers(&mesh_b, b_id, &corrected, clock.now())
        .await
        .expect("node B applies the corrected entry");
    let view = mesh_b
        .peer(a_id)
        .await
        .expect("node B resolves node A in its allow-list");
    assert_eq!(view.base_url, format!("wss://{bound}"));

    // A restart of node B re-applies the same, now-correct entry: a no-op, not an
    // ALREADY_EXISTS failure — this is the boot-loop every redeploy relies on.
    let before = mesh_b
        .peer(a_id)
        .await
        .expect("the peer row exists before the re-apply");
    migod::apply_mesh_peers(&mesh_b, b_id, &corrected, clock.now())
        .await
        .expect("an identical re-apply is a no-op");
    let after = mesh_b
        .peer(a_id)
        .await
        .expect("the peer row exists after the re-apply");
    assert_eq!(
        before.added_at, after.added_at,
        "a restart must not re-stamp when the peer was admitted"
    );

    let transport_b = Arc::new(migod::mesh::MeshTransport::new(
        mesh_b.clone(),
        None,
        None,
        None,
        None,
        None,
        &Registry::new(),
        clock.clone(),
        DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS,
    ));
    transport_b.spawn_runner(clock.clone());

    // Queue one digest for node A, then wait for it to land on the other side.
    mesh_b
        .enqueue(
            FederatedEvent {
                target_node: a_id,
                opcode: Opcode::FedPresenceDigest.to_wire() as i32,
                payload: digest_frame("region-b", b"presence admitted by configuration").to_vec(),
            },
            clock.now(),
        )
        .await
        .expect("a federation-band event enqueues");

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
            "the digest never reached node A over the configured link; ingested so far: \
             {seen:?}; queue still holding: {queue:?}"
        );
        sleep(POLL).await;
    }

    // And the sender's queue settled: the watermark covered the batch, so the event never
    // comes due again.
    let later = clock.now().saturating_add_millis(60_000);
    assert!(
        mesh_b.due(later).await.expect("the queue reads").is_empty(),
        "a delivered event never comes due again"
    );
}

/// Admission refuses what the operator did not name: an entry that is not a node id, a
/// key that is not an Ed25519 point, or a node pointing at itself — and a refused entry
/// admits nothing.
#[tokio::test]
async fn a_malformed_or_self_referential_entry_fails_and_admits_nothing() {
    let mesh = bare_mesh(1, "region-a").await;
    let own_id = Id::from(0x0101);

    // A node id that is not the canonical 26-character text form.
    let mut bad_id = entry(2, "wss://b.invalid:1");
    bad_id.node_id = "not-a-node-id".to_string();
    let error = migod::apply_mesh_peers(&mesh, own_id, &[bad_id], Timestamp::now())
        .await
        .expect_err("a malformed node id fails the boot");
    assert!(
        error.to_string().contains("node id"),
        "the error names the problem: {error}"
    );

    // A key that does not decode to 32 bytes of Ed25519 public key.
    let mut bad_key = entry(2, "wss://b.invalid:1");
    bad_key.public_key = base64::engine::general_purpose::STANDARD.encode([1u8; 3]);
    let error = migod::apply_mesh_peers(&mesh, own_id, &[bad_key], Timestamp::now())
        .await
        .expect_err("a short key fails the boot");
    assert!(
        error.to_string().contains("public key"),
        "the error names the problem: {error}"
    );

    // An entry naming this node itself: always a configuration mistake.
    let error = migod::apply_mesh_peers(
        &mesh,
        own_id,
        &[entry(1, "wss://a.invalid:1")],
        Timestamp::now(),
    )
    .await
    .expect_err("a self-referential entry fails the boot");
    assert!(
        error.to_string().contains("itself"),
        "the error names the problem: {error}"
    );

    // Nothing was admitted along the way.
    let peers = mesh.peers(100).await.expect("the allow-list reads");
    assert!(
        peers.is_empty(),
        "a refused entry admits nothing, saw {peers:?}"
    );
}
