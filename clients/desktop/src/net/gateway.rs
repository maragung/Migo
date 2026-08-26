//! The gateway connection: one WebSocket, one frame per binary message.
//!
//! # Framing
//!
//! MWP frames are self-describing and a WebSocket message already has a length, so a frame is sent as
//! exactly one binary message with no length prefix. The length-prefixed form exists for byte streams
//! (the mesh links between nodes run over one); using it here would be a redundant four bytes on every
//! message and, worse, would desynchronise a peer that reads one frame per message — which is what the
//! server does at `migo-gateway/src/connection.rs`.
//!
//! # The handshake
//!
//! HELLO carries the access token, so a successful connection is authenticated by the time WELCOME
//! arrives — one round trip rather than two. WELCOME reports the negotiated feature bits, which may be
//! fewer than were asked for; the client honours the intersection rather than assuming it got what it
//! requested. AUTHENTICATE exists for re-authenticating a live connection after a token refresh, which
//! is why it is still sent as a separate frame in that one case.
//!
//! # Text messages
//!
//! Rejected. A text frame on this socket means either a proxy rewriting traffic or a peer speaking
//! something that is not MWP, and brief section 178 forbids a JSON realtime path outright; accepting
//! one "just in case" is how a second, undocumented protocol gets born.

use futures_util::{SinkExt, StreamExt};
use migo_protocol::{from_frame, to_frame, Decode, Encode, Frame, Opcode};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// A gateway failure.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("cannot reach the gateway")]
    Transport,

    #[error("the connection closed")]
    Closed,

    /// The server sent a frame this client could not parse. Not attributed further: the bytes came
    /// from the network and are not going in a log line.
    #[error("the gateway sent a frame this client could not read")]
    Malformed,

    /// The server refused, with its own stable error code and public message.
    #[error("{message}")]
    Refused {
        code: u32,
        symbol: String,
        message: String,
    },

    #[error("the gateway did not complete the handshake")]
    NoWelcome,
}

/// A live, authenticated gateway connection.
pub struct Gateway {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// Correlation ids for request frames. Zero means "not a reply to anything", so ids start at one.
    next_correlation: u32,
}

impl Gateway {
    /// Connects, sends HELLO, and waits for WELCOME.
    ///
    /// Returns the connection together with the WELCOME, because the negotiated features and the
    /// server's frame-size limit in it govern everything sent afterwards.
    pub async fn connect(
        url: &str,
        hello: migo_protocol::Hello,
    ) -> Result<(Self, migo_protocol::Welcome), GatewayError> {
        let (socket, _response) = connect_async(url)
            .await
            .map_err(|_| GatewayError::Transport)?;
        let mut gateway = Self {
            socket,
            next_correlation: 1,
        };

        let correlation = gateway.correlate();
        gateway.send(Opcode::Hello, correlation, &hello).await?;

        // The server answers HELLO with exactly one frame, and both answers carry the HELLO opcode: a
        // WELCOME, or the same opcode with the ERROR flag set. The flag is therefore the
        // discriminator, not the opcode — branching on the opcode alone would try to read an error
        // body as a WELCOME. Anything else is a protocol violation and there is nothing useful to do
        // but give up; a client that skipped unexpected frames here would proceed on a connection it
        // had never negotiated.
        let frame = gateway.next_frame().await?;
        if is_error(&frame) {
            return Err(refusal(&frame));
        }
        if Opcode::from_wire(frame.header.opcode) != Some(Opcode::Hello) {
            return Err(GatewayError::NoWelcome);
        }
        let welcome: migo_protocol::Welcome =
            from_frame(&frame).map_err(|_| GatewayError::Malformed)?;
        Ok((gateway, welcome))
    }

    /// A fresh correlation id, for a request whose reply must be matched to it.
    pub fn correlate(&mut self) -> u32 {
        let id = self.next_correlation;
        // Wrapping is fine and deliberate: correlation ids only have to be unique among the requests
        // currently in flight, and four billion is far beyond that. Panicking on overflow, or growing
        // to u64, would both be worse answers to a problem that does not exist.
        self.next_correlation = self.next_correlation.wrapping_add(1).max(1);
        id
    }

    /// Encodes and sends one message as one binary WebSocket frame.
    pub async fn send<T: Encode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        value: &T,
    ) -> Result<(), GatewayError> {
        let frame =
            to_frame(opcode.to_wire(), correlation, value).map_err(|_| GatewayError::Malformed)?;
        let bytes = frame.encode().map_err(|_| GatewayError::Malformed)?;
        self.socket
            .send(Message::Binary(bytes))
            .await
            .map_err(|_| GatewayError::Transport)?;
        Ok(())
    }

    /// Reads the next protocol frame, transparently answering pings and skipping non-frames.
    pub async fn next_frame(&mut self) -> Result<Frame, GatewayError> {
        loop {
            let message = match self.socket.next().await {
                Some(Ok(message)) => message,
                Some(Err(_)) => return Err(GatewayError::Transport),
                None => return Err(GatewayError::Closed),
            };
            match message {
                Message::Binary(bytes) => {
                    return Frame::decode(bytes).map_err(|_| GatewayError::Malformed);
                }
                // Answered by tokio-tungstenite itself; nothing to do but keep reading.
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(frame) => {
                    let normal =
                        frame.is_none_or(|f| matches!(f.code, CloseCode::Normal | CloseCode::Away));
                    return Err(if normal {
                        GatewayError::Closed
                    } else {
                        GatewayError::Transport
                    });
                }
                // See the module note: a text frame is not MWP.
                Message::Text(_) | Message::Frame(_) => return Err(GatewayError::Malformed),
            }
        }
    }

    /// Closes the socket politely, so the server retires the session rather than timing it out.
    pub async fn close(mut self) {
        let _ = self.socket.close(None).await;
    }
}

/// Decodes a frame's payload, mapping a failure to [`GatewayError::Malformed`].
pub fn decode<T: Decode>(frame: &Frame) -> Result<T, GatewayError> {
    from_frame(frame).map_err(|_| GatewayError::Malformed)
}

/// Turns an ERROR frame into a [`GatewayError::Refused`].
///
/// A payload that will not even parse still has to become an error, so the fallback keeps the code
/// from the header context rather than reporting success.
pub fn refusal(frame: &Frame) -> GatewayError {
    match from_frame::<migo_protocol::Error>(frame) {
        Ok(error) => GatewayError::Refused {
            code: error.code,
            symbol: error.symbol,
            // `message` is optional on the wire and is the server's `public_message()` when present.
            // With nothing to show, the symbol is the honest fallback — it is a stable identifier the
            // user can quote in a bug report, which an invented sentence would not be.
            message: error
                .message
                .unwrap_or_else(|| "the server refused the request".to_owned()),
        },
        Err(_) => GatewayError::Malformed,
    }
}

/// Whether a frame is an ERROR, so callers can branch before choosing a payload type.
#[must_use]
pub fn is_error(frame: &Frame) -> bool {
    frame.header.is_error() || Opcode::from_wire(frame.header.opcode) == Some(Opcode::Error)
}
