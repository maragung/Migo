//! Every opcode the SDK awaits must be answered on the wire.
//!
//! The service tests prove the domain crates correct, but they call `rooms.leave` and
//! `presence.set` directly — the dispatch arm that sits between the wire and the service is the
//! one layer no service test exercises. That is how two arms could ship without a reply: the
//! handler ran the service, published the fan-out, and returned `Ok(())`, so every assertion on
//! the service's outcome passed while the client's request timer ran out its thirty seconds.
//! The bug is invisible from inside the process; only the caller on the socket can see it.
//!
//! So these tests are the caller on the socket. They build the whole node exactly the way
//! `App::build` builds it in production — real gateway, real dispatcher, real auth and rooms
//! against the in-memory store — and bind the native TCP listener, because that transport
//! hands frames to the gateway with no test double anywhere in the path. A real account
//! registers through the front door, a real room is created and joined through the services,
//! and then the session sends `PRESENCE_SET` and `ROOM_LEAVE` as frames and asserts the reply
//! record arrives: right correlation, not an error, and an `Acknowledged` that says `ok`.
//!
//! The two room leaves are deliberately both here. The first is a member leaving — the path
//! with a fan-out to publish, where a handler that replies only inside `if let Some(fanout)`
//! still looks correct. The second is the same leave repeated: the service's idempotent
//! no-op, which returns `Ok(None)` and publishes nothing at all. That is the path that proves
//! the reply is unconditional rather than a passenger on the fan-out.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Config, Secret, Timestamp};
use migo_protocol::{
    from_frame, to_frame, Acknowledged, Encode, Frame, Hello, Opcode, Platform, PresenceState,
    PresenceUpdate, RoomJoinRequest, RoomKind, RoomLeaveRequest, Welcome, PROTOCOL_VERSION,
};
use migo_ratelimit::TrustTier;
use migo_rooms::{Caller as RoomCaller, NewRoomRequest};
use migod::App;

/// How long any single client-side step may take before the test declares the server stuck.
/// The bug these tests guard against is silence — a reply that never comes — so the timeout is
/// the assertion's clock: thirty seconds of nothing is exactly the failure mode, caught here
/// in five.
const STEP: Duration = Duration::from_secs(5);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development app with the TCP listener bound, exactly as the listener tests build it: the
/// in-memory backends, the real services over them, and the one environment pair that turns
/// the native transport on.
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

/// Registers one account through the front door and returns the whole grant: the account and
/// device ids go into the room calls, the access token rides in the HELLO. The registration is
/// stamped with the node's own clock, because the token's expiry is measured from that stamp —
/// a fabricated past makes a token that is born expired, and the handshake below would
/// legitimately refuse it.
async fn registered_grant(app: &App, username: &str) -> Grant {
    app.auth
        .register(
            Registration {
                username: username.to_string(),
                email: None,
                phone: None,
                password: Secret::new("correct-horse-battery-staple"),
                locale: "en-US".to_string(),
                country: None,
                gender: None,
                device: DeviceClaim::new(Platform::Web, "reply test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// Sends one request frame: the message encoded under its opcode and correlation, as a
/// length-prefixed record on the stream.
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

/// Reads one length-prefixed reply record from the session.
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

/// A live TCP session with the node: one HELLO carrying the grant's token inline, answered by
/// a WELCOME that names the account. Everything the tests send afterwards travels on this one
/// authenticated session, the way a real client's does.
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

    /// Sends one request frame and returns the `Acknowledged` the server answered it with:
    /// same correlation, not an error. The reply's arrival is the whole assertion — the bug
    /// these tests guard against never sent one — so a stall here fails as a timeout, which is
    /// the client's own experience of the bug made fast.
    async fn ask<M: Encode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        message: &M,
    ) -> Acknowledged {
        send(&mut self.stream, opcode, correlation, message).await;
        let frame = recv(&mut self.stream).await;
        assert_eq!(
            frame.header.correlation, correlation,
            "the reply must echo the request's correlation (section 139)"
        );
        if frame.header.is_error() {
            let refusal: migo_protocol::Error = from_frame(&frame).expect("an error frame decodes");
            panic!("the request was refused: {refusal:?}");
        }
        from_frame(&frame).expect("the reply decodes as an Acknowledged")
    }
}

#[tokio::test]
async fn a_presence_set_and_both_room_leaves_are_answered_on_the_wire() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");

    // The owner founds the room; the member — whose session the test drives — joins it. The
    // owner must not be the one to leave, because an owner's leave is a CONFLICT by design and
    // would answer with an error frame, which is a reply, but not the one under test.
    let owner = registered_grant(&app, "replyowner").await;
    let member = registered_grant(&app, "replymember").await;
    let caller = |grant: &Grant| {
        RoomCaller::new(
            grant.account_id,
            grant.device_id,
            TrustTier::Established,
            Timestamp::from_millis(1),
        )
    };
    let room = app
        .rooms
        .create(
            &caller(&owner),
            NewRoomRequest {
                slug: "reply-room".to_string(),
                name: "The Reply Room".to_string(),
                topic: None,
                kind: RoomKind::Public,
                max_members: None,
            },
        )
        .await
        .expect("the owner founds the room");
    app.rooms
        .join(
            &caller(&member),
            RoomJoinRequest {
                room_id: room.room_id,
                invite_code: None,
            },
        )
        .await
        .expect("the member joins the room");

    let mut session = LiveSession::connect(addr, &member).await;

    // Presence: a state change the profile panel awaits before it tells the person their
    // status was saved. The fan-out excludes the caller, so the only record this session can
    // receive back is the reply itself. (No custom status here: the presence crate refuses
    // that field with FEATURE_DISABLED by design — its home is a profile column — and the
    // refusal path already had a reply; it is the accepted path that did not.)
    let presence = session
        .ask(
            Opcode::PresenceSet,
            41,
            &PresenceUpdate {
                state: PresenceState::Online,
                custom_status: None,
            },
        )
        .await;
    assert!(presence.ok, "the presence acknowledgement says ok");

    // A member leaves: the path with a fan-out to publish, where a reply gated on the fan-out
    // would still look correct.
    let leave = session
        .ask(
            Opcode::RoomLeave,
            42,
            &RoomLeaveRequest {
                room_id: room.room_id,
            },
        )
        .await;
    assert!(leave.ok, "the member leave acknowledgement says ok");

    // The same leave again: the service's idempotent no-op, which publishes nothing. This is
    // the path that proves the reply is not a passenger on the fan-out — a handler that
    // replied only inside `if let Some(fanout)` passes the first leave and fails here.
    let repeat = session
        .ask(
            Opcode::RoomLeave,
            43,
            &RoomLeaveRequest {
                room_id: room.room_id,
            },
        )
        .await;
    assert!(
        repeat.ok,
        "the idempotent no-op leave is still acknowledged"
    );
}
