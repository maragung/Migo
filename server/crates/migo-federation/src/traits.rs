//! What this crate offers the layer above: the whole mesh behind one erased trait.
//!
//! # Three audiences, one trait
//!
//! [`Mesh`] serves three callers. An **operator** administers the allow-list —
//! [`add_peer`](Mesh::add_peer), [`set_peer_status`](Mesh::set_peer_status),
//! [`peers`](Mesh::peers), [`peer`](Mesh::peer) — the deliberate, approved joins section 170
//! requires. The **transport layer** drives a link: it builds a [`hello`](Mesh::hello),
//! signs a [`prove`](Mesh::prove), [`authenticate`](Mesh::authenticate)s the peer's proof,
//! and runs every subsequent packet through [`check_sequence`](Mesh::check_sequence). And a
//! **producer** — the rooms or messaging layer with an event bound for another region — uses
//! the outbox: [`enqueue`](Mesh::enqueue) to hand it over, then a drainer walks
//! [`due`](Mesh::due), [`mark_delivered`](Mesh::mark_delivered) and
//! [`mark_failed`](Mesh::mark_failed).
//!
//! There is deliberately no method that opens a socket, no method that reads a payload, and
//! no method by which a peer admits itself. The transport is the gateway's; the payload is a
//! sealed envelope; and a peer's only vocabulary is the handshake, which begins by looking it
//! up in an allow-list it cannot write to.

use std::sync::Arc;

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};
use migo_crypto::{NodeHello, NodeProof};

use crate::model::{
    FederatedEvent, NewPeerSpec, PeerIdentity, PeerStatus, PeerView, PendingEvent, SequenceVerdict,
};

/// A shared mesh subsystem, the shape the layer above holds.
pub type SharedMesh = Arc<dyn Mesh>;

/// The mesh subsystem, as the layer above reaches it.
///
/// The security boundary of federation is enforced here, not trusted from the caller: a peer
/// absent from the allow-list does not federate, a handshake that fails for any reason fails
/// with the same opaque error (sections 48, 161, 169), and a packet that has been seen before
/// or arrives out of order is refused before it is trusted (sections 152, 169).
#[async_trait]
pub trait Mesh: Send + Sync {
    /// Admits a peer to the allow-list, returning the stored view.
    ///
    /// The deliberate, operator-approved join of section 170: a peer exists in the mesh only
    /// because this was called for it. The public key must be a valid 32-byte Ed25519 key and
    /// the base URL a well-formed `https`/`wss` endpoint; a node id or key already present
    /// fails without overwriting, because a peer's identity is not something a second call may
    /// quietly replace. `now` stamps when the peer was admitted. Authorising that the caller
    /// may manage peers at all is the gateway's, done before this is reached.
    async fn add_peer(&self, spec: NewPeerSpec, now: Timestamp) -> Result<PeerView>;

    /// Sets a peer's allow-list status, returning the updated view.
    ///
    /// How an operator pauses, blocks, or re-allows a peer without forgetting its key — the
    /// row survives every state so a block is reversible without a fresh key exchange. Fails
    /// as [`not_found`](migo_protocol::fault::not_found) if the peer is not in the allow-list.
    async fn set_peer_status(&self, node_id: Id, status: PeerStatus) -> Result<PeerView>;

    /// Every peer in the allow-list, newest first, bounded by the shared page clamp.
    async fn peers(&self, limit: u16) -> Result<Vec<PeerView>>;

    /// One peer by node id, or [`not_found`](migo_protocol::fault::not_found) if it is not in
    /// the allow-list.
    async fn peer(&self, node_id: Id) -> Result<PeerView>;

    /// This node's own region.
    ///
    /// Where the operator configured this node to run. The layer above reads it to report
    /// where the node sits in the mesh; it is never put on the wire, because a handshake
    /// carries only a node id and a nonce (section 169).
    fn region(&self) -> &str;

    /// Builds this node's opening hello: its node id and a fresh random nonce.
    ///
    /// The first message of a handshake, sent to the peer. A new nonce is drawn each time; it
    /// is what binds the peer's proof to this exchange and cannot be reused (section 169).
    fn hello(&self) -> NodeHello;

    /// Signs this node's proof for a completed hello exchange.
    ///
    /// `local` is this node's hello, `remote` the peer's. The proof commits to both nonces and
    /// both ids over the mesh domain, so a man in the middle cannot splice it onto a different
    /// exchange. Infallible: signing is arithmetic, not I/O.
    fn prove(&self, local: &NodeHello, remote: &NodeHello, now: Timestamp) -> NodeProof;

    /// Verifies a peer's proof and resolves it to a [`PeerIdentity`].
    ///
    /// `local` is this node's hello, `remote` the peer's, `proof` the peer's proof over the
    /// exchange. The peer is looked up by `remote.node_id` in the allow-list *first*, and a
    /// node that is unknown, paused, or blocked is refused before the proof is even checked
    /// (sections 169, 170). A replayed nonce, a bad signature, a skewed clock — every failure
    /// returns the one opaque [`mesh_auth_failed`](migo_protocol::fault::mesh_auth_failed),
    /// because the peer must not learn which (section 48). On success the peer's last-seen
    /// time is stamped and its link sequence reset for the new session.
    async fn authenticate(
        &self,
        local: &NodeHello,
        remote: &NodeHello,
        proof: &NodeProof,
        now: Timestamp,
    ) -> Result<PeerIdentity>;

    /// Judges a packet's sequence number on `node`'s link and advances the link if it fits.
    ///
    /// The transport layer calls this for every packet after the handshake. An
    /// [`Accept`](SequenceVerdict::Accept) is safe to process; a [`Replay`](SequenceVerdict::Replay)
    /// must be dropped; a [`Gap`](SequenceVerdict::Gap) means the caller must tear the link
    /// down and re-handshake (section 152). Rejections are counted and logged.
    fn check_sequence(&self, node: Id, seq: u64) -> SequenceVerdict;

    /// Clears a link's sequence state, so its next packet must be sequence 1.
    ///
    /// For the transport layer to call when it drops a link for any reason of its own, so a
    /// reconnection starts numbering cleanly.
    fn reset_link(&self, node: Id);

    /// Checks an incoming routing epoch against the current one.
    ///
    /// The caller routed against a view of the mesh that may since have moved on.
    ///
    /// # Errors
    ///
    /// [`routing_epoch_stale`](migo_protocol::fault::routing_epoch_stale) if `incoming` is
    /// older than what this node knows — the caller should refetch the routing view and retry.
    fn check_epoch(&self, incoming: u64) -> Result<()>;

    /// The current routing epoch.
    fn epoch(&self) -> u64;

    /// Advances the routing epoch and returns the new value.
    ///
    /// The composition root calls this when the routing table it holds changes, so a request
    /// carrying the old epoch can be told it is stale.
    fn bump_epoch(&self) -> u64;

    /// Enqueues an event for delivery to another node, returning the queued view.
    ///
    /// The opcode must fall in the federation band
    /// ([`FEDERATION_OPCODE_MIN`](crate::model::FEDERATION_OPCODE_MIN)`..=`[`FEDERATION_OPCODE_MAX`](crate::model::FEDERATION_OPCODE_MAX))
    /// and the payload be non-empty. Delivery is at least once and the event is durable the
    /// instant this returns, so a crash before it is sent resends rather than loses it.
    async fn enqueue(&self, event: FederatedEvent, now: Timestamp) -> Result<PendingEvent>;

    /// Reads the events due for delivery at or before `now`, oldest first.
    ///
    /// A plain read, not a claim: two drainers may see the same event, which is safe because
    /// delivery is at least once and the consumer is idempotent (section 153).
    async fn due(&self, now: Timestamp) -> Result<Vec<PendingEvent>>;

    /// Marks an event delivered. Idempotent: a second call is harmless and the event is never
    /// handed out by [`due`](Mesh::due) again.
    async fn mark_delivered(&self, event_id: Id, now: Timestamp) -> Result<()>;

    /// Records a failed delivery attempt and reschedules it on an exponential backoff.
    ///
    /// `attempts_so_far` is the number of prior failures — the value carried on the
    /// [`PendingEvent`] — from which the next attempt's delay is computed: `base × 2^attempts`,
    /// clamped to the configured cap. The event stays in the queue and becomes due again then.
    async fn mark_failed(
        &self,
        event_id: Id,
        attempts_so_far: i32,
        now: Timestamp,
        error: &str,
    ) -> Result<()>;
}
