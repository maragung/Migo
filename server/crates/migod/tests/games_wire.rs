//! The game lifecycle's silent half answered on the wire: the two frames a conversation's
//! members used to be owed and never received.
//!
//! `GAME_ACTION` has always published its deltas to the conversation topic, but start and
//! abandon were fetch-only: a member who never asked `GAME_VIEW` learned a game existed
//! when its first move published — and a member watching a solo guessing game learned it
//! had been abandoned never, because the abandoner's reply was a bare ack and no other
//! frame followed. Section 137 makes `GAME_EVENT` mandatory-binary realtime with no
//! fetch-only escape hatch, so the dispatcher now publishes the terminal facts itself:
//!
//! * **Start.** `GAME_START`'s reply *is* the opening view, so the `started` delta goes
//!   out with the section 156 rule — excluding the connection that asked — and the first
//!   test drives a real start over TCP and asserts a *witness* session, subscribed only
//!   to the conversation's topic, hears `started` naming the game the reply created,
//!   while the starting session itself stays silent (its reply already carried the view).
//! * **Abandon.** `GAME_ABANDON`'s reply is a bare `Acknowledged`, so the `finished`
//!   delta takes the `GAME_ACTION` exception — including the abandoning connection — and
//!   the second test asserts *both* the abandoner and the witness hear `finished` with
//!   no winner named, because a forfeit pays nobody.
//!
//! Both tests use the reply rule as their clock: every frame they wait for is one the
//! server owes somebody, so the timeout is the assertion.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Clock, Config, Secret};
use migo_protocol::{
    from_frame, to_frame, ConversationCreateRequest, ConversationKind, Encode, Frame, GameEvent,
    GameId, GameStart, GameViewWire, Hello, Opcode, Platform, SubscribeRequest, SubscribeResponse,
    Topic, TopicKind, Welcome, PROTOCOL_VERSION,
};
use migod::App;

/// How long any single exchange may take before the test declares the server
/// stuck — silence being the bug class both tests exist to catch.
const STEP: Duration = Duration::from_secs(5);

/// How long the start test waits for an echo the design says must not come. Short,
/// because the assertion is the absence of a frame the server would have to have sent
/// on the wrong path — and the witness's copy, already asserted, proves the fan-out ran.
const QUIET: Duration = Duration::from_millis(300);

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
                device: DeviceClaim::new(Platform::Web, "game wire test"),
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

/// A live authenticated TCP session: HELLO with the grant's inline token, then the
/// SUBSCRIBE for its own user topic plus, when given, a conversation's.
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

        let welcome_frame = recv_within(&mut stream, STEP).await;
        assert_eq!(
            Opcode::from_wire(welcome_frame.header.opcode),
            Some(Opcode::Hello),
            "the handshake is answered with a WELCOME"
        );
        assert!(
            !welcome_frame.header.is_error(),
            "the handshake is not refused: {:?}",
            from_frame::<migo_protocol::Error>(&welcome_frame)
        );
        let welcome: Welcome = from_frame(&welcome_frame).expect("the WELCOME decodes");
        assert_eq!(welcome.authenticated_user, Some(grant.account_id));

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
                return Self { stream };
            }
        }
    }

    /// Subscribes to a conversation's topic, the one every game event publishes to.
    async fn watch_conversation(&mut self, correlation: u32, conversation_id: migo_core::Id) {
        send(
            &mut self.stream,
            Opcode::Subscribe,
            correlation,
            &SubscribeRequest {
                topics: vec![Topic {
                    kind: TopicKind::Conversation,
                    id: conversation_id,
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
                let confirmation: SubscribeResponse =
                    from_frame(&frame).expect("the SUBSCRIBE reply decodes");
                assert_eq!(
                    confirmation.accepted.len(),
                    1,
                    "the conversation topic is accepted"
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
}

/// Reads from a session until a frame of the wanted opcode arrives, skipping the
/// unrelated events a subscribed session receives.
async fn next_event_of(stream: &mut tokio::net::TcpStream, want: Opcode) -> Frame {
    loop {
        let frame = recv_within(stream, STEP).await;
        if Opcode::from_wire(frame.header.opcode) == Some(want) {
            return frame;
        }
    }
}

/// Builds the two-member group a solo guessing game is played in front of: the founder
/// starts the game, the witness only watches the conversation's topic.
async fn group_with_witness(
    founder_session: &mut LiveSession,
    witness: &migo_auth::Grant,
) -> migo_core::Id {
    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            11,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![witness.account_id],
                title: Some("The Game Wire Group".to_string()),
            },
        )
        .await;
    summary.conversation_id
}

#[tokio::test]
async fn a_started_game_publishes_the_started_delta_to_the_conversation() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "gamefounder").await;
    let witness = registered_grant(&app, "gamewitness").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let mut witness_session = LiveSession::connect(addr, &witness).await;

    let conversation_id = group_with_witness(&mut founder_session, &witness).await;

    // Both sides listen on the conversation's own topic, which is where the started
    // delta publishes: the witness because they had no other way to learn the game
    // exists, the founder because the exclusion under test is per *connection*, not per
    // account — a founder subscribed to the topic must simply not be sent their own echo.
    founder_session
        .watch_conversation(21, conversation_id)
        .await;
    witness_session
        .watch_conversation(31, conversation_id)
        .await;

    // The start. The reply carries the opening view to the caller alone.
    let view: GameViewWire = founder_session
        .ask(
            Opcode::GameStart,
            22,
            &GameStart {
                conversation_id,
                slug: "guess_number".to_string(),
            },
        )
        .await;

    // The frame the fetch-only world never sent: a witness who never asked GAME_VIEW
    // hears that the game began, naming the game the reply created.
    let frame = next_event_of(&mut witness_session.stream, Opcode::GameEvent).await;
    let event: GameEvent = from_frame(&frame).expect("the started delta decodes");
    assert_eq!(event.event, "started");
    assert_eq!(event.game_id, view.game_id);
    assert_eq!(
        event.room_id, conversation_id,
        "the wire's room_id carries the conversation the game is played in"
    );
    assert_eq!(event.state_version, view.state_version);
    assert_eq!(
        event.actor_id, None,
        "a start is about the game, not about a player"
    );

    // The founder asked, and the reply already carried the view, so the conversation
    // copy must have skipped this connection: nothing arrives on the founder's stream
    // within the quiet window. A frame here is the §156 violation, not a bonus.
    let echo = tokio::time::timeout(QUIET, async {
        loop {
            let frame = recv_within(&mut founder_session.stream, STEP).await;
            if Opcode::from_wire(frame.header.opcode) == Some(Opcode::GameEvent) {
                return frame;
            }
        }
    })
    .await;
    assert!(
        echo.is_err(),
        "the starting connection received its own started echo; the reply was the answer"
    );
}

#[tokio::test]
async fn an_abandoned_game_publishes_the_finished_delta_to_every_player() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "forfeitfounder").await;
    let witness = registered_grant(&app, "forfeitwitness").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let mut witness_session = LiveSession::connect(addr, &witness).await;

    let conversation_id = group_with_witness(&mut founder_session, &witness).await;

    founder_session
        .watch_conversation(21, conversation_id)
        .await;
    witness_session
        .watch_conversation(31, conversation_id)
        .await;

    let view: GameViewWire = founder_session
        .ask(
            Opcode::GameStart,
            22,
            &GameStart {
                conversation_id,
                slug: "guess_number".to_string(),
            },
        )
        .await;

    // The witness drains the started delta before the abandon, so the finished frame
    // asserted below is unambiguously the abandon's.
    let started = next_event_of(&mut witness_session.stream, Opcode::GameEvent).await;
    let started: GameEvent = from_frame(&started).expect("the started delta decodes");
    assert_eq!(started.event, "started");

    // The forfeit. The reply is a bare ack — the abandoner's own knowledge of the end
    // can only come from the fan-out, which is why abandon includes the caller.
    let ack: migo_protocol::Acknowledged = founder_session
        .ask(
            Opcode::GameAbandon,
            23,
            &GameId {
                game_id: view.game_id,
            },
        )
        .await;
    assert!(ack.ok, "the abandon is accepted");

    // The witness hears the game ended...
    let frame = next_event_of(&mut witness_session.stream, Opcode::GameEvent).await;
    let event: GameEvent = from_frame(&frame).expect("the finished delta decodes");
    assert_eq!(event.event, "finished");
    assert_eq!(event.game_id, view.game_id);
    assert_eq!(event.room_id, conversation_id);
    assert_eq!(
        event.actor_id, None,
        "a forfeit names no winner — a win-by-abandon is a farm"
    );

    // ...and so does the abandoning connection itself, whose reply carried nothing.
    let frame = next_event_of(&mut founder_session.stream, Opcode::GameEvent).await;
    let own: GameEvent = from_frame(&frame).expect("the finished delta decodes");
    assert_eq!(own.event, "finished");
    assert_eq!(own.game_id, view.game_id);
}
