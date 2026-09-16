//! Federation, tested at the boundary where one operator's mistake becomes another's incident.
//!
//! This is the one crate where a stranger's bytes are allowed to reach local state, so almost
//! every invariant here is a defence that fails silently and expensively when it regresses —
//! a green screen and a compromised mesh look identical until someone reads the logs.
//!
//! **A peer is authenticated before it is believed, and in that order.** A proof signed by the
//! wrong key, over the wrong domain, for the wrong exchange, or replayed from an earlier one
//! must all be refused — and refused *before* any row is stamped or any nonce is recorded, or
//! the check becomes a way to poison the very state it guards. The single most valuable test
//! in the file is that a peer cannot present an identity whose key it does not hold: the
//! `node_id` a handshake resolves to is the one it *proved*, and the region and URL it resolves
//! to come from the local allow-list row, never from anything the peer put on the wire.
//!
//! **Every rejection looks identical from the outside.** An unknown node, a paused one, a
//! blocked one, and a bad signature all return the one opaque `MESH_AUTH_FAILED` with no public
//! detail, because the gap between "I do not know you" and "your signature was wrong" is an
//! existence oracle a probing peer must not have. The metrics still tell the reasons apart,
//! because an operator watching the mesh is not the adversary — but the peer learns nothing.
//!
//! **The replay defences are stateful, ordered, and gap-aware.** A nonce seen twice inside the
//! window is refused; a per-link sequence must advance by exactly one, a non-advancing number
//! is a replay to drop, and a *gap* is a suspected replay that tears the link down rather than
//! being quietly skipped (section 152).
//!
//! **Failure is bounded and the outbox is durable.** A failed delivery reschedules on an
//! exponential backoff without losing the event or blocking the queue; an id for a queued
//! event is minted locally, never taken from a caller; an unbounded batch is clamped.
//!
//! **A slow peer is marked, not muted.** When the undelivered events aimed at one peer grow
//! past a watermark, the peer is marked degraded — section 173's demand that slowness be
//! visible before it exhausts anything — and the marking changes nothing about delivery:
//! the peer still handshakes, still receives every event owed to it, and the marking clears
//! by itself once the peer catches up. The operator's paused and blocked are never touched
//! by the automatic transitions, in either direction.
//!
//! **No series and no error names a node.** Section 174 forbids a metric labelled by account,
//! and this crate widens that to node, peer, and region: one test renders the whole registry
//! and reads it for any id, URL, region, or fingerprint, and asserts every refusal reason is
//! still a fixed enum label an operator can chart.

use std::sync::Arc;

use migo_core::metrics::Registry;
use migo_core::{Id, Result, SeededRandom, Timestamp};
use migo_crypto::node::{self, MAX_CLOCK_SKEW_MS, MESH_DOMAIN, MESH_PROTOCOL_VERSION, NONCE_LEN};
use migo_crypto::{NodeHello, NodeProof, NodeSecret};
use migo_federation::model::{
    DEFAULT_BACKOFF_BASE_MS, DEFAULT_BACKOFF_CAP_MS, DEFAULT_DEGRADED_OUTBOX_WATERMARK,
    DEFAULT_DUE_BATCH, DEFAULT_MAX_ATTEMPTS, DEFAULT_NONCE_WINDOW_MS,
};
use migo_federation::{
    FederatedEvent, Mesh, MeshConfig, MeshService, NewPeerSpec, PeerIdentity, PeerStatus, PeerView,
    PendingEvent, SequenceVerdict, CONVERSATION_OPCODE_MAX, CONVERSATION_OPCODE_MIN,
    FEDERATION_OPCODE_MAX, FEDERATION_OPCODE_MIN, ROW_OPCODE_MAX, ROW_OPCODE_MIN,
};
use migo_protocol::codes;
use migo_store::traits::FederationStore;
use migo_store::MemoryStore;

// --- Time and identity helpers. -------------------------------------------

/// A fixed base instant, in epoch milliseconds. Every handshake signs a timestamp, so tests
/// pin one clock rather than reading the wall clock and racing the skew window.
const NOW: i64 = 1_700_000_000_000;

/// This node's own id. Deliberately far from any peer id a test mints, because a peer claiming
/// our own id is a reflection the crypto layer refuses on its own — that is not the mesh
/// behaviour under test here.
const OUR_NODE: u128 = 9_000_000;

/// This node's region, chosen distinct from any peer region so a test can tell a value that
/// came from local configuration apart from one that arrived on the wire.
const OUR_REGION: &str = "atlantis";

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

/// A 32-byte handshake nonce filled with one byte. A real nonce is random, but a test only
/// needs two properties: that it is 32 bytes, and that two nonces it wants to differ do — both
/// of which a fill byte gives while staying reproducible. The peer's nonce is never equal to
/// this node's random one, so the reflection check never trips by accident.
fn nonce(fill: u8) -> [u8; NONCE_LEN] {
    [fill; NONCE_LEN]
}

// --- A peer, as the test controls it. -------------------------------------

/// A federation peer the test plays the part of: its own signing key, and the identity fields
/// the operator would have typed when admitting it. The public key is derived from the secret,
/// so a proof this peer signs verifies against the key its allow-list row will hold.
struct Peer {
    node_id: Id,
    secret: NodeSecret,
    public_key: Vec<u8>,
    region: String,
    base_url: String,
}

/// A peer with a deterministic key derived from its id, so a run replays byte for byte and two
/// distinct ids never collide on a key (which the store would reject as a duplicate identity).
fn peer(n: u128) -> Peer {
    let mut rng = SeededRandom::new(0xBEEF_0000 + n as u64);
    let secret = NodeSecret::generate(&mut rng);
    let public_key = secret.public().to_bytes().to_vec();
    Peer {
        node_id: id(n),
        secret,
        public_key,
        region: format!("region-{n}"),
        base_url: format!("https://peer-{n}.mesh.example"),
    }
}

fn spec_of(p: &Peer) -> NewPeerSpec {
    NewPeerSpec {
        node_id: p.node_id,
        public_key: p.public_key.clone(),
        base_url: p.base_url.clone(),
        region: p.region.clone(),
    }
}

/// The peer's opening hello, with a nonce the test chose.
fn remote_hello(p: &Peer, nonce: [u8; NONCE_LEN]) -> NodeHello {
    NodeHello {
        node_id: p.node_id,
        nonce,
        protocol_version: MESH_PROTOCOL_VERSION,
    }
}

/// The proof the peer signs for a completed exchange, from the peer's point of view: its own
/// hello is the signer, ours is the counterparty.
fn peer_proof(p: &Peer, peer_hello: &NodeHello, our_hello: &NodeHello, at: Timestamp) -> NodeProof {
    node::prove(&p.secret, peer_hello, our_hello, at)
}

/// The exact byte layout of a mesh transcript, reproduced here so a test can sign one with a
/// *different* domain separator and prove the domain is bound. It must track
/// `migo_crypto::node::transcript`; the positive-control test below signs with the real
/// `MESH_DOMAIN` and asserts the resulting proof still authenticates, which fails loudly if the
/// layout ever drifts.
fn transcript_with_domain(
    domain: &[u8],
    signer: Id,
    signer_nonce: &[u8; NONCE_LEN],
    peer_id: Id,
    peer_nonce: &[u8; NONCE_LEN],
    signed_at: Timestamp,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(domain);
    out.extend_from_slice(&MESH_PROTOCOL_VERSION.to_be_bytes());
    out.extend_from_slice(signer.as_bytes());
    out.extend_from_slice(peer_id.as_bytes());
    out.extend_from_slice(signer_nonce);
    out.extend_from_slice(peer_nonce);
    out.extend_from_slice(&signed_at.as_millis().to_be_bytes());
    out
}

// --- The harness. ---------------------------------------------------------

/// Everything a test needs: the real service over the real in-memory store, and the registry
/// the service published its counters into. The network transport is never involved — this
/// crate opens no sockets, and a handshake is three values passed by hand — so there is no
/// socket to fake, only a peer whose key the test holds.
struct Harness {
    mesh: MeshService<MemoryStore>,
    store: Arc<MemoryStore>,
    registry: Registry,
    node_id: Id,
}

/// Builds a service, returning the construction error rather than panicking, so the tests that
/// pin the configuration guard can assert on it.
fn build(config: MeshConfig, region: &str) -> Result<MeshService<MemoryStore>> {
    let store = Arc::new(MemoryStore::new());
    let registry = Registry::new();
    let mut key_rng = SeededRandom::new(0x0A);
    let secret = NodeSecret::generate(&mut key_rng);
    MeshService::new(
        store,
        config,
        id(OUR_NODE),
        region.to_string(),
        secret,
        Box::new(SeededRandom::new(0x5151)),
        &registry,
    )
}

impl Harness {
    fn new() -> Self {
        Self::with_config(MeshConfig::default())
    }

    fn with_config(config: MeshConfig) -> Self {
        let store = Arc::new(MemoryStore::new());
        let registry = Registry::new();
        let mut key_rng = SeededRandom::new(0x0A);
        let secret = NodeSecret::generate(&mut key_rng);
        let mesh = MeshService::new(
            Arc::clone(&store),
            config,
            id(OUR_NODE),
            OUR_REGION.to_string(),
            secret,
            Box::new(SeededRandom::new(0x5151)),
            &registry,
        )
        .expect("the configuration under test is valid");
        Self {
            mesh,
            store,
            registry,
            node_id: id(OUR_NODE),
        }
    }

    /// This node's opening hello, drawn from the service's own randomness.
    fn local_hello(&self) -> NodeHello {
        self.mesh.hello()
    }

    /// Admits a peer to the allow-list, allowed.
    async fn admit(&self, p: &Peer) -> PeerView {
        self.mesh
            .add_peer(spec_of(p), ts(NOW))
            .await
            .expect("a fresh peer is admitted")
    }

    /// Admits a peer and then moves it to `status`.
    async fn admit_as(&self, p: &Peer, status: PeerStatus) -> PeerView {
        let view = self.admit(p).await;
        if status == PeerStatus::Allowed {
            view
        } else {
            self.mesh
                .set_peer_status(p.node_id, status)
                .await
                .expect("an admitted peer's status can be set")
        }
    }

    /// A full, honest handshake from `p` at `at`, using nonce `fill`. The result is whatever
    /// the service decided — success or the one opaque failure.
    async fn authenticate(&self, p: &Peer, fill: u8, at: Timestamp) -> Result<PeerIdentity> {
        let our = self.local_hello();
        let theirs = remote_hello(p, nonce(fill));
        let proof = peer_proof(p, &theirs, &our, at);
        self.mesh.authenticate(&our, &theirs, &proof, at).await
    }

    /// The stored peer view as the operator reads it, by node id.
    async fn peer_view(&self, node_id: Id) -> Option<PeerView> {
        match self.mesh.peer(node_id).await {
            Ok(view) => Some(view),
            Err(error) if error.code() == codes::NOT_FOUND => None,
            Err(other) => panic!("unexpected error reading a peer: {other}"),
        }
    }

    fn counter(&self, name: &'static str, labels: &[(&str, &str)]) -> u64 {
        self.registry.counter(name, "", labels).get()
    }

    fn plain(&self, name: &'static str) -> u64 {
        self.counter(name, &[])
    }

    /// Count of handshakes refused for a given reason label.
    fn rejected(&self, reason: &'static str) -> u64 {
        self.counter(
            "migo_federation_handshake_rejected_total",
            &[("reason", reason)],
        )
    }

    /// Count of packets refused by the replay defences for a given reason label.
    fn replay(&self, reason: &'static str) -> u64 {
        self.counter(
            "migo_federation_replay_rejected_total",
            &[("reason", reason)],
        )
    }
}

#[track_caller]
fn expect_code<T>(result: Result<T>, code: u32) {
    let error = result.err().expect("this call must be refused");
    assert_eq!(
        error.code(),
        code,
        "expected code {code}, got {}: {error}",
        error.code()
    );
}

// ===========================================================================
// Construction: the deployment guard.
//
// A misconfigured mesh is caught once, at construction, not per request. The nonce window in
// particular must outlast twice the accepted clock skew, or a replay slips through the gap
// between the timestamp check and the nonce memory — the one config error that would silently
// reopen the replay hole these tests exist to keep shut.
// ===========================================================================

#[tokio::test]
async fn the_default_configuration_builds() {
    assert!(build(MeshConfig::default(), OUR_REGION).is_ok());
}

#[tokio::test]
async fn a_nonce_window_exactly_twice_the_skew_is_accepted() {
    // The bound is `< 2 * skew`, so exactly twice the skew is the smallest window that is not
    // refused: the boundary is inclusive on the safe side.
    let config = MeshConfig {
        nonce_window_ms: 2 * MAX_CLOCK_SKEW_MS,
        ..MeshConfig::default()
    };
    assert!(build(config, OUR_REGION).is_ok());
}

#[tokio::test]
async fn a_nonce_window_one_below_twice_the_skew_is_refused() {
    // One millisecond under the bound is the config error that would let a captured handshake
    // be replayed in the window the nonce memory has already forgotten.
    let config = MeshConfig {
        nonce_window_ms: 2 * MAX_CLOCK_SKEW_MS - 1,
        ..MeshConfig::default()
    };
    expect_code(build(config, OUR_REGION), codes::INTERNAL_ERROR);
}

#[tokio::test]
async fn an_empty_region_is_refused_at_construction() {
    expect_code(build(MeshConfig::default(), "   "), codes::INTERNAL_ERROR);
}

#[tokio::test]
async fn a_non_positive_backoff_base_is_refused() {
    let config = MeshConfig {
        backoff_base_ms: 0,
        ..MeshConfig::default()
    };
    expect_code(build(config, OUR_REGION), codes::INTERNAL_ERROR);
}

#[tokio::test]
async fn a_backoff_cap_below_the_base_is_refused() {
    let config = MeshConfig {
        backoff_base_ms: 10_000,
        backoff_cap_ms: 9_999,
        ..MeshConfig::default()
    };
    expect_code(build(config, OUR_REGION), codes::INTERNAL_ERROR);
}

#[tokio::test]
async fn a_zero_drain_batch_is_refused() {
    let config = MeshConfig {
        due_batch: 0,
        ..MeshConfig::default()
    };
    expect_code(build(config, OUR_REGION), codes::INTERNAL_ERROR);
}

#[tokio::test]
async fn the_region_is_reported_from_local_configuration() {
    // Never put on the wire; the layer above reads it to say where this node sits.
    let h = Harness::new();
    assert_eq!(h.mesh.region(), OUR_REGION);
}

// ===========================================================================
// Invariant 1: a peer is authenticated before it is believed.
// ===========================================================================

#[tokio::test]
async fn an_honest_handshake_from_an_allowed_peer_succeeds() {
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    let identity = h
        .authenticate(&p, 1, ts(NOW))
        .await
        .expect("an allowed peer with a valid proof authenticates");
    assert_eq!(identity.node_id, p.node_id);
    assert_eq!(h.plain("migo_federation_handshakes_total"), 1);
}

#[tokio::test]
async fn a_handshake_from_a_node_not_in_the_allow_list_is_refused() {
    // The allow-list is the boundary of the mesh (section 170): a node the operator never named
    // does not get an anonymous connection, it gets refused before its proof is examined.
    let h = Harness::new();
    let stranger = peer(404);
    expect_code(
        h.authenticate(&stranger, 1, ts(NOW)).await,
        codes::MESH_AUTH_FAILED,
    );
    assert_eq!(h.rejected("unknown_peer"), 1);
}

#[tokio::test]
async fn a_handshake_with_a_signature_from_the_wrong_key_is_refused() {
    // The peer is in the allow-list, but the proof is signed by a key that is not the one the
    // allow-list holds for it. Its own published key is what a signature is checked against.
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    let our = h.local_hello();
    let theirs = remote_hello(&p, nonce(1));
    // An impostor signs the very same exchange with a different key.
    let impostor = peer(2);
    let proof = node::prove(&impostor.secret, &theirs, &our, ts(NOW));
    expect_code(
        h.mesh.authenticate(&our, &theirs, &proof, ts(NOW)).await,
        codes::MESH_AUTH_FAILED,
    );
    assert_eq!(h.rejected("proof_invalid"), 1);
}

#[tokio::test]
async fn a_proof_signed_over_a_different_domain_is_refused() {
    // Domain separation: the peer signs the exact transcript layout, with the peer's real key,
    // but under a different domain label. Verification reconstructs it under the mesh domain, so
    // the only difference — the domain — is what makes it fail. Without domain binding, a node
    // key reused by another subsystem would be a cross-protocol forgery.
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    let our = h.local_hello();
    let theirs = remote_hello(&p, nonce(1));
    let bytes = transcript_with_domain(
        b"migo-mesh-v2-attacker",
        theirs.node_id,
        &theirs.nonce,
        our.node_id,
        &our.nonce,
        ts(NOW),
    );
    let proof = NodeProof {
        signed_at: ts(NOW),
        signature: p.secret.sign(&bytes),
    };
    expect_code(
        h.mesh.authenticate(&our, &theirs, &proof, ts(NOW)).await,
        codes::MESH_AUTH_FAILED,
    );
    assert_eq!(h.rejected("proof_invalid"), 1);
}

#[tokio::test]
async fn the_same_layout_under_the_real_domain_authenticates() {
    // The positive control for the domain test and for the hand-built transcript: sign the same
    // bytes with the genuine `MESH_DOMAIN` and the handshake must succeed, which proves the
    // refusal above was the domain and not a layout mistake in the test helper.
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    let our = h.local_hello();
    let theirs = remote_hello(&p, nonce(1));
    let bytes = transcript_with_domain(
        MESH_DOMAIN,
        theirs.node_id,
        &theirs.nonce,
        our.node_id,
        &our.nonce,
        ts(NOW),
    );
    let proof = NodeProof {
        signed_at: ts(NOW),
        signature: p.secret.sign(&bytes),
    };
    assert!(h
        .mesh
        .authenticate(&our, &theirs, &proof, ts(NOW))
        .await
        .is_ok());
}

#[tokio::test]
async fn a_replayed_nonce_is_refused_even_within_the_clock_window() {
    // A captured handshake, resent while its timestamp is still fresh. The clock check would
    // pass it; the nonce memory is what refuses it the second time.
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    let our = h.local_hello();
    let theirs = remote_hello(&p, nonce(7));
    let proof = peer_proof(&p, &theirs, &our, ts(NOW));
    assert!(h
        .mesh
        .authenticate(&our, &theirs, &proof, ts(NOW))
        .await
        .is_ok());
    // Same nonce, same still-valid proof, a moment later.
    expect_code(
        h.mesh
            .authenticate(&our, &theirs, &proof, ts(NOW + 1))
            .await,
        codes::MESH_AUTH_FAILED,
    );
    assert_eq!(h.replay("nonce_reused"), 1);
}

#[tokio::test]
async fn a_proof_whose_timestamp_is_outside_the_skew_window_is_refused() {
    // A stale or future-dated proof: the timestamp is signed, so it cannot be slid to stay
    // fresh, and one outside the band is refused.
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    let our = h.local_hello();
    let theirs = remote_hello(&p, nonce(1));
    let signed_at = ts(NOW - MAX_CLOCK_SKEW_MS - 1);
    let proof = peer_proof(&p, &theirs, &our, signed_at);
    expect_code(
        h.mesh.authenticate(&our, &theirs, &proof, ts(NOW)).await,
        codes::MESH_AUTH_FAILED,
    );
    assert_eq!(h.rejected("proof_invalid"), 1);
}

// ===========================================================================
// Invariant 2: a peer cannot speak for a node it does not host.
//
// This is the most valuable pair of tests in the file. A handshake resolves to an identity
// only after a proof verifies against the key the *allow-list* holds for the claimed id, and
// the region and URL that identity carries are read from the local row, never from the wire.
// A peer that holds key A therefore cannot present itself as node B, and cannot describe
// itself as living anywhere other than where the operator recorded.
// ===========================================================================

#[tokio::test]
async fn a_peer_cannot_authenticate_as_another_peer_whose_key_it_lacks() {
    let h = Harness::new();
    let victim = peer(1);
    let attacker = peer(2);
    h.admit(&victim).await;
    h.admit(&attacker).await;

    // The attacker announces the victim's node id but can only sign with its own key.
    let our = h.local_hello();
    let lying = NodeHello {
        node_id: victim.node_id,
        nonce: nonce(9),
        protocol_version: MESH_PROTOCOL_VERSION,
    };
    let proof = node::prove(&attacker.secret, &lying, &our, ts(NOW));
    expect_code(
        h.mesh.authenticate(&our, &lying, &proof, ts(NOW)).await,
        codes::MESH_AUTH_FAILED,
    );
    // The victim's row is untouched: impersonation writes nothing to the account it targeted.
    assert!(h
        .peer_view(victim.node_id)
        .await
        .expect("the victim is still in the allow-list")
        .last_seen_at
        .is_none());
    assert_eq!(h.rejected("proof_invalid"), 1);
}

#[tokio::test]
async fn a_resolved_identity_takes_its_region_and_url_from_the_local_row() {
    // The hello a peer sends carries only its node id and a nonce — it structurally cannot
    // claim a region or a URL. Both come from the allow-list row the operator wrote, so nothing
    // a peer says about where it lives is believed.
    let h = Harness::new();
    let p = peer(1);
    let admitted = h.admit(&p).await;
    let identity = h
        .authenticate(&p, 1, ts(NOW))
        .await
        .expect("an allowed peer authenticates");
    assert_eq!(identity.node_id, p.node_id);
    assert_eq!(identity.region, admitted.region);
    assert_eq!(identity.base_url, admitted.base_url);
}

// ===========================================================================
// Invariant 3: nothing a peer sends is trusted as an identity claim.
// ===========================================================================

#[tokio::test]
async fn last_seen_is_stamped_only_after_a_proof_verifies() {
    let h = Harness::new();
    let p = peer(1);
    // Admitted, but never yet handshaked: no last-seen.
    h.admit(&p).await;
    assert!(h.peer_view(p.node_id).await.unwrap().last_seen_at.is_none());
    // A verified handshake is the only thing that stamps it.
    h.authenticate(&p, 1, ts(NOW)).await.expect("authenticates");
    assert_eq!(
        h.peer_view(p.node_id).await.unwrap().last_seen_at,
        Some(ts(NOW))
    );
}

#[tokio::test]
async fn a_failed_handshake_never_stamps_last_seen() {
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    // A proof from the wrong key.
    let our = h.local_hello();
    let theirs = remote_hello(&p, nonce(1));
    let forged = node::prove(&peer(2).secret, &theirs, &our, ts(NOW));
    expect_code(
        h.mesh.authenticate(&our, &theirs, &forged, ts(NOW)).await,
        codes::MESH_AUTH_FAILED,
    );
    assert!(h.peer_view(p.node_id).await.unwrap().last_seen_at.is_none());
}

#[tokio::test]
async fn admitting_a_node_id_already_present_is_refused_without_overwriting() {
    // A peer's identity is not something a second admission may quietly replace.
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    // A second spec for the same id but a different key.
    let other_key = peer(2).public_key;
    let clash = NewPeerSpec {
        node_id: p.node_id,
        public_key: other_key.clone(),
        base_url: "https://elsewhere.example".to_string(),
        region: "elsewhere".to_string(),
    };
    expect_code(h.mesh.add_peer(clash, ts(NOW)).await, codes::ALREADY_EXISTS);
    // The original row is intact: same key, same URL.
    let view = h.peer_view(p.node_id).await.unwrap();
    assert_eq!(view.base_url, p.base_url);
}

#[tokio::test]
async fn admitting_a_key_already_claimed_by_another_node_is_refused() {
    // The key, not the id, is the identity a handshake is checked against, so two nodes cannot
    // share one — a collision on a key is a collision on an identity.
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    let clash = NewPeerSpec {
        node_id: id(2),
        public_key: p.public_key.clone(),
        base_url: "https://twin.example".to_string(),
        region: "twin".to_string(),
    };
    expect_code(h.mesh.add_peer(clash, ts(NOW)).await, codes::ALREADY_EXISTS);
}

// --- Config-driven admission: the restart path. ----------------------------
//
// `add_peer` is the programmatic join; `apply_peer` is what the composition root runs
// once per `federation.peers` entry on every boot. The contract those boots rely on:
// absent means admit, identical means write nothing, changed means converge — and a
// runtime status is never part of the reconciliation, because pausing and blocking are
// operator decisions that must survive a restart.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn applying_an_absent_peer_admits_it_as_add_peer_would() {
    let h = Harness::new();
    let p = peer(1);
    let view = h
        .mesh
        .apply_peer(spec_of(&p), ts(NOW))
        .await
        .expect("a fresh entry is admitted");
    assert_eq!(view.node_id, p.node_id);
    assert_eq!(view.status, PeerStatus::Allowed);
    assert_eq!(h.plain("migo_federation_peers_added_total"), 1);
}

#[tokio::test]
async fn applying_an_unchanged_entry_again_writes_nothing() {
    let h = Harness::new();
    let p = peer(1);
    let first = h
        .mesh
        .apply_peer(spec_of(&p), ts(NOW))
        .await
        .expect("the first boot admits the peer");
    // The next boot applies the very same entry: the row is untouched, not re-added,
    // so a restart converges instead of failing with ALREADY_EXISTS.
    let second = h
        .mesh
        .apply_peer(spec_of(&p), ts(NOW + 60_000))
        .await
        .expect("an identical entry is a no-op");
    assert_eq!(
        second.added_at, first.added_at,
        "a restart must not re-stamp when the peer was admitted"
    );
    assert_eq!(h.plain("migo_federation_peers_added_total"), 1);
}

#[tokio::test]
async fn applying_a_changed_address_and_region_converges_the_row() {
    let h = Harness::new();
    let p = peer(1);
    h.mesh
        .apply_peer(spec_of(&p), ts(NOW))
        .await
        .expect("the peer is admitted first");
    let moved = NewPeerSpec {
        node_id: p.node_id,
        public_key: p.public_key.clone(),
        base_url: "https://peer-1-relocated.example".to_string(),
        region: "region-1-relocated".to_string(),
    };
    let view = h
        .mesh
        .apply_peer(moved, ts(NOW + 60_000))
        .await
        .expect("a changed entry converges");
    assert_eq!(view.base_url, "https://peer-1-relocated.example");
    assert_eq!(view.region, "region-1-relocated");
}

#[tokio::test]
async fn applying_a_rotated_key_replaces_it_and_releases_the_old_one() {
    let h = Harness::new();
    let p = peer(1);
    let before = h
        .mesh
        .apply_peer(spec_of(&p), ts(NOW))
        .await
        .expect("the peer is admitted under its first key");
    // A key no other row holds — peer 9 was never admitted.
    let rotated = NewPeerSpec {
        node_id: p.node_id,
        public_key: peer(9).public_key,
        base_url: p.base_url.clone(),
        region: p.region.clone(),
    };
    let after = h
        .mesh
        .apply_peer(rotated, ts(NOW + 60_000))
        .await
        .expect("a rotated key converges");
    assert_ne!(after.fingerprint, before.fingerprint);
    // The old key is no longer claimed: another node may now be admitted under it.
    let claimant = NewPeerSpec {
        node_id: id(2),
        public_key: p.public_key.clone(),
        base_url: "https://claimant.example".to_string(),
        region: "claimant".to_string(),
    };
    h.mesh
        .add_peer(claimant, ts(NOW + 90_000))
        .await
        .expect("the released key is claimable again");
}

#[tokio::test]
async fn applying_an_unchanged_entry_keeps_a_runtime_status() {
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    h.mesh
        .set_peer_status(p.node_id, PeerStatus::Paused)
        .await
        .expect("an admitted peer's status can be set");
    let view = h
        .mesh
        .apply_peer(spec_of(&p), ts(NOW + 60_000))
        .await
        .expect("the same entry still applies");
    assert_eq!(
        view.status,
        PeerStatus::Paused,
        "reconciliation never touches a runtime status"
    );
}

#[tokio::test]
async fn applying_a_key_claimed_by_another_peer_is_refused() {
    // One key, one node: if the configuration tried to hand node 1 the key node 2
    // already presents, the boot fails rather than silently re-pointing an identity.
    let h = Harness::new();
    let one = peer(1);
    let two = peer(2);
    h.admit(&one).await;
    h.admit(&two).await;
    let takeover = NewPeerSpec {
        node_id: one.node_id,
        public_key: two.public_key.clone(),
        base_url: one.base_url.clone(),
        region: one.region.clone(),
    };
    expect_code(
        h.mesh.apply_peer(takeover, ts(NOW)).await,
        codes::ALREADY_EXISTS,
    );
    // Both rows are intact: the refused application wrote nothing.
    assert!(h.peer_view(one.node_id).await.is_some());
    assert!(h.peer_view(two.node_id).await.is_some());
}

#[tokio::test]
async fn applying_an_entry_validates_exactly_as_add_peer_does() {
    let h = Harness::new();
    let mut bad_key = spec_of(&peer(1));
    bad_key.public_key = vec![1, 2, 3];
    expect_code(
        h.mesh.apply_peer(bad_key, ts(NOW)).await,
        codes::VALIDATION_FAILED,
    );
    let mut bad_url = spec_of(&peer(1));
    bad_url.base_url = "http://insecure.example".to_string();
    expect_code(
        h.mesh.apply_peer(bad_url, ts(NOW)).await,
        codes::VALIDATION_FAILED,
    );
    let mut bad_region = spec_of(&peer(1));
    bad_region.region = "   ".to_string();
    expect_code(
        h.mesh.apply_peer(bad_region, ts(NOW)).await,
        codes::FIELD_REQUIRED,
    );
    assert!(
        h.peer_view(peer(1).node_id).await.is_none(),
        "a refused entry admits nothing"
    );
}

#[tokio::test]
async fn an_enqueued_event_id_is_minted_locally_and_time_ordered() {
    // The producer hands over a target, an opcode, and a payload — never an id. The service
    // mints a fresh, time-ordered id, so a caller cannot choose one that collides with an
    // existing row or forge the ordering the queue drains by.
    let h = Harness::new();
    let target = peer(1);
    let early = h
        .mesh
        .enqueue(
            FederatedEvent {
                target_node: target.node_id,
                opcode: FEDERATION_OPCODE_MIN,
                payload: vec![1, 2, 3],
            },
            ts(NOW),
        )
        .await
        .expect("a valid event enqueues");
    let late = h
        .mesh
        .enqueue(
            FederatedEvent {
                target_node: target.node_id,
                opcode: FEDERATION_OPCODE_MIN,
                payload: vec![4, 5, 6],
            },
            ts(NOW + 10_000),
        )
        .await
        .expect("a second valid event enqueues");
    assert_eq!(early.attempts, 0);
    assert_eq!(early.next_attempt_at, ts(NOW));
    assert_ne!(early.event_id, late.event_id);
    // The later enqueue sorts after the earlier one because its id embeds a later instant.
    assert!(early.event_id < late.event_id);
}

// ===========================================================================
// Invariant 5: existence is a secret across the trust boundary.
//
// Section 48's same-error rule: an unknown node, a blocked one, a paused one, and a bad
// signature all fail with one code, one symbol, and no public detail. The gap between "I do
// not know you" and "your signature was wrong" is an existence oracle a probing peer must not
// have. The operator's metrics still tell the four apart, because the operator is not the
// adversary.
// ===========================================================================

#[tokio::test]
async fn every_handshake_refusal_is_the_same_opaque_error() {
    let h = Harness::new();
    let allowed = peer(1);
    let blocked = peer(2);
    let paused = peer(3);
    h.admit(&allowed).await;
    h.admit_as(&blocked, PeerStatus::Blocked).await;
    h.admit_as(&paused, PeerStatus::Paused).await;

    let unknown_err = h.authenticate(&peer(404), 1, ts(NOW)).await.unwrap_err();
    let blocked_err = h.authenticate(&blocked, 2, ts(NOW)).await.unwrap_err();
    let paused_err = h.authenticate(&paused, 3, ts(NOW)).await.unwrap_err();
    let our = h.local_hello();
    let theirs = remote_hello(&allowed, nonce(4));
    let forged = node::prove(&peer(999).secret, &theirs, &our, ts(NOW));
    let badsig_err = h
        .mesh
        .authenticate(&our, &theirs, &forged, ts(NOW))
        .await
        .unwrap_err();

    for err in [&unknown_err, &blocked_err, &paused_err, &badsig_err] {
        assert_eq!(err.code(), codes::MESH_AUTH_FAILED);
        assert_eq!(err.symbol(), unknown_err.symbol());
        // Nothing a peer can read distinguishes the four reasons.
        assert!(err.public_message().is_empty());
    }
}

#[tokio::test]
async fn the_metrics_tell_the_four_refusal_reasons_apart() {
    // The peer learns one thing; the operator watching a spike of `blocked` versus
    // `proof_invalid` is diagnosing two different attacks and must be able to.
    let h = Harness::new();
    let allowed = peer(1);
    let blocked = peer(2);
    let paused = peer(3);
    h.admit(&allowed).await;
    h.admit_as(&blocked, PeerStatus::Blocked).await;
    h.admit_as(&paused, PeerStatus::Paused).await;

    let _ = h.authenticate(&peer(404), 1, ts(NOW)).await;
    let _ = h.authenticate(&blocked, 2, ts(NOW)).await;
    let _ = h.authenticate(&paused, 3, ts(NOW)).await;
    let our = h.local_hello();
    let theirs = remote_hello(&allowed, nonce(4));
    let forged = node::prove(&peer(999).secret, &theirs, &our, ts(NOW));
    let _ = h.mesh.authenticate(&our, &theirs, &forged, ts(NOW)).await;

    assert_eq!(h.rejected("unknown_peer"), 1);
    assert_eq!(h.rejected("blocked"), 1);
    assert_eq!(h.rejected("paused"), 1);
    assert_eq!(h.rejected("proof_invalid"), 1);
}

#[tokio::test]
async fn reading_an_unknown_peer_is_not_found() {
    let h = Harness::new();
    expect_code(h.mesh.peer(id(1234)).await, codes::NOT_FOUND);
}

#[tokio::test]
async fn setting_the_status_of_an_unknown_peer_is_not_found() {
    let h = Harness::new();
    expect_code(
        h.mesh.set_peer_status(id(1234), PeerStatus::Blocked).await,
        codes::NOT_FOUND,
    );
}

// ===========================================================================
// Invariant 8: admission control is charged only after admission.
//
// The allow-list is this crate's admission gate. An unknown, paused, or blocked peer is turned
// away *before* its nonce is recorded, so a stranger cannot flood or poison the shared nonce
// window — the same ordering a rate limiter needs when it refuses to charge a budget before
// identity is proved.
// ===========================================================================

#[tokio::test]
async fn an_unknown_peers_nonce_is_never_recorded() {
    let h = Harness::new();
    let p = peer(1);
    // First contact while still unknown, using a chosen nonce.
    expect_code(
        h.authenticate(&p, 42, ts(NOW)).await,
        codes::MESH_AUTH_FAILED,
    );
    // Now admitted, the very same nonce must still be usable: the earlier attempt recorded
    // nothing, so this is not seen as a replay.
    h.admit(&p).await;
    assert!(h.authenticate(&p, 42, ts(NOW)).await.is_ok());
    assert_eq!(h.replay("nonce_reused"), 0);
}

#[tokio::test]
async fn a_blocked_peers_nonce_is_never_recorded() {
    let h = Harness::new();
    let p = peer(1);
    h.admit_as(&p, PeerStatus::Blocked).await;
    expect_code(
        h.authenticate(&p, 42, ts(NOW)).await,
        codes::MESH_AUTH_FAILED,
    );
    // Re-allowed, the same nonce is fresh: the blocked attempt never charged the window.
    h.mesh
        .set_peer_status(p.node_id, PeerStatus::Allowed)
        .await
        .expect("a blocked peer can be re-allowed");
    assert!(h.authenticate(&p, 42, ts(NOW)).await.is_ok());
    assert_eq!(h.replay("nonce_reused"), 0);
}

// ===========================================================================
// Invariant 10: ordering and gaps on a link.
//
// A link is a strictly increasing sequence with no gaps. A number that does not advance is a
// replay to drop; a number that skips ahead is a gap that tears the link down and re-handshakes
// (section 152) rather than being quietly accepted. A gap silently skipped is a lost or
// replayed segment believed.
// ===========================================================================

#[tokio::test]
async fn the_first_packet_on_a_link_must_be_sequence_one() {
    let h = Harness::new();
    assert_eq!(h.mesh.check_sequence(id(1), 1), SequenceVerdict::Accept);
}

#[tokio::test]
async fn a_first_packet_that_skips_ahead_is_a_gap() {
    let h = Harness::new();
    // With nothing seen, the link's last is zero, so anything past one is a gap.
    assert_eq!(h.mesh.check_sequence(id(1), 5), SequenceVerdict::Gap);
    assert_eq!(h.replay("sequence_gap"), 1);
    assert_eq!(h.plain("migo_federation_links_reset_total"), 1);
}

#[tokio::test]
async fn sequence_zero_is_never_accepted() {
    let h = Harness::new();
    // Zero is the sentinel for "nothing seen"; a packet numbered zero cannot advance past it.
    assert_eq!(h.mesh.check_sequence(id(1), 0), SequenceVerdict::Replay);
}

#[tokio::test]
async fn consecutive_packets_are_accepted_in_order() {
    let h = Harness::new();
    for seq in 1..=5 {
        assert_eq!(h.mesh.check_sequence(id(1), seq), SequenceVerdict::Accept);
    }
}

#[tokio::test]
async fn a_non_advancing_sequence_is_a_replay_and_does_not_move_the_link() {
    let h = Harness::new();
    assert_eq!(h.mesh.check_sequence(id(1), 1), SequenceVerdict::Accept);
    assert_eq!(h.mesh.check_sequence(id(1), 2), SequenceVerdict::Accept);
    // A repeat and a straggler are both replays.
    assert_eq!(h.mesh.check_sequence(id(1), 2), SequenceVerdict::Replay);
    assert_eq!(h.mesh.check_sequence(id(1), 1), SequenceVerdict::Replay);
    // The link is still at two, so three is the next in-order packet.
    assert_eq!(h.mesh.check_sequence(id(1), 3), SequenceVerdict::Accept);
    assert_eq!(h.replay("sequence_replay"), 2);
}

#[tokio::test]
async fn a_gap_resets_the_link_so_the_next_packet_starts_over() {
    let h = Harness::new();
    assert_eq!(h.mesh.check_sequence(id(1), 1), SequenceVerdict::Accept);
    assert_eq!(h.mesh.check_sequence(id(1), 2), SequenceVerdict::Accept);
    // Four skips three: a gap, which clears the link's state.
    assert_eq!(h.mesh.check_sequence(id(1), 4), SequenceVerdict::Gap);
    // After a gap the link is torn down, so numbering restarts from one.
    assert_eq!(h.mesh.check_sequence(id(1), 1), SequenceVerdict::Accept);
    assert_eq!(h.replay("sequence_gap"), 1);
    assert_eq!(h.plain("migo_federation_links_reset_total"), 1);
}

#[tokio::test]
async fn links_are_tracked_independently_per_peer() {
    let h = Harness::new();
    assert_eq!(h.mesh.check_sequence(id(1), 1), SequenceVerdict::Accept);
    assert_eq!(h.mesh.check_sequence(id(1), 2), SequenceVerdict::Accept);
    // A different peer's link is untouched by the first: its first packet is still one.
    assert_eq!(h.mesh.check_sequence(id(2), 1), SequenceVerdict::Accept);
    // And the first link continues where it left off.
    assert_eq!(h.mesh.check_sequence(id(1), 3), SequenceVerdict::Accept);
}

#[tokio::test]
async fn reset_link_forces_the_next_packet_back_to_one() {
    let h = Harness::new();
    for seq in 1..=3 {
        assert_eq!(h.mesh.check_sequence(id(1), seq), SequenceVerdict::Accept);
    }
    h.mesh.reset_link(id(1));
    assert_eq!(h.mesh.check_sequence(id(1), 1), SequenceVerdict::Accept);
}

#[tokio::test]
async fn a_successful_handshake_resets_the_links_sequence() {
    // A new session numbers its packets from one, so authentication clears any stale link state
    // left from a previous session on the same peer id.
    let h = Harness::new();
    let p = peer(1);
    h.admit(&p).await;
    assert_eq!(h.mesh.check_sequence(p.node_id, 1), SequenceVerdict::Accept);
    assert_eq!(h.mesh.check_sequence(p.node_id, 2), SequenceVerdict::Accept);
    h.authenticate(&p, 1, ts(NOW))
        .await
        .expect("the peer authenticates");
    // Had the handshake not reset the link, one would now be a replay.
    assert_eq!(h.mesh.check_sequence(p.node_id, 1), SequenceVerdict::Accept);
}

// ===========================================================================
// The routing epoch: a stale view is told to refetch, a current-or-future one passes.
// ===========================================================================

#[tokio::test]
async fn the_routing_epoch_starts_at_zero() {
    let h = Harness::new();
    assert_eq!(h.mesh.epoch(), 0);
    assert!(h.mesh.check_epoch(0).is_ok());
}

#[tokio::test]
async fn bumping_the_epoch_is_monotonic_and_returns_the_new_value() {
    let h = Harness::new();
    assert_eq!(h.mesh.bump_epoch(), 1);
    assert_eq!(h.mesh.bump_epoch(), 2);
    assert_eq!(h.mesh.epoch(), 2);
}

#[tokio::test]
async fn an_epoch_older_than_the_current_one_is_stale() {
    let h = Harness::new();
    h.mesh.bump_epoch();
    h.mesh.bump_epoch();
    expect_code(h.mesh.check_epoch(1), codes::ROUTING_EPOCH_STALE);
}

#[tokio::test]
async fn an_epoch_at_or_ahead_of_the_current_one_is_fresh() {
    // Only an older view is stale; a request carrying the current epoch, or a newer one this
    // node has not yet learned, is not refused.
    let h = Harness::new();
    h.mesh.bump_epoch();
    assert!(h.mesh.check_epoch(1).is_ok());
    assert!(h.mesh.check_epoch(2).is_ok());
}

#[tokio::test]
async fn a_refresh_adopts_the_epoch_a_refusal_names() {
    // The transport's answer to ROUTING_EPOCH_STALE: adopt the epoch the refusing peer is
    // current at, so the retry the same drain performs carries a fresh view. One adoption,
    // however many bumps the peer made — a view three generations behind catches up in one
    // hop rather than three refusals.
    let h = Harness::new();
    h.mesh.bump_epoch();
    h.mesh.bump_epoch();
    h.mesh.bump_epoch();
    let view = h.mesh.refresh_routing(3);
    assert_eq!(view, 3, "the view is exactly the peer's");
    assert_eq!(h.mesh.epoch(), 3, "and it is the one the mesh now holds");
    assert!(
        h.mesh.check_epoch(3).is_ok(),
        "a request on the adopted view is no longer stale"
    );
}

#[tokio::test]
async fn a_refresh_never_lowers_or_duplicates_the_view() {
    // Monotonic in both directions: a peer naming an older epoch (a crafted refusal, or a
    // view that moved again mid-flight) cannot drag the view back, and refreshing to the
    // epoch already held is a no-op rather than a bump — bump_epoch stays the only way to
    // move the view forward on purpose.
    let h = Harness::new();
    h.mesh.bump_epoch();
    assert_eq!(h.mesh.refresh_routing(0), 1, "an older epoch is ignored");
    assert_eq!(h.mesh.epoch(), 1);
    assert_eq!(
        h.mesh.refresh_routing(1),
        1,
        "adopting the current epoch changes nothing"
    );
    assert_eq!(h.mesh.epoch(), 1, "and did not count as a bump");
}

// ===========================================================================
// Invariant 6 (batch cap): a peer listing cannot be an unbounded read.
// ===========================================================================

#[tokio::test]
async fn peers_are_listed_newest_first() {
    let h = Harness::new();
    h.admit(&peer(1)).await;
    h.admit(&peer(2)).await;
    h.admit(&peer(3)).await;
    let listed: Vec<Id> = h
        .mesh
        .peers(10)
        .await
        .expect("the allow-list can be read")
        .into_iter()
        .map(|view| view.node_id)
        .collect();
    assert_eq!(listed, vec![id(3), id(2), id(1)]);
}

#[tokio::test]
async fn a_peer_listing_is_clamped_to_the_page_maximum() {
    // An unbounded batch is a denial of service: whatever a caller asks for, one page is the
    // most a single read returns.
    let h = Harness::new();
    for n in 0..201u128 {
        h.admit(&peer(10_000 + n)).await;
    }
    let page = h
        .mesh
        .peers(u16::MAX)
        .await
        .expect("the allow-list can be read");
    assert_eq!(page.len(), 200);
}

#[tokio::test]
async fn a_zero_limit_listing_still_returns_a_bounded_page() {
    let h = Harness::new();
    h.admit(&peer(1)).await;
    h.admit(&peer(2)).await;
    // A zero limit is clamped up to one rather than returning an empty or unbounded page.
    assert_eq!(h.mesh.peers(0).await.expect("readable").len(), 1);
}

#[tokio::test]
async fn a_status_change_is_reversible_without_a_fresh_key() {
    // A block keeps the row and the key, so re-allowing is one call, not a re-admission.
    let h = Harness::new();
    let p = peer(1);
    let admitted = h.admit(&p).await;
    assert_eq!(admitted.status, PeerStatus::Allowed);

    let blocked = h
        .mesh
        .set_peer_status(p.node_id, PeerStatus::Blocked)
        .await
        .expect("a peer can be blocked");
    assert_eq!(blocked.status, PeerStatus::Blocked);
    // The identity survived the block: same fingerprint, so the same key.
    assert!(!blocked.fingerprint.is_empty());
    assert_eq!(blocked.fingerprint, admitted.fingerprint);

    let reallowed = h
        .mesh
        .set_peer_status(p.node_id, PeerStatus::Allowed)
        .await
        .expect("a blocked peer can be re-allowed");
    assert_eq!(reallowed.status, PeerStatus::Allowed);
    assert_eq!(reallowed.fingerprint, admitted.fingerprint);
}

// ===========================================================================
// Invariant 4: failure is bounded, and the outbox is durable.
//
// A peer that is unreachable, slow, or garbage must not take the sender down with it: a failed
// delivery is rescheduled on an exponential backoff (base × 2^attempts, clamped to a cap) and
// the event stays in the queue until it is delivered. The queue is at-least-once, so an event
// is never silently dropped — the give-up decision is a drainer's policy, not a lost row.
// ===========================================================================

/// A federation event bound for a peer, with the least ceremony a test needs.
fn event(target: Id, opcode: i32, payload: &[u8]) -> FederatedEvent {
    FederatedEvent {
        target_node: target,
        opcode,
        payload: payload.to_vec(),
    }
}

#[tokio::test]
async fn an_enqueued_event_is_immediately_due() {
    let h = Harness::new();
    let pending = h
        .mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"payload"), ts(NOW))
        .await
        .expect("a well-formed event enqueues");
    // A fresh event has taken no attempts and is due at once.
    assert_eq!(pending.attempts, 0);
    let due = h.mesh.due(ts(NOW)).await.expect("the queue is readable");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].event_id, pending.event_id);
}

#[tokio::test]
async fn an_enqueued_event_is_durable_in_the_backing_store() {
    // Read straight from the store, past the service, to prove the event is persisted and not
    // merely cached in memory by the service: durability is the store's promise, not a view's.
    let h = Harness::new();
    let pending: PendingEvent = h
        .mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"durable"), ts(NOW))
        .await
        .expect("enqueues");
    let stored = h
        .store
        .due_events(ts(NOW), DEFAULT_DUE_BATCH)
        .await
        .expect("the store is readable");
    assert!(stored
        .iter()
        .any(|record| record.event_id == pending.event_id && record.payload == b"durable"));
}

#[tokio::test]
async fn a_failed_delivery_reschedules_the_event_and_keeps_it_queued() {
    let h = Harness::new();
    let pending = h
        .mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"payload"), ts(NOW))
        .await
        .expect("enqueues");
    h.mesh
        .mark_failed(pending.event_id, 0, ts(NOW), "peer unreachable")
        .await
        .expect("a failure reschedules");
    // Immediately after a failure the event is held back for the backoff interval...
    assert!(h.mesh.due(ts(NOW)).await.expect("readable").is_empty());
    // ...and reappears once the first backoff has elapsed, now carrying one attempt.
    let due = h
        .mesh
        .due(ts(NOW + DEFAULT_BACKOFF_BASE_MS))
        .await
        .expect("readable");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].attempts, 1);
}

#[tokio::test]
async fn the_retry_delay_doubles_with_each_failure() {
    let h = Harness::new();
    let pending = h
        .mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"payload"), ts(NOW))
        .await
        .expect("enqueues");
    // Fail from the same reference instant each time and read back the scheduled delay: base,
    // then twice base, then four times it — base shifted left by the number of prior failures.
    for shift in 0..3u32 {
        let expected_delay = DEFAULT_BACKOFF_BASE_MS << shift;
        h.mesh
            .mark_failed(pending.event_id, shift as i32, ts(NOW), "peer unreachable")
            .await
            .expect("reschedules");
        assert!(
            h.mesh
                .due(ts(NOW + expected_delay - 1))
                .await
                .expect("readable")
                .is_empty(),
            "not due one millisecond early (shift {shift})"
        );
        let due = h
            .mesh
            .due(ts(NOW + expected_delay))
            .await
            .expect("readable");
        assert_eq!(due.len(), 1, "due exactly at the delay (shift {shift})");
        assert_eq!(due[0].next_attempt_at, ts(NOW + expected_delay));
    }
}

#[tokio::test]
async fn the_retry_delay_is_clamped_to_the_cap() {
    // A long-dead event must not schedule its next retry past the heat death of the universe:
    // an enormous attempt count clamps to the cap rather than overflowing the shift.
    let h = Harness::new();
    let pending = h
        .mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"payload"), ts(NOW))
        .await
        .expect("enqueues");
    h.mesh
        .mark_failed(pending.event_id, i32::MAX, ts(NOW), "peer unreachable")
        .await
        .expect("reschedules without overflowing");
    assert!(h
        .mesh
        .due(ts(NOW + DEFAULT_BACKOFF_CAP_MS - 1))
        .await
        .expect("readable")
        .is_empty());
    let due = h
        .mesh
        .due(ts(NOW + DEFAULT_BACKOFF_CAP_MS))
        .await
        .expect("readable");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].next_attempt_at, ts(NOW + DEFAULT_BACKOFF_CAP_MS));
}

#[tokio::test]
async fn a_repeatedly_failing_event_is_never_silently_dropped() {
    // max_attempts is advisory (a drainer's give-up policy); the store keeps a failed event
    // forever. Past that threshold the event is still present and still deliverable — a dropped
    // row here would be a federated message lost without a trace.
    let h = Harness::new();
    let pending = h
        .mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"payload"), ts(NOW))
        .await
        .expect("enqueues");
    let rounds = DEFAULT_MAX_ATTEMPTS + 3;
    for attempts_so_far in 0..rounds {
        h.mesh
            .mark_failed(pending.event_id, attempts_so_far, ts(NOW), "still down")
            .await
            .expect("reschedules");
    }
    // Well past the cap, the event remains — attempts counted, not discarded.
    let due = h
        .mesh
        .due(ts(NOW + DEFAULT_BACKOFF_CAP_MS))
        .await
        .expect("readable");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].event_id, pending.event_id);
    assert_eq!(due[0].attempts, rounds);
}

// ===========================================================================
// Invariant 9: idempotency. At-least-once delivery means the same acknowledgement can arrive
// twice; the second must be a no-op, not an error, and must not resurrect a delivered event.
// ===========================================================================

#[tokio::test]
async fn marking_an_event_delivered_twice_is_idempotent() {
    let h = Harness::new();
    let pending = h
        .mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"payload"), ts(NOW))
        .await
        .expect("enqueues");
    h.mesh
        .mark_delivered(pending.event_id, ts(NOW))
        .await
        .expect("first delivery");
    // A racing retry that delivers again is harmless, not an error.
    h.mesh
        .mark_delivered(pending.event_id, ts(NOW + 5_000))
        .await
        .expect("a second delivery is a no-op");
    // Either way the event has left the queue for good.
    assert!(h
        .mesh
        .due(ts(NOW + DEFAULT_BACKOFF_CAP_MS))
        .await
        .expect("readable")
        .is_empty());
}

#[tokio::test]
async fn a_delivered_event_is_never_due_again() {
    let h = Harness::new();
    let pending = h
        .mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"payload"), ts(NOW))
        .await
        .expect("enqueues");
    assert_eq!(h.mesh.due(ts(NOW)).await.expect("readable").len(), 1);
    h.mesh
        .mark_delivered(pending.event_id, ts(NOW))
        .await
        .expect("delivered");
    assert!(h.mesh.due(ts(NOW)).await.expect("readable").is_empty());
}

#[tokio::test]
async fn a_late_failure_after_delivery_does_not_resurrect_the_event() {
    // Delivery is terminal. A failure report that races in after a successful ack must not
    // put an already-delivered event back on the wire.
    let h = Harness::new();
    let pending = h
        .mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"payload"), ts(NOW))
        .await
        .expect("enqueues");
    h.mesh
        .mark_delivered(pending.event_id, ts(NOW))
        .await
        .expect("delivered");
    h.mesh
        .mark_failed(pending.event_id, 0, ts(NOW), "late error")
        .await
        .expect("a late failure is accepted but inert");
    assert!(h
        .mesh
        .due(ts(NOW + DEFAULT_BACKOFF_CAP_MS + DEFAULT_BACKOFF_BASE_MS))
        .await
        .expect("readable")
        .is_empty());
}

#[tokio::test]
async fn acknowledging_an_unknown_event_is_harmless() {
    // An ack or failure for an id the store never held is a no-op, not an error: at-least-once
    // callers retry blindly and must not be able to turn a stale ack into a fault.
    let h = Harness::new();
    h.mesh
        .mark_delivered(id(0xDEAD_BEEF), ts(NOW))
        .await
        .expect("an unknown delivery is ignored");
    h.mesh
        .mark_failed(id(0xDEAD_BEEF), 0, ts(NOW), "who?")
        .await
        .expect("an unknown failure is ignored");
}

// ===========================================================================
// Invariant 6: the enqueue and admission boundaries clamp every limit.
// An opcode outside the federation band, an empty payload, or a base_url that is too long or
// in the wrong scheme is refused at the boundary with a VALIDATION-class error.
// ===========================================================================

#[tokio::test]
async fn the_lowest_federation_opcode_is_accepted() {
    let h = Harness::new();
    h.mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"x"), ts(NOW))
        .await
        .expect("the lowest opcode in the band is valid");
}

#[tokio::test]
async fn the_highest_federation_opcode_is_accepted() {
    let h = Harness::new();
    h.mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MAX, b"x"), ts(NOW))
        .await
        .expect("the highest opcode in the band is valid");
}

#[tokio::test]
async fn an_opcode_below_the_band_is_refused() {
    let h = Harness::new();
    expect_code(
        h.mesh
            .enqueue(event(id(1), FEDERATION_OPCODE_MIN - 1, b"x"), ts(NOW))
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn an_opcode_above_the_band_is_refused() {
    let h = Harness::new();
    expect_code(
        h.mesh
            .enqueue(event(id(1), FEDERATION_OPCODE_MAX + 1, b"x"), ts(NOW))
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn the_conversation_band_pair_is_accepted() {
    // The conversation tier's carve-out from the head of the reserved range (section 145):
    // 241 FED_CONVERSATION_SUBSCRIBE and 242 FED_CONVERSATION_EVENT are mesh frames too.
    let h = Harness::new();
    h.mesh
        .enqueue(event(id(1), CONVERSATION_OPCODE_MIN, b"x"), ts(NOW))
        .await
        .expect("the conversation band's lowest opcode is valid");
    h.mesh
        .enqueue(event(id(1), CONVERSATION_OPCODE_MAX, b"x"), ts(NOW))
        .await
        .expect("the conversation band's highest opcode is valid");
}

#[tokio::test]
async fn the_row_band_pair_is_accepted() {
    // The row-replication tier's carve-out (section 145's third written decision):
    // 243-246 are mesh frames too — the account pair and the conversation pair.
    let h = Harness::new();
    h.mesh
        .enqueue(event(id(1), ROW_OPCODE_MIN, b"x"), ts(NOW))
        .await
        .expect("the row band's lowest opcode is valid");
    h.mesh
        .enqueue(event(id(1), ROW_OPCODE_MAX, b"x"), ts(NOW))
        .await
        .expect("the row band's highest opcode is valid");
}

#[tokio::test]
async fn the_gaps_around_the_upper_federation_bands_are_refused() {
    // 240 is a live client opcode (ENTITLEMENTS) and 248 is the reserved span,
    // so neither may ride the mesh; the upper bands are exactly the 241-242
    // conversation pair plus the 243-246 row tier and nothing wider.
    let h = Harness::new();
    expect_code(
        h.mesh
            .enqueue(event(id(1), CONVERSATION_OPCODE_MIN - 1, b"x"), ts(NOW))
            .await,
        codes::VALIDATION_FAILED,
    );
    expect_code(
        h.mesh
            .enqueue(event(id(1), ROW_OPCODE_MAX + 1, b"x"), ts(NOW))
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn an_empty_payload_is_refused() {
    // A zero-length payload is a malformed event, not a heartbeat: nothing to deliver.
    let h = Harness::new();
    expect_code(
        h.mesh
            .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b""), ts(NOW))
            .await,
        codes::FIELD_REQUIRED,
    );
}

/// A peer spec reusing an existing key but pointing at a chosen base URL, for scheme and length
/// checks at admission.
fn spec_with_url(p: &Peer, base_url: &str) -> NewPeerSpec {
    NewPeerSpec {
        node_id: p.node_id,
        public_key: p.public_key.clone(),
        base_url: base_url.to_string(),
        region: p.region.clone(),
    }
}

#[tokio::test]
async fn a_base_url_at_the_maximum_length_is_admitted() {
    // 512 bytes exactly is at the limit, and the limit is inclusive.
    let h = Harness::new();
    let url = format!("https://{}", "a".repeat(512 - "https://".len()));
    assert_eq!(url.len(), 512);
    h.mesh
        .add_peer(spec_with_url(&peer(1), &url), ts(NOW))
        .await
        .expect("a 512-byte URL is admitted");
}

#[tokio::test]
async fn a_base_url_over_the_maximum_length_is_refused() {
    let h = Harness::new();
    let url = format!("https://{}", "a".repeat(513 - "https://".len()));
    assert_eq!(url.len(), 513);
    expect_code(
        h.mesh
            .add_peer(spec_with_url(&peer(1), &url), ts(NOW))
            .await,
        codes::FIELD_TOO_LONG,
    );
}

#[tokio::test]
async fn a_plaintext_http_base_url_is_refused() {
    // A federation link is https/wss, never plain http: a typo that would carry mesh traffic in
    // the clear is caught at admission, not discovered on the wire.
    let h = Harness::new();
    expect_code(
        h.mesh
            .add_peer(
                spec_with_url(&peer(1), "http://peer-1.mesh.example"),
                ts(NOW),
            )
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn a_secure_websocket_base_url_is_admitted() {
    let h = Harness::new();
    h.mesh
        .add_peer(
            spec_with_url(&peer(1), "wss://peer-1.mesh.example/mesh"),
            ts(NOW),
        )
        .await
        .expect("a wss URL is admitted");
}

#[tokio::test]
async fn an_empty_base_url_is_refused() {
    let h = Harness::new();
    expect_code(
        h.mesh
            .add_peer(spec_with_url(&peer(1), "   "), ts(NOW))
            .await,
        codes::FIELD_REQUIRED,
    );
}

// ===========================================================================
// The local hello is minted from local configuration, and each carries a fresh nonce.
// ===========================================================================

#[tokio::test]
async fn the_local_hello_advertises_our_node_id_and_protocol_version() {
    let h = Harness::new();
    let hello = h.local_hello();
    // The id and version a peer sees are ours, taken from local state, not echoed from anything.
    assert_eq!(hello.node_id, h.node_id);
    assert_eq!(hello.protocol_version, MESH_PROTOCOL_VERSION);
}

#[tokio::test]
async fn each_local_hello_carries_a_fresh_nonce() {
    // A reused hello nonce is a replay waiting to happen; every hello must mint a new one.
    let h = Harness::new();
    assert_ne!(h.local_hello().nonce, h.local_hello().nonce);
}

#[test]
fn the_default_configuration_uses_the_documented_constants() {
    // The published DEFAULT_* constants are the contract a drainer and an operator read; the
    // built configuration must match them, or the docs describe a system that does not exist.
    let config = MeshConfig::default();
    assert_eq!(config.nonce_window_ms, DEFAULT_NONCE_WINDOW_MS);
    assert_eq!(config.backoff_base_ms, DEFAULT_BACKOFF_BASE_MS);
    assert_eq!(config.backoff_cap_ms, DEFAULT_BACKOFF_CAP_MS);
    assert_eq!(config.max_attempts, DEFAULT_MAX_ATTEMPTS);
    assert_eq!(config.due_batch, DEFAULT_DUE_BATCH);
}

// ===========================================================================
// Invariant 7: the metrics endpoint is not an intelligence feed.
//
// Section 174 forbids a series labelled by account, and this crate widens that to node and
// peer: a counter keyed on a node id would let anyone scraping /metrics rebuild the mesh's
// topology and traffic — who talks to whom, how often, when a link fell quiet. So after a
// full spread of activity, the rendered registry must carry the fixed enum labels and the
// counts, and none of the identifiers, URLs, regions, domain, or payloads that passed through.
// ===========================================================================

#[tokio::test]
async fn the_metrics_render_leaks_no_identifier_url_domain_or_payload() {
    let h = Harness::new();
    let allowed = peer(1);
    let blocked = peer(2);
    let stranger = peer(3);
    let secret_payload = "top-secret-cross-domain-payload";

    h.admit(&allowed).await;
    h.admit_as(&blocked, PeerStatus::Blocked).await;

    // An honest handshake, then a replay of the very same nonce.
    h.authenticate(&allowed, 7, ts(NOW))
        .await
        .expect("the honest handshake succeeds");
    let _ = h.authenticate(&allowed, 7, ts(NOW)).await;
    // Two more refusals whose reasons are counted but whose peers are not named.
    let _ = h.authenticate(&stranger, 8, ts(NOW)).await;
    let _ = h.authenticate(&blocked, 9, ts(NOW)).await;

    // The replay defences on a link: an accept, a replay, a gap.
    assert_eq!(
        h.mesh.check_sequence(allowed.node_id, 1),
        SequenceVerdict::Accept
    );
    let _ = h.mesh.check_sequence(allowed.node_id, 1);
    let _ = h.mesh.check_sequence(allowed.node_id, 9);

    // The outbox flow, carrying a payload that must never surface in a metric.
    let delivered = h
        .mesh
        .enqueue(
            event(
                allowed.node_id,
                FEDERATION_OPCODE_MIN,
                secret_payload.as_bytes(),
            ),
            ts(NOW),
        )
        .await
        .expect("enqueues");
    h.mesh
        .mark_delivered(delivered.event_id, ts(NOW))
        .await
        .expect("delivered");
    let failed = h
        .mesh
        .enqueue(
            event(
                blocked.node_id,
                FEDERATION_OPCODE_MAX,
                secret_payload.as_bytes(),
            ),
            ts(NOW),
        )
        .await
        .expect("enqueues");
    h.mesh
        .mark_failed(failed.event_id, 0, ts(NOW), "peer unreachable")
        .await
        .expect("failed");

    let dump = h.registry.render();

    // Positive control: the dump is populated and carries the fixed enum labels, so the
    // negative assertions below run against real content, not an empty string.
    assert!(dump.contains("migo_federation_handshakes_total"));
    assert!(dump.contains("migo_federation_outbox_failed_total"));
    assert!(
        dump.contains("reason=\"proof_invalid\""),
        "the closed reason enum is registered up front"
    );

    // Nothing that identifies a node, a link, or its traffic may appear.
    let forbidden = [
        allowed.node_id.to_string(),
        blocked.node_id.to_string(),
        stranger.node_id.to_string(),
        allowed.region.clone(),
        blocked.region.clone(),
        allowed.base_url.clone(),
        blocked.base_url.clone(),
        OUR_REGION.to_string(),
        // The mesh domain (the crypto transcript's separation tag).
        String::from_utf8_lossy(MESH_DOMAIN).into_owned(),
        secret_payload.to_string(),
    ];
    for probe in forbidden {
        assert!(
            !dump.contains(&probe),
            "the metrics render leaked {probe:?}:\n{dump}"
        );
    }
}

// ===========================================================================
// Invariant 10 (outbox): the drain order is a schedule, not an arrival order, and one pass is
// bounded by the configured batch.
// ===========================================================================

#[tokio::test]
async fn due_events_are_ordered_by_schedule_not_arrival() {
    let h = Harness::new();
    let first = h
        .mesh
        .enqueue(event(id(1), FEDERATION_OPCODE_MIN, b"a"), ts(NOW))
        .await
        .expect("enqueues");
    let second = h
        .mesh
        .enqueue(event(id(2), FEDERATION_OPCODE_MIN, b"b"), ts(NOW))
        .await
        .expect("enqueues");
    // Both are due now; the tie is broken by age, so the first enqueued drains first.
    let arrival: Vec<Id> = h
        .mesh
        .due(ts(NOW))
        .await
        .expect("readable")
        .into_iter()
        .map(|pending| pending.event_id)
        .collect();
    assert_eq!(arrival, vec![first.event_id, second.event_id]);
    // Fail the first, pushing its next attempt behind the second's. The order now follows the
    // schedule, not the order the events arrived in.
    h.mesh
        .mark_failed(first.event_id, 0, ts(NOW), "slow peer")
        .await
        .expect("reschedules");
    let by_schedule: Vec<Id> = h
        .mesh
        .due(ts(NOW + DEFAULT_BACKOFF_BASE_MS))
        .await
        .expect("readable")
        .into_iter()
        .map(|pending| pending.event_id)
        .collect();
    assert_eq!(by_schedule, vec![second.event_id, first.event_id]);
}

#[tokio::test]
async fn a_single_drain_pass_is_bounded_by_the_due_batch() {
    // The drainer reads a bounded page, never the whole backlog, so a burst of enqueues cannot
    // turn one pass into an unbounded read.
    let h = Harness::with_config(MeshConfig {
        due_batch: 2,
        ..MeshConfig::default()
    });
    for n in 1..=5u128 {
        h.mesh
            .enqueue(event(id(n), FEDERATION_OPCODE_MIN, b"x"), ts(NOW))
            .await
            .expect("enqueues");
    }
    assert_eq!(h.mesh.due(ts(NOW)).await.expect("readable").len(), 2);
}

// ===========================================================================
// Invariant 10: the slow-link marking. A peer whose undelivered backlog crosses the
// degradation watermark is marked degraded — a signal, never a policy change — and the
// marking clears by itself once the peer has caught up to half the watermark. The
// operator's paused and blocked are outside the automatic transitions entirely.
// ===========================================================================

/// A harness with the degradation watermark pinned low, so a handful of queued events is
/// enough to cross it. The watermark's default is pinned separately, below.
fn degraded_harness(watermark: u32) -> Harness {
    Harness::with_config(MeshConfig {
        degraded_outbox_watermark: watermark,
        ..MeshConfig::default()
    })
}

/// Queues `count` events for `target`, returning them in queue order.
async fn queue_for(mesh: &MeshService<MemoryStore>, target: Id, count: usize) -> Vec<PendingEvent> {
    let mut queued = Vec::with_capacity(count);
    for n in 0..count {
        let payload = [u8::try_from(n).expect("a test payload stays in range"); 8];
        let pending = mesh
            .enqueue(event(target, FEDERATION_OPCODE_MIN, &payload), ts(NOW))
            .await
            .expect("a federation-band event enqueues");
        queued.push(pending);
    }
    queued
}

#[test]
fn the_default_watermark_sits_above_the_largest_backlog_the_brief_tests() {
    // Section 173's scenario 10 drains a 300-event backlog as a matter of course — a mass
    // sync after an outage is traffic, not a fault — so the default watermark must sit
    // above it or every recovery burst would cry degraded. The pin is a const assertion:
    // lowering the default past the brief's own scale is a compile error, not a red test.
    const _: () = assert!(
        DEFAULT_DEGRADED_OUTBOX_WATERMARK > 300,
        "the default watermark must clear the brief's 300-event mass-sync backlog"
    );
}

#[tokio::test]
async fn a_zero_degradation_watermark_is_refused_at_construction() {
    let config = MeshConfig {
        degraded_outbox_watermark: 0,
        ..MeshConfig::default()
    };
    expect_code(build(config, OUR_REGION), codes::INTERNAL_ERROR);
}

#[tokio::test]
async fn a_peer_whose_backlog_crosses_the_watermark_is_marked_degraded() {
    let h = degraded_harness(4);
    let slow = peer(41);
    let brisk = peer(42);
    h.admit(&slow).await;
    h.admit(&brisk).await;

    // The control: a backlog at the watermark is a busy peer, not a degraded one. The
    // marking fires only past it.
    queue_for(&h.mesh, brisk.node_id, 4).await;
    let brisk_view = h
        .mesh
        .observe_peer_lag(brisk.node_id)
        .await
        .expect("the observation reads");
    assert_eq!(
        brisk_view.status,
        PeerStatus::Allowed,
        "a busy peer is not degraded"
    );

    // One event further and the peer has fallen behind by more than the sender is willing
    // to hold quietly: the marking fires, and the view an operator reads carries it.
    queue_for(&h.mesh, slow.node_id, 5).await;
    let slow_view = h
        .mesh
        .observe_peer_lag(slow.node_id)
        .await
        .expect("the observation reads");
    assert_eq!(slow_view.status, PeerStatus::Degraded);

    // The marking is on the row, not just the answer: the plain peer read says the same.
    assert_eq!(
        h.peer_view(slow.node_id)
            .await
            .expect("the peer reads")
            .status,
        PeerStatus::Degraded,
        "the degraded marking survives into the operator's view"
    );
    assert_eq!(
        PeerStatus::Degraded.slug(),
        "degraded",
        "the slug the directory carries is stable"
    );
}

#[tokio::test]
async fn a_degraded_peer_is_allowed_again_once_it_has_caught_up() {
    let h = degraded_harness(4);
    let slow = peer(43);
    h.admit(&slow).await;
    // Six owed, not five: the marking, the band, and the clear line below must each see a
    // distinct depth — six past the watermark (4), three inside the band, two at half.
    let queued = queue_for(&h.mesh, slow.node_id, 6).await;
    let view = h
        .mesh
        .observe_peer_lag(slow.node_id)
        .await
        .expect("the observation reads");
    assert_eq!(view.status, PeerStatus::Degraded);

    // Catching up to just inside the watermark is not enough: half, not the watermark
    // itself, is the clear line, so a depth hovering at the threshold cannot flap the
    // status drain by drain. Three still owed sits between half (2) and the watermark (4).
    for pending in &queued[..3] {
        h.mesh
            .mark_delivered(pending.event_id, ts(NOW + 1_000))
            .await
            .expect("the delivery is recorded");
    }
    let still_degraded = h
        .mesh
        .observe_peer_lag(slow.node_id)
        .await
        .expect("the observation reads");
    assert_eq!(
        still_degraded.status,
        PeerStatus::Degraded,
        "a backlog inside the hysteresis band keeps the marking"
    );

    // Two owed — half the watermark — is a peer that has caught up.
    h.mesh
        .mark_delivered(queued[3].event_id, ts(NOW + 2_000))
        .await
        .expect("the delivery is recorded");
    let recovered = h
        .mesh
        .observe_peer_lag(slow.node_id)
        .await
        .expect("the observation reads");
    assert_eq!(
        recovered.status,
        PeerStatus::Allowed,
        "the marking clears by itself when the peer catches up"
    );
}

#[tokio::test]
async fn a_degraded_peer_still_handshakes_and_nothing_it_is_owed_is_dropped() {
    let h = degraded_harness(4);
    let slow = peer(44);
    h.admit(&slow).await;
    let queued = queue_for(&h.mesh, slow.node_id, 5).await;
    let view = h
        .mesh
        .observe_peer_lag(slow.node_id)
        .await
        .expect("the observation reads");
    assert_eq!(view.status, PeerStatus::Degraded);

    // The handshake gate is the operator's alone: a degraded peer's proof still
    // authenticates, because degraded is a signal about the link, not a suspension of it.
    h.authenticate(&slow, 0x44, ts(NOW))
        .await
        .expect("a degraded peer's handshake proceeds exactly as an allowed peer's");

    // And the outbox holds the degraded peer to nothing less than an allowed one: every
    // event is still owed, still due, and still delivered on ack. At-least-once (section
    // 153) is not the marking's to bend.
    let due = h.mesh.due(ts(NOW)).await.expect("the outbox reads");
    assert_eq!(
        due.len(),
        5,
        "a degraded peer's events are neither dropped nor held back"
    );
    for pending in &queued {
        h.mesh
            .mark_delivered(pending.event_id, ts(NOW + 1_000))
            .await
            .expect("the delivery is recorded");
    }
    assert!(
        h.mesh
            .due(ts(NOW + DEFAULT_BACKOFF_CAP_MS))
            .await
            .expect("the outbox reads")
            .is_empty(),
        "a degraded peer's queue settles like anyone else's"
    );
}

#[tokio::test]
async fn the_automatic_transitions_never_touch_an_operators_paused_or_blocked() {
    let h = degraded_harness(4);
    let paused = peer(45);
    let blocked = peer(46);
    h.admit_as(&paused, PeerStatus::Paused).await;
    h.admit_as(&blocked, PeerStatus::Blocked).await;

    // A backlog that would mark an allowed peer degraded must not move a peer the
    // operator took off the mesh: the observation is for the operator's benefit, and
    // overwriting the operator's own decision is the opposite of that.
    for p in [&paused, &blocked] {
        queue_for(&h.mesh, p.node_id, 5).await;
        let view = h
            .mesh
            .observe_peer_lag(p.node_id)
            .await
            .expect("the observation reads");
        assert_eq!(
            view.status,
            if p.node_id == paused.node_id {
                PeerStatus::Paused
            } else {
                PeerStatus::Blocked
            },
            "an operator's status survives a crossing backlog"
        );
    }

    // The same protection on the way down: a degraded peer the operator paused keeps the
    // pause, so the runtime's catch-up clear can never quietly re-open the link.
    let degraded = peer(47);
    h.admit(&degraded).await;
    queue_for(&h.mesh, degraded.node_id, 5).await;
    let view = h
        .mesh
        .observe_peer_lag(degraded.node_id)
        .await
        .expect("the observation reads");
    assert_eq!(view.status, PeerStatus::Degraded);
    h.mesh
        .set_peer_status(degraded.node_id, PeerStatus::Paused)
        .await
        .expect("the operator pauses the degraded peer");
    let view = h
        .mesh
        .observe_peer_lag(degraded.node_id)
        .await
        .expect("the observation reads");
    assert_eq!(
        view.status,
        PeerStatus::Paused,
        "the catch-up clear leaves a peer the operator paused alone"
    );
}

#[tokio::test]
async fn observing_the_lag_of_an_unknown_peer_is_not_found() {
    let h = Harness::new();
    expect_code(h.mesh.observe_peer_lag(id(48)).await, codes::NOT_FOUND);
}

#[test]
fn the_four_statuses_round_trip_and_the_unknown_still_decodes_blocked() {
    // The store's integers and the operator's slugs both have to survive the round trip,
    // and the one value nothing writes (4 and up, or negative) must still land on the
    // safe side of the gate — a corrupt or hostile row closes a link, never opens one.
    for status in PeerStatus::NAMED {
        assert_eq!(PeerStatus::from_i16(status.0.to_i16()), status.0);
        assert_eq!(PeerStatus::from_slug(status.1), Some(status.0));
        assert_eq!(status.0.slug(), status.1);
    }
    assert_eq!(PeerStatus::from_i16(4), PeerStatus::Blocked);
    assert_eq!(PeerStatus::from_i16(-1), PeerStatus::Blocked);
    assert_eq!(PeerStatus::from_slug("sluggish"), None);
}
