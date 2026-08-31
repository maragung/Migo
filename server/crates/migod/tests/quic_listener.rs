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
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
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
