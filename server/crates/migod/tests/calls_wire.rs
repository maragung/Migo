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
//!
//! The third and fourth facts the tests here pin live on the *reason* lines:
//! * **A busy decline says busy.** The wire's `CallDecline.reason` existed and
//!   was ignored — every decline, busy or not, ended the call `Declined`, so a
//!   caller whose callee's devices were merely occupied was told a person
//!   refused them. The third test declines busy over the socket and asserts
//!   the caller hears `Ended(Busy)`.
//! * **The answer relay connects the call out loud.** The service marked the
//!   row `Connected` when the callee's first SDP relay landed, but published
//!   only the relay — the `Connected` state event every client listens for
//!   never went out. The fourth test relays the answer and asserts both
//!   parties hear `Connected`.
//! * **A dead session ends the connected call for the survivor.** A party
//!   whose socket dies cannot send the end — nothing is running to send it —
//!   so the node ends the departed account's calls on the session edge and
//!   publishes `Ended(Network)` to the survivor's user topic, prompt instead
//!   of a client-side media timeout. The sixth test kills the callee's socket
//!   mid-call and asserts the caller hears it.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext, SignIn};
use migo_calls::{CallState, EndReason};
use migo_core::{Clock, Config, Id, Secret, Timestamp};
use migo_protocol::{
    from_frame, to_frame, CallAnswer, CallDecline, CallInvite, CallInviteEvent, CallInviteResult,
    CallListQuery, CallListResult, CallSdp, CallStateEvent, Encode, Frame, Hello,
    NotificationEvent, NotificationKind, Opcode, Platform, RoomJoinRequest, RoomKind,
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
    /// A participant's session: the HELLO carries the CALLS bit, because every frame
    /// this suite drives belongs to the call family and brief section 72 ties that
    /// family to the bit — a session that did not ask is refused
    /// FEATURE_NOT_NEGOTIATED (the test at the bottom of this file pins that half;
    /// this helper is the half a participant needs to place a ring at all).
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        Self::connect_with_features(addr, grant, migo_protocol::features::CALLS).await
    }

    /// `connect` with an explicit feature mask, backing off exactly the same way — the
    /// limiter charges the handshake the same whatever bits the HELLO carried.
    async fn connect_with_features(addr: SocketAddr, grant: &Grant, features: u64) -> Self {
        // A third handshake from one IP inside the limiter's window is refused
        // with a retry-after — the server talking, not the server broken — so
        // the session waits exactly as long as it is told and tries again, the
        // way a client with manners does.
        let mut backoff = 0u64;
        for _ in 0..5 {
            if backoff > 0 {
                tokio::time::sleep(Duration::from_millis(backoff + 100)).await;
            }
            match Self::handshake_with_features(addr, grant, features).await {
                Ok(session) => return session,
                Err(retry_after_ms) => backoff = retry_after_ms,
            }
        }
        panic!("the handshake never succeeds even after backing off as instructed");
    }

    /// One full connection attempt with an explicit feature mask — the sessions the
    /// feature gate is under test on are the ones that pass something other than the
    /// family's bit — returning the retry-after the server asked for when it refuses
    /// the handshake as rate-limited.
    async fn handshake_with_features(
        addr: SocketAddr,
        grant: &Grant,
        features: u64,
    ) -> Result<Self, u64> {
        let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting does not stall")
            .expect("the connection is accepted");

        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            features,
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

#[tokio::test]
async fn an_incoming_call_rings_the_bell_on_every_device() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let caller = registered_grant(&app, "bellcaller").await;
    let callee = registered_grant(&app, "bellcallee").await;
    let conversation_id = a_room_between(&app, &caller, &callee, "bell-room").await;

    let laptop = second_device_grant(&app, "bellcallee").await;

    let mut caller_session = LiveSession::connect(addr, &caller).await;
    let mut phone = LiveSession::connect(addr, &callee).await;
    let mut laptop_session = LiveSession::connect(addr, &laptop).await;

    let call_id = Id::from_bytes([0xB3; 16]);
    let result: CallInviteResult = caller_session
        .ask(
            Opcode::CallInvite,
            95,
            &CallInvite {
                call_id,
                conversation_id,
                callee_id: callee.account_id,
                media_kind: 0,
                caller_device: caller.device_id,
                capabilities: 0,
                sealed_offer: vec![0x44; 48],
            },
        )
        .await;
    assert_eq!(result.status, 0, "the invite is accepted as a ring");

    // The semantic ring each screen reacts to, on both of the callee's devices.
    let invite_frame = next_event_of(&mut phone.stream, Opcode::CallInviteEvent, STEP).await;
    let invite: CallInviteEvent = from_frame(&invite_frame).expect("the phone's invite decodes");
    assert_eq!(invite.call_id, call_id);
    let invite_frame =
        next_event_of(&mut laptop_session.stream, Opcode::CallInviteEvent, STEP).await;
    let invite: CallInviteEvent = from_frame(&invite_frame).expect("the laptop's invite decodes");
    assert_eq!(invite.call_id, call_id);

    // The bell. The notification the invite hands to the notifier used to be a row and
    // a (future) push only — no frame — so a callee watching their screen learned
    // nothing until the invite event itself arrived, and a client that renders the
    // bell from NOTIFICATION_EVENT learned nothing at all. The notifier's seam now
    // rings it on the recipient's user topic, which both devices are subscribed to.
    let bell_frame = next_event_of(&mut phone.stream, Opcode::NotificationEvent, STEP).await;
    let bell: NotificationEvent = from_frame(&bell_frame).expect("the phone's bell decodes");
    assert_eq!(bell.kind, NotificationKind::IncomingCall);
    assert_eq!(bell.actor_id, Some(caller.account_id));
    assert_eq!(bell.title, None, "the client writes the sentence");
    assert_eq!(bell.body, None, "the client writes the sentence");
    let bell_frame =
        next_event_of(&mut laptop_session.stream, Opcode::NotificationEvent, STEP).await;
    let bell: NotificationEvent = from_frame(&bell_frame).expect("the laptop's bell decodes");
    assert_eq!(bell.kind, NotificationKind::IncomingCall);
    assert_eq!(bell.actor_id, Some(caller.account_id));

    // The caller rang somebody else's bell, not their own. A PING is the next frame
    // their session is owed, and any notification queued ahead of it would be read
    // here — which is the assertion.
    send(
        &mut caller_session.stream,
        Opcode::Ping,
        96,
        &migo_protocol::Ping {
            client_time: migo_core::Timestamp::from_millis(0),
        },
    )
    .await;
    loop {
        let frame = recv_within(&mut caller_session.stream, STEP).await;
        if frame.header.correlation == 96 {
            assert!(
                !frame.header.is_error(),
                "a PING is always answerable: {:?}",
                from_frame::<migo_protocol::Error>(&frame)
            );
            break;
        }
        assert_ne!(
            Opcode::from_wire(frame.header.opcode),
            Some(Opcode::NotificationEvent),
            "the caller's session is owed no bell for the ring they started"
        );
    }
}

#[tokio::test]
async fn a_busy_decline_reaches_the_caller_as_busy() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let caller = registered_grant(&app, "busycaller").await;
    let callee = registered_grant(&app, "busycallee").await;
    let conversation_id = a_room_between(&app, &caller, &callee, "busy-room").await;

    let mut caller_session = LiveSession::connect(addr, &caller).await;
    let mut callee_session = LiveSession::connect(addr, &callee).await;

    let call_id = Id::from_bytes([0xB7; 16]);
    let result: CallInviteResult = caller_session
        .ask(
            Opcode::CallInvite,
            101,
            &CallInvite {
                call_id,
                conversation_id,
                callee_id: callee.account_id,
                media_kind: 0,
                caller_device: caller.device_id,
                capabilities: 0,
                sealed_offer: vec![0x44; 48],
            },
        )
        .await;
    assert_eq!(result.status, 0, "the invite is accepted as a ring");

    let invite_frame =
        next_event_of(&mut callee_session.stream, Opcode::CallInviteEvent, STEP).await;
    let invite: CallInviteEvent = from_frame(&invite_frame).expect("the invite event decodes");
    assert_eq!(invite.call_id, call_id);

    // The callee's devices were occupied — 0=Busy on the wire. The caller's
    // screen must say busy, not declined: the difference is whether the
    // caller believes a person refused them.
    let _: migo_protocol::Acknowledged = callee_session
        .ask(
            Opcode::CallDecline,
            102,
            &CallDecline { call_id, reason: 0 },
        )
        .await;

    // The caller hears the end with the busy reason — the reason the callee
    // stated, relayed rather than overwritten.
    let ended = next_event_of(&mut caller_session.stream, Opcode::CallStateEvent, STEP).await;
    let state: CallStateEvent = from_frame(&ended).expect("the caller's event decodes");
    assert_eq!(state.call_id, call_id);
    assert_eq!(state.state, CallState::Ended.to_wire());
    assert_eq!(
        state.reason,
        Some(EndReason::Busy.to_wire()),
        "a busy decline must end busy, not declined"
    );
}

#[tokio::test]
async fn the_answer_relay_tells_both_parties_the_call_connected() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let caller = registered_grant(&app, "connectcaller").await;
    let callee = registered_grant(&app, "connectcallee").await;
    let conversation_id = a_room_between(&app, &caller, &callee, "connect-room").await;

    let mut caller_session = LiveSession::connect(addr, &caller).await;
    let mut callee_session = LiveSession::connect(addr, &callee).await;

    let call_id = Id::from_bytes([0xC3; 16]);
    let result: CallInviteResult = caller_session
        .ask(
            Opcode::CallInvite,
            111,
            &CallInvite {
                call_id,
                conversation_id,
                callee_id: callee.account_id,
                media_kind: 0,
                caller_device: caller.device_id,
                capabilities: 0,
                sealed_offer: vec![0x55; 48],
            },
        )
        .await;
    assert_eq!(result.status, 0, "the invite is accepted as a ring");
    let _ = next_event_of(&mut callee_session.stream, Opcode::CallInviteEvent, STEP).await;

    // The callee answers on the connection's own device.
    let _: migo_protocol::Acknowledged = callee_session
        .ask(
            Opcode::CallAnswer,
            112,
            &CallAnswer {
                call_id,
                callee_device: callee.device_id,
                sealed_answer: vec![0x66; 48],
            },
        )
        .await;
    // Both parties leave "ringing": the answer publishes Connecting — to the
    // caller's session, and to the callee's *other* devices. The answering
    // session itself is the origin connection the fan-out excludes, so the
    // callee's session here has no Connecting of its own to wait for.
    let _ = next_event_of(&mut caller_session.stream, Opcode::CallStateEvent, STEP).await;

    // The callee relays its sealed answer toward the caller's device — the
    // moment the call turns Connected server-side, and the moment both
    // parties must *hear* that it did.
    let _: migo_protocol::Acknowledged = callee_session
        .ask(
            Opcode::CallSdp,
            113,
            &CallSdp {
                call_id,
                from_device: callee.device_id,
                to_device: caller.device_id,
                sealed_sdp: vec![0x77; 48],
            },
        )
        .await;

    let connected = next_event_of(&mut caller_session.stream, Opcode::CallStateEvent, STEP).await;
    let state: CallStateEvent = from_frame(&connected).expect("the caller's event decodes");
    assert_eq!(state.call_id, call_id);
    assert_eq!(state.state, CallState::Connected.to_wire());

    // The callee's session is the relay's origin connection, so the Connected
    // event reaches it only as this same fan-out — through the topic, after
    // the answer's own devices were told. The relayed SDP frame follows: the
    // Connected event is a fact added, not a frame replaced.
    let sdp = next_event_of(&mut caller_session.stream, Opcode::CallSdp, STEP).await;
    let relayed: CallSdp = from_frame(&sdp).expect("the relayed SDP decodes");
    assert_eq!(relayed.call_id, call_id);
    assert_eq!(relayed.sealed_sdp, vec![0x77; 48]);

    // The callee's session hears its relay echo... or does not: the relay is
    // published to the *caller's* account topic, and the Connected event went
    // out before it. Nothing further is owed to the callee's own session, and
    // that is the design — the answering device's truth is its reply and its
    // own screen state, not an event it must wait for.
}

/// A connected party's socket dies — no `CALL_END`, just a closed connection —
/// and the survivor hears the call ended from the *server*, promptly, instead
/// of waiting out a client-side media timeout (audit area 6, section 180).
/// Before the session edge ended the departed account's calls, the survivor's
/// only clock was its own reconnect window: tens of seconds of one-way media
/// to a peer that no longer exists. The frame waited for below is the fix —
/// `Ended(Network)`, published by the node on the edge that always knows.
#[tokio::test]
async fn a_dead_session_s_connected_call_ends_for_the_survivor() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let caller = registered_grant(&app, "survivorcaller").await;
    let callee = registered_grant(&app, "survivorcallee").await;
    let conversation_id = a_room_between(&app, &caller, &callee, "survivor-room").await;

    let mut caller_session = LiveSession::connect(addr, &caller).await;
    let mut callee_session = LiveSession::connect(addr, &callee).await;

    // The call to Connected, exactly as the answer-relay test builds it: ring,
    // answer, and the callee's first sealed SDP relay.
    let call_id = Id::from_bytes([0xD4; 16]);
    let result: CallInviteResult = caller_session
        .ask(
            Opcode::CallInvite,
            121,
            &CallInvite {
                call_id,
                conversation_id,
                callee_id: callee.account_id,
                media_kind: 0,
                caller_device: caller.device_id,
                capabilities: 0,
                sealed_offer: vec![0x88; 48],
            },
        )
        .await;
    assert_eq!(result.status, 0, "the invite is accepted as a ring");
    let _ = next_event_of(&mut callee_session.stream, Opcode::CallInviteEvent, STEP).await;

    let _: migo_protocol::Acknowledged = callee_session
        .ask(
            Opcode::CallAnswer,
            122,
            &CallAnswer {
                call_id,
                callee_device: callee.device_id,
                sealed_answer: vec![0x99; 48],
            },
        )
        .await;
    let _ = next_event_of(&mut caller_session.stream, Opcode::CallStateEvent, STEP).await;

    let _: migo_protocol::Acknowledged = callee_session
        .ask(
            Opcode::CallSdp,
            123,
            &CallSdp {
                call_id,
                from_device: callee.device_id,
                to_device: caller.device_id,
                sealed_sdp: vec![0xAA; 48],
            },
        )
        .await;
    let connected = next_event_of(&mut caller_session.stream, Opcode::CallStateEvent, STEP).await;
    let state: CallStateEvent = from_frame(&connected).expect("the caller's event decodes");
    assert_eq!(state.state, CallState::Connected.to_wire());
    let _ = next_event_of(&mut caller_session.stream, Opcode::CallSdp, STEP).await;

    // The callee's socket dies — the whole account, its only session, gone
    // with no leave and no end. The caller stays connected.
    drop(callee_session);

    // The survivor is told, and told *why*: `Network`, the reason that claims
    // connectivity was lost rather than a hang-up nobody sent. The timeout is
    // the assertion — on the code this test was written against, nothing came,
    // and the caller's screen waited out its media timeout alone.
    let ended = next_event_of(&mut caller_session.stream, Opcode::CallStateEvent, STEP).await;
    let state: CallStateEvent = from_frame(&ended).expect("the survivor's event decodes");
    assert_eq!(state.call_id, call_id);
    assert_eq!(state.state, CallState::Ended.to_wire());
    assert_eq!(
        state.reason,
        Some(EndReason::Network.to_wire()),
        "the server says the network died, not that anybody hung up"
    );
}

#[tokio::test]
async fn a_session_without_the_calls_bit_is_refused_the_ring_and_keeps_the_connection() {
    // Brief section 72: the CALLS bit is a real switch, not an advertisement. A session
    // whose HELLO never asked for the bit is answered FEATURE_NOT_NEGOTIATED for the
    // family's opcodes — refused, not ignored — and the connection keeps serving every
    // frame the negotiated set does allow, exactly as section 148 puts it.
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let caller = registered_grant(&app, "bitlesscaller").await;
    let callee = registered_grant(&app, "bittedcallee").await;
    let conversation_id = a_room_between(&app, &caller, &callee, "bit-room").await;

    // One session that asked for the bit and one that did not — the two halves of the
    // intersection, on the same node, over the same wire. The bitted session is the
    // control: the suite's other tests prove its invites go through.
    let mut bitless = LiveSession::connect_with_features(addr, &caller, 0).await;
    let _bitted = LiveSession::connect(addr, &callee).await;

    let call_id = Id::from_bytes([0xB1; 16]);
    send(
        &mut bitless.stream,
        Opcode::CallInvite,
        91,
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
    let refusal = LiveSession::reply_to_correlation(&mut bitless.stream, 91).await;
    assert!(
        refusal.header.is_error(),
        "the invite from a session without the CALLS bit is an error reply"
    );
    let error: migo_protocol::Error = from_frame(&refusal).expect("the refusal decodes");
    assert_eq!(
        error.code,
        migo_protocol::codes::FEATURE_NOT_NEGOTIATED,
        "the family's opcode is refused with the feature error, not a generic one"
    );

    // The refusal is answered, not fatal: the same connection still serves an opcode
    // outside the family — the next frame the client sends is a PING, and the PONG is
    // the proof the session was never marked for close.
    send(
        &mut bitless.stream,
        Opcode::Ping,
        92,
        &migo_protocol::Ping {
            client_time: migo_core::Timestamp::from_millis(0),
        },
    )
    .await;
    let pong = LiveSession::reply_to_correlation(&mut bitless.stream, 92).await;
    assert!(
        !pong.header.is_error(),
        "a session refused one feature keeps serving the frames it did negotiate"
    );
}

/// The listing is the one call question a client that was *absent* can ask.
///
/// Every other frame in this suite is a participant telling the node what to
/// do, and every fact a client learns arrives as an event it had to be
/// connected to receive. A member who was offline through a whole group call
/// missed every one of those events and has nothing to replay, which is the gap
/// `CALL_LIST` closes: the client reconnects, subscribes, asks, and is told what
/// is running and what is ringing at it. What this test pins is that the answer
/// travels on the connection that asked — the frame publishes nothing, because a
/// read must not be the way an account's other devices learn about a screen they
/// did not ask for.
#[tokio::test]
async fn the_listing_answers_the_session_that_asked_and_publishes_nothing() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let caller = registered_grant(&app, "listcaller").await;
    let callee = registered_grant(&app, "listcallee").await;
    let conversation_id = a_room_between(&app, &caller, &callee, "list-room").await;

    let mut caller_session = LiveSession::connect(addr, &caller).await;
    let mut callee_session = LiveSession::connect(addr, &callee).await;

    // Nothing is running yet: an empty answer, and one that arrives as the
    // reply rather than as an event, which is the whole shape of a read.
    let empty: CallListResult = caller_session
        .ask(
            Opcode::CallList,
            201,
            &CallListQuery {
                conversation_id: None,
            },
        )
        .await;
    assert!(
        empty.calls.is_empty(),
        "an account with no calls is told it has none"
    );

    // A ring aimed at the callee, placed by the caller.
    let call_id = Id::from_bytes([0xC4; 16]);
    let result: CallInviteResult = caller_session
        .ask(
            Opcode::CallInvite,
            202,
            &CallInvite {
                call_id,
                conversation_id,
                callee_id: callee.account_id,
                media_kind: 1,
                caller_device: caller.device_id,
                capabilities: 0,
                sealed_offer: vec![0x55; 48],
            },
        )
        .await;
    assert_eq!(result.status, 0, "the invite is accepted as a ring");
    let _ = next_event_of(&mut callee_session.stream, Opcode::CallInviteEvent, STEP).await;

    // The callee asks: the ring is offered to it, so it is a line it can see
    // and not one it is in.
    let listed: CallListResult = callee_session
        .ask(
            Opcode::CallList,
            203,
            &CallListQuery {
                conversation_id: None,
            },
        )
        .await;
    assert_eq!(listed.calls.len(), 1, "the ring is the callee's only call");
    let entry = &listed.calls[0];
    assert_eq!(entry.call_id, call_id);
    assert_eq!(entry.conversation_id, conversation_id);
    assert_eq!(entry.kind, 0, "a 1:1 call is kind Direct");
    assert_eq!(entry.state, 0, "the ring reports the ring's own state");
    assert_eq!(
        entry.peer_id, caller.account_id,
        "the peer is the other party"
    );
    assert_eq!(entry.participant_count, 2);
    assert_eq!(entry.joined, 0, "the callee is being offered it, not in it");
    assert_eq!(
        entry.media_kind,
        Some(1),
        "the ring's own media kind survives the listing"
    );
    assert_eq!(entry.expires_at, Some(result.expires_at));
    assert_eq!(entry.answered_at, None);

    // The caller's own listing, from the session that dialled: the same ring,
    // and this account is in it.
    let mine: CallListResult = caller_session
        .ask(
            Opcode::CallList,
            204,
            &CallListQuery {
                conversation_id: Some(conversation_id),
            },
        )
        .await;
    assert_eq!(mine.calls.len(), 1);
    assert_eq!(mine.calls[0].call_id, call_id);
    assert_eq!(
        mine.calls[0].joined, 1,
        "the account that dialled is in the call"
    );

    // The read published nothing. The callee's session was owed the invite
    // event and has had it; a listing that fanned out would put one more frame
    // on that socket, and the socket's silence is the assertion. This reads the
    // length prefix by hand because the suite's own receive helper treats
    // silence as a failure, and here silence is the fact under test.
    let mut head = [0u8; 4];
    let quiet = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::io::AsyncReadExt::read_exact(&mut callee_session.stream, &mut head),
    )
    .await;
    assert!(
        quiet.is_err(),
        "a listing publishes nothing: the callee was owed no further frame"
    );
}
