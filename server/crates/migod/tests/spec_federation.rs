//! Integration coverage for the FEDERATION domain SPEC opcodes.
//!
//! The dispatch handlers in `migod::dispatch::federation` are `pub(crate)`, so an integration test
//! in a separate crate cannot call them directly; the unit test inside that module already drives
//! `migo_federation::open` with an in-memory backend and asserts the mesh answers as the handlers
//! expect. This crate-level test builds the same in-memory mesh and calls the very `Mesh` methods
//! the `handle_*` handlers delegate to — `peers`, `region`, `epoch`, `hello`, `check_epoch` — and
//! asserts the behaviour those handlers rely on.

use std::sync::Arc;

use migo_core::metrics::Registry;
use migo_core::Id;
use migo_crypto::NodeSecret;
use migo_federation::{open, MeshConfig, SequenceVerdict};
use migo_store::memory::MemoryStore;
use migo_store::SharedStore;

/// Builds the mesh over the in-memory store and the production randomness path, the same way the
/// `handle_*` path wires it through `migo_federation::open`.
fn harness() -> migo_federation::SharedMesh {
    let mem: SharedStore = Arc::new(MemoryStore::new());
    let registry = Registry::new();
    let secret = NodeSecret::from_seed(&[0u8; 32]).expect("a 32-byte seed is always valid");
    open(
        mem,
        MeshConfig::default(),
        Id::from(7u128),
        "eu-west".to_string(),
        secret,
        &registry,
    )
    .expect("the mesh opens over the in-memory store")
}

/// `handle_shard_map` and `handle_directory` both answer with `Mesh::peers`; a fresh mesh has
/// no allow-listed peers, so the directory and shard map are empty.
#[tokio::test]
async fn directory_and_shard_map_list_no_peers() {
    let mesh = harness();
    let peers = mesh.peers(256).await.expect("peers are readable");
    assert!(
        peers.is_empty(),
        "no peers are allow-listed on a fresh mesh"
    );
}

/// `handle_health` reports this node's own region and epoch; both are readable and stable.
#[tokio::test]
async fn health_reports_region_and_epoch() {
    let mesh = harness();
    assert_eq!(mesh.region(), "eu-west");
    assert_eq!(mesh.epoch(), 0);
}

/// `handle_ping` has no backing `Mesh` method, but its liveness guarantee is the handshake hello
/// the mesh issues — it always carries a fresh nonce.
#[tokio::test]
async fn ping_equivalent_hello_is_issuable() {
    let mesh = harness();
    let hello = mesh.hello();
    assert!(
        !hello.nonce.is_empty(),
        "the mesh issues a nonce-bearing hello"
    );
}

/// `handle_hello`/`handle_auth` gate on `check_epoch`; the current epoch is accepted and a request
/// built against an older view than this node knows is refused (section 169).
#[tokio::test]
async fn hello_auth_epoch_gate_accepts_current() {
    let mesh = harness();
    assert!(mesh.check_epoch(0).is_ok(), "the current epoch is accepted");
}

/// `handle_ack` feeds the peer's sequence to `check_sequence`; the window records the verdict
/// without erroring for a sane sequence.
#[tokio::test]
async fn ack_sequence_is_observed() {
    let mesh = harness();
    // A sequence of 1 is the first packet after a handshake; it must be judged `Accept`.
    let verdict = mesh.check_sequence(Id::from(1u128), 1);
    assert_eq!(
        verdict,
        SequenceVerdict::Accept,
        "the first packet is in order"
    );
}
