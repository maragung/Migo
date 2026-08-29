//! The server-to-server transport that carries the FED_* opcodes between nodes.
//!
//! The [`migo_federation::Mesh`] subsystem is a security boundary: the allow-list, the
//! handshake, the per-link sequence, the routing epoch, and the durable outbox all live
//! there, and none of it touches a socket. This module is the other half — the two tokio
//! tasks and the wire session that make the boundary reach another node:
//!
//! - a **listener**, bound when the operator configures [`Config::node`]`::mesh_bind`,
//!   accepts connections and drives the server side of the handshake;
//! - a **runner**, always spawned, drains the outbox (`Mesh::due`) and delivers each
//!   pending event to its target node as `FED_FORWARD`, marking it delivered on the peer's
//!   cumulative `FED_ACK` watermark and failed — with backoff — otherwise.
//!
//! # The wire
//!
//! Brief section 169: binary MWP/1 frames, length-prefixed with a u32 big-endian, over a
//! stream, no JSON anywhere. One MWP frame per mesh packet, so the frame's `correlation`
//! field carries the per-link sequence number the handshake's packet rules ask for: it
//! starts at one on every link, increases strictly, and a gap tears the link down. The
//! receiver answers with `FED_ACK { seq }`, a cumulative watermark — "everything up to and
//! including this sequence is applied" — which is exactly the bookkeeping
//! [`Mesh::mark_delivered`] needs, because delivery is at least once and the consuming
//! surfaces are idempotent (section 153).
//!
//! The handshake is the one [`migo_crypto`] already implements: both sides send
//! `FED_HELLO` (node id, region, routing epoch, fresh 32-byte nonce), then both send
//! `FED_AUTH` carrying [`migo_crypto::NodeProof`] — a signature over the domain-separated
//! transcript of both nonces and ids. The proof rides in the wire's `signature` field as
//! `signed_at (8 bytes BE) || signature (64 bytes)`, because the transcript commits to the
//! signing time and the wire struct has no separate clock field. Each side verifies the
//! other's proof through [`Mesh::authenticate`], which refuses an unknown, paused, or
//! blocked peer *before* looking at the signature.
//!
//! # What arrives, and where it goes
//!
//! A `FED_FORWARD`'s payload is itself an encoded MWP frame — the original event, opcode
//! and all, opaque to any node it merely passes through (section 169: a relay cannot read
//! a private envelope). The listener parses only the outer frame, checks the sequence, and
//! hands the inner one to [`IngestRouter`], which routes by opcode: room events are
//! published into the local hub so subscribed sessions receive them exactly as if a local
//! session had sent them; the rest are validated, counted, and logged, because their
//! final-mile crates (presence digests, call relay) do not yet expose an ingest port, and
//! the honest place for that limitation is a line of documentation, not a silent ack.
//!
//! # Testing
//!
//! The session runs over any `AsyncRead + AsyncWrite`, so a unit test drives two sessions
//! across a [`tokio::io::duplex`] pair with two real [`MeshService`]s that have each other
//! in their allow-lists — the full handshake, sequence, watermark, and ingest path, no
//! sockets. One integration test binds a real loopback listener to port zero and walks a
//! queued event from one node's outbox into the other's ingest, over TCP.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use migo_core::metrics::{Counter, Registry};
use migo_core::{Clock, Error, Id, Result, Timestamp};
use migo_crypto::node::NONCE_LEN;
use migo_crypto::{NodeHello, NodeProof};
#[cfg(test)]
use migo_federation::model::FederatedEvent;
use migo_federation::model::{PeerStatus, PendingEvent};
use migo_federation::{PeerView, SharedMesh};
use migo_gateway::Gateway;
use migo_protocol::{fault, from_frame, to_frame, Encode, Frame, Opcode};
use migo_wire::limits::MAX_FRAME_BYTES;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

/// How long a delivery session may stay quiet before the sender gives up on its acks and
/// reschedules the batch. A link that cannot ack five seconds' worth of frames is not a
/// link this node should be holding a queue against.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// The most bytes one direction of the mesh will read before refusing. A length prefix
/// that names more than a legal MWP frame is either corruption or an attack; both are the
/// same close-the-socket answer.
const MAX_WIRE_CHUNK: u32 = (MAX_FRAME_BYTES as u32) + 8;

/// The upper bound on how many events one delivery session batches. One event per link
/// round trip would make the outbox drain at the speed of the slowest peer's RTT; an
/// unbounded batch would hold the whole queue behind one link's health.
const BATCH_LIMIT: usize = 128;

/// What the runner does between outbox drains.
const RUNNER_TICK: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Writes one length-prefixed MWP frame.
async fn write_frame<S: AsyncWrite + Unpin + Send>(io: &mut S, frame: &Frame) -> Result<()> {
    let bytes = frame.encode().map_err(fault::from_wire)?;
    let mut chunk = BytesMut::new();
    chunk.reserve(bytes.len() + 4);
    chunk.put_u32(bytes.len() as u32);
    chunk.extend_from_slice(&bytes);
    io.write_all(&chunk)
        .await
        .map_err(|error| fault::internal(format!("mesh link write failed: {error}")))?;
    io.flush()
        .await
        .map_err(|error| fault::internal(format!("mesh link flush failed: {error}")))
}

/// Reads one length-prefixed MWP frame, refusing a prefix that names more than a legal one.
///
/// `Ok(None)` is the peer hanging up with a clean close at a frame boundary — the normal
/// end of a delivery session, since the sender leaves as soon as its watermark is covered.
/// A close that lands mid-frame is corruption or a crash, and stays an error.
async fn read_frame<S: AsyncRead + Unpin + Send>(io: &mut S) -> Result<Option<Frame>> {
    let mut prefix = [0u8; 4];
    match io.read_exact(&mut prefix).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(fault::internal(format!("mesh link read failed: {error}"))),
    }
    let length = u32::from_be_bytes(prefix);
    if length > MAX_WIRE_CHUNK {
        return Err(fault::internal("mesh link announced an oversized frame"));
    }
    let mut body = vec![0u8; length as usize];
    io.read_exact(&mut body)
        .await
        .map_err(|error| fault::internal(format!("mesh link closed mid-frame: {error}")))?;
    Frame::decode(Bytes::from(body))
        .map(Some)
        .map_err(fault::from_wire)
}

/// Builds a mesh frame around a wire struct.
fn framed<T: Encode>(opcode: Opcode, correlation: u32, value: &T) -> Result<Frame> {
    to_frame(opcode.to_wire(), correlation, value).map_err(fault::from_wire)
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

/// One side of a completed handshake.
struct Handshook {
    /// The peer, now proven rather than merely claimed.
    peer: Id,
}

/// Encodes a [`NodeHello`] into the wire's `FED_HELLO` payload.
fn hello_to_wire(hello: &NodeHello, region: &str, epoch: u64) -> migo_protocol::FedHello {
    migo_protocol::FedHello {
        node_id: hello.node_id.to_text(),
        region: region.to_string(),
        epoch,
        nonce: hello.nonce.to_vec(),
    }
}

/// Decodes the wire's `FED_HELLO` payload back into a [`NodeHello`].
fn hello_from_wire(wire: &migo_protocol::FedHello) -> Result<NodeHello> {
    let node_id: Id = wire
        .node_id
        .parse()
        .map_err(|_| fault::mesh_auth_failed("malformed mesh hello"))?;
    let nonce: [u8; NONCE_LEN] = wire
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| fault::mesh_auth_failed("malformed mesh hello"))?;
    Ok(NodeHello {
        node_id,
        nonce,
        // The mesh protocol version is fixed for this build; the wire does not carry it
        // because a version mismatch is a handshake the transcript itself refuses.
        protocol_version: migo_crypto::node::MESH_PROTOCOL_VERSION,
    })
}

/// Encodes a [`NodeProof`] into the wire's `FED_AUTH` payload: the signing time rides
/// ahead of the signature, because the transcript commits to it and the wire has no
/// separate clock field.
fn proof_to_wire(proof: &NodeProof, node_id: Id, epoch: u64) -> migo_protocol::FedAuth {
    let mut signature = Vec::with_capacity(8 + proof.signature.len());
    signature.extend_from_slice(&proof.signed_at.as_millis().to_be_bytes());
    signature.extend_from_slice(&proof.signature);
    migo_protocol::FedAuth {
        node_id: node_id.to_text(),
        signature,
        epoch,
    }
}

/// Decodes the wire's `FED_AUTH` payload back into a [`NodeProof`].
fn proof_from_wire(wire: &migo_protocol::FedAuth) -> Result<NodeProof> {
    if wire.signature.len() != 8 + 64 {
        return Err(fault::mesh_auth_failed("unexpected mesh handshake frame"));
    }
    let signed_at = Timestamp::from_millis(i64::from_be_bytes(
        wire.signature[..8]
            .try_into()
            .map_err(|_| fault::mesh_auth_failed("malformed mesh proof"))?,
    ));
    let signature: [u8; 64] = wire.signature[8..]
        .try_into()
        .map_err(|_| fault::mesh_auth_failed("malformed mesh hello"))?;
    Ok(NodeProof {
        signed_at,
        signature,
    })
}

/// Runs the client side of the handshake on a fresh connection to `peer`.
///
/// Both hellos, both proofs, each side verified through the mesh — after this returns the
/// link is a proven session with `peer`, numbered from sequence one.
async fn handshake<S: AsyncRead + AsyncWrite + Unpin + Send>(
    io: &mut S,
    mesh: &SharedMesh,
    region: &str,
    now: Timestamp,
) -> Result<Handshook> {
    let local = mesh.hello();
    write_frame(
        io,
        &framed(
            Opcode::FedHello,
            0,
            &hello_to_wire(&local, region, mesh.epoch()),
        )?,
    )
    .await?;

    let Some(reply) = read_frame(io).await? else {
        return Err(fault::mesh_auth_failed(
            "the peer closed during the handshake",
        ));
    };
    if reply.header.opcode != Opcode::FedHello.to_wire() {
        return Err(fault::mesh_auth_failed("unexpected mesh handshake frame"));
    }
    let remote_hello = from_frame::<migo_protocol::FedHello>(&reply).map_err(fault::from_wire)?;
    let remote = hello_from_wire(&remote_hello)?;

    write_frame(
        io,
        &framed(
            Opcode::FedAuth,
            0,
            &proof_to_wire(
                &mesh.prove(&local, &remote, now),
                local.node_id,
                mesh.epoch(),
            ),
        )?,
    )
    .await?;

    let Some(counter_reply) = read_frame(io).await? else {
        return Err(fault::mesh_auth_failed(
            "the peer closed during the handshake",
        ));
    };
    if counter_reply.header.opcode != Opcode::FedAuth.to_wire() {
        return Err(fault::mesh_auth_failed("unexpected mesh handshake frame"));
    }
    let counter_proof =
        from_frame::<migo_protocol::FedAuth>(&counter_reply).map_err(fault::from_wire)?;
    let identity = mesh
        .authenticate(&local, &remote, &proof_from_wire(&counter_proof)?, now)
        .await?;
    Ok(Handshook {
        peer: identity.node_id,
    })
}

/// Runs the server side of the handshake on an accepted connection.
///
/// The peer speaks first, so the listener learns who claims to be talking before it
/// reveals anything but its own hello; the proof it demands answers the claim, and the
/// mesh refuses an unknown peer before the signature is even checked (section 169).
async fn handshake_server<S: AsyncRead + AsyncWrite + Unpin + Send>(
    io: &mut S,
    mesh: &SharedMesh,
    now: Timestamp,
) -> Result<Handshook> {
    let Some(opening) = read_frame(io).await? else {
        return Err(fault::mesh_auth_failed(
            "the peer closed during the handshake",
        ));
    };
    if opening.header.opcode != Opcode::FedHello.to_wire() {
        return Err(fault::mesh_auth_failed("unexpected mesh handshake frame"));
    }
    let remote_hello = from_frame::<migo_protocol::FedHello>(&opening).map_err(fault::from_wire)?;
    // A peer working from a stale routing view must refetch and retry; the epoch check is
    // what tells it so, before the handshake is allowed to proceed (section 169).
    mesh.check_epoch(remote_hello.epoch)?;
    let remote = hello_from_wire(&remote_hello)?;

    let local = mesh.hello();
    write_frame(
        io,
        &framed(
            Opcode::FedHello,
            0,
            &hello_to_wire(&local, mesh.region(), mesh.epoch()),
        )?,
    )
    .await?;

    let Some(proof_frame) = read_frame(io).await? else {
        return Err(fault::mesh_auth_failed(
            "the peer closed during the handshake",
        ));
    };
    if proof_frame.header.opcode != Opcode::FedAuth.to_wire() {
        return Err(fault::mesh_auth_failed("unexpected mesh handshake frame"));
    }
    let remote_proof =
        from_frame::<migo_protocol::FedAuth>(&proof_frame).map_err(fault::from_wire)?;
    let identity = mesh
        .authenticate(&local, &remote, &proof_from_wire(&remote_proof)?, now)
        .await?;

    write_frame(
        io,
        &framed(
            Opcode::FedAuth,
            0,
            &proof_to_wire(
                &mesh.prove(&local, &remote, now),
                local.node_id,
                mesh.epoch(),
            ),
        )?,
    )
    .await?;

    Ok(Handshook {
        peer: identity.node_id,
    })
}

// ---------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------

/// Routes one inner event frame to the local surface it belongs to.
///
/// A room event is the one with a complete final mile: its subscribers are already in the
/// gateway's hub, authorized once at subscribe time, so the event is published to the room
/// topic exactly as a local session's would be. The others are real, validated frames whose
/// final-mile crates do not yet expose an ingest port — presence aggregation and call
/// signaling — and the honest treatment is count-and-log, not a pretend success deeper in.
pub(crate) struct IngestRouter {
    gateway: Option<Arc<Gateway>>,
    meters: MeshMeters,
    clock: Arc<dyn Clock>,
    /// What this node has ingested, capped, so the operator's metrics answer "is anything
    /// arriving at all" and a test can assert on the wire path without a gateway.
    seen: parking_lot::Mutex<Vec<(u32, usize)>>,
}

impl IngestRouter {
    fn new(gateway: Option<Arc<Gateway>>, registry: &Registry, clock: Arc<dyn Clock>) -> Self {
        Self {
            gateway,
            meters: MeshMeters::new(registry),
            clock,
            seen: parking_lot::Mutex::new(Vec::new()),
        }
    }

    fn note(&self, opcode: u32, payload_len: usize) {
        let mut seen = self.seen.lock();
        if seen.len() < 1024 {
            seen.push((opcode, payload_len));
        }
    }

    /// What this node has ingested from the mesh so far, oldest first, capped.
    ///
    /// An operator smoke check — "is anything actually arriving over the link" — and the
    /// integration tests' window onto the wire. The cap keeps it from being a log.
    pub fn ingested(&self) -> Vec<(u32, usize)> {
        self.seen.lock().clone()
    }

    fn ingest(&self, inner: Frame) -> Result<()> {
        let opcode = Opcode::from_wire(inner.header.opcode)
            .ok_or_else(|| fault::validation("opcode", "not a known federation event"))?;
        match opcode {
            Opcode::FedRoomEvent => {
                let event: migo_protocol::FedRoomEvent =
                    from_frame(&inner).map_err(fault::from_wire)?;
                self.route_room_event(event)
            }
            Opcode::FedPresenceDigest => {
                let digest: migo_protocol::FedPresenceDigest =
                    from_frame(&inner).map_err(fault::from_wire)?;
                // The final mile is presence's own aggregation, which has no ingest port
                // yet; the digest is validated above and counted below, and the port is
                // the next step rather than a silent success deeper in.
                tracing::info!(
                    region = %digest.region,
                    bytes = digest.digest.len(),
                    "presence digest ingested from the mesh"
                );
                self.note(inner.header.opcode, inner.payload.len());
                self.meters.ingested();
                Ok(())
            }
            Opcode::FedCallRelay => {
                let relay: migo_protocol::FedEvent =
                    from_frame(&inner).map_err(fault::from_wire)?;
                tracing::info!(
                    from = %relay.from,
                    kind = %relay.kind,
                    bytes = relay.payload.len(),
                    "call relay ingested from the mesh"
                );
                self.note(inner.header.opcode, inner.payload.len());
                self.meters.ingested();
                Ok(())
            }
            Opcode::FedRoomSubscribe => {
                let routing: migo_protocol::FedRouting =
                    from_frame(&inner).map_err(fault::from_wire)?;
                tracing::info!(
                    room = %routing.room_id.to_text(),
                    home = %routing.home_region,
                    epoch = routing.epoch,
                    "peer subscribed to a room this node watches"
                );
                self.note(inner.header.opcode, inner.payload.len());
                self.meters.ingested();
                Ok(())
            }
            Opcode::FedKeyRotate => {
                let rotate: migo_protocol::FedKeyRotate =
                    from_frame(&inner).map_err(fault::from_wire)?;
                // Announced, counted, and left to the operator: the allow-list holds one
                // key per peer, and swapping it is a deliberate admission, not something a
                // link decides on its own (section 169's rotation window).
                tracing::warn!(
                    node = %rotate.node_id,
                    bytes = rotate.new_public_key.len(),
                    "peer announced a key rotation; apply it in the allow-list"
                );
                self.note(inner.header.opcode, inner.payload.len());
                self.meters.ingested();
                Ok(())
            }
            Opcode::FedHealth => {
                let health: migo_protocol::FedHealth =
                    from_frame(&inner).map_err(fault::from_wire)?;
                tracing::debug!(node = %health.node_id, status = %health.status, "peer health");
                self.note(inner.header.opcode, inner.payload.len());
                self.meters.ingested();
                Ok(())
            }
            Opcode::FedError => {
                let error: migo_protocol::FedError =
                    from_frame(&inner).map_err(fault::from_wire)?;
                tracing::warn!(node = %error.node_id, code = error.code, "peer reported an error");
                self.note(inner.header.opcode, inner.payload.len());
                self.meters.ingested();
                Ok(())
            }
            _ => Err(fault::validation(
                "opcode",
                "not an event this node ingests from the mesh",
            )),
        }
    }

    /// Publishes a forwarded room event into the local hub.
    ///
    /// The inner payload is itself an encoded frame — the event as the home region's
    /// session would have pushed it — so the routing question is only which topic it
    /// belongs on, and the answer is the room the forward names.
    fn route_room_event(&self, event: migo_protocol::FedRoomEvent) -> Result<()> {
        let inner = Frame::decode(Bytes::from(event.payload)).map_err(fault::from_wire)?;
        let inner_opcode = Opcode::from_wire(inner.header.opcode)
            .ok_or_else(|| fault::validation("opcode", "not a known room event"))?;
        let topic = migo_protocol::Topic {
            kind: migo_protocol::TopicKind::Room,
            id: event.room_id,
        };
        if let Some(gateway) = &self.gateway {
            let now = self.clock.now();
            match inner_opcode {
                Opcode::RoomMemberEvent => {
                    let event: migo_protocol::RoomMemberEvent =
                        from_frame(&inner).map_err(fault::from_wire)?;
                    gateway.broadcast_to_topic(&topic, Opcode::RoomMemberEvent, &event, now);
                }
                Opcode::RoomStateEvent => {
                    let event: migo_protocol::RoomStateEvent =
                        from_frame(&inner).map_err(fault::from_wire)?;
                    gateway.broadcast_to_topic(&topic, Opcode::RoomStateEvent, &event, now);
                }
                _ => {
                    return Err(fault::validation(
                        "opcode",
                        "not an event that belongs on a room topic",
                    ))
                }
            }
        }
        self.note(inner.header.opcode, inner.payload.len());
        self.meters.ingested();
        Ok(())
    }
}

/// The mesh transport's counters.
struct MeshMeters {
    ingested: Arc<Counter>,
    delivered: Arc<Counter>,
    failed: Arc<Counter>,
}

impl MeshMeters {
    fn new(registry: &Registry) -> Self {
        Self {
            ingested: registry.counter(
                "migo_mesh_events_ingested_total",
                "Federation events ingested from peers",
                &[],
            ),
            delivered: registry.counter(
                "migo_mesh_events_delivered_total",
                "Outbox events delivered to peers",
                &[],
            ),
            failed: registry.counter(
                "migo_mesh_events_failed_total",
                "Outbox delivery attempts that failed",
                &[],
            ),
        }
    }

    fn ingested(&self) {
        self.ingested.inc();
    }

    fn delivered(&self, count: u64) {
        self.delivered.add(count);
    }

    fn failed(&self, count: u64) {
        self.failed.add(count);
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Serves one accepted connection to completion: handshake, then the read loop.
///
/// Every accepted `FED_FORWARD` is sequence-checked on the peer's link, ingested, and
/// covered by the cumulative `FED_ACK` this task sends back. A replay is dropped without
/// an ack — the sender will retry it — and a gap is a torn link: the session ends, the
/// link state resets, and the peer re-handshakes from sequence one.
pub(crate) async fn serve_session<S: AsyncRead + AsyncWrite + Unpin + Send>(
    mut io: S,
    mesh: SharedMesh,
    router: Arc<IngestRouter>,
    now: Timestamp,
) -> Result<()> {
    let handshook = handshake_server(&mut io, &mesh, now).await?;
    let peer = handshook.peer;
    let mut watermark: u64 = 0;
    let result = serve_reads(&mut io, &mesh, &router, peer, &mut watermark).await;
    // The link is over, whatever the reason: the next session with this peer must start
    // numbering from one, so the window forgets everything it saw.
    mesh.reset_link(peer);
    result
}

async fn serve_reads<S: AsyncRead + AsyncWrite + Unpin + Send>(
    io: &mut S,
    mesh: &SharedMesh,
    router: &Arc<IngestRouter>,
    peer: Id,
    watermark: &mut u64,
) -> Result<()> {
    loop {
        let Some(frame) = read_frame(io).await? else {
            // The peer hung up with a clean close: a delivery session ending the way
            // delivery sessions end. The link state reset happens in `serve_session`.
            return Ok(());
        };
        let opcode = Opcode::from_wire(frame.header.opcode)
            .ok_or_else(|| fault::validation("opcode", "not a known mesh opcode"))?;
        match opcode {
            Opcode::FedForward => {
                let forward: migo_protocol::FedForward =
                    from_frame(&frame).map_err(fault::from_wire)?;
                let sequence = frame.header.correlation as u64;
                match mesh.check_sequence(peer, sequence) {
                    migo_federation::SequenceVerdict::Accept => {}
                    migo_federation::SequenceVerdict::Replay => {
                        // Already applied. Dropping without an ack is safe: the sender's
                        // watermark already covers it, and if it does not, the next ack
                        // will.
                        tracing::debug!(peer = %peer.to_text(), sequence, "replayed mesh packet dropped");
                        continue;
                    }
                    migo_federation::SequenceVerdict::Gap => {
                        return Err(fault::internal(
                            "mesh link sequence gap; the link must be torn down",
                        ));
                    }
                }
                let inner =
                    Frame::decode(Bytes::from(forward.payload)).map_err(fault::from_wire)?;
                router.ingest(inner)?;
                *watermark = (*watermark).max(sequence);
                write_frame(
                    io,
                    &framed(
                        Opcode::FedAck,
                        0,
                        &migo_protocol::FedAck {
                            node_id: mesh.region().to_string(),
                            seq: *watermark,
                        },
                    )?,
                )
                .await?;
            }
            Opcode::FedPing => {
                let ping: migo_protocol::FedPing = from_frame(&frame).map_err(fault::from_wire)?;
                // There is no FED_PONG opcode: the heartbeat reuses the PING opcode for
                // both directions (section 145), the payload telling them apart.
                write_frame(
                    io,
                    &framed(
                        Opcode::Ping,
                        0,
                        &migo_protocol::FedPong { nonce: ping.nonce },
                    )?,
                )
                .await?;
            }
            Opcode::FedDirectory => {
                let _request: migo_protocol::FedDirectoryReq =
                    from_frame(&frame).map_err(fault::from_wire)?;
                let peers = mesh.peers(256).await?;
                write_frame(
                    io,
                    &framed(
                        Opcode::FedDirectory,
                        0,
                        &migo_protocol::FedDirectory {
                            peers: peers.into_iter().map(peer_to_wire).collect(),
                        },
                    )?,
                )
                .await?;
            }
            _ => {
                return Err(fault::validation(
                    "opcode",
                    "not an opcode this listener carries after the handshake",
                ))
            }
        }
    }
}

/// Projects a mesh peer onto the wire's directory entry.
fn peer_to_wire(peer: PeerView) -> migo_protocol::FedPeerView {
    migo_protocol::FedPeerView {
        node_id: peer.node_id.to_text(),
        region: peer.region,
        status: peer.status.slug().to_string(),
    }
}

/// Delivers a batch of outbox events to one peer over a fresh connection.
///
/// The whole session is one unit of work: handshake, send every event numbered from one,
/// hold the link open until the peer's cumulative watermark covers the batch, and report
/// exactly which events the watermark covered. Anything short of full coverage is a
/// partial failure — the caller marks what arrived and reschedules the rest.
async fn deliver_batch<S: AsyncRead + AsyncWrite + Unpin + Send>(
    mut io: S,
    mesh: &SharedMesh,
    router: &Arc<IngestRouter>,
    peer_view: &PeerView,
    events: &[PendingEvent],
    now: Timestamp,
) -> Result<Vec<Id>> {
    let handshook = handshake(&mut io, mesh, mesh.region(), now).await?;
    let _ = handshook;

    let mut delivered: Vec<Id> = Vec::new();
    let mut highest: u64 = 0;
    for (index, event) in events.iter().enumerate() {
        let sequence = (index + 1) as u32;
        let inner = Frame::decode(Bytes::from(event.payload.clone())).map_err(fault::from_wire)?;
        let forward = migo_protocol::FedForward {
            from: mesh.region().to_string(),
            to: peer_view.region.clone(),
            payload: inner.encode().map_err(fault::from_wire)?.to_vec(),
        };
        write_frame(&mut io, &framed(Opcode::FedForward, sequence, &forward)?).await?;
        highest = sequence as u64;
    }

    // Hold the link until the peer's watermark covers the batch, answering its own pings
    // if one lands mid-flight, so a link that works stays working under load.
    timeout(ACK_TIMEOUT, async {
        loop {
            let Some(frame) = read_frame(&mut io).await? else {
                return Err(fault::internal(
                    "the peer closed the link before acknowledging the batch",
                ));
            };
            match Opcode::from_wire(frame.header.opcode)
                .ok_or_else(|| fault::validation("opcode", "not a known mesh opcode"))?
            {
                Opcode::FedAck => {
                    let ack: migo_protocol::FedAck =
                        from_frame(&frame).map_err(fault::from_wire)?;
                    if ack.seq >= highest {
                        return Ok::<(), Error>(());
                    }
                }
                Opcode::Ping => {
                    let ping: migo_protocol::FedPing =
                        from_frame(&frame).map_err(fault::from_wire)?;
                    write_frame(
                        &mut io,
                        &framed(
                            Opcode::Ping,
                            0,
                            &migo_protocol::FedPong { nonce: ping.nonce },
                        )?,
                    )
                    .await?;
                }
                _ => {
                    return Err(fault::validation(
                        "opcode",
                        "unexpected mesh frame while awaiting acks",
                    ))
                }
            }
        }
    })
    .await
    .map_err(|_| fault::internal("the peer never acknowledged the batch"))??;

    for event in events {
        delivered.push(event.event_id);
    }
    let _ = router; // the sender's router is idle; delivery is the peer's ingest's job
    Ok(delivered)
}

/// The node address a peer's `base_url` names, stripped to `host:port`.
///
/// The allow-list validates `wss://` and `https://` forms; both name the same TCP
/// endpoint this transport speaks, so the scheme is decoration the dialer strips, and a
/// bare `host:port` is accepted for loopback deployments where no TLS terminates in front.
fn endpoint_of(base_url: &str) -> Result<String> {
    let authority = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let authority = authority.split('/').next().unwrap_or(authority);
    if authority.is_empty() || !authority.contains(':') {
        return Err(fault::validation(
            "base_url",
            "the peer endpoint must name a host and a port",
        ));
    }
    Ok(authority.to_string())
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/// The mesh transport a composition root owns: the allow-list and outbox it drains, the
/// gateway its ingest path publishes into, and the counters both report through.
pub struct MeshTransport {
    mesh: SharedMesh,
    router: Arc<IngestRouter>,
    meters: MeshMeters,
    clock: Arc<dyn Clock>,
}

impl MeshTransport {
    pub fn new(
        mesh: SharedMesh,
        gateway: Option<Arc<Gateway>>,
        registry: &Registry,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            router: Arc::new(IngestRouter::new(gateway, registry, Arc::clone(&clock))),
            meters: MeshMeters::new(registry),
            mesh,
            clock,
        }
    }

    /// Binds the mesh listener and spawns its accept loop, returning the bound address —
    /// port zero binds an ephemeral port, which is how tests keep off each other's sockets.
    ///
    /// # Errors
    ///
    /// Propagates the bind failure: a configured listener that cannot bind is a startup
    /// failure, not something to discover when the first peer dials.
    pub async fn spawn_listener(self: &Arc<Self>, bind: &str) -> Result<std::net::SocketAddr> {
        let listener = TcpListener::bind(bind)
            .await
            .map_err(|error| fault::internal(format!("cannot bind the mesh listener: {error}")))?;
        let bound = listener.local_addr().map_err(|error| {
            fault::internal(format!("cannot read the mesh listener address: {error}"))
        })?;
        let transport = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _address)) => {
                        let transport = Arc::clone(&transport);
                        tokio::spawn(async move {
                            let now = transport.clock.now();
                            let mesh = Arc::clone(&transport.mesh);
                            let router = Arc::clone(&transport.router);
                            if let Err(error) = serve_session(stream, mesh, router, now).await {
                                tracing::warn!(%error, "mesh session ended");
                            }
                        });
                    }
                    Err(error) => {
                        tracing::warn!(%error, "mesh listener accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        });
        Ok(bound)
    }

    /// Spawns the outbox runner.
    ///
    /// It runs whether or not any peer is configured: with an empty allow-list the drain
    /// is cheap and the queue stays empty, and a peer added later starts receiving without
    /// anybody restarting anything.
    pub fn spawn_runner(self: &Arc<Self>, clock: Arc<dyn Clock>) {
        let transport = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(RUNNER_TICK).await;
                if let Err(error) = transport.drain_once(clock.now()).await {
                    tracing::warn!(%error, "mesh outbox drain failed");
                }
            }
        });
    }

    /// The ingest window, for the operator and the tests.
    pub fn ingested(&self) -> Vec<(u32, usize)> {
        self.router.ingested()
    }

    /// The ingest router itself, for the tests that drive delivery by hand.
    #[cfg(test)]
    fn router_ref(&self) -> &Arc<IngestRouter> {
        &self.router
    }

    /// One pass of the outbox: group what is due by target node, deliver each group over a
    /// fresh session, and settle the queue.
    ///
    /// Public because it is an operations primitive as much as the runner's step: an
    /// operator — or a test that wants its delivery deterministic — asks the node to
    /// drain now rather than waiting for the tick.
    pub async fn drain_once(&self, now: Timestamp) -> Result<()> {
        let due = self.mesh.due(now).await?;
        if due.is_empty() {
            return Ok(());
        }
        let mut groups: HashMap<Id, Vec<PendingEvent>> = HashMap::new();
        for event in due {
            // One session carries at most BATCH_LIMIT events: a queue far longer than that
            // drains across several sessions, each with its own handshake and watermark,
            // so one slow link cannot hold the whole outbox hostage.
            // One session carries at most BATCH_LIMIT events: a queue far longer than
            // that drains across several passes, each with its own handshake and
            // watermark, so one slow link cannot hold the whole outbox hostage. An event
            // over the cap is left exactly where it is — `due` reads without claiming —
            // and goes out on the next pass.
            let group = groups.entry(event.target_node).or_default();
            if group.len() < BATCH_LIMIT {
                group.push(event);
            }
        }
        for (target, events) in groups {
            let peer = match self.mesh.peer(target).await {
                Ok(peer) if peer.status == PeerStatus::Allowed => peer,
                Ok(_) => continue, // paused or blocked: the events stay queued, unsent
                Err(error) => {
                    tracing::warn!(%error, "cannot resolve the mesh peer an event names");
                    continue;
                }
            };
            let endpoint = match endpoint_of(&peer.base_url) {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    self.settle_failure(&events, now, &error.to_string()).await;
                    continue;
                }
            };
            let stream = match tokio::net::TcpStream::connect(&endpoint).await {
                Ok(stream) => stream,
                Err(error) => {
                    self.settle_failure(&events, now, &format!("cannot reach the peer: {error}"))
                        .await;
                    continue;
                }
            };
            match deliver_batch(stream, &self.mesh, &self.router, &peer, &events, now).await {
                Ok(delivered) => {
                    self.meters.delivered(delivered.len() as u64);
                    for event_id in delivered {
                        self.mesh.mark_delivered(event_id, now).await?;
                    }
                }
                Err(error) => self.settle_failure(&events, now, &error.to_string()).await,
            }
        }
        Ok(())
    }

    /// Reschedules a batch that did not arrive, with the failure the next backoff grows from.
    async fn settle_failure(&self, events: &[PendingEvent], now: Timestamp, why: &str) {
        self.meters.failed(events.len() as u64);
        for event in events {
            if let Err(error) = self
                .mesh
                .mark_failed(event.event_id, event.attempts, now, why)
                .await
            {
                tracing::warn!(%error, "could not reschedule a mesh event");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Two real mesh services that have each other in their allow-lists, joined by a
    //! duplex pair instead of a socket: the handshake, the sequence rules, the watermark,
    //! and the ingest path, byte for byte, with nothing faked but the wire.

    use super::*;
    use migo_core::random::SeededRandom;
    use migo_core::SystemClock;
    use migo_crypto::NodeSecret;
    use migo_federation::{MeshService, NewPeerSpec};
    use migo_store::MemoryStore;
    use tokio::io::duplex;

    const NOW: i64 = 1_700_000_000_000;

    /// A node with its own store, key, and registry, and `peer` admitted to its allow-list.
    async fn node(name: u8, peer: Id, peer_key: &[u8]) -> SharedMesh {
        let registry = Registry::new();
        let mesh = MeshService::new(
            Arc::new(MemoryStore::new()),
            migo_federation::MeshConfig::default(),
            Id::from(u128::from(name) * 0x0101),
            format!("region-{name}"),
            NodeSecret::from_seed(&[name; 32]).expect("a 32-byte seed builds a key"),
            Box::new(SeededRandom::new(u64::from(name) * 7919)),
            &registry,
        )
        .expect("the mesh configuration is valid");
        let mesh: SharedMesh = Arc::new(mesh);
        mesh.add_peer(
            NewPeerSpec {
                node_id: peer,
                public_key: peer_key.to_vec(),
                base_url: format!("wss://node-{name}.test:9999"),
                region: format!("region-{}", if name == 1 { 2 } else { 1 }),
            },
            Timestamp::from_millis(NOW),
        )
        .await
        .expect("a fresh allow-list admits the peer");
        mesh
    }

    async fn pair() -> (SharedMesh, SharedMesh, Id, Id) {
        let a_id = Id::from(0x0101);
        let b_id = Id::from(0x0202);
        // Each side needs the other's public key before either allow-list exists, so the
        // keys are built first and handed to both.
        let key_a = NodeSecret::from_seed(&[1u8; 32])
            .expect("a 32-byte seed builds a key")
            .public()
            .to_bytes();
        let key_b = NodeSecret::from_seed(&[2u8; 32])
            .expect("a 32-byte seed builds a key")
            .public()
            .to_bytes();
        let a = node(1, b_id, &key_b).await;
        let b = node(2, a_id, &key_a).await;
        (a, b, a_id, b_id)
    }

    fn registry() -> Registry {
        Registry::new()
    }

    /// A presence digest encoded as the inner frame a `FED_FORWARD` carries.
    fn digest_frame(note: &str) -> Bytes {
        framed(
            Opcode::FedPresenceDigest,
            0,
            &migo_protocol::FedPresenceDigest {
                region: "region-1".to_string(),
                digest: note.as_bytes().to_vec(),
            },
        )
        .expect("the digest encodes")
        .encode()
        .expect("the frame encodes")
    }

    /// The whole product, over one duplex: node A delivers an outbox event to node B, and
    /// B ingests it. This is the path the mesh exists for.
    #[tokio::test]
    async fn a_queued_event_reaches_the_peer_and_is_marked_delivered() {
        let (mesh_a, mesh_b, _a_id, b_id) = pair().await;
        let now = Timestamp::from_millis(NOW);
        let registry = registry();
        let transport_b = Arc::new(MeshTransport::new(
            mesh_b.clone(),
            None,
            &registry,
            Arc::new(SystemClock),
        ));

        let event = FederatedEvent {
            target_node: b_id,
            opcode: Opcode::FedPresenceDigest.to_wire() as i32,
            payload: digest_frame("hello across the mesh").to_vec(),
        };
        mesh_a
            .enqueue(event, now)
            .await
            .expect("a federation-band event enqueues");

        let due = mesh_a.due(now).await.expect("the queue reads");
        assert_eq!(due.len(), 1, "the event waits to be delivered");

        let (client_io, server_io) = duplex(64 * 1024);
        let peer_view = mesh_a.peer(b_id).await.expect("the peer resolves");
        let server_mesh = mesh_b;
        let server_router = transport_b.router_ref().clone();
        let server =
            tokio::spawn(
                async move { serve_session(server_io, server_mesh, server_router, now).await },
            );
        let delivered = deliver_batch(
            client_io,
            &mesh_a,
            transport_b.router_ref(),
            &peer_view,
            &due,
            now,
        )
        .await
        .expect("the batch is delivered and acknowledged");
        server
            .await
            .expect("the server session completes")
            .expect("the server side of the session is clean");

        assert_eq!(delivered.len(), 1, "the watermark covered the whole batch");
        mesh_a
            .mark_delivered(delivered[0], now)
            .await
            .expect("the delivery settles");
        let later = Timestamp::from_millis(NOW + 60_000);
        assert!(
            mesh_a.due(later).await.expect("the queue reads").is_empty(),
            "a delivered event never comes due again"
        );
        let seen = transport_b.ingested();
        let inner_payload = Frame::decode(digest_frame("hello across the mesh"))
            .expect("the digest frame decodes")
            .payload
            .len();
        assert_eq!(
            seen,
            vec![(Opcode::FedPresenceDigest.to_wire(), inner_payload)],
            "node B ingested the digest the runner forwarded"
        );
    }

    /// Roles flipped from the first test: node B delivers to node A, so the failure mode
    /// the two-node integration test saw — A rejecting B's proof — reproduces in-crate.
    #[tokio::test]
    async fn b_dials_a_over_a_duplex() {
        let (mesh_a, mesh_b, a_id, _b_id) = pair().await;
        let now = Timestamp::from_millis(NOW);
        let registry = registry();
        let transport_a = Arc::new(MeshTransport::new(
            mesh_a.clone(),
            None,
            &registry,
            Arc::new(SystemClock),
        ));

        let event = FederatedEvent {
            target_node: a_id,
            opcode: Opcode::FedPresenceDigest.to_wire() as i32,
            payload: digest_frame("from b").to_vec(),
        };
        mesh_b.enqueue(event, now).await.expect("enqueues");
        let due = mesh_b.due(now).await.expect("the queue reads");
        assert_eq!(due.len(), 1);

        let (client_io, server_io) = duplex(64 * 1024);
        let peer_view = mesh_b.peer(a_id).await.expect("the peer resolves");
        let server_mesh = mesh_a;
        let server_router = transport_a.router_ref().clone();
        let server =
            tokio::spawn(
                async move { serve_session(server_io, server_mesh, server_router, now).await },
            );
        let delivered = deliver_batch(
            client_io,
            &mesh_b,
            transport_a.router_ref(),
            &peer_view,
            &due,
            now,
        )
        .await
        .expect("B delivers to A");
        server
            .await
            .expect("server completes")
            .expect("server session clean");
        assert_eq!(delivered.len(), 1);
    }

    /// A replayed packet is dropped without an ack and without harming the link; a gap is
    /// not — the session must end, because what is missing is unknown and applying what
    /// came after it would be applying events out of order.
    #[tokio::test]
    async fn a_replay_is_dropped_and_a_gap_tears_the_link_down() {
        let (mesh_a, mesh_b, _a_id, _b_id) = pair().await;
        let now = Timestamp::from_millis(NOW);
        let registry = registry();
        let transport_b = Arc::new(MeshTransport::new(
            mesh_b.clone(),
            None,
            &registry,
            Arc::new(SystemClock),
        ));

        let (mut client_io, server_io) = duplex(64 * 1024);
        let server_mesh = mesh_b;
        let server_router = transport_b.router_ref().clone();
        let server =
            tokio::spawn(
                async move { serve_session(server_io, server_mesh, server_router, now).await },
            );

        // Client side of the handshake, by hand, so the sequence can be driven directly.
        // The server waits for the client's hello, so the client speaks first.
        let local = mesh_a.hello();
        write_frame(
            &mut client_io,
            &framed(
                Opcode::FedHello,
                0,
                &hello_to_wire(&local, "region-1", mesh_a.epoch()),
            )
            .expect("the hello encodes"),
        )
        .await
        .expect("the hello is sent");
        let opening = read_frame(&mut client_io)
            .await
            .expect("the server hello arrives")
            .expect("the server hello is a frame");
        assert_eq!(opening.header.opcode, Opcode::FedHello.to_wire());
        let remote_hello =
            from_frame::<migo_protocol::FedHello>(&opening).expect("the hello decodes");
        let remote = hello_from_wire(&remote_hello).expect("the hello builds");
        write_frame(
            &mut client_io,
            &framed(
                Opcode::FedAuth,
                0,
                &proof_to_wire(
                    &mesh_a.prove(&local, &remote, now),
                    local.node_id,
                    mesh_a.epoch(),
                ),
            )
            .expect("the proof encodes"),
        )
        .await
        .expect("the proof is sent");
        let counter = read_frame(&mut client_io)
            .await
            .expect("the server proof arrives")
            .expect("the server proof is a frame");
        let identity = mesh_a
            .authenticate(
                &local,
                &remote,
                &proof_from_wire(
                    &from_frame::<migo_protocol::FedAuth>(&counter).expect("the proof decodes"),
                )
                .expect("the proof builds"),
                now,
            )
            .await
            .expect("the server's proof verifies");

        // Sequence one: applied, acknowledged.
        let forward = migo_protocol::FedForward {
            from: "region-1".to_string(),
            to: "region-2".to_string(),
            payload: digest_frame("first").to_vec(),
        };
        write_frame(
            &mut client_io,
            &framed(Opcode::FedForward, 1, &forward).expect("the forward encodes"),
        )
        .await
        .expect("the first forward is sent");
        let ack = read_frame(&mut client_io)
            .await
            .expect("the ack arrives")
            .expect("the ack is a frame");
        assert_eq!(ack.header.opcode, Opcode::FedAck.to_wire());
        let ack: migo_protocol::FedAck = from_frame(&ack).expect("the ack decodes");
        assert_eq!(ack.seq, 1, "the watermark covers sequence one");

        // Sequence one again: a replay, dropped silently.
        write_frame(
            &mut client_io,
            &framed(Opcode::FedForward, 1, &forward).expect("the forward encodes"),
        )
        .await
        .expect("the replay is sent");
        // Sequence three: a gap, which must end the session rather than be applied.
        write_frame(
            &mut client_io,
            &framed(Opcode::FedForward, 3, &forward).expect("the forward encodes"),
        )
        .await
        .expect("the gap is sent");
        server
            .await
            .expect("the server session ends")
            .expect_err("a gap tears the link down");
        assert_eq!(
            transport_b.ingested().len(),
            1,
            "only the first forward was ingested; the replay was dropped and the gap was not"
        );
        let _ = identity;
    }
}
