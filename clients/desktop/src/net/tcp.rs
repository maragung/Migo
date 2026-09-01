//! The TCP binding of the client's realtime connection — the native default transport.
//!
//! The web client reaches the realtime path over WebSocket because a browser has no TCP socket
//! API; a native desktop client has one, so this is the default here (section 138): one TCP
//! connection, one session, binary length-prefixed frames, the structure every binary messenger
//! since mig33 has used. The `TCP_TRANSPORT` feature bit must be requested in the HELLO for the
//! server to negotiate it (the negotiated set is the intersection), and a node without the
//! listener answers a WELCOME without the bit — that is the contract, and a client that then
//! falls back to WebSocket is behaving correctly, not failing.
//!
//! # One connection, one session
//!
//! Mirroring the server's listener (and the one-WebSocket-per-instance rule, section 148): one
//! TCP connection, one session. TCP supplies no message boundary of its own, so the framing is
//! the brief's stream binding — a `u32` big-endian length prefix followed by one MWP frame — the
//! same framing the server's [`TcpStreamTransport`] peels off and the QUIC path uses.
//!
//! # TLS and who the server is
//!
//! Plain TCP carries no encryption. A production listener is fronted by TLS 1.3; where this
//! client's deployment serves plaintext (development loopback, or a TLS terminator that
//! presents a private port), the session's identity is proven at the application layer with the
//! access token in the HELLO — the same posture every other transport takes.
//!
//! # Falling back
//!
//! [`connect`] never errors on a missing TCP negotiation by itself: it reports the WELCOME, and
//! the caller decides. A node that did not negotiate the TCP bit has a client on a WebSocket it
//! never asked to leave, so the worker reconnects over WebSocket and says so in the connection
//! state — the honest outcome, rather than an error screen for a working server.
//!
//! # Cancel safety
//!
//! `next_frame`'s partial reads land in a buffer owned by the connection, so a dropped future
//! loses nothing — the same discipline the server's transport applies.

use std::time::Duration;

use bytes::{Buf, BytesMut};
use migo_protocol::{from_frame, to_frame, Frame, Opcode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// How long any single handshake or read step may take before the attempt is declared failed
/// rather than waiting on the caller's patience. Generous for a path that crosses the open
/// internet, short enough that a fallback happens in seconds, not minutes.
const STEP: Duration = Duration::from_secs(8);

/// A live TCP realtime connection: one socket carrying length-prefixed frames.
pub struct TcpGateway {
    stream: TcpStream,
    buf: BytesMut,
    /// Correlation ids for request frames. Zero means "not a reply to anything", so ids start at one.
    next_correlation: u32,
}

/// Resolves the endpoint's host, preferring a literal address and falling back to the OS
/// resolver for names — a native client's advantage over the browser, and the reason TCP can be
/// the default here at all.
async fn resolve(host: &str) -> Result<std::net::SocketAddr, TcpError> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(std::net::SocketAddr::new(ip, 0));
    }
    let resolved = tokio::net::lookup_host(format!("{host}:0"))
        .await
        .map_err(|_| TcpError::UnresolvedHost)?
        .next()
        .ok_or(TcpError::UnresolvedHost)?;
    Ok(resolved)
}

/// Connects, sends HELLO, and waits for WELCOME.
///
/// Returns the connection together with the WELCOME, because the negotiated features govern what
/// the caller sends afterwards — most importantly whether the `TCP_TRANSPORT` bit survived the
/// intersection.
pub async fn connect(
    endpoint: &crate::config::ServerEndpoint,
    hello: migo_protocol::Hello,
) -> Result<(TcpGateway, migo_protocol::Welcome), TcpError> {
    let mut addr = resolve(&endpoint.host).await?;
    addr.set_port(endpoint.gateway_port);
    let stream = tokio::time::timeout(STEP, TcpStream::connect(addr))
        .await
        .map_err(|_| TcpError::Timeout)?
        .map_err(|_| TcpError::Transport)?;

    let mut gateway = TcpGateway {
        stream,
        buf: BytesMut::new(),
        next_correlation: 1,
    };

    // HELLO rides the connection framing: length prefix, then the frame. The HELLO carries the
    // transport's own feature bit: the negotiated set is the intersection, so a client that does
    // not ask for TCP gets a WELCOME without it even from a node that serves it — which is the
    // contract, not a fault.
    let mut hello = hello;
    hello.features |= migo_protocol::features::TCP_TRANSPORT;
    let frame = to_frame(Opcode::Hello.to_wire(), gateway.correlate(), &hello)
        .map_err(|_| TcpError::Malformed)?;
    let wire = frame
        .encode_length_prefixed()
        .map_err(|_| TcpError::Malformed)?;
    gateway.send_raw(&wire).await?;

    // The WELCOME — or the ERROR-flagged refusal — comes back under the HELLO opcode; the flag is
    // the discriminator, exactly as on the WebSocket path.
    let frame = gateway.next_frame().await?;
    if super::gateway::is_error(&frame) {
        return Err(TcpError::Refused(super::gateway::refusal(&frame)));
    }
    if Opcode::from_wire(frame.header.opcode) != Some(Opcode::Hello) {
        return Err(TcpError::NoWelcome);
    }
    let welcome: migo_protocol::Welcome = from_frame(&frame).map_err(|_| TcpError::Malformed)?;
    Ok((gateway, welcome))
}

impl TcpGateway {
    /// A fresh correlation id, for a request whose reply must be matched to it.
    pub fn correlate(&mut self) -> u32 {
        let id = self.next_correlation;
        // Wrapping is fine and deliberate: correlation ids only have to be unique among the
        // requests currently in flight. See the WebSocket gateway's note for the full argument.
        self.next_correlation = self.next_correlation.wrapping_add(1).max(1);
        id
    }

    /// Encodes and sends one frame as one length-prefixed record.
    pub async fn send<T: migo_protocol::Encode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        value: &T,
    ) -> Result<(), TcpError> {
        let frame =
            to_frame(opcode.to_wire(), correlation, value).map_err(|_| TcpError::Malformed)?;
        let wire = frame
            .encode_length_prefixed()
            .map_err(|_| TcpError::Malformed)?;
        self.send_raw(&wire).await
    }

    /// Writes one already-framed record to the connection.
    async fn send_raw(&mut self, wire: &[u8]) -> Result<(), TcpError> {
        self.stream
            .write_all(wire)
            .await
            .map_err(|_| TcpError::Transport)?;
        Ok(())
    }

    /// Reads the next protocol frame off the connection, reassembling partial records in `buf`.
    pub async fn next_frame(&mut self) -> Result<Frame, TcpError> {
        // Reads land in a fixed scratch buffer first, then are banked in `buf` — `read` is the
        // cancel-safe primitive, and bytes move to `buf` the moment the read resolves, so a
        // future dropped while pending loses nothing.
        let mut scratch = [0u8; 4 * 1024];
        loop {
            if let Some(frame) = take_frame(&mut self.buf)? {
                return Ok(frame);
            }
            let read = tokio::time::timeout(STEP, self.stream.read(&mut scratch))
                .await
                .map_err(|_| TcpError::Timeout)?
                .map_err(|_| TcpError::Transport)?;
            match read {
                0 => return Err(TcpError::Closed),
                n => self.buf.extend_from_slice(&scratch[..n]),
            }
        }
    }

    /// Closes the connection politely, so the server retires the session rather than timing it
    /// out.
    pub async fn close(&mut self) {
        // Best-effort FIN queue; a stream already closed or a peer already gone is not an error
        // worth surfacing.
        let _ = self.stream.shutdown().await;
    }
}

/// Peels one whole length-prefixed frame off the front of `buf`, or `None` when the buffer holds
/// no whole frame yet. The length ceiling is checked before any body is buffered, so a hostile
/// prefix is refused without allocating for it — the same rule the server's reader applies.
fn take_frame(buf: &mut BytesMut) -> Result<Option<Frame>, TcpError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > migo_wire::limits::MAX_FRAME_BYTES {
        return Err(TcpError::Malformed);
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    buf.advance(4);
    let body = buf.split_to(len).freeze();
    Frame::decode(body)
        .map(Some)
        .map_err(|_| TcpError::Malformed)
}

/// A TCP connection failure. Deliberately coarser than the WebSocket gateway's enum: the caller's
/// answer to any of these is the same — report, fall back to WebSocket, retry on the backoff
/// ladder — so a finer taxonomy would be vocabulary with no behaviour behind it.
#[derive(Debug, thiserror::Error)]
pub enum TcpError {
    #[error("the host could not be resolved")]
    UnresolvedHost,

    #[error("cannot reach the TCP listener")]
    Transport,

    #[error("the TCP listener did not answer in time")]
    Timeout,

    #[error("the TCP connection closed")]
    Closed,

    #[error("the server sent a frame this client could not read")]
    Malformed,

    #[error("the TCP handshake did not complete")]
    NoWelcome,

    #[error("{0}")]
    Refused(#[from] super::gateway::GatewayError),
}

#[cfg(test)]
mod tests {
    use super::*;

    use bytes::Bytes;

    /// One test frame: the bytes the reader must hand back (the encoded MWP frame) and the bytes
    /// that go on the wire (the same frame behind the u32 prefix).
    fn frame_bytes(payload: &[u8]) -> (Bytes, Bytes) {
        let frame = Frame::simple(0, 0, Bytes::copy_from_slice(payload));
        let body = frame.encode().expect("encodes");
        let wire = frame.encode_length_prefixed().expect("encodes");
        (body, wire)
    }

    #[test]
    fn a_whole_prefix_and_frame_is_peeled_in_one_step() {
        let (body, wire) = frame_bytes(b"hello");
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&wire);
        let frame = take_frame(&mut buf).expect("no error").expect("is some");
        assert_eq!(frame.encode().expect("encodes"), body);
        assert!(buf.is_empty(), "the prefix and the body are both consumed");
    }

    #[test]
    fn a_partial_frame_is_not_an_error_and_resumes() {
        let (_, wire) = frame_bytes(b"hello");
        let mut buf = BytesMut::new();
        // Everything except the last payload byte: the prefix is whole but the body is not.
        buf.extend_from_slice(&wire[..wire.len() - 1]);
        assert!(take_frame(&mut buf).expect("no error").is_none());
        buf.extend_from_slice(&wire[wire.len() - 1..]);
        assert!(take_frame(&mut buf).expect("no error").is_some());
    }

    #[test]
    fn a_partial_prefix_is_not_an_error() {
        let (_, wire) = frame_bytes(b"hello");
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&wire[..3]);
        assert!(take_frame(&mut buf).expect("no error").is_none());
    }

    #[test]
    fn two_frames_peel_in_order() {
        let (first, first_wire) = frame_bytes(b"first");
        let (second, second_wire) = frame_bytes(b"second");
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&first_wire);
        buf.extend_from_slice(&second_wire);
        let one = take_frame(&mut buf).unwrap().unwrap();
        let two = take_frame(&mut buf).unwrap().unwrap();
        assert_eq!(one.encode().unwrap(), first);
        assert_eq!(two.encode().unwrap(), second);
    }

    #[test]
    fn a_hostile_prefix_is_refused_without_buffering() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(take_frame(&mut buf), Err(TcpError::Malformed)));
        assert_eq!(buf.len(), 4, "nothing past the prefix was buffered");
    }

    /// A full client handshake against a live deployment: the same call the worker makes when
    /// the endpoint's transport is TCP.
    ///
    /// Set `MIGO_TCP_LIVE_ADDR=host:port` and run with `cargo test tcp -- --ignored` — the same
    /// check an operator runs after flipping `MIGO_TCP__BIND` on, answering the only question
    /// that matters: does this client's own transport connect, send a HELLO with the
    /// `TCP_TRANSPORT` bit, and hear a WELCOME that carries it?
    #[tokio::test]
    #[ignore = "points at a live deployment: set MIGO_TCP_LIVE_ADDR=host:port to run it"]
    async fn the_client_transport_completes_a_live_tcp_handshake() {
        let addr: std::net::SocketAddr = std::env::var("MIGO_TCP_LIVE_ADDR")
            .expect("MIGO_TCP_LIVE_ADDR names the deployment under test, e.g. 152.53.102.150:18081")
            .parse()
            .expect("MIGO_TCP_LIVE_ADDR must be a socket address");

        // A HELLO shaped exactly like the worker's, minus the token: this asserts the transport,
        // not the account. The TCP bit rides along because `connect` ORs it in — the negotiated
        // set is the intersection, so a client that does not ask gets a WELCOME without it.
        let hello = migo_protocol::Hello {
            protocol_version: migo_protocol::PROTOCOL_VERSION,
            features: migo_protocol::features::TCP_TRANSPORT,
            ..Default::default()
        };
        let endpoint = crate::config::ServerEndpoint {
            host: addr.ip().to_string(),
            port: 80,
            gateway_port: addr.port(),
            transport: crate::config::Transport::Tcp,
            scheme: crate::config::Scheme::Tcp(crate::config::TcpScheme::Tcp),
            rest_scheme: crate::config::RestScheme::Http,
        };

        let (mut gateway, welcome) = connect(&endpoint, hello)
            .await
            .expect("the client transport completes the live handshake");

        assert_ne!(
            welcome.features & migo_protocol::features::TCP_TRANSPORT,
            0,
            "a node serving the TCP listener negotiates the TCP_TRANSPORT feature bit"
        );

        // One round trip beyond the handshake: PING answers PONG, proving the connection framing
        // carries a request and its reply, not just the WELCOME.
        let ping = migo_protocol::Ping {
            client_time: migo_core::Timestamp::now(),
        };
        let correlation = gateway.correlate();
        gateway
            .send(Opcode::Ping, correlation, &ping)
            .await
            .expect("the PING writes over the connection framing");
        let frame = tokio::time::timeout(STEP, gateway.next_frame())
            .await
            .expect("the reply arrives within the step budget")
            .expect("the reply reads");
        assert!(
            !super::super::gateway::is_error(&frame),
            "a PING is answered, not refused: {:?}",
            frame.header
        );

        gateway.close().await;
    }
}
