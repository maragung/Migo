//! The one implementation of [`Mesh`]: the store, the node handshake crypto, the two
//! in-memory replay defences, and the metrics, wired into a single subsystem.
//!
//! # What this type composes, and what it refuses to be
//!
//! [`MeshService`] owns nothing novel. The allow-list and the outbox are the
//! [`FederationStore`](migo_store::traits::FederationStore)'s; the handshake is
//! [`migo_crypto::node`]'s; the replay defences are this crate's `NonceWindow`
//! and `LinkSequences`. This type is the place they meet and the place the
//! security posture is enforced: a peer is looked up in the allow-list *before* its proof is
//! examined, every handshake failure returns the one opaque
//! [`mesh_auth_failed`](migo_protocol::fault::mesh_auth_failed) so a prober learns nothing
//! (sections 48, 161, 169), and only the metrics tell the reasons apart.
//!
//! # The node secret arrives, it is never fetched
//!
//! The signing key is handed to [`open`] by the composition root, which read it from a file at
//! startup. This crate never touches the filesystem and never transmits the secret — it holds
//! it to sign proofs and nothing else. The randomness is injected the same way, so a simulation
//! can drive a whole mesh deterministically.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use migo_core::metrics::Registry;
use migo_core::{Id, OsRandom, Random, Result, Timestamp};
use migo_crypto::node::{self, MAX_CLOCK_SKEW_MS};
use migo_crypto::{NodeHello, NodeProof, NodePublic, NodeSecret};
use migo_protocol::fault;
use migo_store::model::{NewOutboxEvent, NewPeer, OutboxRecord, PeerRecord};
use migo_store::{SharedStore, Store};

use crate::link::{LinkHealth, LinkSequences};
use crate::metrics::{HandshakeReject, Meters, ReplayReason};
use crate::model::{
    FederatedEvent, MeshConfig, NewPeerSpec, PeerIdentity, PeerStatus, PeerView, PendingEvent,
    SequenceVerdict, FEDERATION_OPCODE_MAX, FEDERATION_OPCODE_MIN,
};
use crate::replay::NonceWindow;
use crate::traits::{Mesh, SharedMesh};

/// The longest a peer's mesh URL may be. Long enough for a host, a port, and a path; short
/// enough that a malformed allow-list entry cannot bloat a row.
const MAX_BASE_URL_BYTES: usize = 512;

/// The mesh subsystem: the sole implementor of [`Mesh`].
///
/// Generic over the store so a test can drive it against an in-memory backend, defaulting to
/// the erased `dyn Store` the composition root holds. Every field is either shared-immutable or
/// guarded by its own lock, so the whole service is `Send + Sync` behind an `Arc`.
pub struct MeshService<S: ?Sized = dyn Store> {
    store: Arc<S>,
    config: MeshConfig,
    node_id: Id,
    region: String,
    secret: NodeSecret,
    random: Mutex<Box<dyn Random>>,
    nonces: NonceWindow,
    links: LinkSequences,
    health: LinkHealth,
    epoch: AtomicU64,
    meters: Meters,
}

impl<S> MeshService<S>
where
    S: Store + ?Sized,
{
    /// Assembles a service, validating the configuration once up front.
    ///
    /// `secret` is this node's signing key, `region` where it runs, `random` the entropy
    /// source (injected so a simulation can replay a run byte for byte). The routing epoch
    /// starts at zero.
    ///
    /// # Errors
    ///
    /// If `config` is unusable: a nonce window shorter than twice
    /// [`MAX_CLOCK_SKEW_MS`] (which would let a replay slip through the gap between the clock
    /// check and the nonce memory), a non-positive backoff base, a cap below the base, a zero
    /// drain batch, a zero degradation watermark, or an empty region. These are deployment
    /// misconfigurations, caught here rather than per request.
    pub fn new(
        store: Arc<S>,
        config: MeshConfig,
        node_id: Id,
        region: String,
        secret: NodeSecret,
        random: Box<dyn Random>,
        registry: &Registry,
    ) -> Result<Self> {
        if region.trim().is_empty() {
            return Err(fault::internal("mesh region must not be empty"));
        }
        if config.nonce_window_ms < 2 * MAX_CLOCK_SKEW_MS {
            return Err(fault::internal(
                "mesh nonce window must exceed twice the accepted clock skew",
            ));
        }
        if config.backoff_base_ms <= 0 {
            return Err(fault::internal("mesh backoff base must be positive"));
        }
        if config.backoff_cap_ms < config.backoff_base_ms {
            return Err(fault::internal(
                "mesh backoff cap must not be below the base",
            ));
        }
        if config.due_batch == 0 {
            return Err(fault::internal("mesh drain batch must be positive"));
        }
        if config.degraded_outbox_watermark == 0 {
            return Err(fault::internal(
                "mesh degradation watermark must be positive",
            ));
        }
        let nonces = NonceWindow::new(config.nonce_window_ms);
        Ok(Self {
            store,
            config,
            node_id,
            region,
            secret,
            random: Mutex::new(random),
            nonces,
            links: LinkSequences::new(),
            health: LinkHealth::new(),
            epoch: AtomicU64::new(0),
            meters: Meters::new(registry),
        })
    }

    /// A fresh, time-ordered id, drawn under the randomness lock.
    fn new_id(&self, at: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(at, &mut **random)
    }

    /// The delay before the next retry after `attempts` failures: `base × 2^attempts`, clamped
    /// to the configured cap.
    ///
    /// The exponent is clamped to 30 before shifting, so a long-dead event settles at the cap
    /// rather than overflowing the shift; the multiply saturates for the same reason.
    fn backoff_delay(&self, attempts: i32) -> i64 {
        let exponent = u32::try_from(attempts.clamp(0, 30)).unwrap_or(0);
        let factor = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
        self.config
            .backoff_base_ms
            .saturating_mul(factor)
            .min(self.config.backoff_cap_ms)
    }
}

/// Projects a stored peer row into the operator-facing view.
///
/// The fingerprint is derived from the stored key; a key too corrupt to parse yields an empty
/// fingerprint rather than an error, because one broken row must not make the whole allow-list
/// unreadable. A node id too corrupt to parse *is* an error — an id is not optional.
fn view_of(record: PeerRecord) -> Result<PeerView> {
    let node_id =
        Id::parse(&record.node_id).map_err(|_| fault::internal("corrupt node id in peer row"))?;
    let fingerprint = NodePublic::parse(&record.public_key)
        .map(|key| key.fingerprint())
        .unwrap_or_default();
    Ok(PeerView {
        node_id,
        region: record.region,
        base_url: record.base_url,
        status: PeerStatus::from_i16(record.status),
        fingerprint,
        added_at: record.added_at,
        last_seen_at: record.last_seen_at,
    })
}

/// Projects a stored outbox row into the drainer-facing view.
fn pending_of(record: OutboxRecord) -> Result<PendingEvent> {
    let target_node = Id::parse(&record.target_node)
        .map_err(|_| fault::internal("corrupt target node id in outbox row"))?;
    Ok(PendingEvent {
        event_id: record.event_id,
        target_node,
        opcode: record.opcode,
        payload: record.payload,
        attempts: record.attempts,
        next_attempt_at: record.next_attempt_at,
    })
}

/// Checks a peer's base URL is present, bounded, and a mesh scheme.
///
/// The scheme gate is deliberate: a federation link is `https`/`wss`, never plain `http`, so a
/// typo that would carry mesh traffic in the clear is refused at admission rather than
/// discovered on the wire.
fn validate_base_url(raw: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(fault::field_required("base_url"));
    }
    if trimmed.len() > MAX_BASE_URL_BYTES {
        return Err(fault::field_too_long("base_url", MAX_BASE_URL_BYTES));
    }
    if !(trimmed.starts_with("https://") || trimmed.starts_with("wss://")) {
        return Err(fault::validation(
            "base_url",
            "must be an https:// or wss:// URL",
        ));
    }
    Ok(())
}

#[async_trait]
impl<S> Mesh for MeshService<S>
where
    S: Store + ?Sized,
{
    async fn add_peer(&self, spec: NewPeerSpec, now: Timestamp) -> Result<PeerView> {
        // Validate the identity before it reaches the store: a key that is not a 32-byte
        // Ed25519 point could never verify a proof, so admitting it would only create a peer
        // that can never federate.
        NodePublic::parse(&spec.public_key)
            .map_err(|_| fault::validation("public_key", "must be a 32-byte Ed25519 public key"))?;
        validate_base_url(&spec.base_url)?;
        let region = spec.region.trim();
        if region.is_empty() {
            return Err(fault::field_required("region"));
        }
        let record = self
            .store
            .add_peer(NewPeer {
                node_id: spec.node_id.to_string(),
                public_key: spec.public_key,
                base_url: spec.base_url.trim().to_string(),
                region: region.to_string(),
                status: PeerStatus::Allowed.to_i16(),
                added_at: now,
            })
            .await?;
        self.meters.peer_added();
        view_of(record)
    }

    async fn set_peer_status(&self, node_id: Id, status: PeerStatus) -> Result<PeerView> {
        let record = self
            .store
            .set_peer_status(&node_id.to_string(), status.to_i16())
            .await?
            .ok_or_else(|| fault::not_found("peer"))?;
        view_of(record)
    }

    async fn observe_peer_lag(&self, node_id: Id) -> Result<PeerView> {
        // What the sender still owes this peer, whatever the retry schedule — the honest
        // measure of how far behind the link has fallen.
        let depth = self.store.pending_depth(&node_id.to_string()).await?;
        let record = self
            .store
            .peer(&node_id.to_string())
            .await?
            .ok_or_else(|| fault::not_found("peer"))?;
        let status = PeerStatus::from_i16(record.status);
        // The two automatic transitions of the peer state machine, and the only two: an
        // allowed peer whose backlog has crossed the watermark is marked degraded, and a
        // degraded peer that has caught up to half the watermark is allowed again. Half,
        // not the watermark itself, so a depth oscillating around the threshold cannot
        // flap the status drain by drain. Paused and blocked are the operator's and are
        // never touched here — the transition below carries its precondition in the write,
        // so an operator pausing a degraded peer between these lines is never overwritten.
        let watermark = u64::from(self.config.degraded_outbox_watermark);
        let caught_up = watermark / 2;
        let transition = match status {
            PeerStatus::Allowed if depth > watermark => {
                tracing::warn!(
                    node = %node_id,
                    depth,
                    watermark,
                    "peer's undelivered backlog crossed the degradation watermark (section 173)"
                );
                Some((status, PeerStatus::Degraded))
            }
            PeerStatus::Degraded if depth <= caught_up => {
                tracing::info!(
                    node = %node_id,
                    depth,
                    "a degraded peer caught up; the marking clears"
                );
                Some((status, PeerStatus::Allowed))
            }
            _ => None,
        };
        let record = match transition {
            // Compare-and-set: the write lands only if the status is still what this read
            // saw, so a decision an operator made in between stands.
            Some((from, to)) => self
                .store
                .transition_peer_status(&node_id.to_string(), from.to_i16(), to.to_i16())
                .await?
                .unwrap_or(record),
            None => record,
        };
        view_of(record)
    }

    async fn peers(&self, limit: u16) -> Result<Vec<PeerView>> {
        self.store
            .peers(limit)
            .await?
            .into_iter()
            .map(view_of)
            .collect()
    }

    async fn peer(&self, node_id: Id) -> Result<PeerView> {
        let record = self
            .store
            .peer(&node_id.to_string())
            .await?
            .ok_or_else(|| fault::not_found("peer"))?;
        view_of(record)
    }

    fn region(&self) -> &str {
        &self.region
    }

    fn hello(&self) -> NodeHello {
        let mut random = self.random.lock();
        NodeHello::new(self.node_id, &mut **random)
    }

    fn prove(&self, local: &NodeHello, remote: &NodeHello, now: Timestamp) -> NodeProof {
        node::prove(&self.secret, local, remote, now)
    }

    async fn authenticate(
        &self,
        local: &NodeHello,
        remote: &NodeHello,
        proof: &NodeProof,
        now: Timestamp,
    ) -> Result<PeerIdentity> {
        // The allow-list is the boundary of the mesh (section 170): a node the operator never
        // named is refused here, before its proof — or its payload — is looked at.
        let Some(record) = self.store.peer(&remote.node_id.to_string()).await? else {
            self.meters.handshake_rejected(HandshakeReject::UnknownPeer);
            tracing::warn!(node = %remote.node_id, "mesh handshake from a node not in the allow-list");
            return Err(fault::mesh_auth_failed("unknown node in handshake"));
        };
        // A paused or blocked peer is turned away before the proof too — the row and key are
        // kept so the state is reversible, but no handshake succeeds while it stands. A
        // degraded peer is not: degraded is a signal about the link's health, not a
        // suspension, so the handshake proceeds exactly as an allowed peer's (section 153).
        match PeerStatus::from_i16(record.status) {
            PeerStatus::Allowed | PeerStatus::Degraded => {}
            PeerStatus::Blocked => {
                self.meters.handshake_rejected(HandshakeReject::Blocked);
                tracing::warn!(node = %remote.node_id, "mesh handshake from a blocked peer");
                return Err(fault::mesh_auth_failed("handshake from a blocked peer"));
            }
            PeerStatus::Paused => {
                self.meters.handshake_rejected(HandshakeReject::Paused);
                tracing::warn!(node = %remote.node_id, "mesh handshake from a paused peer");
                return Err(fault::mesh_auth_failed("handshake from a paused peer"));
            }
        }
        // Only an allowed peer's nonce is recorded, so an unknown node cannot flood the window.
        // A replayed nonce is refused even if it would still pass the clock check.
        if !self.nonces.check_and_record(&remote.nonce, now) {
            self.meters.replay_rejected(ReplayReason::NonceReused);
            tracing::warn!(node = %remote.node_id, "mesh handshake nonce replayed");
            return Err(fault::mesh_auth_failed("handshake nonce replayed"));
        }
        let Ok(public) = NodePublic::parse(&record.public_key) else {
            self.meters
                .handshake_rejected(HandshakeReject::ProofInvalid);
            tracing::warn!(node = %remote.node_id, "peer key in the allow-list is corrupt");
            return Err(fault::mesh_auth_failed("corrupt peer key in allow-list"));
        };
        if let Err(error) = node::verify_proof(&public, local, remote, proof, now) {
            self.meters
                .handshake_rejected(HandshakeReject::ProofInvalid);
            tracing::warn!(node = %remote.node_id, %error, "mesh handshake proof did not verify");
            return Err(fault::mesh_auth_failed(format!(
                "proof did not verify: {error}"
            )));
        }
        // Proven. Stamp last-seen and clear any stale sequence state so the new session starts
        // numbering from one.
        self.store
            .touch_peer(&remote.node_id.to_string(), now)
            .await?;
        self.links.reset(remote.node_id);
        self.meters.handshake_accepted();
        Ok(PeerIdentity {
            node_id: remote.node_id,
            region: record.region,
            base_url: record.base_url,
        })
    }

    fn check_sequence(&self, node: Id, seq: u64) -> SequenceVerdict {
        let verdict = self.links.observe(node, seq);
        match verdict {
            SequenceVerdict::Accept => {}
            SequenceVerdict::Replay => {
                self.meters.replay_rejected(ReplayReason::SequenceReplay);
                tracing::warn!(node = %node, seq, "federation packet with a non-advancing sequence");
            }
            SequenceVerdict::Gap => {
                self.meters.replay_rejected(ReplayReason::SequenceGap);
                self.meters.link_reset();
                tracing::warn!(node = %node, seq, "sequence gap on a federation link; link reset (section 152)");
            }
        }
        verdict
    }

    fn reset_link(&self, node: Id) {
        self.links.reset(node);
    }

    fn check_epoch(&self, incoming: u64) -> Result<()> {
        let current = self.epoch.load(Ordering::Relaxed);
        if incoming < current {
            return Err(fault::routing_epoch_stale(format!(
                "incoming epoch {incoming} is older than current {current}"
            )));
        }
        Ok(())
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    fn bump_epoch(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn refresh_routing(&self, peer_epoch: u64) -> u64 {
        // A stale-view refusal names the epoch the peer is current at, so adopting it is one
        // compare-and-swap rather than a bump per refusal. The loop only retries when a
        // concurrent refresh or bump moved the counter underneath this call, and the
        // `current < peer_epoch` guard is what keeps the move monotonic: a peer naming an
        // older epoch — the refusal was crafted, or the view moved again mid-flight — never
        // drags this node's view backwards.
        let mut current = self.epoch.load(Ordering::Relaxed);
        while current < peer_epoch {
            match self.epoch.compare_exchange_weak(
                current,
                peer_epoch,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return peer_epoch,
                Err(observed) => current = observed,
            }
        }
        current
    }

    async fn enqueue(&self, event: FederatedEvent, now: Timestamp) -> Result<PendingEvent> {
        if !(FEDERATION_OPCODE_MIN..=FEDERATION_OPCODE_MAX).contains(&event.opcode) {
            return Err(fault::validation(
                "opcode",
                "must be in the federation band 208..=223",
            ));
        }
        if event.payload.is_empty() {
            return Err(fault::field_required("payload"));
        }
        let event_id = self.new_id(now);
        let record = self
            .store
            .enqueue_event(NewOutboxEvent {
                event_id,
                target_node: event.target_node.to_string(),
                opcode: event.opcode,
                payload: event.payload,
                created_at: now,
                next_attempt_at: now,
            })
            .await?;
        self.meters.outbox_enqueued();
        pending_of(record)
    }

    async fn due(&self, now: Timestamp) -> Result<Vec<PendingEvent>> {
        self.store
            .due_events(now, self.config.due_batch)
            .await?
            .into_iter()
            .map(pending_of)
            .collect()
    }

    async fn mark_delivered(&self, event_id: Id, now: Timestamp) -> Result<()> {
        self.store.mark_delivered(event_id, now).await?;
        self.meters.outbox_delivered();
        Ok(())
    }

    async fn mark_failed(
        &self,
        event_id: Id,
        attempts_so_far: i32,
        now: Timestamp,
        error: &str,
    ) -> Result<()> {
        let next_attempt_at = now.saturating_add_millis(self.backoff_delay(attempts_so_far));
        self.store
            .mark_failed(event_id, next_attempt_at, error)
            .await?;
        self.meters.outbox_failed();
        Ok(())
    }

    fn note_link_down(&self, node: Id) {
        // The mark is the whole action: the outbox failure that reported it was already
        // settled on its own backoff, and the rooms whose home node sits behind this
        // link read the mark on their next write attempt (section 170).
        self.health.mark_down(node);
    }

    fn note_link_up(&self, node: Id) {
        self.health.mark_up(node);
    }

    fn link_reachable(&self, node: Id) -> bool {
        self.health.is_reachable(node)
    }
}

/// Builds a shared mesh subsystem for the composition root, with the production randomness.
///
/// `secret` is this node's signing key, already read from its file by the caller; this crate
/// never reads it from disk and never transmits it.
///
/// # Errors
///
/// If the configuration is unusable; see [`MeshService::new`].
pub fn open(
    store: SharedStore,
    config: MeshConfig,
    node_id: Id,
    region: String,
    secret: NodeSecret,
    registry: &Registry,
) -> Result<SharedMesh> {
    let service = MeshService::new(
        store,
        config,
        node_id,
        region,
        secret,
        Box::new(OsRandom) as Box<dyn Random>,
        registry,
    )?;
    Ok(Arc::new(service))
}
