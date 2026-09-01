//! The raw TCP binding of the gateway's [`Transport`] — the native client's default transport.
//!
//! Web clients reach the realtime path over the WebSocket route on the HTTP listener, because a
//! browser has no TCP socket API. Native clients — Android and desktop — default here instead
//! (section 138): one TCP connection, one session, binary length-prefixed frames, the structure
//! every binary messenger since mig33 has used. Bound only when the operator gives `tcp.bind` an
//! address, and advertised to clients through the `TCP_TRANSPORT` feature bit only while this
//! listener is actually serving — the composition root ORs the bit into the node's feature set
//! exactly when it binds this listener, so a client never negotiates a transport the node is not
//! carrying.
//!
//! # One connection, one session
//!
//! A client opens one TCP connection per session, mirroring the one-WebSocket-per-instance rule
//! (section 148). TCP supplies no message boundary of its own, so the framing is the brief's
//! stream binding: a `u32` big-endian length prefix followed by one MWP frame, the same framing
//! the QUIC listener and the federation mesh use. [`TcpStreamTransport`] peels that prefix off
//! and hands the gateway exactly the frame bytes every other transport hands it, so the session
//! driver cannot tell a TCP client from a WebSocket one.
//!
//! # TLS and who the server is
//!
//! Plain TCP carries no encryption. In production the listener must be fronted by TLS 1.3 (the
//! brief's rule: plaintext is for the development loopback only); when it is, this process sees
//! the decrypted stream and nothing here changes. The session's identity is proven at the
//! application layer either way — the AUTHENTICATE step with the access token, the same step
//! every other transport demands.
//!
//! # Cancel safety
//!
//! [`recv`](Transport::recv) is cancel-safe the way the trait requires: every partial read lands
//! in a buffer owned by the transport, so a `recv` future dropped mid-frame (the driver races it
//! against outbound wakeups on every loop turn) loses nothing — the next `recv` resumes from the
//! bytes already banked.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use migo_auth::RequestContext;
use migo_core::{Clock, Shutdown};
use migo_gateway::{Gateway, Transport, TransportError};
use migo_wire::limits::MAX_FRAME_BYTES;

/// The minimum bytes the read buffer grows by when it is full. 4 KiB keeps a small frame
/// arriving in one segment from paying a second read, without over-committing memory to a
/// peer that may never send.
const READ_CHUNK: usize = 4096;

/// A gateway [`Transport`] over one accepted TCP connection.
///
/// The connection carries the brief's stream framing — `u32` big-endian length, then one MWP
/// frame — which this adapter strips: the gateway sees exactly the frame bytes, as it does over
/// WebSocket. Partial frames stay in `buf`, which is what makes a dropped `recv` future lose
/// nothing (see the module doc).
pub struct TcpStreamTransport {
    stream: TcpStream,
    buf: BytesMut,
    eof: bool,
}

impl TcpStreamTransport {
    /// Wraps one accepted connection.
    #[must_use]
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buf: BytesMut::new(),
            eof: false,
        }
    }
}

#[async_trait]
impl Transport for TcpStreamTransport {
    async fn recv(&mut self) -> Result<Option<Bytes>, TransportError> {
        loop {
            // Banked bytes first: a frame may already be whole from a read a previous (dropped)
            // recv future started.
            if let Some(frame) = take_frame(&mut self.buf)? {
                return Ok(Some(frame));
            }
            if self.eof {
                // The peer closed its side and no whole frame is left banked: a clean end, the
                // same shape a WebSocket close reads back as.
                return Ok(None);
            }
            // Read straight into the transport's own buffer so cancellation cannot strand bytes
            // in a dropped future's stack: `read_buf` lands its bytes in `buf` the moment the
            // read resolves, and a future dropped while pending has consumed nothing.
            if self.buf.len() == self.buf.capacity() {
                self.buf.reserve(READ_CHUNK);
            }
            match self.stream.read_buf(&mut self.buf).await {
                // Zero from a buffer that always has spare capacity is the peer's FIN: the same
                // clean end a WebSocket close reads back as.
                Ok(0) => self.eof = true,
                Ok(_) => {}
                Err(error) => return Err(TransportError::Io(error.to_string())),
            }
        }
    }

    async fn send(&mut self, frame: Bytes) -> Result<(), TransportError> {
        // One frame out as one length-prefixed record: the mirror of the receive path, and the
        // reason the peer's reader never has to guess where a frame ends.
        let len = u32::try_from(frame.len()).map_err(|_| {
            TransportError::Protocol(format!(
                "frame of {} bytes exceeds a u32 length",
                frame.len()
            ))
        })?;
        let mut out = BytesMut::with_capacity(4 + frame.len());
        out.put_u32(len);
        out.put_slice(&frame);
        // TCP's stream has no implicit flush discipline of its own beyond the socket buffer —
        // one `write_all` hands the kernel one complete length-prefixed record.
        self.stream
            .write_all(&out)
            .await
            .map_err(|error| TransportError::Io(error.to_string()))
    }

    async fn close(&mut self) {
        // Best-effort: queue the FIN so the peer reads a clean end rather than a reset. A
        // stream that already closed, or a peer already gone, is not an error worth surfacing.
        let _ = self.stream.shutdown().await;
    }
}

/// Peels one whole length-prefixed frame off the front of `buf`.
///
/// `Ok(None)` means the buffer holds no whole frame yet — the normal state of a stream
/// transport, not an error. The length ceiling is checked the moment the prefix is whole and
/// *before* any further bytes are buffered, so a hostile prefix is refused without the reader
/// allocating for it.
fn take_frame(buf: &mut BytesMut) -> Result<Option<Bytes>, TransportError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(TransportError::Protocol(format!(
            "frame length {len} exceeds the {MAX_FRAME_BYTES}-byte ceiling"
        )));
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    buf.advance(4);
    Ok(Some(buf.split_to(len).freeze()))
}

/// Binds the optional TCP listener and serves it until `shutdown` fires.
///
/// Each accepted connection is one realtime session, handed to the gateway with a
/// [`RequestContext`] carrying the peer address the same way the WebSocket upgrade route builds
/// one. Returns the address actually bound, so the caller (and the operator's log) sees the port
/// the OS chose for a `:0` bind.
///
/// # Errors
///
/// Returns an error if the socket cannot be bound — the composition root refuses to start a
/// node that advertised the `TCP_TRANSPORT` feature bit but cannot serve it.
pub async fn spawn_listener(
    gateway: Arc<Gateway>,
    clock: Arc<dyn Clock>,
    shutdown: Shutdown,
    bind: &str,
) -> anyhow::Result<SocketAddr> {
    let addr: SocketAddr = bind
        .parse()
        .map_err(|error| anyhow::anyhow!("tcp.bind {bind:?} is not a socket address: {error}"))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|error| anyhow::anyhow!("cannot bind the TCP listener to {bind}: {error}"))?;
    let bound = listener.local_addr().map_err(|error| {
        anyhow::anyhow!("cannot read the TCP listener's bound address: {error}")
    })?;

    tokio::spawn(async move {
        loop {
            // Biased so a shutdown is always observed, even under a flood of connections.
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Err(error) => {
                        tracing::debug!(%error, "tcp accept failed");
                    }
                    Ok((stream, remote)) => {
                        let gateway = Arc::clone(&gateway);
                        let clock = Arc::clone(&clock);
                        tokio::spawn(async move {
                            tracing::debug!(%remote, "tcp connection accepted");
                            // One connection is one session — the native client's mirror of the
                            // one-WebSocket-per-instance rule.
                            let context = RequestContext::at(clock.now()).from_ip(remote.ip());
                            gateway.serve(TcpStreamTransport::new(stream), context).await;
                        });
                    }
                }
            }
        }
    });

    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    use migo_wire::Frame;

    /// One test frame: the bytes the transport must hand the gateway (the encoded MWP frame)
    /// and the bytes that actually go on the wire (the same frame behind the u32 prefix).
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
        let frame = take_frame(&mut buf).expect("whole frame").expect("is some");
        assert_eq!(frame, body);
        assert!(buf.is_empty(), "the prefix and the body are both consumed");
    }

    #[test]
    fn a_partial_frame_is_not_an_error_and_resumes() {
        let (body, wire) = frame_bytes(b"hello");
        let mut buf = BytesMut::new();
        // Everything except the last payload byte: the prefix is whole but the body is not.
        buf.extend_from_slice(&wire[..wire.len() - 1]);
        assert!(take_frame(&mut buf).expect("no error").is_none());
        buf.extend_from_slice(&wire[wire.len() - 1..]);
        let frame = take_frame(&mut buf).expect("no error").expect("is some");
        assert_eq!(frame, body);
    }

    #[test]
    fn a_partial_prefix_is_not_an_error() {
        let (_, wire) = frame_bytes(b"hello");
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&wire[..3]);
        assert!(take_frame(&mut buf).expect("no error").is_none());
    }

    #[test]
    fn a_hostile_prefix_is_refused_without_buffering() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&u32::MAX.to_be_bytes());
        match take_frame(&mut buf) {
            Err(TransportError::Protocol(detail)) => {
                assert!(
                    detail.contains("ceiling"),
                    "detail names the ceiling: {detail}"
                );
            }
            other => panic!("expected a protocol error, got {other:?}"),
        }
        // The hostile length was never buffered past its own prefix, and the buffer still holds
        // only the four bytes the peer actually sent.
        assert_eq!(buf.len(), 4);
    }

    #[test]
    fn two_frames_peel_in_order() {
        let (first_body, first_wire) = frame_bytes(b"first");
        let (second_body, second_wire) = frame_bytes(b"second");
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&first_wire);
        buf.extend_from_slice(&second_wire);
        assert_eq!(take_frame(&mut buf).unwrap().unwrap(), first_body);
        assert_eq!(take_frame(&mut buf).unwrap().unwrap(), second_body);
    }

    /// The wire round trip over a real socket pair: the transport's framing must carry a frame
    /// out and a frame back, because that is exactly what a native client's session does.
    #[tokio::test]
    async fn a_frame_round_trips_over_a_real_tcp_pair() {
        use tokio::net::TcpListener;

        // Bind on the loopback with an OS-chosen port; the client half connects, the server half
        // becomes the transport under test.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
        let server_addr = listener.local_addr().expect("has a local address");
        let client = tokio::net::TcpStream::connect(server_addr)
            .await
            .expect("connects");
        let (server, _) = listener.accept().await.expect("accepts");

        let mut transport = TcpStreamTransport::new(server);
        let mut client = client;
        let (client_body, client_wire) = frame_bytes(b"from the client");
        client.write_all(&client_wire).await.expect("writes");

        // The transport peels the length prefix and hands back exactly the frame bytes.
        let received = transport.recv().await.expect("reads").expect("is some");
        assert_eq!(received, client_body);

        // And the reverse direction: the transport takes bare frame bytes and frames them
        // itself, so the client must read back one record — the prefix `send` wrote plus exactly
        // the frame — and nothing more.
        let (server_body, server_wire) = frame_bytes(b"from the server");
        transport.send(server_body).await.expect("sends");
        let mut scratch = vec![0u8; server_wire.len()];
        client.read_exact(&mut scratch).await.expect("reads");
        assert_eq!(scratch, server_wire.to_vec());
        // Close and confirm the peer sees a clean FIN rather than a reset: the same end a
        // WebSocket close reads back as.
        transport.close().await;
        let mut tail = [0u8; 1];
        let read = client.read(&mut tail).await.expect("post-close read");
        assert_eq!(read, 0, "the peer's FIN arrives after the last record");
    }
}
