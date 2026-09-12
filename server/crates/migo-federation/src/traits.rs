//! What this crate offers the layer above: the whole mesh behind one erased trait.
//!
//! # Three audiences, one trait
//!
//! [`Mesh`] serves three callers. An **operator** administers the allow-list —
//! [`add_peer`](Mesh::add_peer), [`apply_peer`](Mesh::apply_peer),
//! [`set_peer_status`](Mesh::set_peer_status), [`peers`](Mesh::peers),
//! [`peer`](Mesh::peer) — the deliberate, approved joins section 170
//! requires. The **transport layer** drives a link: it builds a [`hello`](Mesh::hello),
//! signs a [`prove`](Mesh::prove), [`authenticate`](Mesh::authenticate)s the peer's proof,
//! and runs every subsequent packet through [`check_sequence`](Mesh::check_sequence),
//! reporting what its connects discovered — [`note_link_down`](Mesh::note_link_down) when a
//! peer could not be reached, [`note_link_up`](Mesh::note_link_up) when it was — so the
//! layer above can ask [`link_reachable`](Mesh::link_reachable) before writing to a room
//! whose home node sits behind that link (sections 170, 173). And a
//! **producer** — the rooms or messaging layer with an event bound for another region — uses
//! the outbox: [`enqueue`](Mesh::enqueue) to hand it over, then a drainer walks
//! [`due`](Mesh::due), [`mark_delivered`](Mesh::mark_delivered),
//! [`mark_failed`](Mesh::mark_failed), and [`observe_peer_lag`](Mesh::observe_peer_lag).
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

    /// Reconciles one configured peer into the allow-list, idempotently.
    ///
    /// The configuration-driven counterpart to [`add_peer`](Mesh::add_peer): the
    /// composition root calls this once per `federation.peers` entry at startup,
    /// so the operator's config document is the source of truth for the
    /// allow-list. A peer absent from the allow-list is admitted exactly as
    /// `add_peer` would admit it; a peer already admitted with the same key,
    /// address, and region is left untouched, so a restart converges instead of
    /// failing with `ALREADY_EXISTS`; and a changed key, address, or region is
    /// brought to what the configuration says, which is the rotation path
    /// section 170 leaves to operator tooling. A key change is logged, because
    /// swapping the key a handshake is checked against is a trust decision that
    /// belongs in the log even when the operator made it. A peer's allow-list
    /// *status* is never touched here: pausing and blocking are runtime
    /// decisions that survive a restart by design.
    async fn apply_peer(&self, spec: NewPeerSpec, now: Timestamp) -> Result<PeerView>;

    /// Sets a peer's allow-list status, returning the updated view.
    ///
    /// How an operator pauses, blocks, or re-allows a peer without forgetting its key — the
    /// row survives every state so a block is reversible without a fresh key exchange. Fails
    /// as [`not_found`](migo_protocol::fault::not_found) if the peer is not in the allow-list.
    async fn set_peer_status(&self, node_id: Id, status: PeerStatus) -> Result<PeerView>;

    /// Reads how far a peer has fallen behind and moves its status accordingly.
    ///
    /// The drainer calls this once per peer after a drain pass settles, because a pass is
    /// the moment the outbox's depth is a fact about the link rather than a guess. An
    /// allowed peer whose undelivered events aimed at it exceed the configured watermark
    /// becomes [`Degraded`](PeerStatus::Degraded); a degraded peer that has caught up to
    /// half the watermark becomes [`Allowed`](PeerStatus::Allowed) again — half, so a depth
    /// hovering at the threshold cannot flap the status. The transitions are
    /// compare-and-set, so an operator's pause or block is never overwritten by them, and
    /// they are the only automatic ones: this method never touches a paused or blocked
    /// row.
    ///
    /// Degraded is signalling, not policy (section 173): a degraded peer still federates
    /// and still receives everything owed to it, because section 153's at-least-once
    /// guarantee is not the marking's to bend. The transition is written to the peer's row
    /// so the operator surfaces — the allow-list view, the directory — carry it, and the
    /// drainer's log names the peer and the depth when it fires. Fails as
    /// [`not_found`](migo_protocol::fault::not_found) if the peer is not in the allow-list.
    async fn observe_peer_lag(&self, node_id: Id) -> Result<PeerView>;

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

    /// Records that a delivery attempt to `node` could not even connect.
    ///
    /// The transport layer calls this from the one place a partition is actually
    /// discovered: a drain whose TCP connect to the peer failed. This is the evidence
    /// section 170's read-only rule runs on — a room whose home node sits behind a
    /// link marked down is refused further writes with
    /// [`room_read_only_partition`](migo_protocol::fault::room_read_only_partition)
    /// rather than allowed to diverge (section 173, scenario 2). The mark stands until
    /// [`note_link_up`](Mesh::note_link_up) contradicts it.
    fn note_link_down(&self, node: Id);

    /// Records that `node` was reached: a batch was delivered to it, or it completed an
    /// inbound handshake.
    ///
    /// Either direction of the link proves the peer is back, so the rooms it homes may
    /// be written to again — a partition that ends must not leave the room read-only
    /// forever. Idempotent and harmless for a node never marked down.
    fn note_link_up(&self, node: Id);

    /// Whether `node` is believed reachable right now.
    ///
    /// `true` unless a failed connect is still standing uncontradicted. The unknown is
    /// deliberately the permissive answer: the mesh remembers evidence, it does not
    /// guess, so a peer it has never tried reads as reachable and a node this process
    /// just booted beside reads as reachable until its first drain attempt says
    /// otherwise.
    fn link_reachable(&self, node: Id) -> bool;
}
