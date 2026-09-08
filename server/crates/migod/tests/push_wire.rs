//! PUSH_REGISTER and PUSH_UNREGISTER over a real TCP session.
//!
//! The notify service's own suite proves `Notifier::register` and `Notifier::unregister`
//! correct — sealing, hashing, dis-placement, the rate-limit charge — but it calls the
//! service directly. The dispatch arm between the wire and the service is the one layer
//! no service test exercises, and it is the layer where the two registration opcodes
//! lived unreachably since the notify crate shipped: the struct was generated on every
//! platform and no opcode carried it. These tests drive the arms end to end, the same
//! way `dispatch_replies.rs` drives the reply contract: a real account, the native TCP
//! listener, frames in, reply records out.
//!
//! Storage is asserted through the notify service's own metrics rather than a store
//! read: `migo_notify_registrations_total{outcome="registered"}` only moves after
//! `set_push_registration` returns `Ok`, so the counter is the service's own attestation
//! that the credential was written — and it says so without exposing a sealed token or
//! its hash to a test either.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Config, Secret};
use migo_notify::MAX_TOKEN_LEN;
use migo_protocol::{
    codes, from_frame, to_frame, Acknowledged, Encode, Frame, Hello, Opcode, Platform,
    PushRegister, PushUnregister, Welcome, PROTOCOL_VERSION,
};
use migod::App;

/// How long any single client-side step may take before the test declares the server stuck.
const STEP: Duration = Duration::from_secs(5);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development app with the TCP listener bound, exactly as the reply tests build it.
async fn build_app() -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
        ],
    )
    .expect("configuration should parse");
    App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends")
}

/// Registers one account through the front door, as the reply tests do.
async fn registered_grant(app: &App, username: &str) -> Grant {
    app.auth
        .register(
            Registration {
                username: username.to_string(),
                email: None,
                phone: None,
                passphrase: Secret::new("correct-horse-battery-staple"),
                locale: "en-US".to_string(),
                country: None,
                gender: None,
                device: DeviceClaim::new(Platform::Android, "push wire test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// The notify service's registration counters, read from the app's own registry.
fn registrations(app: &App, outcome: &str) -> u64 {
    app.registry
        .counter(
            "migo_notify_registrations_total",
            "",
            &[("outcome", outcome)],
        )
        .get()
}

async fn send<M: Encode>(
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

/// A live, authenticated TCP session: HELLO with the grant's token, answered by a WELCOME
/// that names the account.
struct LiveSession {
    stream: tokio::net::TcpStream,
}

impl LiveSession {
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting does not stall")
            .expect("the connection is accepted");

        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            access_token: Some(grant.access_token.clone()),
            device_id: Some(grant.device_id),
            ..Default::default()
        };
        send(&mut stream, Opcode::Hello, 1, &hello).await;

        let welcome_frame = recv(&mut stream).await;
        assert_eq!(
            Opcode::from_wire(welcome_frame.header.opcode),
            Some(Opcode::Hello),
            "the handshake is answered with a WELCOME, which reuses the HELLO opcode"
        );
        assert!(
            !welcome_frame.header.is_error(),
            "the handshake is not refused"
        );
        let welcome: Welcome = from_frame(&welcome_frame).expect("the WELCOME decodes");
        assert_eq!(
            welcome.authenticated_user,
            Some(grant.account_id),
            "the inline token must promote the session, or nothing after this is meaningful"
        );
        Self { stream }
    }

    /// Sends a request and returns the reply frame, asserting only the correlation — a
    /// refusal is a legitimate outcome for these tests and is inspected by the caller.
    async fn ask_for_frame<M: Encode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        message: &M,
    ) -> Frame {
        send(&mut self.stream, opcode, correlation, message).await;
        let frame = recv(&mut self.stream).await;
        assert_eq!(
            frame.header.correlation, correlation,
            "the reply must echo the request's correlation (section 139)"
        );
        frame
    }

    /// Sends a request that must succeed, returning the decoded `Acknowledged`.
    async fn ask<M: Encode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        message: &M,
    ) -> Acknowledged {
        let frame = self.ask_for_frame(opcode, correlation, message).await;
        assert!(
            !frame.header.is_error(),
            "the request was refused: {:?}",
            from_frame::<migo_protocol::Error>(&frame)
        );
        from_frame(&frame).expect("the reply decodes as an Acknowledged")
    }
}

/// A plausible FCM-shaped registration token: ASCII, in the length band real tokens
/// occupy, and nothing that could be mistaken for a credential of any other kind.
fn fcm_shaped_token(len: usize) -> String {
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
    (0..len)
        .map(|i| char::from(alphabet[i % alphabet.len()]))
        .collect()
}

/// The device hands over a token, the reply says ok, and the service's own metric
/// attests the registration was written. Sending it twice — the cold-start pattern —
/// is answered twice and replaces rather than fails, which is the idempotence the
/// service was built with.
#[tokio::test]
async fn a_push_register_is_stored_answered_and_repeatable() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let grant = registered_grant(&app, "pushowner").await;
    let mut session = LiveSession::connect(addr, &grant).await;

    let request = PushRegister {
        token: fcm_shaped_token(140),
        provider: 0,
    };
    let first = session.ask(Opcode::PushRegister, 51, &request).await;
    assert!(first.ok, "the registration acknowledgement says ok");
    assert_eq!(
        registrations(&app, "registered"),
        1,
        "the service recorded exactly one registration"
    );

    // A client re-registers on every cold start; the service replaces rather than
    // refuses, and the reply must arrive on that path too, not only the first.
    let second = session.ask(Opcode::PushRegister, 52, &request).await;
    assert!(second.ok, "a repeated registration is still acknowledged");
    assert_eq!(registrations(&app, "registered"), 2);
}

/// The withdrawal path: register, then unregister, then unregister again. The store's
/// clear is idempotent, so the second withdrawal is the no-op success the caller still
/// awaits an acknowledgement for — the same reply discipline the room-leave tests pin.
#[tokio::test]
async fn a_push_unregister_is_answered_including_the_no_op_repeat() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let grant = registered_grant(&app, "pushleaver").await;
    let mut session = LiveSession::connect(addr, &grant).await;

    let stored = session
        .ask(
            Opcode::PushRegister,
            61,
            &PushRegister {
                token: fcm_shaped_token(140),
                provider: 0,
            },
        )
        .await;
    assert!(stored.ok);

    let gone = session
        .ask(Opcode::PushUnregister, 62, &PushUnregister {})
        .await;
    assert!(gone.ok, "the withdrawal is acknowledged");
    assert_eq!(
        registrations(&app, "unregistered"),
        1,
        "the service cleared the registration"
    );

    let repeat = session
        .ask(Opcode::PushUnregister, 63, &PushUnregister {})
        .await;
    assert!(
        repeat.ok,
        "withdrawing again is the idempotent no-op and is still acknowledged"
    );
}

/// The auth level is declared in the opcode registry and enforced by the gateway's
/// phase gate, before the dispatcher or the service ever see the frame: a session that
/// never authenticated is refused with an error frame and closed.
#[tokio::test]
async fn an_unauthenticated_push_register_is_refused_and_closes_the_session() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");

    let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
        .await
        .expect("connecting does not stall")
        .expect("the connection is accepted");
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        ..Default::default()
    };
    send(&mut stream, Opcode::Hello, 1, &hello).await;
    let welcome_frame = recv(&mut stream).await;
    assert!(
        !welcome_frame.header.is_error(),
        "an unauthenticated handshake is still a handshake"
    );
    let welcome: Welcome = from_frame(&welcome_frame).expect("the WELCOME decodes");
    assert_eq!(
        welcome.authenticated_user, None,
        "this session carries no account"
    );

    send(
        &mut stream,
        Opcode::PushRegister,
        2,
        &PushRegister {
            token: fcm_shaped_token(140),
            provider: 0,
        },
    )
    .await;
    let refusal = recv(&mut stream).await;
    assert!(
        refusal.header.is_error(),
        "a PUSH_REGISTER before authentication is refused, not dispatched"
    );
    let error: migo_protocol::Error = from_frame(&refusal).expect("the error frame decodes");
    assert_eq!(
        error.code,
        codes::UNEXPECTED_OPCODE,
        "the phase gate names the opcode, not a domain fault: {error:?}"
    );
    assert_eq!(
        registrations(&app, "registered") + registrations(&app, "rejected"),
        0,
        "neither the dispatcher nor the service was reached"
    );

    // A protocol violation closes the session: the next read is EOF, not another frame.
    let closed = tokio::time::timeout(STEP, stream.read_exact(&mut [0u8; 4])).await;
    match closed {
        Err(_) => panic!("the session stayed open after an auth-gate violation"),
        Ok(Err(_)) => {}
        Ok(Ok(n)) => panic!("the session sent {n} more bytes instead of closing"),
    }
}

/// The service's own validation, surfaced through the dispatch arm: an empty token and
/// an over-length one are refused with `VALIDATION_FAILED` naming `push_token`, and the
/// refusal is an error frame the client can act on — never silence, and never a panic.
#[tokio::test]
async fn malformed_tokens_are_refused_as_validation_faults() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let grant = registered_grant(&app, "pushforged").await;
    let mut session = LiveSession::connect(addr, &grant).await;

    // Over the service's limit but under the wire's MAX_STRING_BYTES, so the frame
    // decodes and the refusal comes from the domain, not the codec.
    let oversize = PushRegister {
        token: fcm_shaped_token(MAX_TOKEN_LEN + 1),
        provider: 0,
    };
    let frame = session
        .ask_for_frame(Opcode::PushRegister, 71, &oversize)
        .await;
    assert!(frame.header.is_error(), "an over-length token is refused");
    let refusal: migo_protocol::Error = from_frame(&frame).expect("the error frame decodes");
    assert_eq!(refusal.code, codes::VALIDATION_FAILED, "{refusal:?}");
    // The gateway's error projection carries the field inside the public message rather
    // than the wire's dedicated `field` column (codec.rs leaves that one empty), so the
    // assertion reads the message — the same way the media suite's ticket refusal does.
    assert!(
        refusal
            .message
            .as_deref()
            .is_some_and(|message| message.contains("push_token")),
        "the refusal is about the push token, not some other field: {refusal:?}"
    );

    // Blank — whitespace is empty to the service, not to a length check.
    let blank = PushRegister {
        token: "   ".to_string(),
        provider: 0,
    };
    let frame = session
        .ask_for_frame(Opcode::PushRegister, 72, &blank)
        .await;
    assert!(frame.header.is_error(), "a blank token is refused");
    let refusal: migo_protocol::Error = from_frame(&frame).expect("the error frame decodes");
    assert_eq!(refusal.code, codes::VALIDATION_FAILED, "{refusal:?}");
    assert!(
        refusal
            .message
            .as_deref()
            .is_some_and(|message| message.contains("push_token")),
        "the blank refusal is about the push token too: {refusal:?}"
    );

    assert_eq!(
        registrations(&app, "registered"),
        0,
        "no registration was written by either refusal"
    );
    assert_eq!(
        registrations(&app, "rejected"),
        2,
        "the service counted both refusals"
    );
}
