//! The group call answered on the wire: the frames a joining member cannot
//! hear through the conversation topic alone.
//!
//! The SFU this node serves is a *forwarding* one (brief section 166): the
//! roster, its events, and sealed descriptions moved between seated devices.
//! The seven tests here drive real TCP sessions against a bound migod and pin
//! the properties the service's own tests cannot see:
//!
//! * **The joiner hears their roster.** The reply frame is a TURN list (the
//!   shape the registry froze for the opcode), so the roster itself arrives
//!   as a `CALL_SFU_EVENT` on the joiner's *own user topic* — the one topic
//!   every session holds from its handshake, and the only one a client that
//!   has not yet loaded the conversation is listening on.
//! * **The roster hears the join.** A second member's join is published on
//!   the conversation's topic, naming the joiner and the size after the
//!   change; a session subscribed to the conversation receives it.
//! * **A stranger's join is refused.** An account that is not a member of
//!   the conversation gets `NOT_FOUND`, not a ring and not a hint about
//!   which conversations hold calls — and the refusal is no longer the
//!   `FEATURE_DISABLED` the opcode used to answer.
//! * **A leave with no seat is answered.** A retried leave after a seat
//!   replacement, and the leave of a member who never joined, both find
//!   the call alive with nothing to remove — the acknowledgement is owed
//!   either way, and silence was the bug.
//! * **A rotation reaches the other seat.** `CALL_KEY_UPDATE` names no
//!   target, so the dispatcher asks the roster *who* and publishes to each
//!   account once; a seated device that did not mint the frame hears the
//!   material it cannot open, byte for byte (brief section 180).
//! * **A sealed frame reaches the seat it names.** `CALL_SDP` on a group
//!   call routes through the group store and lands on the named device's
//!   account, while a device that holds no seat is refused rather than
//!   silently dropped.
//!
//! Each test uses the reply rule as its clock: every frame waited for is one
//! the server owes somebody, so the timeout is the assertion.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Clock, Config, Secret};
use migo_protocol::{
    from_frame, to_frame, CallInvite, CallStateEvent, ConversationCreateRequest, ConversationKind,
    Encode, Frame, Hello, Opcode, Platform, SubscribeRequest, SubscribeResponse, Topic, TopicKind,
    Welcome, PROTOCOL_VERSION,
};
use migod::App;

/// How long any single exchange may take before the test declares the server
/// stuck — silence being the bug class these tests exist to catch.
const STEP: Duration = Duration::from_secs(5);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development app with the TCP listener bound, as the listener tests build it.
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

/// Registers one account through the front door, stamped with the node's own clock.
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
                device: DeviceClaim::new(Platform::Web, "sfu wire test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
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

/// A live authenticated TCP session on its own user topic, plus whichever
/// conversation topics the test subscribes it to.
struct LiveSession {
    stream: tokio::net::TcpStream,
}

impl LiveSession {
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        // A third handshake from one IP inside the limiter's window is refused
        // with a retry-after — the server talking, not the server broken, and
        // the seatless-leave test below opens three sessions from 127.0.0.1 —
        // so the session waits exactly as long as it is told and tries again,
        // the way a client with manners does.
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
            assert_eq!(welcome.authenticated_user, Some(grant.account_id));
        } else {
            // Any other answer is the server refusing the session; only a
            // rate-limit refusal is worth retrying, because only it names a
            // time at which the answer changes.
            let refusal: migo_protocol::Error =
                from_frame(&welcome_frame).expect("the refusal decodes");
            assert!(
                refusal.code == migo_protocol::codes::RATE_LIMITED,
                "the handshake is refused outright: {refusal:?}"
            );
            return Err(u64::from(refusal.retry_after_ms.unwrap_or(1000)));
        }

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
        loop {
            let frame = recv_within(&mut stream, STEP).await;
            if frame.header.correlation == 2 {
                assert!(
                    !frame.header.is_error(),
                    "the self-subscription is accepted: {:?}",
                    from_frame::<migo_protocol::Error>(&frame)
                );
                let confirmation: SubscribeResponse =
                    from_frame(&frame).expect("the SUBSCRIBE reply decodes");
                assert_eq!(
                    confirmation.accepted.len(),
                    1,
                    "the own user topic is accepted"
                );
                return Ok(Self { stream });
            }
        }
    }

    /// Subscribes to a conversation's topic, as a member's client does once
    /// it has loaded the conversation.
    async fn subscribe_conversation(&mut self, conversation: migo_core::Id, correlation: u32) {
        send(
            &mut self.stream,
            Opcode::Subscribe,
            correlation,
            &SubscribeRequest {
                topics: vec![Topic {
                    kind: TopicKind::Conversation,
                    id: conversation,
                }],
            },
        )
        .await;
        loop {
            let frame = recv_within(&mut self.stream, STEP).await;
            if frame.header.correlation == correlation {
                assert!(
                    !frame.header.is_error(),
                    "the conversation subscription is accepted: {:?}",
                    from_frame::<migo_protocol::Error>(&frame)
                );
                return;
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
        send(&mut self.stream, opcode, correlation, message).await;
        loop {
            let frame = recv_within(&mut self.stream, STEP).await;
            if frame.header.correlation == correlation {
                assert!(
                    !frame.header.is_error(),
                    "the request was refused: {:?}",
                    from_frame::<migo_protocol::Error>(&frame)
                );
                return from_frame(&frame).expect("the reply decodes");
            }
        }
    }

    /// Sends a request that must fail, returning the error frame.
    async fn ask_error<M: Encode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        message: &M,
    ) -> migo_protocol::Error {
        send(&mut self.stream, opcode, correlation, message).await;
        loop {
            let frame = recv_within(&mut self.stream, STEP).await;
            if frame.header.correlation == correlation {
                assert!(frame.header.is_error(), "the request was expected to fail");
                return from_frame(&frame).expect("the error decodes");
            }
        }
    }
}

/// Reads from a session until a frame of the wanted opcode arrives, skipping
/// the unrelated events a subscribed session receives.
async fn next_event_of(stream: &mut tokio::net::TcpStream, want: Opcode) -> Frame {
    loop {
        let frame = recv_within(stream, STEP).await;
        if Opcode::from_wire(frame.header.opcode) == Some(want) {
            return frame;
        }
    }
}

/// The join frame a scripted client sends: the `CallInvite` shape the registry
/// froze for the opcode, with group semantics — the roster is the audience,
/// so `callee_id` is a placeholder the server never reads.
fn sfu_join(call_id: migo_core::Id, conversation_id: migo_core::Id) -> CallInvite {
    CallInvite {
        call_id,
        conversation_id,
        callee_id: migo_core::Id::from(0u128),
        media_kind: 0,
        caller_device: migo_core::Id::from(0u128),
        capabilities: 0,
        sealed_offer: b"sealed-group-offer".to_vec(),
    }
}

#[tokio::test]
async fn a_joiner_receives_the_roster_on_their_own_topic() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "sfufounder").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;

    // A group conversation the founder shares with a second registered
    // account — a group conversation needs somebody other than its creator,
    // and a group call's roster is exactly that conversation's members.
    let peer = registered_grant(&app, "sfujoinpeer").await;
    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            11,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![peer.account_id],
                title: Some("The SFU Group".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    // The join: the reply is the TURN list (empty in a dev app), and the
    // roster arrives as a CALL_SFU_EVENT on the founder's own user topic.
    let call_id = migo_core::Id::from(0x5f00u128);
    let reply: migo_protocol::CallTurnResponse = founder_session
        .ask(Opcode::CallSfuJoin, 12, &sfu_join(call_id, conversation_id))
        .await;
    assert!(reply.servers.is_empty());

    let frame = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;
    let event: CallStateEvent = from_frame(&frame).expect("the roster event decodes");
    assert_eq!(event.call_id, call_id);
    assert_eq!(event.conversation_id, Some(conversation_id));
    assert_eq!(event.user_id, Some(founder.account_id));
    assert_eq!(event.participant_count, Some(1));
    let roster = event.participants.expect("the roster is attached");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0].user_id, founder.account_id);
    assert_eq!(roster[0].device_id, founder.device_id);
    assert_eq!(roster[0].sealed_offer, b"sealed-group-offer");
}

#[tokio::test]
async fn the_roster_hears_the_second_join_on_the_conversation_topic() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "sfurosterfounder").await;
    let second = registered_grant(&app, "sfurostersecond").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let mut second_session = LiveSession::connect(addr, &second).await;

    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            11,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![second.account_id],
                title: Some("The Roster Group".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    // The founder subscribes to the conversation the way a client that has
    // loaded it does — the announcement this test waits for is published to
    // the conversation's topic, and a session that never subscribed hears
    // nothing.
    founder_session
        .subscribe_conversation(conversation_id, 12)
        .await;

    let call_id = migo_core::Id::from(0x5f01u128);
    let _: migo_protocol::CallTurnResponse = founder_session
        .ask(Opcode::CallSfuJoin, 13, &sfu_join(call_id, conversation_id))
        .await;
    // Drain the founder's own roster event so it cannot be mistaken for the
    // announcement below.
    let _ = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;
    second_session
        .subscribe_conversation(conversation_id, 14)
        .await;

    // The second member joins; the founder — subscribed to the conversation
    // — hears the announcement naming the joiner.
    let _: migo_protocol::CallTurnResponse = second_session
        .ask(Opcode::CallSfuJoin, 15, &sfu_join(call_id, conversation_id))
        .await;

    let frame = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;
    let event: CallStateEvent = from_frame(&frame).expect("the announcement decodes");
    assert_eq!(event.call_id, call_id);
    assert_eq!(event.user_id, Some(second.account_id));
    assert_eq!(event.device_id, Some(second.device_id));
    assert_eq!(event.participant_count, Some(2));
    assert_eq!(
        event.sealed_offer.as_deref(),
        Some(b"sealed-group-offer".as_ref()),
        "the joiner's sealed offer rides the announcement, unopened"
    );

    // And the second member's own roster event names both seats.
    let frame = next_event_of(&mut second_session.stream, Opcode::CallSfuEvent).await;
    let event: CallStateEvent = from_frame(&frame).expect("the roster decodes");
    let roster = event.participants.expect("the roster is attached");
    assert_eq!(roster.len(), 2);
    assert!(roster.iter().any(|p| p.user_id == founder.account_id));
    assert!(roster.iter().any(|p| p.user_id == second.account_id));
}

#[tokio::test]
async fn a_stranger_s_join_is_refused_not_disabled() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "sfuclosedfounder").await;
    let stranger = registered_grant(&app, "sfuoutsider").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let mut stranger_session = LiveSession::connect(addr, &stranger).await;

    // A group conversation between the founder and a second registered
    // account: the stranger is not a member of it, which is the gate under
    // test. A group conversation needs somebody other than its creator, so
    // the founder brings a member the stranger is not.
    let insider = registered_grant(&app, "sfuclosedinsider").await;
    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            11,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![insider.account_id],
                title: Some("The Closed Group".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    let error = stranger_session
        .ask_error(
            Opcode::CallSfuJoin,
            12,
            &sfu_join(migo_core::Id::from(0x5f02u128), conversation_id),
        )
        .await;
    // NOT_FOUND, the same answer a missing conversation gives — and crucially
    // not FEATURE_DISABLED, which was this opcode's whole answer before.
    assert_eq!(error.code, migo_protocol::codes::NOT_FOUND);
}

#[tokio::test]
async fn a_leave_tells_the_roster_and_the_last_leave_retires_the_call() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "sfuleavefounder").await;
    let second = registered_grant(&app, "sfuleavesecond").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let mut second_session = LiveSession::connect(addr, &second).await;

    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            11,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![second.account_id],
                title: Some("The Leaving Group".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    // The founder subscribes to the conversation the way a client that has
    // loaded it does — the departure this test waits for is published to the
    // conversation's topic, and a session that never subscribed hears
    // nothing.
    founder_session
        .subscribe_conversation(conversation_id, 12)
        .await;

    let call_id = migo_core::Id::from(0x5f03u128);
    let _: migo_protocol::CallTurnResponse = founder_session
        .ask(Opcode::CallSfuJoin, 13, &sfu_join(call_id, conversation_id))
        .await;
    let _ = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;
    second_session
        .subscribe_conversation(conversation_id, 14)
        .await;
    let _: migo_protocol::CallTurnResponse = second_session
        .ask(Opcode::CallSfuJoin, 15, &sfu_join(call_id, conversation_id))
        .await;
    // Drain each session's own roster event.
    let _ = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;
    let _ = next_event_of(&mut second_session.stream, Opcode::CallSfuEvent).await;

    // The second member leaves; the founder hears the departure on the
    // conversation topic.
    let _: migo_protocol::Acknowledged = second_session
        .ask(
            Opcode::CallEnd,
            16,
            &migo_protocol::CallEnd { call_id, reason: 0 },
        )
        .await;

    let frame = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;
    let event: CallStateEvent = from_frame(&frame).expect("the departure decodes");
    assert_eq!(event.user_id, Some(second.account_id));
    assert_eq!(event.participant_count, Some(1));

    // The founder leaves last; the call retires, and a further leave under
    // the same id is NOT_FOUND — the call is gone, not merely empty.
    let _: migo_protocol::Acknowledged = founder_session
        .ask(
            Opcode::CallEnd,
            16,
            &migo_protocol::CallEnd { call_id, reason: 0 },
        )
        .await;
    let error = founder_session
        .ask_error(
            Opcode::CallEnd,
            17,
            &migo_protocol::CallEnd { call_id, reason: 0 },
        )
        .await;
    assert_eq!(error.code, migo_protocol::codes::NOT_FOUND);
}

#[tokio::test]
async fn a_seatless_leave_is_acknowledged_not_silent() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "sfuseatfounder").await;
    let second = registered_grant(&app, "sfuseatsecond").await;
    let bystander = registered_grant(&app, "sfuseatbystander").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let mut second_session = LiveSession::connect(addr, &second).await;
    let mut bystander_session = LiveSession::connect(addr, &bystander).await;

    // A group conversation whose third member never joins the call: their
    // leave below finds the call alive with no seat of theirs to vacate, and
    // the reply rule still owes them an answer.
    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            11,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![second.account_id, bystander.account_id],
                title: Some("The Seatless Group".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    founder_session
        .subscribe_conversation(conversation_id, 12)
        .await;

    let call_id = migo_core::Id::from(0x5f04u128);
    let _: migo_protocol::CallTurnResponse = founder_session
        .ask(Opcode::CallSfuJoin, 13, &sfu_join(call_id, conversation_id))
        .await;
    let _ = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;
    second_session
        .subscribe_conversation(conversation_id, 14)
        .await;
    let _: migo_protocol::CallTurnResponse = second_session
        .ask(Opcode::CallSfuJoin, 15, &sfu_join(call_id, conversation_id))
        .await;
    // Drain each session's own roster event.
    let _ = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;
    let _ = next_event_of(&mut second_session.stream, Opcode::CallSfuEvent).await;

    // The second member leaves, then retries the leave: the call is still
    // alive (the founder holds a seat), but the retry has no seat to vacate.
    let _: migo_protocol::Acknowledged = second_session
        .ask(
            Opcode::CallEnd,
            16,
            &migo_protocol::CallEnd { call_id, reason: 0 },
        )
        .await;
    let frame = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;
    let event: CallStateEvent = from_frame(&frame).expect("the departure decodes");
    assert_eq!(event.user_id, Some(second.account_id));
    assert_eq!(event.participant_count, Some(1));

    // The retry is acknowledged as the idempotent no-op it is: before the
    // fix this exchange hung until the client's request timer fired, and the
    // timeout inside `ask` is the assertion.
    let retry: migo_protocol::Acknowledged = second_session
        .ask(
            Opcode::CallEnd,
            17,
            &migo_protocol::CallEnd { call_id, reason: 0 },
        )
        .await;
    assert!(
        retry.ok,
        "a retried leave of a seat already vacated is a success"
    );

    // And the member who never joined: the call is not theirs to leave, but
    // the state they ask for already holds, so the frame is answered too.
    let bystander_leave: migo_protocol::Acknowledged = bystander_session
        .ask(
            Opcode::CallEnd,
            18,
            &migo_protocol::CallEnd { call_id, reason: 0 },
        )
        .await;
    assert!(
        bystander_leave.ok,
        "a leave from a member with no seat is a success"
    );
}

/// Seats two accounts on one group call and returns the sessions, both
/// subscribed to the conversation, past the point where both joins have been
/// announced.
///
/// The app is borrowed rather than built here because it owns the listener
/// the sessions are talking to: a caller that let it drop would be driving
/// sockets into a closed server.
///
/// Waiting for the founder's announcement of the second arrival — and not
/// merely for the second join's reply — is what makes the tests below
/// deterministic: the roster the rotation is read against must already hold
/// both seats when the frame is sent, and the announcement is the last fact
/// the server produces for the second join.
async fn a_group_call_with_two_seats(
    app: &App,
    call_id: migo_core::Id,
    founder_name: &str,
    second_name: &str,
) -> (LiveSession, LiveSession, Grant, Grant) {
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(app, founder_name).await;
    let second = registered_grant(app, second_name).await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let mut second_session = LiveSession::connect(addr, &second).await;

    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            11,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![second.account_id],
                title: Some("The Sealed Group".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    founder_session
        .subscribe_conversation(conversation_id, 12)
        .await;
    second_session
        .subscribe_conversation(conversation_id, 13)
        .await;

    let _: migo_protocol::CallTurnResponse = founder_session
        .ask(Opcode::CallSfuJoin, 14, &sfu_join(call_id, conversation_id))
        .await;
    let _ = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;

    let _: migo_protocol::CallTurnResponse = second_session
        .ask(Opcode::CallSfuJoin, 15, &sfu_join(call_id, conversation_id))
        .await;
    let _ = next_event_of(&mut second_session.stream, Opcode::CallSfuEvent).await;
    let _ = next_event_of(&mut founder_session.stream, Opcode::CallSfuEvent).await;

    (founder_session, second_session, founder, second)
}

#[tokio::test]
async fn a_group_key_rotation_reaches_the_seat_that_did_not_mint_it() {
    let app = build_app().await;
    let call_id = migo_core::Id::from(0x5f05u128);
    let (mut founder_session, mut second_session, _founder, _second) =
        a_group_call_with_two_seats(&app, call_id, "sfurekeyfounder", "sfurekeysecond").await;

    // The rotation the founder's device mints. It names no target — a
    // rotation is every participant's business — and the material is sealed,
    // so nothing in this frame is something the server can read.
    let update = migo_protocol::CallKeyUpdate {
        call_id,
        epoch: 2,
        sealed_key_material: b"sealed-epoch-two".to_vec(),
    };
    let ack: migo_protocol::Acknowledged = founder_session
        .ask(Opcode::CallKeyUpdate, 16, &update)
        .await;
    assert!(ack.ok, "the rotation is acknowledged");

    // The other seat hears it on its own user topic, unchanged: the server
    // routed the frame by roster and never opened it.
    let frame = next_event_of(&mut second_session.stream, Opcode::CallKeyUpdate).await;
    let heard: migo_protocol::CallKeyUpdate =
        from_frame(&frame).expect("the relayed rotation decodes");
    assert_eq!(
        heard, update,
        "the rotation reaches the other seat byte for byte"
    );
}

#[tokio::test]
async fn a_group_relay_lands_on_the_named_seat_and_refuses_a_stranger() {
    let app = build_app().await;
    let call_id = migo_core::Id::from(0x5f06u128);
    let (mut founder_session, mut second_session, founder, second) =
        a_group_call_with_two_seats(&app, call_id, "sfurelayfounder", "sfurelaysecond").await;

    // The addressed frame: the roster, not a pair, says whether `to_device`
    // is somewhere real. The seat the join recorded is the connection's own
    // device, which is what a client that has not re-joined since a
    // replacement would still name.
    let sdp = migo_protocol::CallSdp {
        call_id,
        from_device: founder.device_id,
        to_device: second.device_id,
        sealed_sdp: b"sealed-group-renegotiation".to_vec(),
    };
    let ack: migo_protocol::Acknowledged = founder_session.ask(Opcode::CallSdp, 17, &sdp).await;
    assert!(ack.ok, "the relay is acknowledged");

    let frame = next_event_of(&mut second_session.stream, Opcode::CallSdp).await;
    let heard: migo_protocol::CallSdp = from_frame(&frame).expect("the relayed SDP decodes");
    assert_eq!(
        heard.sealed_sdp, sdp.sealed_sdp,
        "the sealed description arrives unchanged"
    );
    assert_eq!(heard.from_device, founder.device_id);

    // A device nobody seated is not a target on a group call either: the
    // refusal is the same one a stranger's device gets on a 1:1 relay, and
    // it is *not* `NOT_FOUND` — the id names a live call, and the frame was
    // refused on the roster rather than handed to the other store.
    let stranger = migo_protocol::CallSdp {
        call_id,
        from_device: founder.device_id,
        to_device: migo_core::Id::from(0xdead_beefu128),
        sealed_sdp: b"sealed-to-nobody".to_vec(),
    };
    let error = founder_session
        .ask_error(Opcode::CallSdp, 18, &stranger)
        .await;
    assert_eq!(error.code, migo_protocol::codes::PERMISSION_DENIED);
}
