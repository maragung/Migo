//! The seam between the session driver and the socket underneath it.
//!
//! The gateway does not know or care whether a session runs over a `WebSocket`, a QUIC
//! stream, or an in-memory pipe in a test. It knows only [`Transport`]: a thing it can pull a
//! frame's bytes from, push a frame's bytes to, and close. The composition root supplies the
//! real implementation — brief section 138 binds one MWP frame to one binary `WebSocket`
//! message, and that adaptor lives in `migod`, where the HTTP upgrade happens — so this crate
//! stays free of any particular socket library and stays testable against a pipe.
//!
//! # One frame per message
//!
//! Every [`recv`](Transport::recv) yields exactly one frame's bytes, and every
//! [`send`](Transport::send) takes exactly one frame's bytes: the length comes from the
//! message boundary the transport already provides (section 139), so the driver never parses
//! a length prefix and never coalesces two frames into one read.

use async_trait::async_trait;
use bytes::Bytes;

/// Why a transport operation failed.
///
/// Deliberately coarse: the driver reacts to *whether* the socket is still usable, not to the
/// particular errno. A [`Closed`](TransportError::Closed) ends the session cleanly; anything
/// else ends it as a transport error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The peer closed the connection, or it was already closed.
    Closed,
    /// The peer broke the transport contract — a text message where a binary one was required
    /// (section 138), or a frame past the size ceiling. The detail is for the log, never the
    /// peer.
    Protocol(String),
    /// The underlying socket failed for a reason below the protocol — a reset, a timeout, a
    /// broken pipe.
    Io(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("transport closed"),
            Self::Protocol(detail) => write!(f, "transport protocol error: {detail}"),
            Self::Io(detail) => write!(f, "transport io error: {detail}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// A bidirectional, message-framed byte transport for one session.
///
/// # Cancel safety
///
/// [`recv`](Transport::recv) **must be cancel-safe**: the driver races it against outbound
/// wakeups and heartbeat ticks in a `select`, so a pending `recv` future is dropped and
/// recreated on every loop turn. Dropping it must not consume or lose a frame. A `Stream`
/// adaptor over a `WebSocket` satisfies this; a hand-rolled read that buffers a partial
/// message across calls does not.
#[async_trait]
pub trait Transport: Send {
    /// Pulls the next frame's bytes, or `None` once the peer has closed cleanly.
    ///
    /// # Errors
    ///
    /// [`TransportError::Protocol`] if the peer sent something the binding forbids (a text
    /// message, an oversize frame); [`TransportError::Io`] if the socket failed underneath.
    async fn recv(&mut self) -> Result<Option<Bytes>, TransportError>;

    /// Writes one frame's bytes as one message.
    ///
    /// # Errors
    ///
    /// [`TransportError::Closed`] if the peer has gone; [`TransportError::Io`] on a socket
    /// failure.
    async fn send(&mut self, frame: Bytes) -> Result<(), TransportError>;

    /// Closes the transport, best-effort. A second call, or a call after the peer already
    /// left, is a no-op rather than an error.
    async fn close(&mut self);
}
