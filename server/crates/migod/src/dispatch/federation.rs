//! The FEDERATION application opcodes: the server-to-server mesh (brief sections 152, 169, 170).
//!
//! Every opcode is a thin translation from a wire frame onto the `Mesh` subsystem. The mesh owns
//! every rule — the allow-list, the handshake envelope, the per-link sequence, the routing epoch,
//! and the durable outbox — so these handlers only decode, call the one backing method, and reply.
//! The shape follows the other dispatch modules exactly: decode the body with
//! [`from_frame`], await the single service method where one backs the opcode, and
//! [`reply`](ClientContext::reply) with the named response.
//!
//! # Opcode → method map
//!
//! | Opcode             | Wire payload      | `Mesh` method          | Response        |
//! |--------------------|-------------------|------------------------|-----------------|
//! | `FED_HELLO`        | `FedHello`        | `Mesh::check_epoch`    | `Acknowledged`  |
//! | `FED_AUTH`         | `FedAuth`         | `Mesh::check_epoch`    | `Acknowledged`  |
//! | `FED_PING`         | `FedPing`         | —                      | `FedPong`       |
//! | `FED_FORWARD`      | `FedForward`      | —                      | `Acknowledged`  |
//! | `FED_ACK`          | `FedAck`          | `Mesh::check_sequence` | `Acknowledged`  |
//! | `FED_ROOM_SUBSCRIBE` | `FedRouting`   | —                      | `Acknowledged`  |
//! | `FED_ROOM_EVENT`   | `FedRoomEvent`    | —                      | `Acknowledged`  |
//! | `FED_PRESENCE_DIGEST` | `FedPresenceDigest` | —                 | `Acknowledged`  |
//! | `FED_KEY_ROTATE`   | `FedKeyRotate`    | —                      | `Acknowledged`  |
//! | `FED_HEALTH`       | `FedHealth`       | —                      | `Acknowledged`  |
//! | `FED_SHARD_MAP`    | `FedShardMap`     | `Mesh::peers`          | `FedShardMap`   |
//! | `FED_ERROR`        | `FedError`        | —                      | `Acknowledged`  |
//! | `FED_CALL_RELAY`   | `FedEvent`        | —                      | `Acknowledged`  |
//! | `FED_DIRECTORY`    | `FedDirectoryReq` | `Mesh::peers`          | `FedDirectory`  |
//!
//! The `Mesh` subsystem is a security boundary, not an application router: it keeps the allow-list,
//! the handshake, the sequence window, and the routing epoch (section 169), and it carries opaque
//! envelopes in its outbox without opening them. So `FED_FORWARD`, `FED_ROOM_EVENT`,
//! `FED_PRESENCE_DIGEST`, `FED_KEY_ROTATE`, `FED_HEALTH`, and `FED_CALL_RELAY` have no backing
//! `Mesh` method — the substantive effect of each lands in the rooms, presence, or messaging
//! surfaces that the mesh only delivers to, never interprets — and the handler decodes the frame
//! (proving it is well-formed) and acknowledges. `FED_HELLO`/`FED_AUTH` carry the peer's claimed
//! routing epoch and are the two opcodes that name a mesh method directly: `check_epoch` rejects a
//! request built against a view older than this node knows (section 169). `FED_SHARD_MAP` and
//! `FED_DIRECTORY` are the read side of the same allow-list the operator administers, answered with
//! the peer list `Mesh::peers` returns.

use migo_core::Id;
use migo_core::Error;
use migo_federation::{PeerView, SharedMesh};
use migo_gateway::ClientContext;
use migo_protocol::{
    fault, from_frame, Frame, Acknowledged, FedAck, FedAuth, FedDirectory, FedDirectoryReq, FedError,
    FedEvent, FedForward, FedHealth, FedHello, FedKeyRotate, FedPeerView, FedPing,
    FedPong, FedPresenceDigest, FedRoomEvent, FedRouting, FedShardMap,
};

/// The default page clamp handed to `Mesh::peers` when a directory or shard-map view is requested.
const PEER_LIST_LIMIT: u16 = 256;

/// Validates a peer's claimed routing epoch and acknowledges.
///
/// `FED_HELLO` opens a handshake and carries the epoch the peer built its request against. A peer
/// working from a stale routing view must refetch and retry, so the epoch is checked before the
/// handshake proceeds further (section 169); every other failure mode of a handshake is the
/// opaque `mesh_auth_failed`, but a stale epoch is a client-retryable condition, not a security
/// refusal, and is reported through the standard epoch check.
pub(crate) async fn handle_hello(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedMesh,
) -> Result<(), Error> {
    let hello: FedHello = from_frame(frame).map_err(fault::from_wire)?;
    svc.check_epoch(hello.epoch)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Validates the authenticating peer's claimed routing epoch and acknowledges.
///
/// Like [`handle_hello`], the proof frame names the epoch the peer routed against; a stale epoch is
/// the one handshake-stage condition the node answers with a retryable error rather than the opaque
/// `mesh_auth_failed` (section 169). The proof itself is verified by `Mesh::authenticate`, which the
/// transport layer drives with the handshake crypto — not from this frame — so the handler only
/// gates on the epoch and acknowledges.
pub(crate) async fn handle_auth(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedMesh,
) -> Result<(), Error> {
    let auth: FedAuth = from_frame(frame).map_err(fault::from_wire)?;
    svc.check_epoch(auth.epoch)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Echoes the peer's nonce back as a `FedPong`.
///
/// The mesh carries no liveness state of its own; a ping proves the link is up and that the peer
/// decoded our frames, so the reply is the nonce we were sent, unchanged. No `Mesh` method backs a
/// ping — there is nothing to mutate — and the echoed nonce is what lets the sender correlate the
/// round trip.
pub(crate) async fn handle_ping(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    _svc: &SharedMesh,
) -> Result<(), Error> {
    let ping: FedPing = from_frame(frame).map_err(fault::from_wire)?;
    ctx.reply(&FedPong { nonce: ping.nonce })
}

/// Accepts a forwarded envelope and acknowledges.
///
/// A federation frame is a sealed envelope the mesh stores and forwards without opening (section
/// 169); the routing that decides its next hop lives above this crate. The handler decodes the frame
/// — proving it is well-formed — and acknowledges; the durable hand-off to the outbox is the
/// producer's (`Mesh::enqueue`), driven by the rooms or messaging layer, not reached from this
/// opcode.
pub(crate) async fn handle_forward(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    _svc: &SharedMesh,
) -> Result<(), Error> {
    let _forward = from_frame::<FedForward>(frame).map_err(fault::from_wire)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Feeds the peer's sequence number to the per-link window and acknowledges.
///
/// Every packet after the handshake is judged by `Mesh::check_sequence`: an `Accept` is safe to
/// process, a `Replay` must be dropped, and a `Gap` means the link must be torn down and
/// re-handshake (section 152). The verdict is recorded here; the transport layer acts on it for the
/// packets that follow. A `FedAck` names both the peer and the sequence it has seen, so the window
/// is advanced for exactly that node.
pub(crate) async fn handle_ack(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedMesh,
) -> Result<(), Error> {
    let ack: FedAck = from_frame(frame).map_err(fault::from_wire)?;
    let node = Id::parse(&ack.node_id).map_err(|_| fault::validation("node_id", "invalid node id"))?;
    svc.check_sequence(node, ack.seq);
    ctx.reply(&Acknowledged { ok: true })
}

/// Accepts a room subscription and acknowledges.
///
/// A peer joining a room's federation stream is an event for the rooms surface, which the mesh only
/// delivers to; there is no `Mesh` method that admits a subscription. The handler decodes the frame —
/// proving it is well-formed — and acknowledges. The rooms layer's own subscribe path is what
/// actually wires the peer into the room's topic.
pub(crate) async fn handle_room_subscribe(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    _svc: &SharedMesh,
) -> Result<(), Error> {
    let _subscribe = from_frame::<FedRouting>(frame).map_err(fault::from_wire)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Accepts a room event and acknowledges.
///
/// A federated room event is a sealed envelope for the rooms surface, which the mesh only forwards;
/// no `Mesh` method opens or stores it here. The handler decodes the frame — proving it is
/// well-formed — and acknowledges. The rooms layer's own ingest path is what applies the change to
/// the room's state.
pub(crate) async fn handle_room_event(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    _svc: &SharedMesh,
) -> Result<(), Error> {
    let _event = from_frame::<FedRoomEvent>(frame).map_err(fault::from_wire)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Accepts a presence digest and acknowledges.
///
/// A presence digest is a sealed summary for the presence surface, which the mesh only delivers;
/// no `Mesh` method inspects it. The handler decodes the frame — proving it is well-formed — and
/// acknowledges. The presence layer's own merge path is what folds the digest into this node's view.
pub(crate) async fn handle_presence_digest(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    _svc: &SharedMesh,
) -> Result<(), Error> {
    let _digest = from_frame::<FedPresenceDigest>(frame).map_err(fault::from_wire)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Accepts a key rotation and acknowledges.
///
/// A peer rotating its mesh signing key is an operator action against the allow-list (section 170),
/// not a frame this node applies unilaterally; no `Mesh` method swaps a key from a peer frame. The
/// handler decodes the frame — proving it is well-formed — and acknowledges. The operator tooling's
/// `add_peer`/`set_peer_status` path is what records the new key.
pub(crate) async fn handle_key_rotate(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    _svc: &SharedMesh,
) -> Result<(), Error> {
    let _rotate = from_frame::<FedKeyRotate>(frame).map_err(fault::from_wire)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Accepts a health report and acknowledges.
///
/// A peer's health is observability the operator reads from metrics, not state this crate keeps; no
/// `Mesh` method records it. The handler decodes the frame — proving it is well-formed — and
/// acknowledges. The node's own liveness is reported through `Mesh::region`/`Mesh::epoch` to the
/// operator's tooling.
pub(crate) async fn handle_health(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    _svc: &SharedMesh,
) -> Result<(), Error> {
    let _health = from_frame::<FedHealth>(frame).map_err(fault::from_wire)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Answers a shard-map request with this node's region and the allow-list it serves.
///
/// The shard map is the operator-administered allow-list (section 170) read back through
/// `Mesh::peers`, projected onto the wire `FedPeerView` each peer exposes. The region is this node's
/// own, taken from `Mesh::region`, because a shard map names where a peer is reachable from here.
pub(crate) async fn handle_shard_map(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedMesh,
) -> Result<(), Error> {
    let _request = from_frame::<FedShardMap>(frame).map_err(fault::from_wire)?;
    let peers = svc.peers(PEER_LIST_LIMIT).await?;
    let nodes: Vec<_> = peers.into_iter().map(peer_to_view).collect();
    ctx.reply(&FedShardMap {
        region: svc.region().to_string(),
        nodes,
    })
}

/// Accepts a mesh error report and acknowledges.
///
/// A federation error is observability for the operator (section 169), not a frame this crate acts
/// on; no `Mesh` method records a peer's error here. The handler decodes the frame — proving it is
/// well-formed — and acknowledges. The offending condition is retried through the outbox's normal
/// backoff.
pub(crate) async fn handle_error(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    _svc: &SharedMesh,
) -> Result<(), Error> {
    let _report = from_frame::<FedError>(frame).map_err(fault::from_wire)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Accepts a relayed call and acknowledges.
///
/// A 1:1 or SFU call relay is a sealed envelope for the call surface, which the mesh only forwards;
/// no `Mesh` method opens or routes it here. The handler decodes the frame — proving it is
/// well-formed — and acknowledges. The call surface's own relay path is what lands the media on the
/// far peer.
pub(crate) async fn handle_call_relay(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    _svc: &SharedMesh,
) -> Result<(), Error> {
    let _relay = from_frame::<FedEvent>(frame).map_err(fault::from_wire)?;
    ctx.reply(&Acknowledged { ok: true })
}

/// Answers a directory request with the allow-list this node serves.
///
/// The federation directory is the operator-administered allow-list (section 170) read back through
/// `Mesh::peers`, projected onto the wire `FedPeerView` each peer exposes. The query string on the
/// request is a forward-compatibility hook for a future filtered lookup; this build answers with the
/// whole list, bounded by the shared page clamp.
pub(crate) async fn handle_directory(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedMesh,
) -> Result<(), Error> {
    let _request = from_frame::<FedDirectoryReq>(frame).map_err(fault::from_wire)?;
    let peers = svc.peers(PEER_LIST_LIMIT).await?;
    let peers: Vec<_> = peers.into_iter().map(peer_to_view).collect();
    ctx.reply(&FedDirectory { peers })
}

/// Projects a domain [`PeerView`] onto the wire [`FedPeerView`] a peer may see.
///
/// The operator-visible identity — node id, region, and allow-list status — is what a peer learns
/// about another peer; the raw public key stays server-side (section 170) and the fingerprint is an
/// operator concern, not something a peer exposes about itself on the wire.
fn peer_to_view(peer: PeerView) -> FedPeerView {
    FedPeerView {
        node_id: peer.node_id.to_string(),
        region: peer.region,
        status: peer.status.slug().to_string(),
    }
}

