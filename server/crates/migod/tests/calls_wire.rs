//! The call lifecycle answered on the wire: the two deaths a ring can die
//! without the participant who caused it saying anything.
//!
//! The service tests prove `migo-calls` retires expired invites and reports
//! answers; these tests prove the *sockets* hear it, because a ring is a
//! promise made to a screen, not a row. Two failures live exactly in that gap:
//!
//! * **The unclaimed ring.** A caller whose browser died cannot cancel, and a
//!   callee left ringing has nothing to decline — so until the node ran a
//!   sweeper of its own, a dead caller's invite rang on the callee's device for
//!   as long as the client's own patience lasted, and the call row answered
//!   "ringing" forever. `App::spawn_call_sweeper` is the fix, and the first
//!   test drives it against the real TCP transport: nobody answers, nobody
//!   cancels, and *both* sessions still receive `Ended(NoAnswer)`.
//! * **The ring answered elsewhere.** The answer used to be published to the
//!   caller alone, so a callee with two devices — phone and laptop, both
//!   rang — kept one of them ringing after the other answered. The second test
//!   answers on one device and asserts the sibling hears `Connecting`, the
//!   event a client treats as "answered elsewhere, stand down".
//!
//! Both tests use the reply rule as their clock: every frame they wait for is
//! one the server owes somebody, so the timeout is the assertion.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext, SignIn};
use migo_calls::{CallState, EndReason};
use migo_core::{Clock, Config, Id, Secret, Timestamp};
use migo_protocol::{
    from_frame, to_frame, CallAnswer, CallInvite, CallInviteEvent, CallInviteResult,
    CallStateEvent, Encode, Frame, Hello, Opcode, Platform, RoomJoinRequest, RoomKind,
    SubscribeRequest, SubscribeResponse, Topic, TopicKind, Welcome, PROTOCOL_VERSION,
};
use migo_ratelimit::TrustTier;
use migo_rooms::{Caller as RoomCaller, NewRoomRequest};
use migo_social::Caller as SocialCaller;
use migod::App;

/// How long any single exchange may take before the test declares the server
/// stuck — silence being the bug class both tests exist to catch.
const STEP: Duration = Duration::from_secs(5);

/// How long the ring may take to die on its own: the test's `RING_TTL_MS` is
/// the configured floor of five seconds, plus one sweeper tick, plus the margin
/// a slow CI runner is owed. The assertion is not *when* the ring died but that
/// it died without either participant sending anything.
const RING_DIES: Duration = Duration::from_secs(20);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development app with the ring cut to the configuration's floor — five
/// seconds, the shortest `MIGO_CALLS__RING_TTL_MS` the validator accepts — so
/// the first test does not wait out the production thirty.
async fn build_app() -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
            ("MIGO_CALLS__RING_TTL_MS".to_string(), "5000".to_string()),
        ],
    )
    .expect("configuration should parse");
    App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends")
}

/// Registers one account through the front door, stamped with the node's own
/// clock so the inline token is not born expired.
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
                device: DeviceClaim::new(Platform::Web, "call wire test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// Signs an existing account in on a second device — the laptop to the phone —
/// because the answered-elsewhere fan-out is only observable with two devices
/// of one account both subscribed to the same user topic.
async fn second_device_grant(app: &App, username: &str) -> Grant {
    app.auth
        .sign_in(
            SignIn {
                identifier: username.to_string(),
                passphrase: Secret::new("correct-horse-battery-staple"),
                device: DeviceClaim::new(Platform::Web, "the other device"),
                captcha: None,
                server: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a fresh second sign-in needs no captcha and succeeds")
}

/// Sends one request frame as a length-prefixed record.
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

/// Reads one length-prefixed frame, allowing `limit` for it to arrive.
async fn recv_within(stream: &mut tokio::net::TcpStream, limit: Duration) -> Frame {
    let body = tokio::time::timeout(limit, async {
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
    .expect("the frame does not stall — silence here is the bug these tests exist to catch");
    Frame::decode(Bytes::from(body)).expect("the frame decodes")
}

/// A live authenticated TCP session: HELLO with the grant's inline token,
/// answered by a WELCOME that names the account.
struct LiveSession {
    stream: tokio::net::TcpStream,
}

impl LiveSession {
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        // A third handshake from one IP inside the limiter's window is refused
        // with a retry-after — the server talking, not the server broken — so
        // the session waits exactly as long as it is told and tries again, the
        // way a client with manners does.
        let mut backoff = 0u64;
        for _ in 0..5 {
            if backoff > 0 {
                tokio::time::sleep(Duration::from_millis(backoff + 100)).await;
            }
            match Self::handshake(addr, grant).await {
                Ok(session) => return session,
                Err(retry_after_ms) => backoff = retry_after_ms,
            }
        }
        panic!("the handshake never succeeds even after backing off as instructed");
    }

    /// One full connection attempt, returning the retry-after the server asked
    /// for when it refuses the handshake as rate-limited.
    async fn handshake(addr: SocketAddr, grant: &Grant) -> Result<Self, u64> {
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

        let welcome_frame = recv_within(&mut stream, STEP).await;
        if Opcode::from_wire(welcome_frame.header.opcode) == Some(Opcode::Hello)
            && !welcome_frame.header.is_error()
        {
            let welcome: Welcome = from_frame(&welcome_frame).expect("the WELCOME decodes");
            assert_eq!(
                welcome.authenticated_user,
                Some(grant.account_id),
                "the inline token must promote the session"
            );

            // The subscription gate: a session hears nothing on any topic — not
            // even its own — until it asks, exactly as the real client does
            // right after its handshake.
            send(
                &mut stream,
                Opcode::Subscribe,
                2,
                &SubscribeRequest {
                    topics: vec![Topic {
                        kind: TopicKind::User,
                        id: grant.account_id,
                    }],
                },
            )
            .await;
            let subscribed = Self::reply_to_correlation(&mut stream, 2).await;
            assert!(
                !subscribed.header.is_error(),
                "the self-subscription is accepted: {:?}",
                from_frame::<migo_protocol::Error>(&subscribed)
            );
            let confirmation: SubscribeResponse =
                from_frame(&subscribed).expect("the SUBSCRIBE reply decodes");
            assert!(
                confirmation.accepted.len() == 1 && confirmation.rejected.is_none(),
                "the own user topic is accepted"
            );
            return Ok(Self { stream });
        }

        // Any other answer is the server refusing the session; only a
        // rate-limit refusal is worth retrying, because only it names a time
        // at which the answer changes.
        let refusal: migo_protocol::Error =
            from_frame(&welcome_frame).expect("the refusal decodes");
        assert!(
            refusal.code == migo_protocol::codes::RATE_LIMITED,
            "the handshake is refused outright: {refusal:?}"
        );
        Err(u64::from(refusal.retry_after_ms.unwrap_or(1000)))
    }

    /// Reads frames until one carries the correlation, skipping server-originated
    /// events (which are always correlated 0).
    async fn reply_to_correlation(stream: &mut tokio::net::TcpStream, correlation: u32) -> Frame {
        loop {
            let frame = recv_within(stream, STEP).await;
            if frame.header.correlation == correlation {
                return frame;
            }
        }
    }

    /// Sends a request and returns the frame carrying its correlation —
    /// skipping whatever server-originated events landed first, which a
    /// two-session call flow interleaves freely.
    async fn ask_for_frame<M: Encode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        message: &M,
    ) -> Frame {
        send(&mut self.stream, opcode, correlation, message).await;
        loop {
            let frame = recv_within(&mut self.stream, STEP).await;
            if frame.header.correlation == correlation {
                return frame;
            }
        }
    }

    /// Sends a request that must succeed, returning the decoded reply.
    async fn ask<M: Encode, R: migo_protocol::Decode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        message: &M,
    ) -> R {
        let frame = self.ask_for_frame(opcode, correlation, message).await;
        assert!(
            !frame.header.is_error(),
            "the request was refused: {:?}",
            from_frame::<migo_protocol::Error>(&frame)
        );
        from_frame(&frame).expect("the reply decodes")
    }
}

/// Reads from a session until a frame of the wanted opcode arrives, skipping
/// the unrelated events a subscribed session receives — the price of listening
/// on a user topic is hearing everything the topic says.
async fn next_event_of(stream: &mut tokio::net::TcpStream, want: Opcode, limit: Duration) -> Frame {
    loop {
        let frame = recv_within(stream, limit).await;
        if Opcode::from_wire(frame.header.opcode) == Some(want) {
            return frame;
        }
    }
}

/// One call's preconditions: a friendship (the default call policy is
/// friends-only) and a room both accounts are members of, returning the
/// conversation the invite will name.
async fn a_room_between(app: &App, caller: &Grant, callee: &Grant, slug: &str) -> Id {
    let now = Timestamp::from_millis(1);
    let as_caller = SocialCaller::new(
        caller.account_id,
        caller.device_id,
        TrustTier::Established,
        now,
    );
    let as_callee = SocialCaller::new(
        callee.account_id,
        callee.device_id,
        TrustTier::Established,
        now,
    );
    app.social
        .request_friend(&as_caller, callee.account_id)
        .await
        .expect("the friend request is sent");
    app.social
        .respond_friend(&as_callee, caller.account_id, true)
        .await
        .expect("the friend request is accepted");

    let as_owner = RoomCaller::new(
        caller.account_id,
        caller.device_id,
        TrustTier::Established,
        now,
    );
    let as_member = RoomCaller::new(
        callee.account_id,
        callee.device_id,
        TrustTier::Established,
        now,
    );
    let room = app
        .rooms
        .create(
            &as_owner,
            NewRoomRequest {
                slug: slug.to_string(),
                name: "The Ring Room".to_string(),
                topic: None,
                kind: RoomKind::Public,
                max_members: None,
            },
        )
        .await
        .expect("the caller founds the room");
    let (joined, _) = app
        .rooms
        .join(
            &as_member,
            RoomJoinRequest {
                room_id: room.room_id,
                invite_code: None,
            },
        )
        .await
        .expect("the callee joins the room");
    joined.conversation_id
}

#[tokio::test]
async fn an_unanswered_ring_dies_on_its_own_and_both_parties_hear_it() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let caller = registered_grant(&app, "ringcaller").await;
    let callee = registered_grant(&app, "ringcallee").await;
    let conversation_id = a_room_between(&app, &caller, &callee, "ring-room").await;

    // The undertaker: in production `App::serve` spawns it; the test starts it
    // by hand because the app itself never serves.
    let _sweeper = app.spawn_call_sweeper();

    let mut caller_session = LiveSession::connect(addr, &caller).await;
    let mut callee_session = LiveSession::connect(addr, &callee).await;

    // The ring starts.
    let call_id = Id::from_bytes([0xC0; 16]);
    let result: CallInviteResult = caller_session
        .ask(
            Opcode::CallInvite,
            81,
            &CallInvite {
                call_id,
                conversation_id,
                callee_id: callee.account_id,
                media_kind: 0,
                caller_device: caller.device_id,
                capabilities: 0,
                sealed_offer: vec![0x11; 48],
            },
        )
        .await;
    assert_eq!(result.status, 0, "the invite is accepted as a ring");
    assert_eq!(result.call_id, call_id);

    // The callee's device hears the ring.
    let invite_frame =
        next_event_of(&mut callee_session.stream, Opcode::CallInviteEvent, STEP).await;
    let invite: CallInviteEvent = from_frame(&invite_frame).expect("the invite event decodes");
    assert_eq!(invite.call_id, call_id);
    assert_eq!(invite.caller_id, caller.account_id);

    // And now the whole test: nobody answers, nobody cancels, nobody sends
    // anything at all. The caller could even be dead — the sweeper does not
    // care, and that is the point.
    let died = next_event_of(
        &mut callee_session.stream,
        Opcode::CallStateEvent,
        RING_DIES,
    )
    .await;
    let state: CallStateEvent = from_frame(&died).expect("the death decodes");
    assert_eq!(state.call_id, call_id, "the death names the ring that died");
    assert_eq!(state.state, CallState::Ended.to_wire());
    assert_eq!(
        state.reason,
        Some(EndReason::NoAnswer.to_wire()),
        "a ring nobody answered ends as a no-answer"
    );

    // The caller hears the same death — the sweeper tells both parties, not
    // just the one still listening.
    let theirs = next_event_of(
        &mut caller_session.stream,
        Opcode::CallStateEvent,
        RING_DIES,
    )
    .await;
    let state: CallStateEvent = from_frame(&theirs).expect("the caller's death decodes");
    assert_eq!(state.call_id, call_id);
    assert_eq!(state.state, CallState::Ended.to_wire());
    assert_eq!(state.reason, Some(EndReason::NoAnswer.to_wire()));
}

#[tokio::test]
async fn a_call_answered_on_one_device_stops_the_ring_on_the_other() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let caller = registered_grant(&app, "answercaller").await;
    let callee = registered_grant(&app, "answercallee").await;
    let conversation_id = a_room_between(&app, &caller, &callee, "answer-room").await;

    let laptop = second_device_grant(&app, "answercallee").await;

    let mut caller_session = LiveSession::connect(addr, &caller).await;
    let mut phone = LiveSession::connect(addr, &callee).await;
    let mut laptop_session = LiveSession::connect(addr, &laptop).await;

    // The ring reaches both of the callee's devices.
    let call_id = Id::from_bytes([0xA1; 16]);
    let result: CallInviteResult = caller_session
        .ask(
            Opcode::CallInvite,
            91,
            &CallInvite {
                call_id,
                conversation_id,
                callee_id: callee.account_id,
                media_kind: 0,
                caller_device: caller.device_id,
                capabilities: 0,
                sealed_offer: vec![0x22; 48],
            },
        )
        .await;
    assert_eq!(result.status, 0, "the invite is accepted as a ring");
    let invite_frame = next_event_of(&mut phone.stream, Opcode::CallInviteEvent, STEP).await;
    let invite: CallInviteEvent = from_frame(&invite_frame).expect("the phone's invite decodes");
    assert_eq!(invite.call_id, call_id);
    let invite_frame =
        next_event_of(&mut laptop_session.stream, Opcode::CallInviteEvent, STEP).await;
    let invite: CallInviteEvent = from_frame(&invite_frame).expect("the laptop's invite decodes");
    assert_eq!(invite.call_id, call_id);

    // The laptop answers. The phone does nothing — that is the scenario: the
    // device still ringing is not the device that answered.
    let _: migo_protocol::Acknowledged = laptop_session
        .ask(
            Opcode::CallAnswer,
            92,
            &CallAnswer {
                call_id,
                callee_device: laptop.device_id,
                sealed_answer: vec![0x33; 48],
            },
        )
        .await;

    // The caller's screen leaves "ringing" for "connecting" — the behaviour
    // this path always had.
    let connecting = next_event_of(&mut caller_session.stream, Opcode::CallStateEvent, STEP).await;
    let state: CallStateEvent = from_frame(&connecting).expect("the caller's event decodes");
    assert_eq!(state.call_id, call_id);
    assert_eq!(state.state, CallState::Connecting.to_wire());

    // And the phone — the sibling nobody told before — hears the same
    // `Connecting`, which is the event a client reads as "answered elsewhere,
    // stand down".
    let connecting = next_event_of(&mut phone.stream, Opcode::CallStateEvent, STEP).await;
    let state: CallStateEvent = from_frame(&connecting).expect("the phone's event decodes");
    assert_eq!(state.call_id, call_id);
    assert_eq!(
        state.state,
        CallState::Connecting.to_wire(),
        "the still-ringing sibling must hear the answer"
    );
}
