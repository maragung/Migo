//! The WebSocket binding of the gateway's [`Transport`].
//!
//! The gateway is written against [`Transport`], not against any particular socket: it pulls frame
//! bytes and writes frame bytes and knows nothing else about the wire. [`WsTransport`] is the
//! adapter that makes an axum [`WebSocket`] look like that trait, and it is the only place in the
//! server that touches WebSocket message framing.
//!
//! Two rules from the brief live here. The transport is binary: the protocol frames its own
//! messages inside binary WebSocket messages, so a text message is a protocol violation (section
//! 138), refused rather than decoded. And ping/pong are the WebSocket layer's own keepalive, not
//! application frames — the layer beneath answers a ping automatically, so this adapter skips them
//! and pulls the next message rather than surfacing them upward. A close, or a stream that has run
//! out, is a clean end and reads back as `None`.

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use tokio::sync::Mutex;

use migo_gateway::{Transport, TransportError};

/// A gateway [`Transport`] over one axum [`WebSocket`] connection.
///
/// The socket is wrapped in a [`Mutex`], and the reason is purely one of marker traits. An axum
/// `WebSocket` is `Send` but not `Sync`. The gateway runs each connection from a single task, but a
/// few of its handshake steps borrow the connection shared (`&self`) across an await, which leaves
/// the connection `Send` only when its transport is `Sync`. `Mutex<WebSocket>` is `Sync` whenever
/// `WebSocket` is `Send`, so wrapping the socket is what lets the connection future cross the thread
/// boundary axum's upgrade handler requires.
///
/// It costs nothing at runtime. Every [`Transport`] method takes `&mut self`, so each reaches the
/// socket through [`Mutex::get_mut`], which hands back the inner `&mut WebSocket` by plain field
/// access — no lock is taken, and there is no contention to take one for, since exactly one task
/// ever owns the transport.
pub struct WsTransport {
    socket: Mutex<WebSocket>,
}

impl WsTransport {
    /// Wraps an upgraded WebSocket as a transport.
    #[must_use]
    pub fn new(socket: WebSocket) -> Self {
        Self {
            socket: Mutex::new(socket),
        }
    }
}

#[async_trait]
impl Transport for WsTransport {
    async fn recv(&mut self) -> Result<Option<Bytes>, TransportError> {
        let socket = self.socket.get_mut();
        // Loop so a keepalive frame does not surface as "no application frame"; keep pulling until
        // the peer sends something that means something to the protocol, closes, or breaks.
        loop {
            match socket.recv().await {
                // The stream ended without a close frame: treat as a clean end.
                None => return Ok(None),
                Some(Ok(Message::Binary(data))) => return Ok(Some(data)),
                Some(Ok(Message::Close(_))) => return Ok(None),
                // Binary transport only: a text message breaks the contract (section 138). The
                // detail is for the log; the peer only learns the connection is closing.
                Some(Ok(Message::Text(_))) => {
                    return Err(TransportError::Protocol(
                        "text message on a binary transport".to_owned(),
                    ));
                }
                // Keepalive belongs to the WebSocket layer, which answers a ping on its own.
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Err(error)) => return Err(TransportError::Io(error.to_string())),
            }
        }
    }

    async fn send(&mut self, frame: Bytes) -> Result<(), TransportError> {
        // One protocol frame is one binary WebSocket message. A send that fails means the peer is
        // gone or the socket broke; either way the connection is finished, so it is an I/O-class
        // error carrying the detail for the log.
        self.socket
            .get_mut()
            .send(Message::Binary(frame))
            .await
            .map_err(|error| TransportError::Io(error.to_string()))
    }

    async fn close(&mut self) {
        // Best-effort: a close after the peer already left errors, and that is fine to ignore.
        let _ = self.socket.get_mut().send(Message::Close(None)).await;
    }
}
