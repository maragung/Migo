//! The media plane over a real socket, driven by a real WebTransport client.
//!
//! These tests bind `Server` exactly the way `migosfud` binds it — a loopback address chosen by
//! the OS, a ticket key from configuration — and then dial it the way a Migo client does: a
//! WebTransport session over HTTP/3, one bidirectional stream for control, datagrams for media.
//! What they prove, in order:
//!
//!   1. Admission is a ticket and nothing else: a ticket this node minted seats the device and
//!      answers with the call's roster, a ticket another key minted is refused, and a ticket
//!      whose expiry has passed is refused with the one message an honest client can act on.
//!   2. The forwarding rule of section 166: a sealed frame reaches exactly the seats subscribed
//!      to its stream, and never the publisher — which is the whole of what an SFU is allowed to
//!      do with media, and the one thing it must not get wrong.
//!   3. A seat's end is observed: an explicit `LEAVE` reaches the other seats as a departure,
//!      and the seat is gone from the roster the next joiner is answered with.
//!
//! The client in this file skips certificate verification on purpose. The listener's leaf is
//! self-signed by design — `migo_sfu_node::server`'s module doc explains why an SFU's identity is
//! established by the ticket rather than by a chain — so a test client asserts on the session the
//! media plane runs, not on a certificate the deployment never promised.
//!
//! The negative assertions (a frame that must *not* arrive) run against a short window rather
//! than the step budget: absence is only observable by waiting, and the shortest wait that
//! reliably proves it is the one that keeps the suite quick. Every other step is bounded by
//! [`STEP`], so a listener that hangs fails as a stalled test rather than as a CI timeout.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::{Bytes, BytesMut};
use url::Url;
use web_transport_quinn as wt;

use migo_core::config::SfuNodeConfig;
use migo_core::metrics::Registry;
use migo_core::{Id, Secret, Shutdown, Timestamp};
use migo_sfu::{Layer, Member, StreamKind};

use migo_sfu_node::ticket::{SfuTicket, TicketKey};
use migo_sfu_node::wire::{
    body_len, decode_delivery_datagram, decode_reply, encode_publish_datagram, encode_request,
    frame_bytes, Reply, Request,
};
use migo_sfu_node::Server;

/// How long any single client-side step may take before the test declares the listener stuck.
const STEP: Duration = Duration::from_secs(5);

/// How long a negative assertion waits before it concludes nothing arrived.
const QUIET: Duration = Duration::from_millis(400);

fn key_bytes() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

fn other_key_bytes() -> String {
    base64::engine::general_purpose::STANDARD.encode([9u8; 32])
}

fn config(ticket_key: &str) -> SfuNodeConfig {
    SfuNodeConfig {
        bind: Some("127.0.0.1:0".to_string()),
        public_url: "quic://127.0.0.1:0".to_string(),
        metrics_bind: None,
        ticket_key: Secret::new(ticket_key),
        ticket_ttl_ms: 120_000,
        max_audio_participants: None,
        max_active_video_streams: None,
        outbound_queue: 64,
    }
}

/// A bound media plane, with the key its tickets are minted under.
struct Plane {
    addr: SocketAddr,
    key: TicketKey,
    registry: Arc<Registry>,
    _server: Arc<Server>,
}

async fn serve(ticket_key: &str) -> Plane {
    let registry = Arc::new(Registry::new());
    let server = Arc::new(
        Server::new(&config(ticket_key), Duration::from_millis(1_000), &registry)
            .expect("the media plane builds"),
    );
    let addr = server
        .bind(Shutdown::new())
        .await
        .expect("the media plane binds");
    Plane {
        addr,
        key: TicketKey::from_config(ticket_key).expect("the key is usable"),
        registry,
        _server: server,
    }
}

impl Plane {
    fn ticket(&self, call: u8, account: u8, device: u8) -> String {
        self.key.mint(&SfuTicket {
            call_id: id(call),
            member: member(account, device),
            expires_at: Timestamp::from_millis(Timestamp::now().as_millis() + 60_000),
        })
    }
}

fn id(seed: u8) -> Id {
    Id::from_bytes([seed; 16])
}

fn member(account: u8, device: u8) -> Member {
    Member {
        account_id: id(account),
        device_id: id(device),
    }
}

/// A certificate verifier that accepts the listener's self-signed leaf.
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

/// One connected client: its endpoint, its WebTransport session, and its control stream.
///
/// The endpoint is owned here because dropping it would end the session with it.
struct Client {
    _endpoint: quinn::Endpoint,
    session: wt::Session,
    send: wt::SendStream,
    recv: wt::RecvStream,
}

impl Client {
    /// Dials `addr` and opens a control stream, without joining yet.
    async fn connect(addr: SocketAddr) -> Client {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("TLS 1.3 is available")
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        crypto
            .dangerous()
            .set_certificate_verifier(Arc::new(AcceptAnyServerCert { provider }));
        // The one thing this client needs that a QUIC one does not: a WebTransport session is
        // HTTP/3, so a handshake that negotiates no protocol never opens one.
        crypto.alpn_protocols = vec![wt::ALPN.as_bytes().to_vec()];
        let quic = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .expect("the client crypto is QUIC-compatible");

        let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().expect("a loopback bind"))
            .expect("a client endpoint binds");
        let client = wt::Client::new(endpoint.clone(), quinn::ClientConfig::new(Arc::new(quic)));
        let url = Url::parse(&format!("https://{addr}/")).expect("a WebTransport URL");
        let session = tokio::time::timeout(STEP, client.connect(url))
            .await
            .expect("the handshake does not stall")
            .expect("the handshake completes");
        let (send, recv) = tokio::time::timeout(STEP, session.open_bi())
            .await
            .expect("opening a stream does not stall")
            .expect("the stream opens");
        Client {
            _endpoint: endpoint,
            session,
            send,
            recv,
        }
    }

    /// Dials, presents `ticket`, and returns the first reply — whatever it is.
    async fn join(addr: SocketAddr, ticket: &str) -> Reply {
        let mut client = Client::connect(addr).await;
        client
            .send_request(&Request::Join {
                ticket: ticket.to_string(),
            })
            .await;
        client
            .await_reply(|_| true)
            .await
            .expect("an admission is answered")
    }

    /// Writes one request, framed, on the control stream.
    ///
    /// Written on the caller's own task rather than a spawned one: a control stream's write half
    /// has one writer by construction, and a test that spawned a writer per request would be
    /// testing an ordering no client would ever produce.
    async fn send_request(&mut self, request: &Request) {
        let framed = frame_bytes(&encode_request(request));
        self.send.write_all(&framed).await.expect("the write lands");
    }

    /// Writes raw bytes on the control stream, for the malformed-frame cases.
    async fn send_raw(&mut self, bytes: &[u8]) {
        self.send.write_all(bytes).await.expect("the write lands");
    }

    /// Reads replies until one satisfies `want`, or the step budget runs out.
    async fn await_reply<F: Fn(&Reply) -> bool>(&mut self, want: F) -> Option<Reply> {
        let deadline = tokio::time::Instant::now() + STEP;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let body = match tokio::time::timeout(remaining, self.next_body()).await {
                Ok(Ok(body)) => body,
                Ok(Err(_)) | Err(_) => return None,
            };
            let reply = decode_reply(&body).expect("a reply this protocol has");
            if want(&reply) {
                return Some(reply);
            }
        }
    }

    /// Reads the next control frame's body, if one arrives before the stream ends.
    async fn next_body(&mut self) -> Result<Bytes, std::io::Error> {
        let mut prefix = [0u8; 4];
        self.recv
            .read_exact(&mut prefix)
            .await
            .map_err(std::io::Error::other)?;
        let len = body_len(&prefix).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        let mut body = vec![0u8; len];
        self.recv
            .read_exact(&mut body)
            .await
            .map_err(std::io::Error::other)?;
        Ok(Bytes::from(body))
    }

    /// Sends one sealed frame as a datagram.
    fn publish_frame(&self, stream_id: Id, sequence: u64, layer: Layer, payload: &[u8]) {
        let datagram = encode_publish_datagram(stream_id, sequence, layer, payload);
        self.session
            .send_datagram(datagram)
            .expect("the datagram is small enough for the path");
    }

    /// Reads the next delivery datagram, or `None` if none arrives within `within`.
    async fn delivery_within(&self, within: Duration) -> Option<(Member, Id, u64, Layer, Vec<u8>)> {
        match tokio::time::timeout(within, self.session.read_datagram()).await {
            Ok(Ok(bytes)) => {
                let (from, stream_id, sequence, layer, payload) =
                    decode_delivery_datagram(&bytes).expect("a delivery this protocol has");
                Some((from, stream_id, sequence, layer, payload.to_vec()))
            }
            Ok(Err(_)) | Err(_) => None,
        }
    }
}

/// Declares a stream, which every subscriber test needs first.
async fn publish_audio(client: &mut Client, stream_id: Id) {
    client
        .send_request(&Request::Publish {
            stream_id,
            kind: StreamKind::Audio,
            layers: Vec::new(),
        })
        .await;
    let reply = client
        .await_reply(|reply| matches!(reply, Reply::Ok | Reply::Error { .. }))
        .await
        .expect("a publish is answered");
    assert_eq!(reply, Reply::Ok, "the stream is declared");
}

#[tokio::test]
async fn a_ticket_this_node_minted_seats_the_device_and_answers_with_the_roster() {
    let plane = serve(&key_bytes()).await;
    let device = member(2, 3);

    let reply = Client::join(plane.addr, &plane.ticket(1, 2, 3)).await;
    match reply {
        Reply::Seated { seats, .. } => {
            assert_eq!(seats.len(), 1, "the joining seat is in its own roster");
            assert_eq!(seats[0].member(), device);
            assert!(
                seats[0].streams.is_empty(),
                "a seat that has published nothing carries no streams"
            );
        }
        other => panic!("expected a seat, got {other:?}"),
    }

    let rendered = plane.registry.render();
    assert!(
        rendered.contains("migo_sfu_node_admissions_total 1"),
        "the admission is counted:\n{rendered}"
    );
    assert!(
        rendered.contains("migo_sfu_node_sessions 1"),
        "the session is counted:\n{rendered}"
    );
}

#[tokio::test]
async fn a_ticket_another_key_minted_is_refused() {
    let plane = serve(&key_bytes()).await;
    let elsewhere = TicketKey::from_config(&other_key_bytes()).expect("the other key is usable");
    let forged = elsewhere.mint(&SfuTicket {
        call_id: id(1),
        member: member(2, 3),
        expires_at: Timestamp::from_millis(Timestamp::now().as_millis() + 60_000),
    });

    let reply = Client::join(plane.addr, &forged).await;
    let Reply::Error { code, message, .. } = reply else {
        panic!("expected a refusal, got {reply:?}");
    };
    assert_eq!(
        code,
        migo_protocol::fault::permission_denied("ticket").code(),
        "a forged ticket is a permission denial: {message}"
    );
    assert!(
        !message.contains("expired"),
        "a forgery is not told which half of the claim failed: {message}"
    );
    let rendered = plane.registry.render();
    assert!(
        rendered.contains("migo_sfu_node_admissions_refused_total 1"),
        "the refusal is counted:\n{rendered}"
    );
    assert!(
        rendered.contains("migo_sfu_node_sessions 0"),
        "a refused connection holds no seat:\n{rendered}"
    );
}

#[tokio::test]
async fn a_ticket_past_its_expiry_is_refused_with_the_one_message_a_client_can_act_on() {
    let plane = serve(&key_bytes()).await;
    let expired = plane.key.mint(&SfuTicket {
        call_id: id(1),
        member: member(2, 3),
        // Minted for a moment that has already passed: the signature is genuine, and the claim
        // it makes is no longer worth anything.
        expires_at: Timestamp::from_millis(1),
    });

    let reply = Client::join(plane.addr, &expired).await;
    let Reply::Error { code, message, .. } = reply else {
        panic!("expected a refusal, got {reply:?}");
    };
    assert_eq!(
        code,
        migo_protocol::fault::permission_denied("ticket").code()
    );
    assert!(
        message.contains("expired"),
        "an honest client is told to mint another: {message}"
    );
}

#[tokio::test]
async fn a_sealed_frame_reaches_the_subscriber_and_never_the_publisher() {
    let plane = serve(&key_bytes()).await;
    let stream = id(9);
    let publisher_seat = member(2, 3);

    let mut publisher = Client::connect(plane.addr).await;
    publisher
        .send_request(&Request::Join {
            ticket: plane.ticket(1, 2, 3),
        })
        .await;
    assert!(
        publisher
            .await_reply(|reply| matches!(reply, Reply::Seated { .. }))
            .await
            .is_some(),
        "the publisher is seated"
    );
    publish_audio(&mut publisher, stream).await;

    let mut subscriber = Client::connect(plane.addr).await;
    subscriber
        .send_request(&Request::Join {
            ticket: plane.ticket(1, 4, 5),
        })
        .await;
    assert!(
        subscriber
            .await_reply(|reply| matches!(reply, Reply::Seated { .. }))
            .await
            .is_some(),
        "the subscriber is seated"
    );
    subscriber
        .send_request(&Request::Subscribe {
            account_id: publisher_seat.account_id,
            device_id: publisher_seat.device_id,
            stream_id: stream,
            layer: Layer::Low,
        })
        .await;
    assert_eq!(
        subscriber
            .await_reply(|reply| matches!(reply, Reply::Ok | Reply::Error { .. }))
            .await
            .expect("a subscribe is answered"),
        Reply::Ok,
        "the subscription is taken"
    );

    let payload = b"sealed bytes nobody here can read";
    publisher.publish_frame(stream, 1, Layer::Low, payload);

    let delivery = subscriber
        .delivery_within(STEP)
        .await
        .expect("the subscriber receives the frame");
    assert_eq!(delivery.0, publisher_seat, "the delivery names its sender");
    assert_eq!(delivery.1, stream);
    assert_eq!(delivery.2, 1);
    assert_eq!(delivery.3, Layer::Low);
    assert_eq!(delivery.4, payload, "the payload crosses unopened");

    assert!(
        publisher.delivery_within(QUIET).await.is_none(),
        "a publisher is never its own subscriber"
    );
}

#[tokio::test]
async fn a_frame_for_a_stream_nobody_subscribed_to_reaches_nobody() {
    let plane = serve(&key_bytes()).await;
    let stream = id(9);

    let mut publisher = Client::connect(plane.addr).await;
    publisher
        .send_request(&Request::Join {
            ticket: plane.ticket(1, 2, 3),
        })
        .await;
    assert!(publisher
        .await_reply(|reply| matches!(reply, Reply::Seated { .. }))
        .await
        .is_some());
    publish_audio(&mut publisher, stream).await;

    publisher.publish_frame(stream, 1, Layer::Low, b"nobody is listening");
    assert!(
        publisher.delivery_within(QUIET).await.is_none(),
        "a stream with no subscribers delivers to nobody"
    );

    let rendered = plane.registry.render();
    assert!(
        rendered.contains("migo_sfu_node_media_frames_total 0"),
        "no frame was offered for delivery:\n{rendered}"
    );
}

#[tokio::test]
async fn a_leave_reaches_the_other_seats_and_retires_the_seat() {
    let plane = serve(&key_bytes()).await;
    let leaver = member(2, 3);
    let stayer = member(4, 5);

    let mut first = Client::connect(plane.addr).await;
    first
        .send_request(&Request::Join {
            ticket: plane.ticket(1, 2, 3),
        })
        .await;
    assert!(first
        .await_reply(|reply| matches!(reply, Reply::Seated { .. }))
        .await
        .is_some());

    let mut second = Client::connect(plane.addr).await;
    second
        .send_request(&Request::Join {
            ticket: plane.ticket(1, 4, 5),
        })
        .await;
    match second
        .await_reply(|reply| matches!(reply, Reply::Seated { .. }))
        .await
        .expect("the second seat is answered")
    {
        Reply::Seated { seats, .. } => {
            let seats: Vec<Member> = seats.iter().map(|seat| seat.member()).collect();
            assert_eq!(seats.len(), 2, "both seats are known");
            assert!(seats.contains(&leaver) && seats.contains(&stayer));
        }
        other => panic!("expected a seat, got {other:?}"),
    }

    // The arrival is pushed to the seat that was already there.
    assert_eq!(
        first
            .await_reply(|reply| matches!(reply, Reply::Peer { .. }))
            .await
            .expect("the arrival is announced"),
        Reply::Peer {
            joined: true,
            member: stayer,
        }
    );

    second.send_request(&Request::Leave).await;
    assert_eq!(
        second
            .await_reply(|reply| matches!(reply, Reply::Ok | Reply::Error { .. }))
            .await
            .expect("a leave is answered"),
        Reply::Ok
    );
    assert_eq!(
        first
            .await_reply(|reply| matches!(reply, Reply::Peer { .. }))
            .await
            .expect("the departure is announced"),
        Reply::Peer {
            joined: false,
            member: stayer,
        }
    );

    // And the seat is gone from the roster a later joiner is answered with.
    let third = Client::join(plane.addr, &plane.ticket(1, 6, 7)).await;
    match third {
        Reply::Seated { seats, .. } => {
            let seats: Vec<Member> = seats.iter().map(|seat| seat.member()).collect();
            assert_eq!(seats.len(), 2, "the departed seat is not in the roster");
            assert!(seats.contains(&leaver));
            assert!(!seats.contains(&stayer));
        }
        other => panic!("expected a seat, got {other:?}"),
    }
}

#[tokio::test]
async fn a_connection_whose_first_frame_is_not_a_join_is_closed_without_a_seat() {
    let plane = serve(&key_bytes()).await;
    let mut client = Client::connect(plane.addr).await;
    client.send_request(&Request::Roster).await;

    // The stream is never answered: there was nobody to answer when the frame arrived.
    assert!(
        client.await_reply(|_| true).await.is_none(),
        "a non-join opening frame is not answered"
    );
    let rendered = plane.registry.render();
    assert!(
        rendered.contains("migo_sfu_node_sessions 0"),
        "no seat was taken:\n{rendered}"
    );
}

#[tokio::test]
async fn a_malformed_control_frame_is_refused_and_the_session_survives() {
    let plane = serve(&key_bytes()).await;
    let mut client = Client::connect(plane.addr).await;
    client
        .send_request(&Request::Join {
            ticket: plane.ticket(1, 2, 3),
        })
        .await;
    assert!(
        client
            .await_reply(|reply| matches!(reply, Reply::Seated { .. }))
            .await
            .is_some(),
        "the client is seated"
    );

    // A length prefix announcing a body that is not a frame this protocol has.
    let mut framed = BytesMut::new();
    framed.extend_from_slice(&3u32.to_be_bytes());
    framed.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
    client.send_raw(&framed).await;

    let reply = client
        .await_reply(|reply| matches!(reply, Reply::Error { .. }))
        .await
        .expect("the bad frame is refused");
    let Reply::Error { code, .. } = reply else {
        unreachable!()
    };
    assert_eq!(
        code,
        migo_protocol::fault::malformed_frame("x").code(),
        "a frame that cannot be read is a malformed frame"
    );

    // And the seat is still there: one bad frame is not grounds for dropping a call.
    client.send_request(&Request::Roster).await;
    match client
        .await_reply(|reply| matches!(reply, Reply::Roster { .. }))
        .await
        .expect("the roster is answered")
    {
        Reply::Roster { seats } => assert_eq!(seats.len(), 1),
        other => panic!("expected a roster, got {other:?}"),
    }
}
