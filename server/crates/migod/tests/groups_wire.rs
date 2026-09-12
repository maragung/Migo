//! The group lifecycle answered on the wire: the two frames a participant
//! cannot hear through the conversation topic alone.
//!
//! The conversation topic is the audience every group event publishes to, and
//! it is the wrong audience for exactly two members of it:
//!
//! * **The invited.** A member who has just been added cannot be subscribed to
//!   a topic their client has never heard of, so the `Joined` event the group
//!   hears was a frame the invitee could not receive — their list learned
//!   nothing until the next full refresh. The dispatcher now publishes the
//!   same event to the joined account's *user* topic, the one every session
//!   subscribes to at its handshake; the first test drives a real invite over
//!   TCP and asserts the invited session hears `Joined` naming itself.
//! * **The renamer.** The conversation copy of a state delta excludes the
//!   device that asked for it (its request was answered by the reply), so a
//!   client that renders nothing until the server says so kept showing the
//!   title it renamed away. The dispatcher now publishes the delta to the
//!   actor's user topic *including* that device; the second test renames over
//!   the wire and asserts the renaming session itself hears the new title.
//!
//! Both tests use the reply rule as their clock: every frame they wait for is
//! one the server owes somebody, so the timeout is the assertion.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Clock, Config, Secret};
use migo_protocol::{
    from_frame, to_frame, ConversationCreateRequest, ConversationInviteRequest, ConversationKind,
    ConversationMemberEvent, ConversationStateEvent, ConversationUpdateRequest, Encode, Frame,
    Hello, MemberChange, MessageKind, MessageSend, Opcode, Platform, ReactionSet, SubscribeRequest,
    SubscribeResponse, Topic, TopicKind, Welcome, PROTOCOL_VERSION,
};
use migod::App;

/// How long any single exchange may take before the test declares the server
/// stuck — silence being the bug class both tests exist to catch.
const STEP: Duration = Duration::from_secs(5);

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
                device: DeviceClaim::new(Platform::Web, "group wire test"),
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

/// A live authenticated TCP session: HELLO with the grant's inline token, then
/// the SUBSCRIBE every client sends for its own user topic — the topic these
/// tests are about, because it is the one topic a member who has never heard
/// of a conversation is already listening on.
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
                let confirmation: SubscribeResponse =
                    from_frame(&frame).expect("the SUBSCRIBE reply decodes");
                assert_eq!(
                    confirmation.accepted.len(),
                    1,
                    "the own user topic is accepted"
                );
                return Self { stream };
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
async fn next_event_of(stream: &mut tokio::net::TcpStream, want: Opcode) -> Frame {
    loop {
        let frame = recv_within(stream, STEP).await;
        if Opcode::from_wire(frame.header.opcode) == Some(want) {
            return frame;
        }
    }
}

#[tokio::test]
async fn an_invited_member_hears_the_join_on_their_own_topic() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "groupfounder").await;
    let witness = registered_grant(&app, "groupwitness").await;
    let invited = registered_grant(&app, "groupinvitee").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    // The invited session is *only* on its own user topic — it has never heard
    // of the conversation, which is precisely the deafness under test.
    let mut invited_session = LiveSession::connect(addr, &invited).await;

    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            11,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![witness.account_id],
                title: Some("The Wire Group".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    let _: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationInvite,
            12,
            &ConversationInviteRequest {
                conversation_id,
                members: vec![invited.account_id],
            },
        )
        .await;

    // The frame the conversation topic could never deliver: the invitee is not
    // a subscriber of a topic they have never loaded, so this arrives on their
    // own user topic or not at all.
    let frame = next_event_of(&mut invited_session.stream, Opcode::ConversationMemberEvent).await;
    let event: ConversationMemberEvent = from_frame(&frame).expect("the join decodes");
    assert_eq!(event.conversation_id, conversation_id);
    assert_eq!(
        event.user_id, invited.account_id,
        "the join names the invited"
    );
    assert_eq!(event.change, MemberChange::Joined);
}

#[tokio::test]
async fn the_renamer_s_own_device_hears_the_new_title() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "grouprenamer").await;
    let invited = registered_grant(&app, "renamewitness").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;

    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            21,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![invited.account_id],
                title: Some("Before".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    let _: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationUpdate,
            22,
            &ConversationUpdateRequest {
                conversation_id,
                title: Some("After".to_string()),
            },
        )
        .await;

    // The conversation copy of this delta excludes the renaming connection, so
    // the only copy this session can receive is the actor's user-topic one —
    // which is the fix: a client that renders nothing until the server says so
    // still gets the new title.
    let frame = next_event_of(&mut founder_session.stream, Opcode::ConversationStateEvent).await;
    let event: ConversationStateEvent = from_frame(&frame).expect("the rename decodes");
    assert_eq!(event.conversation_id, conversation_id);
    assert_eq!(event.title.as_deref(), Some("After"));
}

#[tokio::test]
async fn a_retried_reaction_is_one_reaction_not_two() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let sender = registered_grant(&app, "reactioner").await;
    let witness = registered_grant(&app, "reactionwitness").await;

    let mut sender_session = LiveSession::connect(addr, &sender).await;
    let mut witness_session = LiveSession::connect(addr, &witness).await;

    // Default message privacy accepts friends only, so the two accounts are
    // made friends first — through the domain service, as the dispatcher tests
    // do it, because the wire path here is the reaction, not the friendship.
    let friender = migo_social::Caller::new(
        sender.account_id,
        sender.device_id,
        migo_ratelimit::TrustTier::Established,
        app.clock.now(),
    );
    let accepter = migo_social::Caller::new(
        witness.account_id,
        witness.device_id,
        migo_ratelimit::TrustTier::Established,
        app.clock.now(),
    );
    app.social
        .request_friend(&friender, witness.account_id)
        .await
        .expect("a friend request between fresh accounts must be taken");
    app.social
        .respond_friend(&accepter, sender.account_id, true)
        .await
        .expect("the friend request must be accepted");

    // A direct conversation between the two: the smallest audience a reaction
    // can travel to, and the one both sessions can subscribe to.
    let summary: migo_protocol::ConversationSummary = sender_session
        .ask(
            Opcode::ConversationCreate,
            31,
            &ConversationCreateRequest {
                kind: ConversationKind::Direct,
                members: vec![witness.account_id],
                title: None,
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    // The witness listens on the conversation's own topic, which is where a
    // message's fanout goes — and where a duplicate reaction would land twice.
    let _: migo_protocol::SubscribeResponse = witness_session
        .ask(
            Opcode::Subscribe,
            31,
            &SubscribeRequest {
                topics: vec![Topic {
                    kind: TopicKind::Conversation,
                    id: conversation_id,
                }],
            },
        )
        .await;

    // The message to react to. The witness sees it too; that frame is drained
    // below so the count that matters starts clean.
    let message_id = migo_core::Id::generate(
        app.clock.now().as_unix_ms().max(0) as u64,
        &mut migo_core::OsRandom,
    );
    let accepted: migo_protocol::MessageAccepted = sender_session
        .ask(
            Opcode::MessageSend,
            32,
            &MessageSend {
                message_id,
                conversation_id,
                kind: MessageKind::Text,
                envelope: b"the message being reacted to".to_vec(),
                reply_to: None,
                expires_in_ms: None,
                sender_key_id: None,
            },
        )
        .await;
    assert!(!accepted.duplicate.unwrap_or(false), "the message is new");

    // Identical fields, twice — exactly what a client retry after a timeout
    // re-sends. The handler derives the reaction's message id from these bytes
    // rather than minting one, so both deliveries must converge on one row.
    let reaction = ReactionSet {
        target_message_id: message_id,
        conversation_id,
        envelope: b"a sealed reaction".to_vec(),
    };
    let first: migo_protocol::Acknowledged =
        sender_session.ask(Opcode::ReactionSet, 33, &reaction).await;
    assert!(first.ok, "the first reaction is accepted");
    let second: migo_protocol::Acknowledged =
        sender_session.ask(Opcode::ReactionSet, 34, &reaction).await;
    assert!(second.ok, "the retry is accepted — a retry is a success");

    // The witness's topic saw the plain message, then must see exactly one
    // reaction event. A second distinct reaction event is the bug: it would
    // render the reaction twice and notify twice for one user action.
    let mut reactions = 0;
    let mut saw_plain_message = false;
    let deadline = tokio::time::timeout(STEP, async {
        loop {
            let frame = recv_within(&mut witness_session.stream, STEP).await;
            if Opcode::from_wire(frame.header.opcode) == Some(Opcode::MessageEvent) {
                let event: migo_protocol::MessageEvent =
                    from_frame(&frame).expect("the message event decodes");
                if event.reply_to == Some(message_id) {
                    reactions += 1;
                } else {
                    saw_plain_message = true;
                }
            }
            if saw_plain_message && reactions >= 1 {
                return;
            }
        }
    })
    .await;
    assert!(
        deadline.is_ok(),
        "the witness saw the message and a reaction within the step budget"
    );
    assert_eq!(
        reactions, 1,
        "a retried reaction is delivered once — the retry converged on the stored row"
    );
}

#[tokio::test]
async fn a_created_conversation_names_its_members_on_their_own_topics() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "dmfounder").await;
    let peer = registered_grant(&app, "dmpeer").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let mut peer_session = LiveSession::connect(addr, &peer).await;

    // A direct conversation created by the founder. Before the fix, the create
    // produced no fanout at all: the peer was a member of a conversation their
    // client had never heard of, so the first message sealed for an audience
    // that could not be subscribed to the topic.
    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            31,
            &ConversationCreateRequest {
                kind: ConversationKind::Direct,
                members: vec![peer.account_id],
                title: None,
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    // The frame the conversation topic could never deliver: the peer is not a
    // subscriber of a topic they have never loaded, so this arrives on their
    // own user topic or not at all — the same door the invite path uses.
    let frame = next_event_of(&mut peer_session.stream, Opcode::ConversationMemberEvent).await;
    let event: ConversationMemberEvent = from_frame(&frame).expect("the join decodes");
    assert_eq!(event.conversation_id, conversation_id);
    assert_eq!(event.user_id, peer.account_id, "the join names the peer");
    assert_eq!(event.change, MemberChange::Joined);
}
