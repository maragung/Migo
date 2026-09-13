//! Room membership answered on the wire: the two guarantees a room's frames rest on.
//!
//! A room is two topics — its own (roster, state) and its conversation (the chat) — and both
//! are gated by membership only at `SUBSCRIBE` time: authorisation is checked when a subscribe
//! arrives and never after. Two bugs lived in that gap, and both are driven here over real TCP
//! sockets, the way the groups suite drives its two:
//!
//! * **The leaver who kept listening.** A leave emptied the seat but not the subscriptions:
//!   the wire event said `joined: false` with no `change`, the dispatcher's removal gate keys
//!   on the verb, and a session whose room topics survived its own leave kept receiving every
//!   frame the room published afterwards. The first test drives a real leave, waits out the
//!   acknowledgement, and then proves the room cannot reach that socket again — not with a
//!   message, not with a state delta — while another member, the control, hears both.
//! * **The joiner nobody told.** A join's member event reaches the room's subscribers, and a
//!   member who has just joined is not one yet on any device but the one that asked — their
//!   other sessions cannot be subscribed to a room they have never heard of. The dispatcher
//!   now rings the same doorbell conversations ring for an invite: the join event, published
//!   to the joiner's *user* topic, is the one frame that reaches them. The second test joins
//!   on one session and asserts a second session of the same account hears the bell, then
//!   re-derives its subscriptions the way a client does — an idempotent re-join — and
//!   receives the room's next message live.
//!
//! The third and fourth tests drive the resume retention's topic half (section 158's "a
//! resumed reconnect does no sync and no resubscribe" is only honest if delivery actually
//! continues): a dropped session's topics are restored through authorisation on the reconnect,
//! and a topic the membership lost while the session was down is not.
//!
//! All four use the reply rule as their clock: every frame waited for is one the server owes
//! somebody, so the timeout is the assertion — and every frame waited *out* is one the server
//! must not send, so a short quiet window is the assertion there.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Clock, Config, Id, Secret};
use migo_protocol::{
    from_frame, to_frame, Acknowledged, Decode, DeliveryClass, Encode, Frame, Hello, MemberChange,
    MessageKind, MessageSend, Opcode, Platform, ResumeRequest, RoomCreate, RoomJoinRequest,
    RoomJoinResponse, RoomLeaveRequest, RoomMemberEvent, RoomStateEvent, RoomUpdate,
    SubscribeRequest, SubscribeResponse, Topic, TopicKind, Welcome, PROTOCOL_VERSION,
};
use migod::App;

/// How long any single exchange may take before the test declares the server stuck — silence
/// being the bug class these tests exist to catch.
const STEP: Duration = Duration::from_secs(5);

/// How long a socket must stay quiet to prove the server stopped talking to it. Anything the
/// server was going to send was enqueued before the window opens — the assertions that gate it
/// (an acknowledgement, another member's receipt of the same fan-out) have already completed —
/// so a second of silence is the frame never coming, not the frame being late.
const QUIET: Duration = Duration::from_secs(1);

/// How long the drain before a reset waits for the socket to run dry, so the Critical-frame
/// watermark a resume will claim is the whole truth.
const DRAIN: Duration = Duration::from_millis(500);

/// What a room member says when the test needs a live message: opaque to the server, so any
/// bytes are honest, and compared byte-for-byte at the far end so the test knows it heard the
/// same envelope and not a neighbour's.
const SEALED: &[u8] = b"sealed-for-the-room-wire-test";

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development app with the TCP listener bound, as the listener tests build it. The limiter
/// ceilings are raised the way the cross-node resume suite raises them: a room is several
/// sessions from one peer address driving a burst of joins and subscribes, and the default
/// endpoint bucket is sized for strangers, not for a scripted client proving a delivery
/// guarantee — configuration, not clock-waiting, with the ask/handshake politeness below as
/// the net for whatever still lands outside the burst.
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
                "MIGO_RATE_LIMIT__USER_BURST".to_string(),
                "1000".to_string(),
            ),
        ],
    )
    .expect("configuration should parse");
    App::build(&config)
        .await
        .expect("a development app must build against in-memory backends")
}

/// Registers one account through the front door, stamped with the node's own clock so the
/// inline token is not born expired.
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
                device: DeviceClaim::new(Platform::Web, "room wire test"),
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

/// Reads one length-prefixed frame, allowing `limit` for it to arrive. `None` when the limit
/// passes first, which is how a drain proves a socket has run dry.
async fn recv_within(stream: &mut tokio::net::TcpStream, limit: Duration) -> Option<Frame> {
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
    .ok()?;
    Some(Frame::decode(Bytes::from(body)).expect("the frame decodes"))
}

/// A live authenticated TCP session: HELLO with the grant's inline token, then the SUBSCRIBE
/// every client sends for its own user topic. Tracks the session id (the resume key) and the
/// Critical-frame count (the honest watermark a resume would claim — the server assigns a
/// `frame_seq` to a frame iff it is Critical and left through the session mailbox, and the
/// client's mirror of that counter is how many such frames it has seen; the WELCOME bypasses
/// the mailbox and carries no seq, so the mirror starts at zero, and an off-by-one here turns
/// a clean resume into a refused one).
struct LiveSession {
    stream: tokio::net::TcpStream,
    session_id: Id,
    /// How many sequenced Critical frames this connection has received, as the SDK counts them.
    critical_seen: u64,
    correlation: u32,
}

impl LiveSession {
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        // The test-open burst can trip the connection limiter — a room of founder, members,
        // and second devices is several handshakes from one IP inside the limiter's window —
        // and the refusal names a time at which the answer changes, so the session waits
        // exactly as long as it is told and tries again, the way a client with manners does.
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

    /// One full connection attempt, returning the retry-after the server asked for when it
    /// refuses the handshake as rate-limited.
    async fn handshake(addr: SocketAddr, grant: &Grant) -> Result<Self, u64> {
        let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting does not stall")
            .expect("the connection is accepted");

        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            // Every frame this suite drives belongs to the rooms family, and brief section 72
            // ties that family to the ROOMS bit — a session that did not ask for it is refused
            // FEATURE_NOT_NEGOTIATED before the first ROOM_CREATE, exactly as a CALLS-less
            // session is refused before it may place a ring.
            features: migo_protocol::features::ROOMS,
            access_token: Some(grant.access_token.clone()),
            device_id: Some(grant.device_id),
            ..Default::default()
        };
        send(&mut stream, Opcode::Hello, 1, &hello).await;

        let welcome_frame = recv_within(&mut stream, STEP)
            .await
            .expect("the WELCOME does not stall");
        assert_eq!(
            Opcode::from_wire(welcome_frame.header.opcode),
            Some(Opcode::Hello),
            "the handshake is answered with a WELCOME"
        );
        if !welcome_frame.header.is_error() {
            // The WELCOME is itself a Critical reply, so the mirror starts at one.
            let welcome: Welcome = from_frame(&welcome_frame).expect("the WELCOME decodes");
            assert_eq!(welcome.authenticated_user, Some(grant.account_id));

            let mut session = Self {
                stream,
                session_id: welcome.session_id,
                // The WELCOME itself is not sequenced — it is written straight to the socket,
                // outside the mailbox — so the mirror the SDK keeps starts at zero.
                critical_seen: 0,
                correlation: 1,
            };
            session
                .subscribe(&[Topic {
                    kind: TopicKind::User,
                    id: grant.account_id,
                }])
                .await;
            return Ok(session);
        }
        // Any other answer is the server refusing the session; only a rate-limit refusal is
        // worth retrying, because only it names a time at which the answer changes.
        let refusal: migo_protocol::Error =
            from_frame(&welcome_frame).expect("the refusal decodes");
        assert!(
            refusal.code == migo_protocol::codes::RATE_LIMITED,
            "the handshake is refused outright: {refusal:?}"
        );
        Err(u64::from(refusal.retry_after_ms.unwrap_or(1000)))
    }

    /// Reads one frame, advancing the Critical-frame mirror a resume would claim.
    async fn next_frame(&mut self) -> Frame {
        let frame = recv_within(&mut self.stream, STEP).await.expect(
            "the frame does not stall — silence here is the bug these tests exist to catch",
        );
        if let Some(opcode) = Opcode::from_wire(frame.header.opcode) {
            if opcode.class() == DeliveryClass::Critical {
                self.critical_seen += 1;
            }
        }
        frame
    }

    /// Reads until the socket runs dry, advancing the Critical-frame mirror as it goes, so a
    /// resume claiming this watermark is claiming everything the connection received — a
    /// watermark short of the truth would replay frames the tests mean to prove absent.
    async fn drain(&mut self) {
        while let Some(frame) = recv_within(&mut self.stream, DRAIN).await {
            if let Some(opcode) = Opcode::from_wire(frame.header.opcode) {
                if opcode.class() == DeliveryClass::Critical {
                    self.critical_seen += 1;
                }
            }
        }
    }

    /// Sends a request that must succeed, returning the decoded reply. Frames that are not the
    /// reply — the events a subscribed session receives — are read and kept, never discarded.
    /// A rate-limited refusal is waited out and retried, exactly as told: a room's setup is a
    /// burst of joins and subscribes inside the limiter's window, and "retry in N ms" is the
    /// server talking, not the server broken.
    async fn ask<M: Encode, R: Decode>(&mut self, opcode: Opcode, message: &M) -> R {
        self.correlation += 1;
        let correlation = self.correlation;
        let mut backoff = 0u64;
        for _ in 0..5 {
            if backoff > 0 {
                tokio::time::sleep(Duration::from_millis(backoff + 100)).await;
            }
            send(&mut self.stream, opcode, correlation, message).await;
            loop {
                let frame = self.next_frame().await;
                if frame.header.correlation != correlation {
                    continue;
                }
                if !frame.header.is_error() {
                    return from_frame(&frame).expect("the reply decodes");
                }
                let refusal: migo_protocol::Error =
                    from_frame(&frame).expect("the refusal decodes");
                assert!(
                    refusal.code == migo_protocol::codes::RATE_LIMITED,
                    "the request was refused outright: {refusal:?}"
                );
                backoff = u64::from(refusal.retry_after_ms.unwrap_or(1000));
                break;
            }
        }
        panic!("the request never succeeds even after backing off as instructed");
    }

    /// Subscribes a set of topics, asserting every one was accepted.
    async fn subscribe(&mut self, topics: &[Topic]) {
        let confirmation: SubscribeResponse = self
            .ask(
                Opcode::Subscribe,
                &SubscribeRequest {
                    topics: topics.to_vec(),
                },
            )
            .await;
        assert_eq!(
            confirmation.accepted.len(),
            topics.len(),
            "every asked topic is granted to a member: {:?}",
            confirmation
        );
    }

    /// Joins a room (or re-joins it — the call is idempotent), returning the handle.
    async fn join_room(&mut self, room_id: Id) -> RoomJoinResponse {
        self.ask(
            Opcode::RoomJoin,
            &RoomJoinRequest {
                room_id,
                invite_code: None,
            },
        )
        .await
    }

    /// Creates a room and returns the join handle the wire answers creation with.
    async fn create_room(&mut self, slug: &str, name: &str) -> RoomJoinResponse {
        self.ask(
            Opcode::RoomCreate,
            &RoomCreate {
                slug: slug.to_string(),
                name: name.to_string(),
                kind: 1, // RoomKind::Public
                topic: None,
                max_members: None,
            },
        )
        .await
    }

    /// Sends one sealed message into a room's conversation.
    async fn send_room_message(&mut self, conversation_id: Id, nonce: u128) {
        let message_id = Id::from(nonce);
        let accepted: migo_protocol::MessageAccepted = self
            .ask(
                Opcode::MessageSend,
                &MessageSend {
                    message_id,
                    conversation_id,
                    kind: MessageKind::Text,
                    envelope: SEALED.to_vec(),
                    reply_to: None,
                    expires_in_ms: None,
                    sender_key_id: None,
                },
            )
            .await;
        assert_eq!(accepted.message_id, message_id);
    }

    /// Drops the connection the way a flaky network does: abruptly, with a reset rather than a
    /// FIN, so the server reads a transport error and not a client saying "I am done" — the
    /// difference between a resume the gateway retains (section 150) and one it keeps nothing
    /// for.
    // `set_linger` is deprecated for the nonzero timeouts, which block the thread on drop; a
    // zero-second linger is exactly the RST this severs with and blocks nothing, and there is
    // no other portable way to ask the kernel for one.
    #[allow(deprecated)]
    fn sever(self) {
        // A zero-second linger turns the close into an RST: the kernel discards the unsent
        // tail and the peer's read fails, which is the involuntary death the retention exists
        // for. On a clean FIN the server would rightly keep nothing.
        let _ = self.stream.set_linger(Some(Duration::from_secs(0)));
        drop(self.stream);
    }
}

/// Reads from a session until a frame of the wanted opcode arrives, skipping the unrelated
/// events a subscribed session receives — including the ring's replay after a resume, which is
/// exactly the traffic a resumed session wades through before the frames it is owed.
async fn next_event_of(session: &mut LiveSession, want: Opcode) -> Frame {
    loop {
        let frame = session.next_frame().await;
        if Opcode::from_wire(frame.header.opcode) == Some(want) {
            return frame;
        }
    }
}

/// Asserts that nothing at all arrives on the socket for the quiet window: any frame — any
/// opcode, any correlation — is a frame the server had no business sending.
async fn assert_quiet(session: &mut LiveSession) {
    let outcome = tokio::time::timeout(QUIET, async {
        let mut head = [0u8; 4];
        session
            .stream
            .read_exact(&mut head)
            .await
            .expect("the length of a stray frame arrives");
        let len = u32::from_be_bytes(head) as usize;
        let mut body = vec![0u8; len];
        session
            .stream
            .read_exact(&mut body)
            .await
            .expect("the body of a stray frame arrives");
        Frame::decode(Bytes::from(body)).expect("the stray frame decodes")
    })
    .await;
    if let Ok(frame) = outcome {
        panic!(
            "a frame arrived for a session that had no business hearing one: {:?}",
            Opcode::from_wire(frame.header.opcode)
        );
    }
}

/// Asserts that no frame the room's two topics carry arrives for the quiet window. Unlike
/// [`assert_quiet`], unrelated frames are tolerated, because a session that legitimately holds
/// its *own* user topic still hears the account's presence move — the assertion here is about
/// the room, whose topics a departed member must not be handed back by a resume.
async fn assert_no_room_frames(session: &mut LiveSession) {
    let outcome = tokio::time::timeout(QUIET, async {
        loop {
            let mut head = [0u8; 4];
            session
                .stream
                .read_exact(&mut head)
                .await
                .expect("the length of an arriving frame is readable");
            let len = u32::from_be_bytes(head) as usize;
            let mut body = vec![0u8; len];
            session
                .stream
                .read_exact(&mut body)
                .await
                .expect("the body of an arriving frame is readable");
            let frame = Frame::decode(Bytes::from(body)).expect("the frame decodes");
            if matches!(
                Opcode::from_wire(frame.header.opcode),
                Some(
                    Opcode::MessageEvent
                        | Opcode::MessageReceipt
                        | Opcode::RoomMemberEvent
                        | Opcode::RoomStateEvent
                )
            ) {
                return frame;
            }
        }
    })
    .await;
    if let Ok(frame) = outcome {
        panic!(
            "a room frame arrived for a session whose membership was gone: {:?}",
            Opcode::from_wire(frame.header.opcode)
        );
    }
}

/// What a reconnect needs from the connection that died: the session id to name and the
/// Critical-frame watermark to claim. Read before the socket is severed, because after it only
/// these two facts remain meaningful.
struct DroppedSession {
    session_id: Id,
    critical_seen: u64,
}

/// Reconnects the way the SDK does after a connection-level failure: HELLO carrying the
/// dropped session's id and the Critical-frame watermark the connection had honestly reached.
/// Retries while the server still holds the old session live — the reset takes a moment to be
/// read — and panics if the resume never lands, because every test that calls this is a test
/// of the resume itself.
async fn resume_session(addr: SocketAddr, grant: &Grant, dropped: &DroppedSession) -> LiveSession {
    for _ in 0..50 {
        let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting does not stall")
            .expect("the connection is accepted");
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            // The resumed session asks for the rooms family too: it will re-join, leave, and
            // hear the room's frames, and the feature it negotiated is a fact of the session —
            // a fresh HELLO on the reconnect negotiates from zero, not from what the dead
            // session had.
            features: migo_protocol::features::ROOMS,
            access_token: Some(grant.access_token.clone()),
            device_id: Some(grant.device_id),
            resume: Some(ResumeRequest {
                session_id: dropped.session_id,
                last_frame_seq: dropped.critical_seen,
            }),
            ..Default::default()
        };
        send(&mut stream, Opcode::Hello, 1, &hello).await;
        let answer = recv_within(&mut stream, STEP)
            .await
            .expect("the resume answer does not stall");
        if Opcode::from_wire(answer.header.opcode) == Some(Opcode::Hello)
            && !answer.header.is_error()
        {
            let welcome: Welcome = from_frame(&answer).expect("the WELCOME decodes");
            assert_eq!(
                welcome.resumed,
                Some(true),
                "a watermark the ring covers resumes the session"
            );
            assert_eq!(
                welcome.authenticated_user,
                Some(grant.account_id),
                "the inline token authenticated the resumed session"
            );
            return LiveSession {
                stream,
                session_id: welcome.session_id,
                // As on a fresh connect: the resumed WELCOME is not sequenced either.
                critical_seen: 0,
                correlation: 1,
            };
        }
        // Not resumable yet — most likely the reset has not been read — so drop this attempt
        // without lingering and let the old session finish dying. A rate-limited attempt
        // backs off as told instead: hammering the limiter the resume is waiting behind would
        // only extend its window.
        let wait = from_frame::<migo_protocol::Error>(&answer)
            .ok()
            .and_then(|refusal| refusal.retry_after_ms.map(u64::from))
            .map_or(100, |told| told + 100);
        drop(stream);
        tokio::time::sleep(Duration::from_millis(wait)).await;
    }
    panic!("the dropped session never became resumable — the server did not retain it");
}

/// The stage the room tests stand on: a founder's room, a second member, a third member, all
/// three subscribed to both of the room's topics, and the grants (a reconnect needs the one
/// whose session died).
struct Stage {
    founder: LiveSession,
    second: LiveSession,
    third: LiveSession,
    room_id: Id,
    conversation_id: Id,
    second_grant: Grant,
}

/// Builds the stage: the founder creates the room, the second and third accounts join it, and
/// everyone subscribes both topics — the state every room member's client reaches.
async fn staged_room(
    app: &App,
    founder_name: &str,
    second_name: &str,
    third_name: &str,
    slug: &str,
) -> Stage {
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(app, founder_name).await;
    let second = registered_grant(app, second_name).await;
    let third = registered_grant(app, third_name).await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let created = founder_session.create_room(slug, "The Wire Room").await;
    let (room_id, conversation_id) = (created.room.room_id, created.conversation_id);
    let both = |room: Id, conversation: Id| {
        vec![
            Topic {
                kind: TopicKind::Room,
                id: room,
            },
            Topic {
                kind: TopicKind::Conversation,
                id: conversation,
            },
        ]
    };

    founder_session
        .subscribe(&both(room_id, conversation_id))
        .await;

    let mut second_session = LiveSession::connect(addr, &second).await;
    second_session.join_room(room_id).await;
    second_session
        .subscribe(&both(room_id, conversation_id))
        .await;

    let mut third_session = LiveSession::connect(addr, &third).await;
    third_session.join_room(room_id).await;
    third_session
        .subscribe(&both(room_id, conversation_id))
        .await;

    Stage {
        founder: founder_session,
        second: second_session,
        third: third_session,
        room_id,
        conversation_id,
        second_grant: second,
    }
}

#[tokio::test]
async fn a_leaver_hears_nothing_the_room_says_after_the_acknowledgement() {
    let app = build_app().await;
    let mut stage = staged_room(
        &app,
        "quietfounder",
        "quietleaver",
        "quietstayer",
        "quiet-wire",
    )
    .await;
    let (room_id, conversation_id) = (stage.room_id, stage.conversation_id);

    // The leave, answered after the teardown it orders: the acknowledgement is the moment the
    // caller has been told "you are out", and everything after it is the silence under test.
    // The ask consumes whatever the room published before it — the later joins' member events,
    // the leaver's own doorbell — so the socket is quiet when the ack lands.
    let acknowledged: Acknowledged = stage
        .second
        .ask(Opcode::RoomLeave, &RoomLeaveRequest { room_id })
        .await;
    assert!(acknowledged.ok, "the leave is acknowledged");

    // A message into the room's conversation. The founder — still a member, still subscribed —
    // is the control: the fan-out the leaver must not hear is proven live by the member who
    // must hear it.
    stage
        .third
        .send_room_message(conversation_id, 0xA11CE)
        .await;
    let heard = next_event_of(&mut stage.founder, Opcode::MessageEvent).await;
    let message: migo_protocol::MessageEvent = from_frame(&heard).expect("the message decodes");
    assert_eq!(message.conversation_id, conversation_id);
    assert_eq!(message.envelope, SEALED, "the control heard the message");
    assert_quiet(&mut stage.second).await;

    // A state delta onto the room's own topic — the other half of the pair a leave must take
    // away. The stayer hears the new topic line; the leaver, again, must hear nothing.
    let _: Acknowledged = stage
        .founder
        .ask(
            Opcode::RoomUpdate,
            &RoomUpdate {
                room_id,
                name: None,
                topic: Some("the wire room's new topic".to_string()),
                slow_mode_ms: None,
            },
        )
        .await;
    let delta_frame = next_event_of(&mut stage.third, Opcode::RoomStateEvent).await;
    let delta: RoomStateEvent = from_frame(&delta_frame).expect("the state delta decodes");
    assert_eq!(delta.room_id, room_id);
    assert_eq!(
        delta.topic.as_deref(),
        Some("the wire room's new topic"),
        "the control heard the rename"
    );
    assert_quiet(&mut stage.second).await;
}

#[tokio::test]
async fn a_room_join_rings_the_joiner_s_other_session_and_the_room_follows() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "bellfounder").await;
    let joiner = registered_grant(&app, "belljoiner").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let created = founder_session
        .create_room("bell-wire", "The Bell Room")
        .await;
    let (room_id, conversation_id) = (created.room.room_id, created.conversation_id);
    founder_session
        .subscribe(&[
            Topic {
                kind: TopicKind::Room,
                id: room_id,
            },
            Topic {
                kind: TopicKind::Conversation,
                id: conversation_id,
            },
        ])
        .await;

    // Two sessions of the joining account. The second has never heard of the room — it holds
    // only its own user topic, which is precisely the deafness the doorbell exists to cure.
    let mut joining_device = LiveSession::connect(addr, &joiner).await;
    let mut other_device = LiveSession::connect(addr, &joiner).await;

    joining_device.join_room(room_id).await;

    // The bell: the join event, published to the joiner's user topic, is the one frame that
    // reaches a session that cannot be a subscriber of the room yet.
    let bell_frame = next_event_of(&mut other_device, Opcode::RoomMemberEvent).await;
    let bell: RoomMemberEvent = from_frame(&bell_frame).expect("the bell decodes");
    assert_eq!(bell.room_id, room_id);
    assert_eq!(bell.user_id, joiner.account_id, "the bell names the joiner");
    assert_eq!(bell.change, Some(MemberChange::Joined));

    // The reaction a client makes: the idempotent re-join (the account is already a member, so
    // the room hears nothing — section 156's no-fanout rule) and the subscriptions the
    // reaction derives from the handle it answers with.
    let rejoined = other_device.join_room(room_id).await;
    assert_eq!(rejoined.conversation_id, conversation_id);
    other_device
        .subscribe(&[
            Topic {
                kind: TopicKind::Room,
                id: room_id,
            },
            Topic {
                kind: TopicKind::Conversation,
                id: conversation_id,
            },
        ])
        .await;

    // And the point of the whole exercise: the room's next message reaches the device that
    // never asked to join, live, because the bell told it to listen.
    founder_session
        .send_room_message(conversation_id, 0xBE11)
        .await;
    let heard = next_event_of(&mut other_device, Opcode::MessageEvent).await;
    let message: migo_protocol::MessageEvent = from_frame(&heard).expect("the message decodes");
    assert_eq!(message.conversation_id, conversation_id);
    assert_eq!(message.envelope, SEALED);
}

#[tokio::test]
async fn a_resumed_session_keeps_hearing_the_room_without_resubscribing() {
    let app = build_app().await;
    let mut stage = staged_room(
        &app,
        "resumfounder",
        "resumroamer",
        "resumstayer",
        "resum-wire",
    )
    .await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");

    // The drop: a transport death, not a goodbye, so the gateway retains the ring *and* the
    // topics for the reconnect (sections 150/158). The drain first, so the watermark the
    // resume claims is everything the connection ever received.
    stage.second.drain().await;
    let dropped = DroppedSession {
        session_id: stage.second.session_id,
        critical_seen: stage.second.critical_seen,
    };
    stage.second.sever();

    // The reconnect resumes — and sends no SUBSCRIBE. Not for the room, not for the
    // conversation, not even the user topic every fresh session takes. Section 158's promise
    // is that none is needed, and the frames below are the proof: delivery must continue past
    // the replayed ring on the strength of what the server alone restores.
    let mut resumed = resume_session(addr, &stage.second_grant, &dropped).await;

    // The room talks: a message on the conversation, a delta on the room's own topic.
    stage
        .founder
        .send_room_message(stage.conversation_id, 0x5EED)
        .await;
    let heard = next_event_of(&mut resumed, Opcode::MessageEvent).await;
    let message: migo_protocol::MessageEvent = from_frame(&heard).expect("the message decodes");
    assert_eq!(message.conversation_id, stage.conversation_id);
    assert_eq!(message.envelope, SEALED);

    let _: Acknowledged = stage
        .founder
        .ask(
            Opcode::RoomUpdate,
            &RoomUpdate {
                room_id: stage.room_id,
                name: None,
                topic: Some("resumed rooms still hear state".to_string()),
                slow_mode_ms: None,
            },
        )
        .await;
    let delta_frame = next_event_of(&mut resumed, Opcode::RoomStateEvent).await;
    let delta: RoomStateEvent = from_frame(&delta_frame).expect("the state delta decodes");
    assert_eq!(delta.room_id, stage.room_id);
}

#[tokio::test]
async fn a_resume_does_not_hand_back_the_room_a_departed_member_lost() {
    let app = build_app().await;
    let mut stage = staged_room(&app, "leftfounder", "leftroamer", "leftstayer", "left-wire").await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");

    // The member's session dies mid-membership — retained, resumable.
    stage.second.drain().await;
    let dropped = DroppedSession {
        session_id: stage.second.session_id,
        critical_seen: stage.second.critical_seen,
    };
    stage.second.sever();

    // ...and while it is down, the account leaves through another session. The seat empties
    // server-side; the retained topic list is now a claim authorisation must refuse, because a
    // reconnect is not a way back into a room the account walked out of.
    //
    // The pause first: the reset has to be *read* before the leave's fan-out could ever reach
    // the dead session's mailbox, or the member event would ride the retained ring into the
    // resume and the test would measure a race instead of the rule.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut leaver_device = LiveSession::connect(addr, &stage.second_grant).await;
    leaver_device.join_room(stage.room_id).await;
    let acknowledged: Acknowledged = leaver_device
        .ask(
            Opcode::RoomLeave,
            &RoomLeaveRequest {
                room_id: stage.room_id,
            },
        )
        .await;
    assert!(acknowledged.ok, "the other device's leave is acknowledged");

    // The resume still lands — the ring bridges the gap, the session is the old one — but the
    // topics it held are re-asked, not trusted, and a departed member's ask is refused.
    let mut resumed = resume_session(addr, &stage.second_grant, &dropped).await;

    // The room talks, and the resumed session of a departed member must not hear it: neither
    // the conversation's messages nor the room's own state.
    stage
        .founder
        .send_room_message(stage.conversation_id, 0xD00D)
        .await;
    let _: Acknowledged = stage
        .founder
        .ask(
            Opcode::RoomUpdate,
            &RoomUpdate {
                room_id: stage.room_id,
                name: None,
                topic: Some("departed members hear none of this".to_string()),
                slow_mode_ms: None,
            },
        )
        .await;
    assert_no_room_frames(&mut resumed).await;
}
