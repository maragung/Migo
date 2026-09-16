//! The TLS 1.3 channel on the mesh links, proven the way section 7 states it: every
//! server-to-server connection is encrypted and mutually authenticated, with no
//! plaintext mode and no peer the allow-list does not name.
//!
//! These tests sit beside the delivery suites (`federation_link.rs`,
//! `node_failures.rs`) on purpose: those prove the link's *behaviour* — delivery,
//! ordering, budgets — over the channel this file pins down. What this file proves is
//! the channel itself, in three parts:
//!
//! * a meshed pair completes both halves — the TLS 1.3 handshake and the application
//!   handshake inside it — and delivers events, and the negotiated protocol version
//!   really is TLS 1.3;
//! * a dialer whose certificate key is not in the listener's allow-list is refused
//!   *inside the TLS handshake*, before a single mesh byte is exchanged;
//! * a plaintext client cannot speak to the listener at all: the listener answers
//!   nothing that is not a TLS 1.3 handshake, so MWP frames never ride a bare socket.

use std::sync::Arc;
use std::time::{Duration, Instant};

use migo_core::config::DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS;
use migo_core::metrics::Registry;
use migo_core::{Id, SystemClock, Timestamp};
use migo_crypto::NodeSecret;
use migo_federation::{FederatedEvent, MeshConfig, MeshService, NewPeerSpec, SharedMesh};
use migo_protocol::{to_frame, Opcode};
use migo_store::MemoryStore;
use migod::mesh::MeshTransport;
use migod::mesh_tls::MeshTls;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::sleep;

/// How long a test waits for an asynchronous condition before failing.
const WAIT_LIMIT: Duration = Duration::from_secs(10);

/// How often a waiting test re-checks the condition.
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

/// The TLS 1.3 identity for one node, minted from the same seed its mesh service signs
/// with — the pairing the pin depends on, because the certificate is a carrier for the
/// identity key the peer's allow-list names.
fn tls_for(name: u8) -> MeshTls {
    MeshTls::from_secret(&NodeSecret::from_seed(&[name; 32]).expect("a 32-byte seed builds a key"))
        .expect("the node identity key mints a TLS leaf")
}

/// The public key of `node(name)` in the fixed-width form the TLS pin takes.
fn key32(name: u8) -> [u8; 32] {
    NodeSecret::from_seed(&[name; 32])
        .expect("a 32-byte seed builds a key")
        .public()
        .to_bytes()
}

/// The public key of `node(name)` as the raw bytes an allow-list entry stores.
fn key_bytes(name: u8) -> Vec<u8> {
    key32(name).to_vec()
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

/// The presence digest the tests ship across the link, encoded as the inner MWP frame a
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

/// A node's listener, bound on an ephemeral loopback port with nothing behind the
/// transport but the mesh — the same shape every link test in this suite uses.
async fn listening_node(
    name: u8,
    region: &str,
) -> (SharedMesh, Arc<MeshTransport>, std::net::SocketAddr) {
    let mesh = bare_mesh(name, region).await;
    let transport = Arc::new(MeshTransport::new(
        mesh.clone(),
        tls_for(name),
        None,
        None,
        None,
        None,
        None,
        None,
        &Registry::new(),
        Arc::new(SystemClock) as Arc<dyn migo_core::Clock>,
        DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS,
    ));
    let bound = transport
        .spawn_listener("127.0.0.1:0")
        .await
        .expect("the listener binds");
    (mesh, transport, bound)
}

/// Section 7's link, whole: node B's runner delivers its queued digest to node A over a
/// real loopback socket wrapped in the mesh's mandatory TLS 1.3 channel — the mutual
/// certificate pin, then the application handshake, then the delivery — and a direct
/// dial in the same test reads the negotiated version off the connection itself, so
/// "TLS 1.3" is asserted against the channel, not assumed from the configuration.
#[tokio::test]
async fn a_meshed_pair_delivers_events_over_a_channel_that_negotiates_tls_1_3() {
    let (mesh_a, transport_a, bound) = listening_node(1, "region-a").await;
    let a_id = Id::from(0x0101);
    let b_id = Id::from(0x0202);

    let mesh_b = bare_mesh(2, "region-b").await;
    admit(
        &mesh_a,
        b_id,
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;
    admit(
        &mesh_b,
        a_id,
        &key_bytes(1),
        format!("wss://{bound}"),
        "region-a",
    )
    .await;
    let transport_b = Arc::new(MeshTransport::new(
        mesh_b.clone(),
        tls_for(2),
        None,
        None,
        None,
        None,
        None,
        None,
        &Registry::new(),
        Arc::new(SystemClock) as Arc<dyn migo_core::Clock>,
        DEFAULT_FEDERATION_HANDSHAKE_TIMEOUT_MS,
    ));
    transport_b.spawn_runner(Arc::new(SystemClock) as Arc<dyn migo_core::Clock>);

    let now = Timestamp::now();
    mesh_b
        .enqueue(
            FederatedEvent {
                target_node: a_id,
                opcode: Opcode::FedPresenceDigest.to_wire() as i32,
                payload: digest_frame("region-b", b"presence over the pinned channel").to_vec(),
            },
            now,
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
            "the digest never reached node A over the TLS link; ingested so far: {seen:?}; \
             queue still holding: {queue:?}"
        );
        sleep(POLL).await;
    }

    // The sender's queue settled: the watermark covered the batch.
    let later = now.saturating_add_millis(60_000);
    assert!(
        mesh_b.due(later).await.expect("the queue reads").is_empty(),
        "a delivered event never comes due again"
    );

    // And the channel itself: a direct dial of the same pinned client answers with TLS
    // 1.3 or not at all — the configs offer no other version, and this reads what the
    // two sides actually agreed on rather than what they were built to offer.
    let tcp = TcpStream::connect(bound)
        .await
        .expect("the listener accepts the TCP dial");
    let connector = tokio_rustls::TlsConnector::from(
        tls_for(2)
            .client_config(key32(1))
            .expect("the client config mints from the node identity"),
    );
    let mut channel = connector
        .connect(
            rustls::pki_types::ServerName::IpAddress(bound.ip().into()),
            tcp,
        )
        .await
        .expect("the pinned TLS channel completes");
    assert_eq!(
        channel.get_ref().1.protocol_version(),
        Some(rustls::ProtocolVersion::TLSv1_3),
        "a mesh link negotiates TLS 1.3 and nothing else"
    );
    drop(channel);
}

/// A dialer whose certificate key the listener's allow-list does not name is refused
/// inside the TLS handshake: node A admits node B only, and node C — whose client pin
/// is correct, whose application identity is real, whose key simply was never admitted
/// — cannot complete the channel. No `FED_HELLO` crosses the wire, and node A's ingest
/// window stays empty, because the refusal happened a layer below the mesh.
#[tokio::test]
async fn a_dialer_whose_key_is_not_in_the_allow_list_is_refused_at_the_tls_handshake() {
    let (mesh_a, transport_a, bound) = listening_node(1, "region-a").await;

    // Node A's allow-list names node B's key and nothing else.
    admit(
        &mesh_a,
        Id::from(0x0202),
        &key_bytes(2),
        "wss://b.invalid:1".to_string(),
        "region-b",
    )
    .await;

    // Node C dials, pinned to node A's key: the client's own verification succeeds, so
    // the only thing that can fail — and must — is the listener rejecting C's leaf.
    let tcp = TcpStream::connect(bound)
        .await
        .expect("the listener accepts the TCP dial");
    let connector = tokio_rustls::TlsConnector::from(
        tls_for(3)
            .client_config(key32(1))
            .expect("the client config mints from the node identity"),
    );
    let refused = tokio::time::timeout(
        WAIT_LIMIT,
        connector.connect(
            rustls::pki_types::ServerName::IpAddress(bound.ip().into()),
            tcp,
        ),
    )
    .await
    .expect("the refusal is prompt, not a hang");
    assert!(
        refused.is_err(),
        "a dialer outside the allow-list must not complete the TLS handshake"
    );

    // And nothing of the mesh ran behind the refusal: the server settled its side of
    // the handshake while the client was being refused, so a grace period is enough to
    // see that no session formed and nothing was ingested.
    sleep(POLL).await;
    assert!(
        transport_a.ingested().is_empty(),
        "a refused TLS handshake must ingest nothing, saw {:?}",
        transport_a.ingested()
    );
}

/// A plaintext client cannot speak to the listener: the accept loop hands every
/// connection to the TLS gate before a single mesh byte is read, so a raw TCP client
/// writing MWP-shaped bytes gets the connection closed on it and the ingest window
/// stays empty. This is the no-plaintext-mode half of section 7 — there is no
/// configuration that turns the gate off, and this test is what keeps it honest.
#[tokio::test]
async fn a_plaintext_client_cannot_speak_to_the_listener() {
    let (_mesh_a, transport_a, bound) = listening_node(1, "region-a").await;

    // A raw socket playing a mesh client: TCP only, then a length-prefixed frame a
    // plaintext world would have accepted — a `FED_HELLO` header's worth of bytes.
    let mut plain = TcpStream::connect(bound)
        .await
        .expect("the listener accepts the TCP dial");
    let fake_hello = digest_frame("region-b", b"plaintext must not pass");
    let mut wire = (fake_hello.len() as u32).to_be_bytes().to_vec();
    wire.extend_from_slice(&fake_hello);
    plain.write_all(&wire).await.expect("the bytes go out");

    // The gate answers a non-TLS ClientHello the only way it can: the connection ends
    // within the handshake budget — a close, a reset, or a few bytes of TLS alert — and
    // nothing the client can read back is a mesh frame.
    let mut answered = Vec::new();
    tokio::time::timeout(WAIT_LIMIT, plain.read_to_end(&mut answered))
        .await
        .expect("the gate settles the connection promptly, not by holding it open");
    // A TLS alert is at most a few bytes; a mesh frame is a length-prefixed body. Either
    // way the client never receives anything it could mistake for MWP.
    assert!(
        answered.len() < 16,
        "the plaintext client got {answered:?} back, which is not a mesh answer"
    );

    // And behind the gate: nothing ingested, ever.
    sleep(POLL).await;
    assert!(
        transport_a.ingested().is_empty(),
        "a plaintext client must never reach the ingest path, saw {:?}",
        transport_a.ingested()
    );
}
