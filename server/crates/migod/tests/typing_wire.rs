//! The typing indicator's two deaths on the wire: the ends a typer who never
//! said "stop" still owes everybody.
//!
//! The service tests prove `migo-messaging` claims expired marks and that the
//! removal paths return a `Stop`; these tests prove the *sockets* hear it,
//! because an indicator is a promise made to a screen, not a cache entry. Two
//! failures live exactly in that gap:
//!
//! * **The dead typer.** A client killed mid-word cannot send `Stop`, and
//!   until the node ran a sweeper of its own, the mark's deadline passed
//!   silently inside the cache — the two clients with no local timeout
//!   (Android, desktop) showed "typing…" forever. `App::spawn_typing_sweeper`
//!   is the fix, and the first test drives it against the real TCP transport:
//!   one `Start`, then nobody sends anything at all, and the recipient still
//!   receives a `Stop` naming the typer within the TTL plus a tick.
//! * **The kicked typer.** A member removed while typing cannot be relied on
//!   to send their own `Stop` — their client may not even know yet — and the
//!   revocation that follows a kick takes the conversation topic away from
//!   them, so a `Stop` published after it would reach nobody who left. The
//!   second test kicks a typing member over the wire and asserts both a
//!   remaining member and the kicked member's own session receive the `Stop`
//!   *before* the member event that announces the removal.
//!
//! Both tests use the reply rule as their clock: every frame they wait for is
//! one the server owes somebody, so the timeout is the assertion.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Clock, Config, Id, Secret};
use migo_economy::{Grant as CoinGrant, Reason};
use migo_protocol::{
    from_frame, to_frame, ConversationCreateRequest, ConversationKickRequest, ConversationKind,
    Encode, Frame, Hello, Opcode, Platform, SubscribeRequest, SubscribeResponse, Topic, TopicKind,
    TypingEvent, TypingState, Welcome, PROTOCOL_VERSION,
};
use migo_store::model::Currency;
use migod::App;

/// How long any single exchange may take before the test declares the server
/// stuck — silence being the bug class both tests exist to catch.
const STEP: Duration = Duration::from_secs(5);

/// How long the indicator may take to die on its own: the typing TTL is ten
/// seconds, the sweeper ticks every one, and the margin is what a slow CI
/// runner is owed. The assertion is not *when* the indicator died but that it
/// died without the typer — or anybody else — saying anything.
const TYPING_DIES: Duration = Duration::from_secs(20);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development app with the TCP listener bound, as the listener tests build
/// it: the in-memory backends, the real services over them, and the one
/// environment pair that turns the native transport on.
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
                device: DeviceClaim::new(Platform::Web, "typing wire test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// Hands one account a coin, because an outright kick costs one and the test
/// is about the typing mark, not the tariff.
async fn a_coin_for(app: &App, account: Id) {
    app.economy
        .grant(CoinGrant {
            account_id: account,
            currency: Currency::Coins,
            amount: 1,
            reason: Reason::Grant,
            ref_id: None,
            idempotency_key: format!("typing-wire:{account}"),
            created_by: None,
            at: app.clock.now(),
        })
        .await
        .expect("the seed coin lands");
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
/// answered by a WELCOME that names the account, then the self-subscription
/// every real client sends right after its handshake.
struct LiveSession {
    stream: tokio::net::TcpStream,
}

impl LiveSession {
    /// Connects, backing off exactly as long as the limiter asks when several
    /// handshakes from one IP land inside one window — the server talking,
    /// not the server broken.
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
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

        // The TYPING bit: the typing family is gated on the negotiated feature
        // (brief section 72/148) — a session that never asked for it is answered
        // FEATURE_NOT_NEGOTIATED for the request, and the hub withholds the
        // family's frames from it besides. Every session in these tests sends
        // and receives typing marks, so every handshake asks for the bit.
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            features: migo_protocol::features::TYPING,
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
            let mut session = Self { stream };
            session
                .subscribe(
                    2,
                    &Topic {
                        kind: TopicKind::User,
                        id: grant.account_id,
                    },
                )
                .await;
            return Ok(session);
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

    /// Subscribes to one topic, using `correlation` for the ask.
    async fn subscribe(&mut self, correlation: u32, topic: &Topic) {
        send(
            &mut self.stream,
            Opcode::Subscribe,
            correlation,
            &SubscribeRequest {
                topics: vec![topic.clone()],
            },
        )
        .await;
        loop {
            let frame = recv_within(&mut self.stream, STEP).await;
            if frame.header.correlation == correlation {
                assert!(
                    !frame.header.is_error(),
                    "the subscription is accepted: {:?}",
                    from_frame::<migo_protocol::Error>(&frame)
                );
                let confirmation: SubscribeResponse =
                    from_frame(&frame).expect("the SUBSCRIBE reply decodes");
                assert_eq!(confirmation.accepted.len(), 1, "the topic is accepted");
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

/// Reads from a session until a frame of the wanted opcode arrives, skipping
/// the unrelated events a subscribed session receives.
async fn next_event_of(stream: &mut tokio::net::TcpStream, want: Opcode, limit: Duration) -> Frame {
    loop {
        let frame = recv_within(stream, limit).await;
        if Opcode::from_wire(frame.header.opcode) == Some(want) {
            return frame;
        }
    }
}

#[tokio::test]
async fn a_dead_typer_s_indicator_is_stopped_by_the_sweep() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let typer = registered_grant(&app, "deadytyper").await;
    let reader = registered_grant(&app, "patientreader").await;

    // The undertaker: in production `App::serve` spawns it; the test starts it
    // by hand because the app itself never serves.
    let _sweeper = app.spawn_typing_sweeper();

    let summary: migo_protocol::ConversationSummary = {
        // The conversation is created over the front door by the typer, so the
        // mark the sweep will claim is born the way a real one is. A group,
        // because the create's privacy gate refuses a direct conversation
        // between two accounts with no friendship to stand on — the sweep's
        // claim is the same in either kind, and this test is about the sweep.
        let mut creator = LiveSession::connect(addr, &typer).await;
        creator
            .ask(
                Opcode::ConversationCreate,
                11,
                &ConversationCreateRequest {
                    kind: ConversationKind::Group,
                    members: vec![reader.account_id],
                    title: Some("The Silence After".to_string()),
                },
            )
            .await
    };
    let conversation_id = summary.conversation_id;

    let mut typer_session = LiveSession::connect(addr, &typer).await;
    let mut reader_session = LiveSession::connect(addr, &reader).await;
    let topic = Topic {
        kind: TopicKind::Conversation,
        id: conversation_id,
    };
    typer_session.subscribe(3, &topic).await;
    reader_session.subscribe(3, &topic).await;

    // The typer starts, and that is the last thing they ever say.
    send(
        &mut typer_session.stream,
        Opcode::Typing,
        21,
        &TypingEvent {
            conversation_id,
            state: TypingState::Start,
            user_id: None,
        },
    )
    .await;

    let start_frame = next_event_of(&mut reader_session.stream, Opcode::Typing, STEP).await;
    let start: TypingEvent = from_frame(&start_frame).expect("the start decodes");
    assert_eq!(start.conversation_id, conversation_id);
    assert_eq!(start.state, TypingState::Start);
    assert_eq!(start.user_id, Some(typer.account_id));

    // And now the whole test: no Stop is ever sent, by anybody. The typer's
    // app may as well be dead — the sweeper does not care, and that is the
    // point.
    let stop_frame = next_event_of(&mut reader_session.stream, Opcode::Typing, TYPING_DIES).await;
    let stop: TypingEvent = from_frame(&stop_frame).expect("the stop decodes");
    assert_eq!(stop.conversation_id, conversation_id);
    assert_eq!(
        stop.state,
        TypingState::Stop,
        "the frame that ends an expired indicator is a Stop"
    );
    assert_eq!(
        stop.user_id,
        Some(typer.account_id),
        "the sweep's Stop names the typer whose mark it claimed"
    );
}

#[tokio::test]
async fn a_kicked_typer_s_indicator_stops_before_the_revocation() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "kickfounder").await;
    let witness = registered_grant(&app, "kickwitness").await;
    let typer = registered_grant(&app, "kicktyper").await;
    a_coin_for(&app, founder.account_id).await;

    let summary: migo_protocol::ConversationSummary = {
        let mut creator = LiveSession::connect(addr, &founder).await;
        creator
            .ask(
                Opcode::ConversationCreate,
                11,
                &ConversationCreateRequest {
                    kind: ConversationKind::Group,
                    members: vec![witness.account_id, typer.account_id],
                    title: None,
                },
            )
            .await
    };
    let conversation_id = summary.conversation_id;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let mut witness_session = LiveSession::connect(addr, &witness).await;
    let mut typer_session = LiveSession::connect(addr, &typer).await;
    let topic = Topic {
        kind: TopicKind::Conversation,
        id: conversation_id,
    };
    founder_session.subscribe(3, &topic).await;
    witness_session.subscribe(3, &topic).await;
    typer_session.subscribe(3, &topic).await;

    // The member who is about to be removed is typing when it happens.
    send(
        &mut typer_session.stream,
        Opcode::Typing,
        21,
        &TypingEvent {
            conversation_id,
            state: TypingState::Start,
            user_id: None,
        },
    )
    .await;

    let start_frame = next_event_of(&mut witness_session.stream, Opcode::Typing, STEP).await;
    let start: TypingEvent = from_frame(&start_frame).expect("the start decodes");
    assert_eq!(start.user_id, Some(typer.account_id));
    assert_eq!(start.state, TypingState::Start);

    // The kick. The typer sends nothing else — not a Stop, not a goodbye.
    let _: migo_protocol::Acknowledged = founder_session
        .ask(
            Opcode::ConversationKick,
            31,
            &ConversationKickRequest {
                conversation_id,
                target_id: typer.account_id,
            },
        )
        .await;

    // A member who stays hears the Stop: their screen's indicator ends even
    // though the typer it named can never send the frame themselves.
    let witness_stop = next_event_of(&mut witness_session.stream, Opcode::Typing, STEP).await;
    let stop: TypingEvent = from_frame(&witness_stop).expect("the witness's stop decodes");
    assert_eq!(stop.conversation_id, conversation_id);
    assert_eq!(stop.state, TypingState::Stop);
    assert_eq!(stop.user_id, Some(typer.account_id));

    // And the ordering the revocation depends on: the kicked member's own
    // session — still subscribed for these two frames, and never after them —
    // receives the Stop first and the removal second. A Stop published after
    // the revocation would be a frame this loop never sees.
    let own_stop = next_event_of(&mut typer_session.stream, Opcode::Typing, STEP).await;
    let stop: TypingEvent = from_frame(&own_stop).expect("the typer's own stop decodes");
    assert_eq!(stop.state, TypingState::Stop);
    assert_eq!(stop.user_id, Some(typer.account_id));
    let removal = next_event_of(
        &mut typer_session.stream,
        Opcode::ConversationMemberEvent,
        STEP,
    )
    .await;
    let member: migo_protocol::ConversationMemberEvent =
        from_frame(&removal).expect("the removal decodes");
    assert_eq!(member.user_id, typer.account_id);
    assert_eq!(member.change, migo_protocol::MemberChange::Kicked);
}
