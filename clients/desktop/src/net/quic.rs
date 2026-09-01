//! The QUIC binding of the client's realtime connection — the optional second transport.
//!
//! WebSocket is the default and stays the default; this module exists so that when a user picks
//! QUIC on the sign-in form, the pick means the wire, not just a label. The QUIC feature bit must
//! be requested in the HELLO for the server to negotiate it (the negotiated set is the
//! intersection), and a node without the listener answers a WELCOME without the bit — that is the
//! contract, and a client that then falls back to WebSocket is behaving correctly, not failing.
//!
//! # One stream, one session
//!
//! Mirroring the server's listener (and the one-WebSocket-per-instance rule, section 148): one
//! QUIC connection, one bidirectional stream, one session. A stream supplies no message boundary
//! of its own, so the framing is the brief's stream binding — a `u32` big-endian length prefix
//! followed by one MWP frame — the same framing the server's [`QuicStreamTransport`] peels off.
//!
//! # TLS and who the server is
//!
//! QUIC mandates TLS 1.3. The server's listener presents a self-signed leaf minted at boot; the
//! identity is proven at the application layer, with the access token in the HELLO — the same
//! posture the federation mesh takes. This client therefore skips chain verification on purpose
//! and asserts the session instead: a transport that is confidential and integrity-protected,
//! authenticated by the token the WELCOME handshake accepts.
//!
//! # Falling back
//!
//! [`connect`] never errors on a missing QUIC negotiation by itself: it reports the WELCOME, and
//! the caller decides. A node that did not negotiate QUIC has a client on a WebSocket it never
//! asked to leave, so the worker reconnects over WebSocket and says so in the connection state —
//! the honest outcome, rather than an error screen for a working server.
//!
//! # Cancel safety
//!
//! `next_frame`'s partial reads land in a buffer owned by the connection, so a dropped future
//! loses nothing — the same discipline the server's transport applies.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, BytesMut};
use migo_protocol::{from_frame, to_frame, Frame, Opcode};

/// How long any single handshake or read step may take before the attempt is declared failed
/// rather than waiting on the caller's patience. Generous for a path that crosses the open
/// internet, short enough that a fallback happens in seconds, not minutes.
const STEP: Duration = Duration::from_secs(8);

/// How long the connection may sit idle before the client side pings it. QUIC keeps its own
/// keep-alive; this is the floor for it, and the reason a NAT mapping does not silently retire
/// the connection while the user reads a thread.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// The server name offered in the TLS handshake. The leaf is self-signed and minted for
/// `migo-node`; the session's real identity proof is the access token in the HELLO.
const SERVER_NAME: &str = "migo-node";

/// A live QUIC realtime connection: one bidirectional stream carrying length-prefixed frames.
pub struct QuicGateway {
    connection: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    buf: BytesMut,
    /// Correlation ids for request frames. Zero means "not a reply to anything", so ids start at one.
    next_correlation: u32,
}

/// Builds a client endpoint that accepts the server's self-signed leaf.
///
/// Chain verification would fail by design: the leaf has no chain. The session is authenticated
/// by the token carried in the HELLO, the same way a WebSocket session proves itself, so the
/// honest configuration here skips the check the deployment never promised.
fn client_endpoint() -> anyhow::Result<quinn::Endpoint> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    crypto
        .dangerous()
        .set_certificate_verifier(Arc::new(AcceptSelfSigned { provider }));

    let mut quic = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
    ));
    // A NAT-silent connection is indistinguishable from a dead one; keep the mapping warm while
    // the user reads a thread.
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    quic.transport_config(Arc::new(transport));

    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(quic);
    Ok(endpoint)
}

/// Connects, opens the session stream, sends HELLO, and waits for WELCOME.
///
/// Returns the connection together with the WELCOME, because the negotiated features govern what
/// the caller sends afterwards — most importantly whether the QUIC bit survived the intersection.
pub async fn connect(
    endpoint: &crate::config::ServerEndpoint,
    hello: migo_protocol::Hello,
) -> Result<(QuicGateway, migo_protocol::Welcome), QuicError> {
    let addr = SocketAddr::new(
        endpoint
            .host
            .parse::<std::net::IpAddr>()
            .map_err(|_| QuicError::UnresolvedHost)?,
        endpoint.gateway_port,
    );
    let quic = client_endpoint().map_err(|_| QuicError::Transport)?;
    let connecting = quic
        .connect(addr, SERVER_NAME)
        .map_err(|_| QuicError::Transport)?;
    let connection = tokio::time::timeout(STEP, connecting)
        .await
        .map_err(|_| QuicError::Timeout)?
        .map_err(|_| QuicError::Transport)?;

    // One stream is one session: the mirror of the server's accept loop.
    let (send, recv) = tokio::time::timeout(STEP, connection.open_bi())
        .await
        .map_err(|_| QuicError::Timeout)?
        .map_err(|_| QuicError::Transport)?;

    let mut gateway = QuicGateway {
        connection,
        send,
        recv,
        buf: BytesMut::new(),
        next_correlation: 1,
    };

    // HELLO rides the stream framing: length prefix, then the frame. The HELLO carries the
    // transport's own feature bit: the negotiated set is the intersection, so a client that does
    // not ask for QUIC gets a WELCOME without it even from a node that serves it — which is the
    // contract, not a fault.
    let mut hello = hello;
    hello.features |= migo_protocol::features::QUIC;
    let frame = to_frame(Opcode::Hello.to_wire(), gateway.correlate(), &hello)
        .map_err(|_| QuicError::Malformed)?;
    let wire = frame
        .encode_length_prefixed()
        .map_err(|_| QuicError::Malformed)?;
    gateway.send_raw(&wire).await?;

    // The WELCOME — or the ERROR-flagged refusal — comes back under the HELLO opcode; the flag is
    // the discriminator, exactly as on the WebSocket path.
    let frame = gateway.next_frame().await?;
    if super::gateway::is_error(&frame) {
        return Err(QuicError::Refused(super::gateway::refusal(&frame)));
    }
    if Opcode::from_wire(frame.header.opcode) != Some(Opcode::Hello) {
        return Err(QuicError::NoWelcome);
    }
    let welcome: migo_protocol::Welcome = from_frame(&frame).map_err(|_| QuicError::Malformed)?;
    Ok((gateway, welcome))
}

impl QuicGateway {
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
    ) -> Result<(), QuicError> {
        let frame =
            to_frame(opcode.to_wire(), correlation, value).map_err(|_| QuicError::Malformed)?;
        let wire = frame
            .encode_length_prefixed()
            .map_err(|_| QuicError::Malformed)?;
        self.send_raw(&wire).await
    }

    /// Writes one already-framed record to the stream.
    async fn send_raw(&mut self, wire: &[u8]) -> Result<(), QuicError> {
        self.send
            .write_all(wire)
            .await
            .map_err(|_| QuicError::Transport)?;
        Ok(())
    }

    /// Reads the next protocol frame off the stream, reassembling partial records in `buf`.
    pub async fn next_frame(&mut self) -> Result<Frame, QuicError> {
        // Reads land in a fixed scratch buffer first, then are banked in `buf` — `read` is the
        // cancel-safe primitive, and bytes move to `buf` the moment the read resolves, so a
        // future dropped while pending loses nothing.
        let mut scratch = [0u8; 4 * 1024];
        loop {
            if let Some(frame) = take_frame(&mut self.buf)? {
                return Ok(frame);
            }
            let read = tokio::time::timeout(STEP, self.recv.read(&mut scratch))
                .await
                .map_err(|_| QuicError::Timeout)?
                .map_err(|_| QuicError::Transport)?;
            match read {
                None => return Err(QuicError::Closed),
                Some(0) => continue,
                Some(n) => self.buf.extend_from_slice(&scratch[..n]),
            }
        }
    }

    /// Closes the stream politely, so the server retires the session rather than timing it out.
    pub async fn close(&mut self) {
        // Best-effort FIN queue; a stream already closed or a peer already gone is not an error
        // worth surfacing.
        let _ = self.send.finish();
        self.connection.close(0u32.into(), b"bye");
    }
}

/// Peels one whole length-prefixed frame off the front of `buf`, or `None` when the buffer holds
/// no whole frame yet. The length ceiling is checked before any body is buffered, so a hostile
/// prefix is refused without allocating for it — the same rule the server's reader applies.
fn take_frame(buf: &mut BytesMut) -> Result<Option<Frame>, QuicError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > migo_wire::limits::MAX_FRAME_BYTES {
        return Err(QuicError::Malformed);
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    buf.advance(4);
    let body = buf.split_to(len).freeze();
    Frame::decode(body)
        .map(Some)
        .map_err(|_| QuicError::Malformed)
}

/// A QUIC connection failure. Deliberately coarser than the WebSocket gateway's enum: the caller's
/// answer to any of these is the same — report, fall back to WebSocket, retry on the backoff
/// ladder — so a finer taxonomy would be vocabulary with no behaviour behind it.
#[derive(Debug, thiserror::Error)]
pub enum QuicError {
    #[error("the host is not a literal IP address")]
    UnresolvedHost,

    #[error("cannot reach the QUIC listener")]
    Transport,

    #[error("the QUIC listener did not answer in time")]
    Timeout,

    #[error("the QUIC connection closed")]
    Closed,

    #[error("the server sent a frame this client could not read")]
    Malformed,

    #[error("the QUIC handshake did not complete")]
    NoWelcome,

    #[error("{0}")]
    Refused(#[from] super::gateway::GatewayError),
}

/// A certificate verifier that accepts the listener's self-signed leaf. See [`client_endpoint`].
#[derive(Debug)]
struct AcceptSelfSigned {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for AcceptSelfSigned {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General(
            "QUIC is TLS 1.3 only; a TLS 1.2 signature has no business appearing".into(),
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use bytes::Bytes;

    /// One test frame: the bytes the reader must hand back (the encoded MWP frame) and the bytes
    /// that go on the stream (the same frame behind the u32 prefix).
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
        assert!(matches!(take_frame(&mut buf), Err(QuicError::Malformed)));
        assert_eq!(buf.len(), 4, "nothing past the prefix was buffered");
    }

    /// A full client handshake against a live deployment: the same call the worker makes when the
    /// user picks QUIC on the sign-in form.
    ///
    /// Set `MIGO_QUIC_LIVE_ADDR=host:port` and run with
    /// `cargo test quic -- --ignored` — the same check an operator runs after flipping
    /// `MIGO_QUIC__BIND` on, answering the only question that matters: does this client's own
    /// transport complete the TLS 1.3 handshake, open the session stream, send a HELLO with the
    /// QUIC bit, and hear a WELCOME that carries it?
    #[tokio::test]
    #[ignore = "points at a live deployment: set MIGO_QUIC_LIVE_ADDR=host:port to run it"]
    async fn the_client_transport_completes_a_live_quic_handshake() {
        let addr: SocketAddr = std::env::var("MIGO_QUIC_LIVE_ADDR")
            .expect(
                "MIGO_QUIC_LIVE_ADDR names the deployment under test, e.g. 152.53.102.150:18443",
            )
            .parse()
            .expect("MIGO_QUIC_LIVE_ADDR must be a socket address");

        // A HELLO shaped exactly like the worker's, minus the token: this asserts the transport,
        // not the account. The QUIC bit rides along because `connect` ORs it in — the negotiated
        // set is the intersection, so a client that does not ask gets a WELCOME without it.
        let hello = migo_protocol::Hello {
            protocol_version: migo_protocol::PROTOCOL_VERSION,
            features: migo_protocol::features::QUIC,
            ..Default::default()
        };
        let endpoint = crate::config::ServerEndpoint {
            host: match addr.ip() {
                std::net::IpAddr::V4(ip) => ip.to_string(),
                std::net::IpAddr::V6(ip) => ip.to_string(),
            },
            port: 80,
            gateway_port: addr.port(),
            transport: crate::config::Transport::Quic,
            scheme: crate::config::Scheme::Quic(crate::config::QuicScheme::QuicTls),
            rest_scheme: crate::config::RestScheme::Http,
        };

        let (mut gateway, welcome) = connect(&endpoint, hello)
            .await
            .expect("the client transport completes the live handshake");

        assert_ne!(
            welcome.features & migo_protocol::features::QUIC,
            0,
            "a node serving the QUIC listener negotiates the QUIC feature bit"
        );

        // One round trip beyond the handshake: PING answers PONG, proving the stream framing
        // carries a request and its reply, not just the WELCOME.
        let ping = migo_protocol::Ping {
            client_time: migo_core::Timestamp::now(),
        };
        let correlation = gateway.correlate();
        gateway
            .send(Opcode::Ping, correlation, &ping)
            .await
            .expect("the PING writes over the stream framing");
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
