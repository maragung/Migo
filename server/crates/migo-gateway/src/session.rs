//! What a session is, reduced to the part other tasks may hold.
//!
//! A connection's own driver owns the mutable, un-shared state — which lifecycle [`Phase`] it
//! is in, the identity once it authenticates, the timers. What every *other* task needs — the
//! hub fanning an event out, a peer publishing to a topic — is only the ability to address
//! this session and drop bytes into its mailbox. That is [`SessionHandle`]: an id and an
//! [`Outbound`], cheap to clone and safe to hold after the driver has moved on.

use std::sync::Arc;

use migo_core::Id;

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
}

impl SessionHandle {
    /// Builds a handle around a mailbox.
    pub(crate) fn new(session_id: Id, outbound: Arc<Outbound>) -> Self {
        Self {
            session_id,
            outbound,
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
}
