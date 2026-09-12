//! What a session is, reduced to the part other tasks may hold.
//!
//! A connection's own driver owns the mutable, un-shared state — which lifecycle [`Phase`] it
//! is in, the identity once it authenticates, the timers. What every *other* task needs — the
//! hub fanning an event out, a peer publishing to a topic — is only the ability to address
//! this session and drop bytes into its mailbox. That is [`SessionHandle`]: an id and an
//! [`Outbound`], cheap to clone and safe to hold after the driver has moved on.

use std::sync::Arc;

use migo_core::Id;
use migo_protocol::BandwidthMode;

use crate::outbound::Outbound;

/// Where an *established* session is in its lifecycle, from brief section 149.
///
/// The pre-`HELLO` interval — where only `HELLO` is accepted — is not a state here: it is owned
/// by the connection's handshake step, before a session (and so a `Phase`) exists at all. Once a
/// session is established it is one of these two.
///
/// The gate is strict: before a session is [`Ready`](Phase::Ready) only a narrow set of opcodes
/// is accepted, and a frame out of turn is a protocol violation that closes the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Greeted but not authenticated. A second `HELLO` is a violation; `AUTHENTICATE`, `PING`,
    /// and `ACK` are accepted, application opcodes are not.
    AwaitingAuth,
    /// Authenticated. The full opcode surface is open, subject to capability and rate limits.
    Ready,
}

/// The shareable face of a session: how to address it and how to reach its mailbox.
///
/// Cloning is an [`Arc`] bump. Pushing into a handle whose connection has already closed is a
/// no-op the [`Outbound`] absorbs, so a stale handle held briefly by the hub during a
/// disconnect race is harmless rather than a panic.
#[derive(Clone)]
pub(crate) struct SessionHandle {
    session_id: Id,
    outbound: Arc<Outbound>,
    /// The bandwidth mode this session negotiated in its `HELLO`.
    ///
    /// Stored on the handle — the one per-session thing every later frame may need to
    /// consult — so the dispatcher reads it off the context without the connection
    /// having to thread it through every request (brief section 75: the server adapts
    /// its event cadence to the mode the client declared, which only works if the mode
    /// outlives the handshake that carried it).
    bandwidth_mode: BandwidthMode,
    /// The feature set this session negotiated: the client's requested bits masked against
    /// the node's advertised set, the same intersection `WELCOME` reports.
    ///
    /// Stored beside the bandwidth mode for the same reason — the one per-session fact every
    /// later frame may need to consult. Section 148 makes the intersection fixed for the
    /// session's lifetime (a feature that appears mid-session reaches a client on its next
    /// connection), so a value decided once at the handshake is the whole truth, and a
    /// dispatcher reads it off the context to gate the opcodes the registry ties to a bit.
    features: u64,
}

impl SessionHandle {
    /// Builds a handle around a mailbox, the mode the session negotiated, and the feature
    /// set the handshake settled on.
    pub(crate) fn new(
        session_id: Id,
        outbound: Arc<Outbound>,
        bandwidth_mode: BandwidthMode,
        features: u64,
    ) -> Self {
        Self {
            session_id,
            outbound,
            bandwidth_mode,
            features,
        }
    }

    /// This session's id, the key the hub files it under.
    pub(crate) fn session_id(&self) -> Id {
        self.session_id
    }

    /// This session's mailbox.
    pub(crate) fn outbound(&self) -> &Arc<Outbound> {
        &self.outbound
    }

    /// The bandwidth mode the session negotiated in `HELLO`.
    pub(crate) fn bandwidth_mode(&self) -> BandwidthMode {
        self.bandwidth_mode
    }

    /// The feature set this session negotiated: requested bits ∩ advertised bits.
    ///
    /// Fixed for the session's lifetime (section 148), so a handler asking "did this client
    /// negotiate the bit?" is reading a fact the handshake already settled.
    pub(crate) fn features(&self) -> u64 {
        self.features
    }
}
