//! The native-client TCP listener, exercised end to end over a real socket.
//!
//! These tests bind the listener exactly the way `App::build` binds it in production — from the
//! `MIGO_TCP__BIND` environment pair, against an in-memory development configuration — and then
//! drive it with a plain `tokio::net::TcpStream` client speaking the stream framing: a u32
//! big-endian length, then one MWP frame. What they prove, in order:
//!
//!   1. The feature contract: a node with `tcp.bind` set advertises the `TCP_TRANSPORT` feature
//!      bit and one without it does not, so a client can never negotiate a transport the node is
//!      not serving (brief section 138 — TCP is the native default, QUIC the second option).
//!   2. The listener really serves: a connection is accepted as one realtime session, the
//!      length-prefixed stream framing carries the session's lifecycle — here a hostile length
//!      prefix far past the MAX_FRAME_BYTES ceiling, which the gateway must answer by ending the
//!      session cleanly rather than by hanging or tearing down the process.
//!
//! No TLS is involved anywhere here: these tests speak plaintext to a loopback listener, which is
//! the one place the brief allows it. The production posture (TLS 1.3 in front of the listener) is
//! a deployment's trust story, not a session-protocol one, and the live test covers what an
//! operator actually runs.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

#[tokio::test]
async fn a_node_without_tcp_configured_advertises_no_tcp() {
    let app = build_app(&[]).await;
    assert!(app.tcp_bind.is_none(), "no tcp.bind, no listener");
    assert_eq!(
        app.features & features::TCP_TRANSPORT,
        0,
        "the TCP_TRANSPORT bit must not be advertised when the listener is not bound"
    );
}

#[tokio::test]
async fn a_node_with_tcp_bound_advertises_the_tcp_bit() {
    let app = build_app(&[("MIGO_TCP__BIND", "127.0.0.1:0")]).await;
    assert!(
        app.tcp_bind.is_some(),
        "tcp.bind set means a listener is bound"
    );
    assert_ne!(
        app.features & features::TCP_TRANSPORT,
        0,
        "the TCP_TRANSPORT bit is advertised exactly while the listener is serving"
    );
}

#[tokio::test]
async fn the_listener_accepts_a_connection_and_ends_the_session_on_a_hostile_prefix() {
    let app = build_app(&[("MIGO_TCP__BIND", "127.0.0.1:0")]).await;
    let addr = app.tcp_bind.expect("the listener is bound");

    let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
        .await
        .expect("connecting does not stall")
        .expect("the connection is accepted");

    // A frame the wire codec must refuse: the length prefix names a frame far past the
    // MAX_FRAME_BYTES ceiling, so the session driver should end the session rather than buffer
    // for it.
    stream
        .write_all(&u32::MAX.to_be_bytes())
        .await
        .expect("the hostile prefix is written");

    // The session ends: the gateway closes its side, which the client reads as the connection's
    // clean end — not as a hang, and not as a process-level failure.
    tokio::time::timeout(STEP, async {
        let mut scratch = [0u8; 8];
        loop {
            match stream.read(&mut scratch).await {
                Ok(0) => break,
                Ok(_) => continue,
                Err(error) => panic!("unexpected read error: {error}"),
            }
        }
    })
    .await
    .expect("the session ends promptly after a hostile prefix");
}

/// A full handshake against a live deployment, not one built in this process.
///
/// The tests above prove the listener against an [`App`] assembled here; this one proves the node
/// an operator actually started. Set `MIGO_TCP_LIVE_ADDR=host:port` and run it with
/// `cargo test -p migod --test tcp_listener -- --ignored` — the same check an operator runs
/// after flipping `MIGO_TCP__BIND` on, answering the only question that matters at that moment:
/// does a real client send a length-prefixed HELLO and hear a WELCOME whose negotiated features
/// carry the TCP_TRANSPORT bit the listener's existence promised?
#[tokio::test]
#[ignore = "points at a live deployment: set MIGO_TCP_LIVE_ADDR=host:port to run it"]
async fn a_live_listener_answers_hello_with_a_welcome_that_carries_the_tcp_bit() {
    let addr: SocketAddr = std::env::var("MIGO_TCP_LIVE_ADDR")
        .expect("MIGO_TCP_LIVE_ADDR names the deployment under test, e.g. 152.53.102.150:18081")
        .parse()
        .expect("MIGO_TCP_LIVE_ADDR must be a socket address");

    // A HELLO shaped exactly like a native client's, minus the token: this asserts the transport,
    // not the account. The TCP bit rides in the feature mask because the negotiated set is the
    // intersection — a client that does not ask gets a WELCOME without it.
    let hello = migo_protocol::Hello {
        protocol_version: migo_protocol::PROTOCOL_VERSION,
        features: features::TCP_TRANSPORT,
        ..Default::default()
    };
    let frame = migo_protocol::to_frame(migo_protocol::Opcode::Hello.to_wire(), 1, &hello)
        .expect("the HELLO encodes");
    let wire = frame.encode_length_prefixed().expect("the record encodes");

    let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
        .await
        .expect("connecting to the live listener does not stall")
        .expect("the live listener accepts the connection");
    stream.write_all(&wire).await.expect("the HELLO is written");

    // Read the reply record: a u32 big-endian length, then the frame. One WELCOME — or one
    // ERROR-flagged refusal — is one record; anything else is the listener speaking a dialect
    // this test cannot vouch for.
    let reply = tokio::time::timeout(STEP, async {
        let mut head = [0u8; 4];
        stream
            .read_exact(&mut head)
            .await
            .expect("the length arrives");
        let len = u32::from_be_bytes(head) as usize;
        let mut body = vec![0u8; len];
        stream
            .read_exact(&mut body)
            .await
            .expect("the body arrives");
        body
    })
    .await
    .expect("the WELCOME does not stall");

    let frame = migo_protocol::Frame::decode(reply.into()).expect("the reply decodes");
    assert!(!frame.header.is_error(), "a HELLO is answered, not refused");
    assert_eq!(
        migo_protocol::Opcode::from_wire(frame.header.opcode),
        Some(migo_protocol::Opcode::Hello),
        "the reply carries the HELLO opcode, which is how WELCOME is identified"
    );
    let welcome: migo_protocol::Welcome =
        migo_protocol::from_frame(&frame).expect("the WELCOME decodes");
    assert_ne!(
        welcome.features & features::TCP_TRANSPORT,
        0,
        "a node serving the TCP listener negotiates the TCP_TRANSPORT feature bit"
    );
}
