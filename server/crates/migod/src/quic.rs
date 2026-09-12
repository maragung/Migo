//! The QUIC binding of the gateway's [`Transport`] — the optional second realtime listener.
//!
//! TCP is the default: every deployment serves the realtime path over the WebSocket route on the
//! HTTP listener, and nothing here changes that. QUIC is the second option the brief describes
//! (section 138): bound only when the operator gives `quic.bind` an address, and advertised to
//! clients through the `QUIC` feature bit only while this listener is actually serving — the
//! composition root ORs the bit into the node's feature set exactly when it binds this listener,
//! so a client never negotiates a transport the node is not carrying.
//!
//! # One stream, one session
//!
//! A client opens one QUIC connection and one bidirectional stream per session, mirroring the
//! one-WebSocket-per-instance rule (section 148). A stream supplies no message boundary of its
//! own, so the framing is the brief's stream binding: a `u32` big-endian length prefix followed
//! by one MWP frame, the same framing the federation mesh and the length-prefixed stream
//! transport already use. [`QuicStreamTransport`] peels that prefix off and hands the gateway
//! exactly the frame bytes every other transport hands it, so the session driver cannot tell a
//! QUIC client from a WebSocket one.
//!
//! # TLS and who the server is
//!
//! QUIC mandates TLS 1.3, and the listener complies with a self-signed leaf minted at boot by
//! `rcgen`. That gives the stream confidentiality and integrity; it does not by itself prove the
//! server's identity, which is the same posture the federation mesh takes — the *application*
//! layer authenticates. A realtime session proves itself with its access token in the
//! `AUTHENTICATE` step, the same step a WebSocket session proves itself with, so a listener
//! fronted by a self-signed certificate asks no new trust of the client the protocol does not
//! already demand.
//!
//! # Datagrams, when the peer offers them
//!
//! The brief's second QUIC binding (section 138): one MWP frame per QUIC datagram, no length
//! prefix — the datagram's own boundary is the length. A datagram belongs to the *connection*,
//! not the stream, so [`QuicStreamTransport`] holds the `quinn::Connection` alongside the stream
//! halves and merges `read_datagram` into the same `recv` the session driver already races. A
//! datagram's bytes are decoded as exactly one frame and returned directly; they never touch the
//! stream buffer, so a datagram can neither corrupt nor delay the stream's own framing. Sends
//! mirror this: a frame that fits `max_datagram_size` rides one bare datagram, and anything
//! larger — or a peer with no datagram support — stays on the length-prefixed stream, so the
//! peer always has a reader for every record it receives.
//!
//! # Cancel safety
//!
//! [`recv`](Transport::recv) is cancel-safe the way the trait requires: every partial read lands
//! in a buffer owned by the transport, so a `recv` future dropped mid-frame (the driver races it
//! against outbound wakeups on every loop turn) loses nothing — the next `recv` resumes from the
//! bytes already banked. A dropped `read_datagram` future has consumed nothing either: quinn only
//! dequeues the datagram when the future resolves.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::io::AsyncReadExt;

use migo_auth::RequestContext;
use migo_core::{Clock, Shutdown};
use migo_gateway::{Gateway, Transport, TransportError};
use migo_protocol::DeliveryClass;
use migo_wire::limits::MAX_FRAME_BYTES;

/// The minimum bytes the read buffer grows by when it is full. 4 KiB keeps a small frame
/// arriving in one segment from paying a second read, without over-committing memory to a
/// peer that may never send.
const READ_CHUNK: usize = 4096;

/// A gateway [`Transport`] over one bidirectional QUIC stream, plus the connection's datagrams.
///
/// The stream carries the brief's stream framing — `u32` big-endian length, then one MWP frame —
/// which this adapter strips: the gateway sees exactly the frame bytes, as it does over
/// WebSocket. Partial frames stay in `buf`, which is what makes a dropped `recv` future lose
/// nothing (see the module doc).
///
/// The connection carries the brief's datagram binding when the peer supports it: one bare MWP
/// frame per datagram, no prefix. Both bindings feed the same [`recv`](Transport::recv), so the
/// session driver stays one session no matter which path a record arrived on.
pub struct QuicStreamTransport {
    connection: quinn::Connection,
    write: quinn::SendStream,
    read: quinn::RecvStream,
    buf: BytesMut,
    eof: bool,
    /// Set once the connection is gone, as reported by the datagram reader — which errors
    /// instantly and forever after that, so polling it again would spin. The stream read is
    /// the transport's authority on session lifetime; this flag only retires the loser of
    /// the race to report the same end.
    datagrams_dead: bool,
}

impl QuicStreamTransport {
    /// Wraps the connection and the two halves of one accepted bidirectional stream. The
    /// connection is what datagrams arrive on and are sent by, so every transport holds a clone
    /// of the handle.
    #[must_use]
    pub fn new(
        connection: quinn::Connection,
        write: quinn::SendStream,
        read: quinn::RecvStream,
    ) -> Self {
        Self {
            connection,
            write,
            read,
            buf: BytesMut::new(),
            eof: false,
            datagrams_dead: false,
        }
    }
}

#[async_trait]
impl Transport for QuicStreamTransport {
    async fn recv(&mut self) -> Result<Option<Bytes>, TransportError> {
        loop {
            // Banked bytes first: a frame may already be whole from a read a previous (dropped)
            // recv future started.
            if let Some(frame) = take_frame(&mut self.buf)? {
                return Ok(Some(frame));
            }
            if self.eof {
                // The peer finished its stream and no whole frame is left banked: a clean end,
                // the same shape a WebSocket close reads back as.
                return Ok(None);
            }
            // Read straight into the transport's own buffer so cancellation cannot strand bytes
            // in a dropped future's stack: `read_buf` lands its bytes in `buf` the moment the
            // read resolves, and a future dropped while pending has consumed nothing. Datagrams
            // race the stream read because they arrive on the connection, not the stream: one
            // whole bare frame per datagram (section 138), returned without ever touching `buf`
            // — a datagram must not be able to corrupt or delay the stream's own framing. Both
            // futures are cancel-safe, so losing the race loses nothing.
            if self.buf.len() == self.buf.capacity() {
                self.buf.reserve(READ_CHUNK);
            }
            let read = if self.datagrams_dead {
                // The datagram reader already reported the connection gone (and would error
                // instantly and forever, so it is no longer polled); the stream read — the
                // transport's authority on session lifetime — reports the end through the path
                // that owns it.
                self.read
                    .read_buf(&mut self.buf)
                    .await
                    .map_err(|_| TransportError::Io("the QUIC connection ended".to_string()))
            } else {
                tokio::select! {
                    read = self.read.read_buf(&mut self.buf) => {
                        read.map_err(|error| TransportError::Io(error.to_string()))
                    }
                    datagram = self.connection.read_datagram() => {
                        match datagram {
                            Ok(bytes) => {
                                // A datagram bigger than the frame ceiling is refused the
                                // moment it is whole — the same rule the stream reader applies
                                // to a hostile length prefix, without ever copying it into
                                // `buf`.
                                if bytes.len() > MAX_FRAME_BYTES {
                                    return Err(TransportError::Protocol(format!(
                                        "datagram of {} bytes exceeds the {MAX_FRAME_BYTES}-byte ceiling",
                                        bytes.len()
                                    )));
                                }
                                return Ok(Some(bytes));
                            }
                            // Every failure here is a ConnectionError: the connection is gone,
                            // and the datagram reader will keep erroring instantly, so retire
                            // it and let the stream read report the end.
                            Err(_) => {
                                self.datagrams_dead = true;
                                self.read
                                    .read_buf(&mut self.buf)
                                    .await
                                    .map_err(|_| TransportError::Io("the QUIC connection ended".to_string()))
                            }
                        }
                    }
                }
            };
            // Zero from a buffer that always has spare capacity is the stream's FIN: the same
            // clean end a WebSocket close reads back as.
            match read {
                Ok(0) => self.eof = true,
                Ok(_) => {}
                Err(error) => return Err(error),
            }
        }
    }

    async fn send(&mut self, frame: Bytes) -> Result<(), TransportError> {
        // The datagram binding when the record fits it: one bare MWP frame per datagram, bounded
        // by the path MTU the connection reports (section 138). But a datagram is unreliable —
        // it may be lost or arrive out of order — so only a frame whose delivery class already
        // tolerates that may ride one. A Critical frame (a message, a reply, anything the
        // session cannot afford to lose) always takes the length-prefixed stream, whose reader
        // is always listening, so no record ever depends on datagram support to arrive.
        if datagram_eligible(&self.connection, &frame) {
            return self
                .connection
                .send_datagram(frame)
                .map_err(|error| TransportError::Io(error.to_string()));
        }
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
        // quinn's writes go straight to the connection — there is no userspace buffer to
        // flush — so one `write_all` is one complete length-prefixed record on the wire.
        self.write
            .write_all(&out)
            .await
            .map_err(|error| TransportError::Io(error.to_string()))
    }

    async fn close(&mut self) {
        // Best-effort: queue the FIN so the peer reads a clean end rather than a reset. A
        // stream that already closed, or a peer already gone, is not an error worth surfacing.
        let _ = self.write.finish();
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

/// Whether one already-encoded frame may ride a QUIC datagram on `connection`.
///
/// Both conditions from section 138 must hold: the peer's datagram support and the path MTU
/// leave room for the whole frame, and the frame's own delivery class tolerates an unreliable,
/// unordered ride (see [`frame_may_ride_datagram`]).
fn datagram_eligible(connection: &quinn::Connection, frame: &[u8]) -> bool {
    let Some(max) = connection.max_datagram_size() else {
        return false;
    };
    frame.len() <= max && frame_may_ride_datagram(frame)
}

/// Whether one already-encoded frame's content may take a datagram's unreliable, unordered ride
/// at all, independent of any connection's MTU.
///
/// A Critical frame is never eligible — the class is the wire's own statement that the session
/// cannot afford to lose the record, and a datagram may be lost. A malformed frame is not
/// eligible either, though it should be unreachable here (the transport is only ever handed
/// frames this build encoded); letting it fall to the stream path keeps a decode bug from
/// turning into silent loss.
fn frame_may_ride_datagram(frame: &[u8]) -> bool {
    match migo_wire::Frame::decode(Bytes::copy_from_slice(frame)) {
        Ok(decoded) => {
            // An ERROR-flagged frame is a reply the caller is waiting on, whatever opcode it
            // rides under — the flag outranks the opcode's class.
            if decoded.header.is_error() {
                return false;
            }
            match migo_protocol::Opcode::from_wire(decoded.header.opcode) {
                // Only a class that already tolerates loss may take a lossy ride. Typing,
                // reactions, presence — the ephemeral, high-frequency signals where skipping
                // head-of-line blocking pays — are exactly the ones the wire marks
                // Coalescable or Droppable.
                Some(opcode) => !matches!(opcode.class(), DeliveryClass::Critical),
                None => false,
            }
        }
        Err(_) => false,
    }
}

/// Binds the optional QUIC listener and serves it until `shutdown` fires.
///
/// Each accepted connection may open any number of bidirectional streams; each stream is one
/// realtime session, handed to the gateway with a [`RequestContext`] carrying the peer address
/// the same way the WebSocket upgrade route builds one. Returns the address actually bound, so
/// the caller (and the operator's log) sees the port the OS chose for a `:0` bind.
///
/// # Errors
///
/// Returns an error if the certificate cannot be minted or the socket cannot be bound — the
/// composition root refuses to start a node that advertised the `QUIC` feature bit but cannot
/// serve it.
pub async fn spawn_listener(
    gateway: Arc<Gateway>,
    clock: Arc<dyn Clock>,
    shutdown: Shutdown,
    bind: &str,
) -> anyhow::Result<SocketAddr> {
    let addr: SocketAddr = bind
        .parse()
        .map_err(|error| anyhow::anyhow!("quic.bind {bind:?} is not a socket address: {error}"))?;

    // QUIC mandates TLS 1.3, and this listener's leaf is self-signed: the transport is encrypted
    // and integrity-protected, and the session's identity is proven at the application layer by
    // the same AUTHENTICATE step every other transport demands (see the module doc).
    let certificate =
        rcgen::generate_simple_self_signed(vec!["migo-node".to_owned()]).map_err(|error| {
            anyhow::anyhow!("cannot mint the QUIC self-signed certificate: {error}")
        })?;
    let server_config = quinn::ServerConfig::with_single_cert(
        vec![rustls::pki_types::CertificateDer::from(certificate.cert)],
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der()).into(),
    )
    .map_err(|error| anyhow::anyhow!("cannot build the QUIC server TLS config: {error}"))?;

    let endpoint = quinn::Endpoint::server(server_config, addr)
        .map_err(|error| anyhow::anyhow!("cannot bind the QUIC listener to {bind}: {error}"))?;
    let bound = endpoint.local_addr()?;

    tokio::spawn(async move {
        loop {
            // Biased so a shutdown is always observed, even under a flood of connections.
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                incoming = endpoint.accept() => match incoming {
                    None => break,
                    Some(incoming) => {
                        let gateway = Arc::clone(&gateway);
                        let clock = Arc::clone(&clock);
                        tokio::spawn(async move {
                            let connection = match incoming.await {
                                Ok(connection) => connection,
                                Err(error) => {
                                    tracing::debug!(%error, "quic handshake abandoned by peer");
                                    return;
                                }
                            };
                            let remote = connection.remote_address();
                            tracing::debug!(%remote, "quic connection accepted");
                            loop {
                                // One stream is one session. A connection may open them
                                // sequentially (reconnects) or side by side; each gets its own
                                // task and its own context, exactly as separate WebSocket
                                // connections would.
                                match connection.accept_bi().await {
                                    Ok((write, read)) => {
                                        let context =
                                            RequestContext::at(clock.now()).from_ip(remote.ip());
                                        gateway
                                            .serve(
                                                QuicStreamTransport::new(
                                                    connection.clone(),
                                                    write,
                                                    read,
                                                ),
                                                context,
                                            )
                                            .await;
                                    }
                                    Err(quinn::ConnectionError::LocallyClosed) => break,
                                    Err(error) => {
                                        tracing::debug!(%error, %remote, "quic connection ended");
                                        break;
                                    }
                                }
                            }
                        });
                    }
                }
            }
        }
        endpoint.close(0u32.into(), b"shutdown");
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

    /// One encoded frame for an opcode, so the datagram-eligibility tests speak the real wire
    /// bytes the transport is actually handed.
    fn opcode_frame(opcode: migo_protocol::Opcode, payload: &[u8]) -> Bytes {
        Frame::simple(opcode.to_wire(), 0, Bytes::copy_from_slice(payload))
            .encode()
            .expect("encodes")
    }

    #[test]
    fn a_critical_frame_never_rides_a_datagram() {
        // A message is the one record the session cannot afford to lose: the wire says so with
        // its class, and the transport must agree.
        let frame = opcode_frame(migo_protocol::Opcode::MessageSend, b"payload");
        assert!(
            !frame_may_ride_datagram(&frame),
            "a Critical frame must stay on the reliable stream"
        );
    }

    #[test]
    fn a_coalescable_frame_may_ride_a_datagram() {
        // Typing is the ephemeral, high-frequency signal the datagram binding exists for: an
        // old one landing late (or never) is the outcome the class already promises.
        let frame = opcode_frame(migo_protocol::Opcode::Typing, b"payload");
        assert!(
            frame_may_ride_datagram(&frame),
            "a Coalescable frame may take the lossy ride"
        );
    }

    #[test]
    fn an_error_reply_never_rides_a_datagram() {
        // An ERROR-flagged frame is a reply the caller is waiting on, whatever opcode it rides
        // under — the flag must outrank the opcode's class.
        let header =
            migo_wire::FrameHeader::new(migo_protocol::Opcode::Typing.to_wire(), 0).error();
        let frame = migo_wire::Frame::new(header, Bytes::new())
            .encode()
            .expect("encodes");
        assert!(
            !frame_may_ride_datagram(&frame),
            "an ERROR reply must stay on the reliable stream"
        );
    }

    #[test]
    fn a_droppable_frame_may_ride_a_datagram() {
        let frame = opcode_frame(migo_protocol::Opcode::CallStats, b"payload");
        assert!(
            frame_may_ride_datagram(&frame),
            "a Droppable frame may take the lossy ride"
        );
    }

    #[test]
    fn a_malformed_frame_falls_back_to_the_stream() {
        assert!(
            !frame_may_ride_datagram(b"not a frame"),
            "bytes that do not decode must not take a lossy ride"
        );
    }

    #[test]
    fn an_unknown_opcode_falls_back_to_the_stream() {
        // A future opcode this build does not know cannot have its class judged, so it must
        // take the reliable path rather than guess.
        let frame = migo_wire::Frame::simple(9_999, 0, Bytes::new())
            .encode()
            .expect("encodes");
        assert!(
            !frame_may_ride_datagram(&frame),
            "an unknown opcode must stay on the reliable stream"
        );
    }
}
