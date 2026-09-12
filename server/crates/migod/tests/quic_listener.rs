//! The optional QUIC listener, exercised end to end over a real socket.
//!
//! These tests bind the listener exactly the way `App::build` binds it in production — from the
//! `MIGO_QUIC__BIND` environment pair, against an in-memory development configuration — and then
//! drive it with the same quinn/rustls client stack a QUIC-capable Migo client would use. What
//! they prove, in order:
//!
//!   1. The feature contract: a node with `quic.bind` set advertises the `QUIC` feature bit and
//!      one without it does not, so a client can never negotiate a transport the node is not
//!      serving (brief section 138 — TCP is the default, QUIC the second option).
//!   2. The listener really serves: the TLS 1.3 handshake completes against the self-signed
//!      leaf, a bidirectional stream is accepted as a realtime session, and the session's
//!      length-prefixed stream framing carries the session's lifecycle — here an invalid first
//!      frame, which the gateway must answer by ending the session cleanly rather than by
//!      hanging or tearing down the process.
//!   3. The datagram binding (section 138): a whole MWP frame sent as one bare QUIC datagram
//!      reaches the session's frame reader — same session, same stream, no length prefix —
//!      and a frame too large for the datagram path stays on the length-prefixed stream.
//!
//! The client verifier in this file skips certificate verification on purpose: the listener's
//! leaf is self-signed by design (the module doc in `migod::quic` explains why), so a test client
//! asserts on the session the application layer runs, not on a certificate chain the deployment
//! never promised.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;

use migo_core::Config;
use migo_protocol::features;
use migod::App;

/// How long any single client-side step may take before the test declares the listener stuck
/// rather than waiting on the CI timeout to do it.
const STEP: Duration = Duration::from_secs(5);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

fn env(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> =
        vec![("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key())];
    pairs.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
    );
    pairs
}

async fn build_app(extra: &[(&str, &str)]) -> App {
    let config = Config::from_sources(&[], &env(extra)).expect("configuration should parse");
    App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends")
}

/// A certificate verifier that accepts the listener's self-signed leaf.
///
/// Production clients verify a real chain; this test client knows the leaf is self-signed by
/// design and asserts on the session instead (see the module doc).
#[derive(Debug)]
struct AcceptAnyServerCert {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCert {
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

/// Connects a quinn client to `addr`, skipping chain verification (see [`AcceptAnyServerCert`]).
///
/// The client endpoint binds to the loopback of the same family as the target: quinn does not
/// translate families, so dialing an IPv6 server from an IPv4 client socket (or the reverse)
/// fails at the OS layer before the handshake ever starts. A Migo client picks its local bind
/// the same way.
async fn connect(addr: SocketAddr) -> anyhow::Result<quinn::Connection> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut client_crypto = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    client_crypto
        .dangerous()
        .set_certificate_verifier(Arc::new(AcceptAnyServerCert { provider }));

    let quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?;
    let local: std::net::SocketAddr = if addr.is_ipv6() {
        "[::1]:0".parse()?
    } else {
        "127.0.0.1:0".parse()?
    };
    let mut endpoint = quinn::Endpoint::client(local)?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic_config)));
    let connecting = endpoint.connect(addr, "migo-node")?;
    let connection = tokio::time::timeout(STEP, connecting)
        .await
        .expect("the handshake does not stall")?;
    Ok(connection)
}

#[tokio::test]
async fn a_node_without_quic_configured_advertises_no_quic() {
    let app = build_app(&[]).await;
    assert!(app.quic_bind.is_none(), "no quic.bind, no listener");
    assert_eq!(
        app.features & features::QUIC,
        0,
        "the QUIC bit must not be advertised when the listener is not bound"
    );
}

#[tokio::test]
async fn a_node_with_quic_bound_advertises_the_quic_bit() {
    let app = build_app(&[("MIGO_QUIC__BIND", "127.0.0.1:0")]).await;
    assert!(
        app.quic_bind.is_some(),
        "quic.bind set means a listener is bound"
    );
    assert_ne!(
        app.features & features::QUIC,
        0,
        "the QUIC bit is advertised exactly while the listener is serving"
    );
}

#[tokio::test]
async fn the_listener_accepts_a_stream_and_ends_the_session_on_an_invalid_frame() {
    let app = build_app(&[("MIGO_QUIC__BIND", "127.0.0.1:0")]).await;
    let addr = app.quic_bind.expect("the listener is bound");

    let connection = connect(addr)
        .await
        .expect("the TLS 1.3 handshake completes against the self-signed leaf");

    // One bidirectional stream is one realtime session. Open it and speak the stream framing:
    // a u32 big-endian length, then the frame.
    let (mut send, mut recv) = tokio::time::timeout(STEP, connection.open_bi())
        .await
        .expect("opening a stream does not stall")
        .expect("the stream opens");

    // A frame the wire codec must refuse: the length prefix names a frame far past the
    // MAX_FRAME_BYTES ceiling, so the session driver should end the session rather than buffer
    // for it.
    send.write_all(&u32::MAX.to_be_bytes())
        .await
        .expect("the hostile prefix is written");

    // The session ends: the gateway closes its side, which the client reads as the stream's
    // clean end — not as a hang, and not as a process-level failure.
    tokio::time::timeout(STEP, async {
        let mut scratch = [0u8; 8];
        loop {
            match recv.read(&mut scratch).await {
                Ok(Some(0)) | Ok(None) => break,
                Ok(Some(_)) => continue,
                Err(quinn::ReadError::ConnectionLost(_)) => break,
                Err(quinn::ReadError::Reset(_)) => break,
                Err(error) => panic!("unexpected read error: {error}"),
            }
        }
    })
    .await
    .expect("the session ends promptly after an invalid frame");

    let _ = send.finish();
}

/// IPv6 is not a second-class transport: a node bound to the IPv6 loopback serves the same
/// TLS 1.3 handshake, stream framing, and session lifecycle over `::1` that it serves over
/// `127.0.0.1`, and a deployment that binds `[::]` serves both families from the one listener.
/// The client side of this test binds its local endpoint to the matching family, exactly the way
/// a Migo client must when its operator hands it an IPv6 endpoint.
#[tokio::test]
async fn the_listener_serves_the_ipv6_loopback_the_same_way() {
    let app = build_app(&[("MIGO_QUIC__BIND", "[::1]:0")]).await;
    let addr = app.quic_bind.expect("the listener is bound");
    assert!(
        addr.is_ipv6(),
        "an [::1] bind must report an IPv6 socket address, got {addr}"
    );

    let connection = connect(addr)
        .await
        .expect("the TLS 1.3 handshake completes over IPv6");

    let (mut send, mut recv) = tokio::time::timeout(STEP, connection.open_bi())
        .await
        .expect("opening an IPv6 stream does not stall")
        .expect("the stream opens");

    // The same hostile prefix as the IPv4 test, on purpose: the codec and the session driver
    // must not care which family the datagrams arrived on.
    send.write_all(&u32::MAX.to_be_bytes())
        .await
        .expect("the hostile prefix is written");

    tokio::time::timeout(STEP, async {
        let mut scratch = [0u8; 8];
        loop {
            match recv.read(&mut scratch).await {
                Ok(Some(0)) | Ok(None) => break,
                Ok(Some(_)) => continue,
                Err(quinn::ReadError::ConnectionLost(_)) => break,
                Err(quinn::ReadError::Reset(_)) => break,
                Err(error) => panic!("unexpected read error: {error}"),
            }
        }
    })
    .await
    .expect("the IPv6 session ends promptly after an invalid frame");

    let _ = send.finish();
}

/// The datagram binding (section 138), from the client's side of the same connection the
/// session's stream lives on.
///
/// A whole MWP frame sent as one bare QUIC datagram — no length prefix, the datagram's own
/// boundary as the length — reaches the session the stream serves. The observable proof from
/// outside the process is the session's response: an eligible frame the gateway must answer
/// (an ERROR reply, which the class table marks critical but the flag makes a reply) rides back
/// on the *stream*, because a reply is a record the session cannot drop. So the test sends a
/// bare PING datagram, and asserts the PONG comes back framed on the stream — proving both
/// halves at once: the datagram arrived as a frame, and the reply stayed where the class rules
/// put it.
///
/// A PING datagram is Critical-class, which the *client's* send path would hold back to its
/// stream; here the test drives the raw wire the way a peer that chose to send it anyway
/// would, and the listener's job — reading datagrams as frames and routing them into the
/// session — is what is under test.
#[tokio::test]
async fn a_bare_frame_datagram_reaches_the_session_and_the_reply_rides_the_stream() {
    let app = build_app(&[("MIGO_QUIC__BIND", "127.0.0.1:0")]).await;
    let addr = app.quic_bind.expect("the listener is bound");

    let connection = connect(addr)
        .await
        .expect("the TLS 1.3 handshake completes against the self-signed leaf");

    let (mut send, mut recv) = tokio::time::timeout(STEP, connection.open_bi())
        .await
        .expect("opening a stream does not stall")
        .expect("the stream opens");

    // The session must exist before its datagram reader can matter, so the handshake rides the
    // stream first — exactly as a real client does.
    let hello = migo_protocol::Hello {
        protocol_version: migo_protocol::PROTOCOL_VERSION,
        features: features::QUIC,
        ..Default::default()
    };
    let frame = migo_protocol::to_frame(migo_protocol::Opcode::Hello.to_wire(), 7, &hello)
        .expect("the HELLO encodes");
    let wire = frame
        .encode_length_prefixed()
        .expect("the HELLO frames for the stream binding");
    send.write_all(&wire).await.expect("the HELLO is written");

    // Drain the WELCOME off the stream before sending the datagram, so the reply asserted on
    // below cannot be the handshake's own.
    let mut scratch = vec![0u8; 64 * 1024];
    let mut seen = Vec::new();
    tokio::time::timeout(STEP, async {
        loop {
            let read = recv.read(&mut scratch).await.expect("the stream reads");
            let Some(n) = read else {
                panic!("the stream ended before the WELCOME")
            };
            seen.extend_from_slice(&scratch[..n]);
            if let Ok(Some((_welcome, consumed))) =
                migo_wire::Frame::decode_length_prefixed(&bytes::Bytes::copy_from_slice(&seen))
            {
                seen.drain(..consumed);
                break;
            }
        }
    })
    .await
    .expect("the WELCOME arrives within the step budget");

    // One bare PING frame as one QUIC datagram: no length prefix anywhere.
    let ping = migo_protocol::Ping {
        client_time: migo_core::Timestamp::now(),
    };
    let frame = migo_protocol::to_frame(migo_protocol::Opcode::Ping.to_wire(), 8, &ping)
        .expect("the PING encodes");
    let bytes = frame.encode().expect("the PING encodes as a bare frame");
    connection
        .send_datagram(bytes)
        .expect("the datagram is queued");

    // The reply rides the stream framing — the length-prefixed path — because a reply is a
    // record the session cannot afford to lose. Reading it there proves the datagram was
    // consumed as a frame by the session's own reader.
    let mut seen = Vec::new();
    let reply = tokio::time::timeout(STEP, async {
        loop {
            if let Ok(Some((frame, consumed))) =
                migo_wire::Frame::decode_length_prefixed(&bytes::Bytes::copy_from_slice(&seen))
            {
                let _ = consumed;
                return frame;
            }
            let read = recv.read(&mut scratch).await.expect("the stream reads");
            let Some(n) = read else {
                panic!("the stream ended before the reply")
            };
            seen.extend_from_slice(&scratch[..n]);
        }
    })
    .await
    .expect("the reply arrives within the step budget");

    assert_ne!(
        reply.header.opcode,
        migo_protocol::Opcode::Hello.to_wire(),
        "the reply is not the handshake echoed back"
    );
    assert!(
        !reply.header.is_error(),
        "a datagram-delivered PING is answered, not refused: {:?}",
        reply.header
    );

    let _ = send.finish();
}

/// The other half of the datagram contract (section 138): a frame too large for the datagram
/// path -- or one whose delivery class forbids the lossy ride -- stays on the length-prefixed
/// stream. What is under test is the transport adapter itself, so this builds a bare quinn
/// endpoint pair (no gateway, no session) and drives `QuicStreamTransport` from both sides:
///
///   * a datagram the client sends whole arrives from `recv` as exactly those frame bytes,
///     never mixed into the stream buffer;
///   * a frame the transport sends that fits the MTU and the class rules rides one bare
///     datagram;
///   * a Critical frame rides the length-prefixed stream even when it would fit a datagram,
///     and so does an eligible frame that does not fit.
#[tokio::test]
async fn the_datagram_binding_round_trips_and_oversized_frames_stay_on_the_stream() {
    use migo_gateway::Transport as _;
    use migod::quic::QuicStreamTransport;

    // A bare server endpoint with its own self-signed leaf, the same way the listener mints
    // one -- the transport adapter is what is under test, so no session, no gateway.
    let certificate = rcgen::generate_simple_self_signed(vec!["migo-node".to_owned()])
        .expect("the test certificate mints");
    let server_config = quinn::ServerConfig::with_single_cert(
        vec![rustls::pki_types::CertificateDer::from(certificate.cert)],
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der()).into(),
    )
    .expect("the test server config builds");
    let server = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap())
        .expect("the test endpoint binds");

    let client = connect(
        server
            .local_addr()
            .expect("the test endpoint reports its address"),
    )
    .await
    .expect("the client connects to the test endpoint");

    // One stream, opened by the client exactly the way a session's is: the server's transport
    // writes to its half, the client reads from its half, and datagrams ride the connection
    // alongside.
    let (mut client_send, mut client_recv) = tokio::time::timeout(STEP, client.open_bi())
        .await
        .expect("opening a stream does not stall")
        .expect("the stream opens");
    let _ = &mut client_send;

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("a connection arrives");
        let connection = incoming.await.expect("the handshake completes");
        let (write, read) = connection.accept_bi().await.expect("the stream opens");
        let mut transport = QuicStreamTransport::new(connection.clone(), write, read);

        // A whole bare frame arrives from recv as exactly those bytes: no prefix peeled, no
        // mixing into the stream buffer.
        let received = transport
            .recv()
            .await
            .expect("the datagram is not an error")
            .expect("the datagram is a frame");
        (transport, received)
    });

    // One bare frame as one datagram from the client -- no length prefix anywhere.
    let ping = migo_protocol::Ping {
        client_time: migo_core::Timestamp::now(),
    };
    let bare = migo_protocol::to_frame(migo_protocol::Opcode::Ping.to_wire(), 1, &ping)
        .expect("the frame encodes")
        .encode()
        .expect("the frame encodes as a bare frame");
    client
        .send_datagram(bare.clone())
        .expect("the datagram is queued");

    let (mut transport, received) = tokio::time::timeout(STEP, server_task)
        .await
        .expect("the datagram round trip completes")
        .expect("the server task finishes");
    assert_eq!(
        received, bare,
        "a datagram arrives from recv as exactly one bare frame"
    );

    // A frame the transport sends rides one bare datagram when it fits and the class allows:
    // TYPING is Coalescable, the whole point of the binding.
    let typing = migo_wire::Frame::simple(
        migo_protocol::Opcode::Typing.to_wire(),
        2,
        bytes::Bytes::from_static(b"typing"),
    )
    .encode()
    .expect("the TYPING encodes");
    transport
        .send(typing.clone())
        .await
        .expect("the eligible frame sends");
    let back = tokio::time::timeout(STEP, client.read_datagram())
        .await
        .expect("the datagram arrives within the step budget")
        .expect("the datagram reads");
    assert_eq!(
        back, typing,
        "an eligible frame rides one bare datagram, no prefix"
    );

    // A Critical frame stays on the length-prefixed stream even though it would fit a
    // datagram: a PING reply (opcode 2 reuses PING for the reply, per section 139) is not a
    // record the session may drop.
    let pong = migo_protocol::to_frame(
        migo_protocol::Opcode::Ping.to_wire(),
        3,
        &migo_protocol::Pong {
            client_time: migo_core::Timestamp::now(),
            server_time: migo_core::Timestamp::now(),
        },
    )
    .expect("the PONG encodes")
    .encode()
    .expect("the PONG encodes as a frame");
    transport
        .send(pong.clone())
        .await
        .expect("the critical frame sends");

    // An eligible frame that does not fit the datagram path stays on the stream too: the
    // fallback is part of the binding, not a failure of it.
    let big = migo_wire::Frame::simple(
        migo_protocol::Opcode::Typing.to_wire(),
        4,
        bytes::Bytes::from(vec![0u8; 64 * 1024]),
    )
    .encode()
    .expect("the big TYPING encodes");
    transport
        .send(big.clone())
        .await
        .expect("the oversized frame sends");

    /// Reads one length-prefixed record off the client's receive half of the stream.
    async fn read_stream_record(client_recv: &mut quinn::RecvStream) -> bytes::Bytes {
        let mut seen = Vec::new();
        loop {
            if let Ok(Some((frame, _consumed))) =
                migo_wire::Frame::decode_length_prefixed(&bytes::Bytes::copy_from_slice(&seen))
            {
                return frame.encode().expect("the frame re-encodes");
            }
            let mut buf = [0u8; 16 * 1024];
            let read = client_recv
                .read(&mut buf)
                .await
                .expect("the stream reads")
                .expect("the stream has not ended");
            seen.extend_from_slice(&buf[..read]);
        }
    }

    // Both stream-bound records arrive in order behind the u32 prefix: the Critical PONG and
    // then the oversized-but-eligible TYPING.
    let framed_pong = tokio::time::timeout(STEP, read_stream_record(&mut client_recv))
        .await
        .expect("the critical reply arrives within the step budget");
    assert_eq!(
        framed_pong, pong,
        "a Critical frame rides the length-prefixed stream, not a datagram"
    );
    let framed_big = tokio::time::timeout(STEP, read_stream_record(&mut client_recv))
        .await
        .expect("the oversized frame arrives within the step budget");
    assert_eq!(
        framed_big, big,
        "a frame too large for a datagram rides the length-prefixed stream"
    );
}

/// A full handshake against a live deployment, not one built in this process.
///
/// The tests above prove the listener against an [`App`] assembled here; this one proves the node
/// an operator actually started. Set `MIGO_QUIC_LIVE_ADDR=host:port` and run it with
/// `cargo test -p migod --test quic_listener -- --ignored` — the same check an operator runs
/// after flipping `MIGO_QUIC__BIND` on, answering the only question that matters at that moment:
/// does a real client complete the TLS 1.3 handshake, open a stream, and hear a WELCOME whose
/// negotiated features carry the QUIC bit the listener's existence promised?
#[tokio::test]
#[ignore = "points at a live deployment: set MIGO_QUIC_LIVE_ADDR=host:port to run it"]
async fn a_live_listener_answers_hello_with_a_welcome_that_carries_the_quic_bit() {
    let addr: SocketAddr = std::env::var("MIGO_QUIC_LIVE_ADDR")
        .expect("MIGO_QUIC_LIVE_ADDR names the deployment under test, e.g. 152.53.102.150:18443")
        .parse()
        .expect("MIGO_QUIC_LIVE_ADDR must be a socket address");

    let connection = connect(addr)
        .await
        .expect("the TLS 1.3 handshake completes against the live listener");

    // One stream is one session. Open it and speak the stream framing.
    let (mut send, mut recv) = tokio::time::timeout(STEP, connection.open_bi())
        .await
        .expect("opening a stream does not stall")
        .expect("the stream opens");

    // A real opening HELLO: the version this build speaks, no token, exactly the frame a fresh
    // client puts on the wire first — and the QUIC bit requested, because the negotiated set is
    // the intersection with what the client asks for. A client that does not ask gets a WELCOME
    // without QUIC even from a node that serves it, which is the contract, not a fault.
    let hello = migo_protocol::Hello {
        protocol_version: migo_protocol::PROTOCOL_VERSION,
        features: features::QUIC,
        ..Default::default()
    };
    let frame = migo_protocol::to_frame(migo_protocol::Opcode::Hello.to_wire(), 7, &hello)
        .expect("the HELLO encodes");
    let wire = frame
        .encode_length_prefixed()
        .expect("the HELLO frames for the stream binding");
    send.write_all(&wire).await.expect("the HELLO is written");

    // The reply rides the same framing: a u32 big-endian length, then the frame.
    let mut prefix = [0u8; 4];
    tokio::time::timeout(STEP, recv.read_exact(&mut prefix))
        .await
        .expect("the WELCOME arrives within the step budget")
        .expect("the length prefix reads");
    let len = u32::from_be_bytes(prefix) as usize;
    assert!(
        len <= migo_wire::limits::MAX_FRAME_BYTES,
        "the reply respects the frame ceiling: {len}"
    );
    let mut body = vec![0u8; len];
    tokio::time::timeout(STEP, recv.read_exact(&mut body))
        .await
        .expect("the WELCOME body arrives within the step budget")
        .expect("the body reads");

    let reply = migo_wire::Frame::decode(bytes::Bytes::copy_from_slice(&body))
        .expect("the reply decodes as an MWP frame");
    assert_eq!(
        reply.header.opcode,
        migo_protocol::Opcode::Hello.to_wire(),
        "the handshake is answered under the HELLO opcode"
    );
    assert!(
        !reply.header.is_error(),
        "a valid HELLO is answered with a WELCOME, not a fault: {:?}",
        reply.header
    );
    let welcome =
        migo_protocol::from_frame::<migo_protocol::Welcome>(&reply).expect("the WELCOME decodes");
    assert_ne!(
        welcome.features & features::QUIC,
        0,
        "a node serving the QUIC listener negotiates the QUIC feature bit"
    );

    let _ = send.finish();
}
