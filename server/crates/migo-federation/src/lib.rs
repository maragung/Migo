//! Federation — the mesh of brief sections 152, 169 and 170, and the four things this
//! crate is responsible for: who is allowed to federate, proving a peer is who it claims,
//! refusing a packet that has been seen before, and carrying an event to another region
//! even across a restart.
//!
//! # A peer is an entry in an allow-list, never a stranger who connected
//!
//! Section 170 is explicit that a node joins the mesh by an operator's deliberate,
//! approved act — there is no open auto-discovery, no "any node that can sign gets in".
//! So the boundary of the mesh is the allow-list: [`Mesh::add_peer`]
//! is the only way a node becomes known, and every handshake begins by looking the peer up
//! in it. A node the operator never named does not get an anonymous connection refused
//! politely — it is refused *before its payload is decoded* ([`Mesh::authenticate`]), which
//! is the difference between a mesh and a public endpoint.
//!
//! # Every handshake failure looks the same from outside
//!
//! An unknown node, a paused one, a blocked one, a bad signature, a skewed clock, a
//! replayed nonce — [`Mesh::authenticate`] answers every one of
//! them with the single opaque [`fault::mesh_auth_failed`](migo_protocol::fault::mesh_auth_failed):
//! same code, same symbol, nothing in the public detail. Section 48's same-error rule and
//! section 169's fail-closed handshake meet here — a peer probing the mesh must not be able
//! to tell "I do not know you" from "your signature was wrong", because the gap between them
//! is an oracle. The *metrics* still tell the reasons apart, because an operator watching a
//! spike of `blocked` versus `proof_invalid` is diagnosing two different attacks; the
//! **peer** learns only that it was turned away.
//!
//! # A packet is trusted once, in order, and never again
//!
//! Two defences sit under the handshake, both stateful and both this crate's. A **nonce
//! window** (`replay`) remembers the random 32-byte nonce of every recent handshake and
//! rejects a repeat, so a captured-and-replayed hello cannot re-authenticate within the
//! tolerance window section 169 requires. A **per-link sequence** (`link`) demands that
//! each packet on a link carry a number exactly one greater than the last: a number that
//! does not advance is a replay, and a *gap* is a suspected replay or a lost segment, which
//! section 152 says must reset the link rather than be quietly accepted. Neither defence can
//! read a payload — they guard the envelope, which is all an intermediate node is ever
//! allowed to see (section 169).
//!
//! # The outbox is durable because a federated event must survive a crash
//!
//! An event bound for another region is not delivered inline; it is written to the
//! [`FederationStore`](migo_store::traits::FederationStore) outbox in the same transaction as
//! the change it announces, and a sender drains it afterwards. That is what makes delivery
//! survive a restart, and what makes it *at least once* — a crash between the wire
//! acknowledgement and the delivered-mark resends, so every federation consumer must be
//! idempotent (section 153). [`Mesh::mark_failed`] pushes a failed
//! event's next attempt out on an exponential backoff, so a dead region costs a decaying
//! trickle of retries rather than a hot loop.
//!
//! # What this crate will not do
//!
//! It opens no sockets and frames no bytes: the handshake methods take and return the
//! [`migo_crypto::node`] messages, and the transport that carries them is the gateway's. It
//! holds no routing table and no shard map — it keeps only a routing *epoch*, a monotonic
//! counter the composition root bumps, so it can answer
//! [`routing_epoch_stale`](migo_protocol::fault::routing_epoch_stale) for a request made
//! against a stale view; the table those requests route against lives above. And it never
//! reads a message: a federated payload is a sealed envelope, opaque bytes it stores and
//! forwards without opening.
//!
//! [`Mesh::authenticate`]: traits::Mesh::authenticate

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]

mod link;
mod metrics;
pub mod model;
mod replay;
pub mod service;
pub mod traits;

pub use crate::model::{
    FederatedEvent, MeshConfig, NewPeerSpec, PeerIdentity, PeerStatus, PeerView, PendingEvent,
    SequenceVerdict, FEDERATION_OPCODE_MAX, FEDERATION_OPCODE_MIN,
};
pub use crate::service::{open, MeshService};
pub use crate::traits::{Mesh, SharedMesh};
