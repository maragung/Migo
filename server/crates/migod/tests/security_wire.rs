//! Negative wire tests over a real TCP listener — brief sections 95 and 172.
//!
//! The gateway suite in `migo-gateway` proves each refusal rule against an
//! in-process pipe; these tests prove the same rules travel: the whole node —
//! `App::build`, the TCP listener, the length-prefixed stream framing, the
//! gateway — is assembled exactly the way a deployment assembles it, and the
//! hostile bytes arrive on a real socket, because the seam between "the
//! transport" and "the gateway" is where a refusal has historically gone
//! missing (a listener that answers a stranger differently than the test double
//! did is exactly the bug no in-process suite can see).
//!
//! What is asserted is the refusal contract, not the implementation:
//!
//! * **The reserved span is terminal.** Every opcode in 248..=255 — not a
//!   sample, the whole span — is answered `UNKNOWN_OPCODE` with the public
//!   hint "reserved opcode", and the connection is closed.
//! * **The allocated head is not in that span.** 240 (`ENTITLEMENTS`, section
//!   145's store carve-out) and 247 (`CALL_LIST`, the same section's latest) are
//!   both allocated, so from an unauthenticated session each is refused by the
//!   *phase* gate with `UNEXPECTED_OPCODE` and **no message at all** — whether
//!   this build even knows the opcode is opt-in disclosure, and a stranger
//!   gets neither the fact nor the reason. The conversation-federation pair
//!   241-242, the row-replication tier 243-246, and the call-row pair 248-249
//!   are likewise allocated, and their client-side refusals are proven by the
//!   gateway suite's range-gate tests rather than repeated here.
//! * **Unknown is answered, not fatal.** A never-allocated opcode gets
//!   `UNKNOWN_OPCODE` and the session continues — a newer client is not a
//!   protocol violation.
//! * **Auth is a gate, not a hint.** A user-level opcode before
//!   authentication, and a forged inline token, both end in the same opaque
//!   phase refusal. The forged token is not fatal at the handshake on purpose
//!   (a bad token opens an unauthenticated session, never an authenticated
//!   one), so the refusal afterwards is what proves the forgery bought
//!   nothing.
//! * **Replayed and oversized input cannot wedge the listener.** A second
//!   HELLO, garbage that is not a frame, and a length prefix one byte past
//!   the frame ceiling all end the session cleanly; the next connection is
//!   served.
//!
//! Every step is bounded by `STEP`, so a wedged listener fails as a timeout
//! rather than as a CI job that never ends. The anonymous rate limits are
//! raised for the whole suite — these tests are about opcode and phase policy,
//! and the throttle behaviour is pinned by the gateway stress suite instead.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_core::Config;
use migo_protocol::{
    codes, from_frame, to_frame, Encode, Error as ErrorMessage, Frame, Hello, Opcode, Ping,
    ProfileRequest, PROTOCOL_VERSION,
};
use migo_wire::limits::MAX_FRAME_BYTES;
use migod::App;

/// How long any single client-side step may take before the test declares the
/// listener wedged rather than waiting on the CI timeout to do it.
const STEP: Duration = Duration::from_secs(5);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development node with the native TCP listener bound, and anonymous limits
/// raised so a suite of fresh connections is never throttled for the sake of a
/// test that is not about throttling.
async fn build_app() -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
            (
                "MIGO_RATE_LIMIT__ANONYMOUS_BURST".to_string(),
                "1000".to_string(),
            ),
            (
                "MIGO_RATE_LIMIT__ANONYMOUS_REFILL_PER_SECOND".to_string(),
                "500".to_string(),
            ),
        ],
    )
    .expect("configuration should parse");
    App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends")
}

/// A minimal, valid opening greeting that authenticates nobody.
fn anonymous_hello() -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        ..Default::default()
    }
}

/// Opens a connection and completes the handshake, returning the stream and
/// the WELCOME the server answered with.
async fn handshake(addr: SocketAddr) -> (tokio::net::TcpStream, Frame) {
    let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
        .await
        .expect("connecting does not stall")
        .expect("the connection is accepted");
    send_msg(&mut stream, Opcode::Hello, 1, &anonymous_hello()).await;
    let welcome = recv(&mut stream).await;
    assert_eq!(
        Opcode::from_wire(welcome.header.opcode),
        Some(Opcode::Hello),
        "the handshake is answered with a WELCOME on the HELLO opcode"
    );
    assert!(
        !welcome.header.is_error(),
        "an anonymous HELLO is not refused"
    );
    (stream, welcome)
}

/// Sends one request frame as a length-prefixed record on the stream.
async fn send_msg<M: Encode>(
    stream: &mut tokio::net::TcpStream,
    opcode: Opcode,
    correlation: u32,
    message: &M,
) {
    let frame = to_frame(opcode.to_wire(), correlation, message)
        .expect("a scripted client message must encode");
    let wire = frame.encode_length_prefixed().expect("the record encodes");
    tokio::time::timeout(STEP, stream.write_all(&wire))
        .await
        .expect("writing does not stall")
        .expect("the frame is written");
}

/// Sends one already-encoded frame's record, for the tests that must control
/// the opcode exactly — including the ones this build refuses to construct
/// through `to_frame` because the opcode has no message type.
async fn send_frame(stream: &mut tokio::net::TcpStream, frame: &Frame) {
    let wire = frame.encode_length_prefixed().expect("the record encodes");
    tokio::time::timeout(STEP, stream.write_all(&wire))
        .await
        .expect("writing does not stall")
        .expect("the record is written");
}

/// Sends a raw length-prefixed record whose body nobody promises is a frame.
async fn send_raw(stream: &mut tokio::net::TcpStream, body: &[u8]) {
    let mut record = (body.len() as u32).to_be_bytes().to_vec();
    record.extend_from_slice(body);
    tokio::time::timeout(STEP, stream.write_all(&record))
        .await
        .expect("writing does not stall")
        .expect("the record is written");
}

/// Reads one length-prefixed reply record and decodes it as a frame.
async fn recv(stream: &mut tokio::net::TcpStream) -> Frame {
    let body = tokio::time::timeout(STEP, async {
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
    .expect("the reply does not stall — silence here is the bug these tests exist to catch");
    Frame::decode(Bytes::from(body)).expect("the reply decodes")
}

/// Reads until the server closes the connection, bounded by `STEP`.
async fn read_to_close(stream: &mut tokio::net::TcpStream) {
    tokio::time::timeout(STEP, async {
        let mut scratch = [0u8; 512];
        loop {
            match stream.read(&mut scratch).await {
                Ok(0) => break,
                Ok(_) => continue,
                Err(error) => panic!("unexpected read error: {error}"),
            }
        }
    })
    .await
    .expect("the connection ends promptly");
}

/// Reads one frame, asserts it is the given error, and asserts the connection
/// then closes. Returns the decoded error for the caller's further assertions.
async fn assert_error_then_close(
    stream: &mut tokio::net::TcpStream,
    expected_code: u32,
    what: &str,
) -> ErrorMessage {
    let frame = recv(stream).await;
    assert!(
        frame.header.is_error(),
        "{what} must be answered with an error frame, got {frame:?}"
    );
    let error: ErrorMessage = from_frame(&frame).expect("an error frame decodes");
    assert_eq!(error.code, expected_code, "{what}: got {error:?}");
    read_to_close(stream).await;
    error
}

/// Every opcode in the never-allocated span 251-255 is refused with the public
/// hint "reserved opcode" and closes the connection. The whole span, not a
/// sample: a regression that frees one number at the tail is exactly as wrong
/// as one at the head.
#[tokio::test]
async fn every_opcode_in_the_never_allocated_span_is_refused_and_closes_the_connection() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");
    for raw in 251u32..=255 {
        let (mut stream, _welcome) = handshake(addr).await;
        let frame = Frame::new(migo_wire::FrameHeader::new(raw, 7), Bytes::from_static(&[]));
        send_frame(&mut stream, &frame).await;
        let error =
            assert_error_then_close(&mut stream, codes::UNKNOWN_OPCODE, "a reserved opcode").await;
        assert_eq!(
            error.message.as_deref(),
            Some("reserved opcode"),
            "the range gate's public hint, and nothing more, for opcode {raw}"
        );
    }
}

/// The numbers at the allocated head of the reserved range — 240
/// (`ENTITLEMENTS`), 247 (`CALL_LIST`), and 250 (`CALL_HISTORY`) — are each
/// allocated, so none is
/// the range gate's to refuse: from an unauthenticated session it is the phase
/// gate that answers, with `UNEXPECTED_OPCODE` and no message at all — whether
/// this build knows the opcode is opt-in disclosure, and a stranger gets
/// neither the fact nor the reason. All three are walked, because the head
/// moving is exactly what a new allocation does here, and a test that pinned
/// only the old head would keep passing while the gate quietly swallowed the
/// new one.
#[tokio::test]
async fn the_allocated_head_of_the_reserved_range_is_refused_by_the_phase_gate_without_disclosure()
{
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");
    for raw in [240u32, 247, 250] {
        let (mut stream, _welcome) = handshake(addr).await;
        let frame = Frame::new(migo_wire::FrameHeader::new(raw, 7), Bytes::from_static(&[]));
        send_frame(&mut stream, &frame).await;
        let error = assert_error_then_close(
            &mut stream,
            codes::UNEXPECTED_OPCODE,
            "the allocated head of the reserved span, before authentication",
        )
        .await;
        assert_eq!(
            error.message, None,
            "the phase gate discloses nothing about opcode {raw}: not the opcode, not the state"
        );
    }
}

/// A never-allocated opcode is answered, not punished: the same connection
/// must still carry a PING afterwards, because a newer client is not a
/// protocol violation.
#[tokio::test]
async fn an_unknown_opcode_is_answered_and_the_session_survives() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");
    let (mut stream, _welcome) = handshake(addr).await;
    // 205 sits in the unallocated gap between MODERATION_EVENT (194) and
    // FED_HELLO (208): legal to speak, unknown to this build.
    let never_allocated = Frame::new(migo_wire::FrameHeader::new(205, 7), Bytes::from_static(&[]));
    send_frame(&mut stream, &never_allocated).await;
    let frame = recv(&mut stream).await;
    assert!(frame.header.is_error(), "the unknown opcode is answered");
    let error: ErrorMessage = from_frame(&frame).expect("an error frame decodes");
    assert_eq!(error.code, codes::UNKNOWN_OPCODE, "got {error:?}");
    assert_eq!(
        error.message.as_deref(),
        Some("unknown opcode"),
        "the answer names the refusal, not the version table"
    );

    // The session is still alive: a PING is answered with a PONG on the same
    // connection, which is the whole difference between "unknown" and
    // "reserved".
    send_msg(
        &mut stream,
        Opcode::Ping,
        8,
        &Ping {
            client_time: migo_core::Timestamp::from_millis(0),
        },
    )
    .await;
    let pong = recv(&mut stream).await;
    assert_eq!(
        pong.header.correlation, 8,
        "the PONG echoes the PING's correlation"
    );
    assert!(
        !pong.header.is_error(),
        "the session survived the unknown opcode"
    );
}

/// A user-level opcode before authentication is the plain auth-bypass attempt:
/// refused by the phase gate, opaquely, and the connection is closed.
#[tokio::test]
async fn a_user_opcode_before_authentication_is_refused_opaquely() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");
    let (mut stream, _welcome) = handshake(addr).await;
    send_msg(
        &mut stream,
        Opcode::ProfileFetch,
        3,
        &ProfileRequest::default(),
    )
    .await;
    let error = assert_error_then_close(
        &mut stream,
        codes::UNEXPECTED_OPCODE,
        "a user-level opcode before authentication",
    )
    .await;
    assert_eq!(
        error.message, None,
        "the phase gate never says whether the session exists"
    );
}

/// A forged inline token is not fatal at the handshake — by design, a bad
/// token opens an unauthenticated session rather than an authenticated one —
/// so what proves the forgery bought nothing is the phase refusal that follows.
#[tokio::test]
async fn a_forged_inline_token_opens_no_authenticated_session() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");

    let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
        .await
        .expect("connecting does not stall")
        .expect("the connection is accepted");
    let forged = Hello {
        protocol_version: PROTOCOL_VERSION,
        access_token: Some("a-token-nobody-issued".to_string()),
        device_id: Some(migo_core::Id::from_bytes([9u8; 16])),
        ..Default::default()
    };
    send_msg(&mut stream, Opcode::Hello, 1, &forged).await;
    let welcome = recv(&mut stream).await;
    assert!(
        !welcome.header.is_error(),
        "a bad token is answered with a WELCOME, not a refusal — the session is \
         simply never authenticated"
    );

    // The gate that matters: a user-level opcode on the session the forgery
    // opened is refused exactly as if no token had been sent.
    send_msg(
        &mut stream,
        Opcode::ProfileFetch,
        2,
        &ProfileRequest::default(),
    )
    .await;
    let error = assert_error_then_close(
        &mut stream,
        codes::UNEXPECTED_OPCODE,
        "the opcode after a forged token",
    )
    .await;
    assert_eq!(error.message, None, "nothing about the token is disclosed");
}

/// A server-to-client opcode arriving from a client is a direction violation:
/// terminal, opaquely.
#[tokio::test]
async fn a_server_to_client_opcode_from_a_client_is_terminal() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");
    let (mut stream, _welcome) = handshake(addr).await;
    let event = Frame::new(
        migo_wire::FrameHeader::new(Opcode::MessageEvent.to_wire(), 4),
        Bytes::from_static(&[]),
    );
    send_frame(&mut stream, &event).await;
    let error = assert_error_then_close(
        &mut stream,
        codes::UNEXPECTED_OPCODE,
        "a server-to-client opcode from a client",
    )
    .await;
    assert_eq!(error.message, None, "no direction oracle for a stranger");
}

/// A second HELLO after the handshake is the replayed-handshake case: the
/// session that already exists is not re-opened, re-negotiated, or confused.
#[tokio::test]
async fn a_replayed_hello_after_the_handshake_is_terminal() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");
    let (mut stream, _welcome) = handshake(addr).await;
    send_msg(&mut stream, Opcode::Hello, 9, &anonymous_hello()).await;
    let error = assert_error_then_close(
        &mut stream,
        codes::UNEXPECTED_OPCODE,
        "a second HELLO after the handshake",
    )
    .await;
    assert_eq!(error.message, None, "no handshake state is disclosed");
}

/// Garbage that is not a frame — a valid record prefix, a body of nothing but
/// 0xFF — ends the session with a protocol violation and no decoder oracle.
#[tokio::test]
async fn garbage_that_is_not_a_frame_ends_the_session_without_a_decoder_oracle() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");
    let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
        .await
        .expect("connecting does not stall")
        .expect("the connection is accepted");
    send_raw(&mut stream, &[0xFFu8; 16]).await;
    // The lead 0xFF byte is not protocol version 1, so the refusal is the
    // version error; the point of the test is what the refusal does *not* say.
    let error = assert_error_then_close(
        &mut stream,
        codes::PROTOCOL_VERSION_UNSUPPORTED,
        "sixteen bytes of 0xFF",
    )
    .await;
    assert_eq!(
        error.message, None,
        "the refusal says nothing about what the decoder wanted instead"
    );
}

/// A length prefix one byte past the frame ceiling: the session must end
/// without the body ever being sent — if the listener tried to read the
/// declared 262_145 bytes it would wait for them, and the timeout is the
/// assertion.
#[tokio::test]
async fn a_record_one_byte_past_the_frame_ceiling_is_refused_without_reading_the_body() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");
    let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
        .await
        .expect("connecting does not stall")
        .expect("the connection is accepted");
    let prefix = (MAX_FRAME_BYTES + 1) as u32;
    tokio::time::timeout(STEP, stream.write_all(&prefix.to_be_bytes()))
        .await
        .expect("writing the prefix does not stall")
        .expect("the prefix is written");
    // No body follows. A listener that trusted the prefix would block here;
    // one that checks it closes.
    read_to_close(&mut stream).await;
}

/// After every refusal above, the listener still serves the next stranger:
/// each refusal is a connection's end, never the listener's.
#[tokio::test]
async fn the_listener_still_serves_after_refusing_everything_above() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");

    // One of each hostile shape, on separate connections: a reserved number,
    // the allocated head of the reserved range, and a server-to-client
    // opcode. All three are terminal, so the connection ends after the
    // answer. 250 stands for the reserved span the way the gateway suite's
    // own reserved test uses it.
    for hostile in [250u32, 240, Opcode::MessageEvent.to_wire()] {
        let (mut stream, _welcome) = handshake(addr).await;
        let frame = Frame::new(
            migo_wire::FrameHeader::new(hostile, 7),
            Bytes::from_static(&[]),
        );
        send_frame(&mut stream, &frame).await;
        // Whatever the answer was, the connection is over afterwards.
        let _ = recv(&mut stream).await;
        read_to_close(&mut stream).await;
    }

    // And a merely unknown opcode is answered with the session still open —
    // dropped here from the client side, which must not disturb the listener.
    {
        let (mut stream, _welcome) = handshake(addr).await;
        let frame = Frame::new(migo_wire::FrameHeader::new(205, 7), Bytes::from_static(&[]));
        send_frame(&mut stream, &frame).await;
        let _ = recv(&mut stream).await;
    }

    // And then an ordinary handshake is served as if nothing had happened.
    let (_stream, welcome) = handshake(addr).await;
    assert!(
        !welcome.header.is_error(),
        "the listener serves the next client"
    );
}
