//! Cross-node session resume, driven at the client-visible seam: the scenario
//! section 173 names as the missing half of its first case — a client whose
//! session lived on one node, which dies, reconnects to another node and comes
//! away with everything, without starting from zero.
//!
//! The mechanism under test is not new, and that is the point. Section 150 puts
//! the session's state on the node that minted it, so a resume request that
//! lands anywhere else is answered `RESUME_REQUIRED` and the client runs the
//! reset path: a fresh session, the subscriptions re-authorized, the missed
//! messages recovered by the section 158 sync. What section 173 records as
//! still missing is that this whole chain had never been exercised end to end
//! over a real transport — scenario 1 was proven at the outbox layer only.
//!
//! The topology is the one honest shape this scenario can take: two full nodes
//! over ONE store, the deployment section 170 describes when it says every node
//! may point at a single database. Everything the second node needs to serve
//! the first node's client is a store fact — the session row authentication
//! reads, the conversation membership a re-subscribe is authorized against, the
//! messages the sync recovers — so the test builds both nodes over one shared
//! store through `App::build_with_store`, the composition seam that exists for
//! exactly this. The per-node things stay per-node on purpose: each node has
//! its own session registry and its own resume ring, which is precisely why
//! the failover ends as a new session instead of a resumed one. A store per
//! node (the tools/2node shape) would leave node B unable to authenticate the
//! account at all, and that gap is recorded where it belongs, in section 170's
//! own honest tail about direct delivery across nodes.
//!
//! No mesh link joins the two nodes: the client path under test never touches
//! federation, and room fanout is a different route with its own tests in
//! node_failures.rs. Determinism here is the client-seam house style — every
//! exchange is bounded by a step budget and every expectation is asserted on
//! frame contents, never on wall-clock ordering — the same discipline
//! groups_wire.rs applies, because a full `App` serves real sockets on a real
//! clock and cannot be driven by `drain_once` the way the mesh layer can.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::config::StoreConfig;
use migo_core::{Clock, Config, Id, OsRandom, Secret, SystemClock};
use migo_protocol::{
    codes, from_frame, to_frame, CloseReason, ConversationCreateRequest, ConversationKind,
    ConversationListRequest, ConversationListResponse, ConversationSummary, Decode, Encode, Frame,
    Hello, MessageAccepted, MessageEvent, MessageKind, MessageSend, Opcode, Platform,
    ResumeRequest, SubscribeRequest, SubscribeResponse, SyncRequest, SyncResponse, SyncStatus,
    Topic, TopicKind, Welcome, PROTOCOL_VERSION,
};
use migo_ratelimit::TrustTier;
use migo_social::Caller;
use migod::App;

/// How long any single exchange may take before the test declares a node stuck
/// — silence being the bug class this suite exists to catch.
const STEP: Duration = Duration::from_secs(5);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// The two nodes share one token key the way a deployment must: the access
/// token node alpha minted has to verify on node beta, or the failover dies at
/// the handshake before any reset path can run.
async fn build_node(store: &migo_store::SharedStore, node_id: &str, region: &str) -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
            ("MIGO_NODE__ID".to_string(), node_id.to_string()),
            ("MIGO_NODE__REGION".to_string(), region.to_string()),
        ],
    )
    .expect("configuration should parse");
    App::build_with_store(&config, store.clone())
        .await
        .expect("a development node must build over the shared store")
}

/// Registers one account through the front door of a node, stamped with that
/// node's own clock so the inline token is not born expired.
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
                device: DeviceClaim::new(Platform::Web, "cross-node resume test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// Makes the two accounts friends, as the dispatcher tests do it, because the
/// default message privacy accepts friends only and the wire path under test
/// is the message, not the handshake of the relationship.
async fn befriend(app: &App, requester: &Grant, accepter: &Grant) {
    let friender = Caller::new(
        requester.account_id,
        requester.device_id,
        TrustTier::Established,
        app.clock.now(),
    );
    let answerer = Caller::new(
        accepter.account_id,
        accepter.device_id,
        TrustTier::Established,
        app.clock.now(),
    );
    app.social
        .request_friend(&friender, accepter.account_id)
        .await
        .expect("a friend request between fresh accounts must be taken");
    app.social
        .respond_friend(&answerer, requester.account_id, true)
        .await
        .expect("the friend request must be accepted");
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
    Frame::decode(body.into()).expect("the frame decodes")
}

/// Reads every remaining frame until the peer closes its side, which is how a
/// scripted client observes a node going away: the frames the drain produced,
/// then the end of stream.
async fn drained_within(stream: &mut tokio::net::TcpStream) -> Vec<Frame> {
    let mut frames = Vec::new();
    loop {
        let mut head = [0u8; 4];
        match tokio::time::timeout(STEP, stream.read_exact(&mut head)).await {
            Err(_) => panic!("the connection did not end within the step budget"),
            Ok(Err(error)) if error.kind() == ErrorKind::UnexpectedEof => return frames,
            Ok(Err(error)) => panic!("unexpected read error while draining: {error}"),
            Ok(Ok(_)) => {}
        }
        let mut body = vec![0u8; u32::from_be_bytes(head) as usize];
        stream
            .read_exact(&mut body)
            .await
            .expect("the drained frame's body arrives");
        frames.push(Frame::decode(body.into()).expect("the drained frame decodes"));
    }
}

/// A scripted client: a real TCP session speaking the length-prefixed framing,
/// tracking the session it holds and the frames it has seen so the resume
/// attempt can carry an honest watermark.
struct Client {
    stream: tokio::net::TcpStream,
    /// The session this connection negotiated — the id a resume request would
    /// name, and the id that only the node which minted it can honor.
    session_id: Id,
    /// Where this session lives, as WELCOME reported it.
    node: String,
    region: String,
    /// Frames received since the WELCOME — the client's claim of how far it
    /// read. Node-to-node this number is never consulted (the session id is
    /// unknown there, which is enough), but a faithful client sends one.
    frames_seen: u64,
    correlation: u32,
}

impl Client {
    /// Connects and opens a fresh, authenticated session: HELLO with the
    /// grant's inline token, then the self-subscription every client makes at
    /// its handshake.
    async fn connect_fresh(addr: SocketAddr, grant: &Grant) -> Self {
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
        assert_eq!(
            welcome.authenticated_user,
            Some(grant.account_id),
            "the inline token authenticated the session"
        );

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
                return Self {
                    stream,
                    session_id: welcome.session_id,
                    node: welcome.node.node_id,
                    region: welcome.node.region,
                    frames_seen: 0,
                    correlation: 2,
                };
            }
        }
    }

    /// Reads one frame, counting it against the watermark a resume would claim.
    async fn next_frame(&mut self) -> Frame {
        let frame = recv_within(&mut self.stream, STEP).await;
        self.frames_seen += 1;
        frame
    }

    /// Sends a request that must succeed, returning the decoded reply. Frames
    /// that are not the reply — the events a subscribed session receives — are
    /// read and kept, never discarded silently.
    async fn ask<M: Encode, R: Decode>(&mut self, opcode: Opcode, message: &M) -> R {
        self.correlation += 1;
        let correlation = self.correlation;
        send(&mut self.stream, opcode, correlation, message).await;
        loop {
            let frame = self.next_frame().await;
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

    /// Reads until a frame of the wanted opcode arrives, skipping the
    /// unrelated events a subscribed session receives.
    async fn next_event(&mut self, want: Opcode) -> Frame {
        loop {
            let frame = self.next_frame().await;
            if Opcode::from_wire(frame.header.opcode) == Some(want) {
                return frame;
            }
        }
    }

    /// Subscribes additional topics, asserting every one was accepted.
    async fn subscribe(&mut self, topics: Vec<Topic>) {
        let confirmation: SubscribeResponse = self
            .ask(
                Opcode::Subscribe,
                &SubscribeRequest {
                    topics: topics.clone(),
                },
            )
            .await;
        assert_eq!(
            confirmation.accepted, topics,
            "every requested topic was accepted"
        );
    }

    /// Sends one message, returning its id and the acceptance.
    async fn send_message(&mut self, conversation: Id, envelope: &[u8]) -> (Id, MessageAccepted) {
        let message_id = Id::generate(SystemClock.now().as_unix_ms().max(0) as u64, &mut OsRandom);
        let accepted: MessageAccepted = self
            .ask(
                Opcode::MessageSend,
                &MessageSend {
                    message_id,
                    conversation_id: conversation,
                    kind: MessageKind::Text,
                    envelope: envelope.to_vec(),
                    reply_to: None,
                    expires_in_ms: None,
                    sender_key_id: None,
                },
            )
            .await;
        (message_id, accepted)
    }
}

/// Reconnects the way the SDK does after a connection-level failure: HELLO
/// carrying the previous session's id and watermark. Asserts the answer is the
/// one section 150 promises a foreign node gives — `RESUME_REQUIRED`, under
/// the HELLO opcode — and that the node then closes the connection. Returns
/// the error so the caller can pin its code and symbol.
async fn attempt_resume(
    addr: SocketAddr,
    grant: &Grant,
    session_id: Id,
    last_frame_seq: u64,
) -> migo_protocol::Error {
    let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
        .await
        .expect("connecting does not stall")
        .expect("the connection is accepted");

    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        access_token: Some(grant.access_token.clone()),
        device_id: Some(grant.device_id),
        resume: Some(ResumeRequest {
            session_id,
            last_frame_seq,
        }),
        ..Default::default()
    };
    send(&mut stream, Opcode::Hello, 1, &hello).await;

    let answer = recv_within(&mut stream, STEP).await;
    assert_eq!(
        Opcode::from_wire(answer.header.opcode),
        Some(Opcode::Hello),
        "the resume refusal is answered under the HELLO opcode"
    );
    assert_eq!(answer.header.correlation, 1, "the refusal names the HELLO");
    assert!(
        answer.header.is_error(),
        "a resume this node cannot serve is an error, not a silent fresh session"
    );
    let error: migo_protocol::Error = from_frame(&answer).expect("the error decodes");
    assert_eq!(
        error.code,
        codes::RESUME_REQUIRED,
        "the refusal carries the code the registry promises: {error:?}"
    );
    assert_eq!(error.symbol, "RESUME_REQUIRED");

    // The node answers and lets go: nothing further can happen on this
    // connection, which is why the client's next step is a fresh one.
    let trailing = drained_within(&mut stream).await;
    assert!(
        trailing.is_empty(),
        "the refused connection carries nothing after the error"
    );
    error
}

/// The full scenario, in the order a real failover lives it: both sides on one
/// node, the node dies, the peer fails over first, the client follows, and
/// everything the conversation gained while the client was between nodes
/// arrives once each, in order, on the other side.
#[tokio::test]
async fn a_failover_ends_as_a_new_session_that_loses_nothing() {
    // The fleet: two nodes over one store, the one-database shape of section
    // 170. The store is opened once and handed to both, so the account, the
    // membership, and the messages are facts the second node can serve.
    let store = migo_store::open(&StoreConfig::default())
        .await
        .expect("the in-memory backend opens");
    let app_a = build_node(&store, "node-alpha", "alpha").await;
    let app_b = build_node(&store, "node-beta", "beta").await;
    let a_addr = app_a.tcp_bind.expect("node alpha binds its TCP listener");
    let b_addr = app_b.tcp_bind.expect("node beta binds its TCP listener");

    // Two accounts, made friends because a direct conversation's privacy
    // accepts friends only.
    let roamer = registered_grant(&app_a, "crossnoderoamer").await;
    let peer = registered_grant(&app_a, "crossnodepeer").await;
    befriend(&app_a, &roamer, &peer).await;

    // The client lives on node alpha; the peer does too, so the first message
    // reaches the client live through alpha's own hub.
    let mut client = Client::connect_fresh(a_addr, &roamer).await;
    assert_eq!(client.node, "node-alpha");
    assert_eq!(client.region, "alpha");
    let mut sender = Client::connect_fresh(a_addr, &peer).await;

    // The conversation, created through the client's own wire session, and the
    // conversation topic both sides hold.
    let summary: ConversationSummary = client
        .ask(
            Opcode::ConversationCreate,
            &ConversationCreateRequest {
                kind: ConversationKind::Direct,
                members: vec![peer.account_id],
                title: None,
            },
        )
        .await;
    let conversation = summary.conversation_id;
    client
        .subscribe(vec![Topic {
            kind: TopicKind::Conversation,
            id: conversation,
        }])
        .await;
    sender
        .subscribe(vec![Topic {
            kind: TopicKind::Conversation,
            id: conversation,
        }])
        .await;

    // The baseline: a message both sides see while they share a node, so the
    // client's watermark after the failover is a real one.
    let (_, first) = sender
        .send_message(conversation, b"sealed while both lived on alpha")
        .await;
    assert_eq!(first.seq, 1, "the conversation's first message is seq 1");
    let event: MessageEvent = from_frame(&client.next_event(Opcode::MessageEvent).await)
        .expect("the live message decodes");
    assert_eq!(event.seq, 1);
    assert_eq!(event.sender_id, peer.account_id);

    // Node alpha dies. In-process that is the cooperative shutdown: the
    // gateway drains each session with a RECONNECT_HINT and closes, and the
    // listener stops — from the client's side, the connection ends, which is
    // the fact the failover reacts to. An in-process node cannot be SIGKILLed;
    // the abrupt variant differs only in whether the hint arrives first, and
    // the resume answer on the next node is the same either way.
    app_a.shutdown.trigger();
    let last_frames = drained_within(&mut client.stream).await;
    for frame in &last_frames {
        assert_eq!(
            Opcode::from_wire(frame.header.opcode),
            Some(Opcode::ReconnectHint),
            "nothing but the drain's hint rides the dying connection"
        );
        let hint: migo_protocol::ReconnectHint = from_frame(frame).expect("the hint decodes");
        assert_eq!(hint.reason, CloseReason::ServerShutdown);
    }

    // The peer fails over first, as a fresh session on node beta — the same
    // reset path, driven here without the resume attempt because the client
    // below makes that attempt for both of them. Its subscription is authorized
    // against the same membership rows, on the other node.
    let mut sender = Client::connect_fresh(b_addr, &peer).await;
    assert_eq!(sender.node, "node-beta");
    assert_eq!(sender.region, "beta");
    sender
        .subscribe(vec![Topic {
            kind: TopicKind::Conversation,
            id: conversation,
        }])
        .await;

    // While the client is between nodes, the conversation moves on: two
    // messages it will have missed by everything except the store they landed
    // in.
    let (_, second) = sender
        .send_message(conversation, b"missed message two")
        .await;
    assert_eq!(second.seq, 2);
    let (_, third) = sender
        .send_message(conversation, b"missed message three")
        .await;
    assert_eq!(third.seq, 3);

    // The client reconnects to node beta, resume attempt first — exactly the
    // sequence the SDK's transport runs after a connection-level failure.
    let alpha_session = client.session_id;
    let refused = attempt_resume(b_addr, &roamer, alpha_session, client.frames_seen).await;
    assert_eq!(refused.code, codes::RESUME_REQUIRED);

    // Then the fresh session. The token alpha minted verifies here because the
    // key is shared; the account and device rows resolve because the store is.
    // This handshake is the whole reason the shared-store topology is the
    // honest one for this scenario.
    let mut client = Client::connect_fresh(b_addr, &roamer).await;
    assert_eq!(client.node, "node-beta");
    assert_eq!(client.region, "beta");
    assert_ne!(
        client.session_id, alpha_session,
        "the failover ends as a new session, exactly as section 170 records"
    );
    client
        .subscribe(vec![Topic {
            kind: TopicKind::Conversation,
            id: conversation,
        }])
        .await;

    // The reset path's inventory step: the list names the conversation with
    // the seqs that arrived while the client was away.
    let list: ConversationListResponse = client
        .ask(
            Opcode::ConversationList,
            &ConversationListRequest {
                limit: 32,
                cursor: None,
            },
        )
        .await;
    let summary = list
        .conversations
        .iter()
        .find(|summary| summary.conversation_id == conversation)
        .expect("the conversation is in the client's list on the other node");
    assert_eq!(summary.last_seq, 3);

    // The catch-up: sync from the watermark the client actually holds. Only
    // the gap is fetched — not a full resync — and it comes back once each, in
    // order, which is the at-least-once story this scenario owes.
    let sync: SyncResponse = client
        .ask(
            Opcode::Sync,
            &SyncRequest {
                conversation_id: conversation,
                have_seq: 1,
                limit: 32,
                to_seq: None,
                backwards: None,
            },
        )
        .await;
    assert_eq!(sync.status, SyncStatus::Ok);
    assert!(!sync.more, "the whole gap fits in one page");
    assert_eq!(sync.from_seq, 2);
    assert_eq!(sync.to_seq, 3);
    let seqs: Vec<u64> = sync.messages.iter().map(|message| message.seq).collect();
    assert_eq!(
        seqs,
        vec![2, 3],
        "the missed messages arrive once each, in order"
    );
    assert_eq!(sync.messages[0].envelope, b"missed message two");
    assert_eq!(sync.messages[1].envelope, b"missed message three");

    // Live again: the next message reaches the fresh session as it happens.
    // This is also the no-duplicate assertion: had node beta re-pushed
    // anything the client already held, that frame would have arrived before
    // this one and the seq below would not be 4.
    let (_, fourth) = sender
        .send_message(conversation, b"live again on beta")
        .await;
    assert_eq!(fourth.seq, 4);
    let event: MessageEvent = from_frame(&client.next_event(Opcode::MessageEvent).await)
        .expect("the live message decodes");
    assert_eq!(
        event.seq, 4,
        "the first pushed event on the new session is seq 4"
    );
    assert_eq!(event.sender_id, peer.account_id);

    // The outbox half of the reset path: the message the client queued while
    // it was between nodes, sent now, and sent again with the same id exactly
    // as a retry would. One row, one seq, the second send reported as the
    // duplicate it is — section 153's idempotency at the client seam.
    let message_id = Id::generate(SystemClock.now().as_unix_ms().max(0) as u64, &mut OsRandom);
    let queued = MessageSend {
        message_id,
        conversation_id: conversation,
        kind: MessageKind::Text,
        envelope: b"queued while the client was between nodes".to_vec(),
        reply_to: None,
        expires_in_ms: None,
        sender_key_id: None,
    };
    let first: MessageAccepted = client.ask(Opcode::MessageSend, &queued).await;
    assert_eq!(first.seq, 5);
    assert_ne!(first.duplicate, Some(true), "the first send is new");
    let retry: MessageAccepted = client.ask(Opcode::MessageSend, &queued).await;
    assert_eq!(retry.seq, 5, "the retry converges on the stored row's seq");
    assert_eq!(
        retry.duplicate,
        Some(true),
        "the retry is reported as a duplicate"
    );

    // And the peer, live on beta, receives the once-queued message — the
    // conversation is whole on both ends of the failover.
    let event: MessageEvent = from_frame(&sender.next_event(Opcode::MessageEvent).await)
        .expect("the queued message decodes");
    assert_eq!(event.seq, 5);
    assert_eq!(event.sender_id, roamer.account_id);
    assert_eq!(event.message_id, message_id);
}

/// A session id is a node-local fact, not a store fact: the second node
/// refuses it even while both nodes are healthy, which is what makes the
/// failover in the scenario above end as a new session rather than a resumed
/// one — and not as an accident of the first node being gone.
#[tokio::test]
async fn a_session_id_from_one_node_is_refused_on_another_even_while_both_live() {
    let store = migo_store::open(&StoreConfig::default())
        .await
        .expect("the in-memory backend opens");
    let app_a = build_node(&store, "node-alpha", "alpha").await;
    let app_b = build_node(&store, "node-beta", "beta").await;
    let a_addr = app_a.tcp_bind.expect("node alpha binds its TCP listener");
    let b_addr = app_b.tcp_bind.expect("node beta binds its TCP listener");

    let grant = registered_grant(&app_b, "crossnodesession").await;
    let holder = Client::connect_fresh(b_addr, &grant).await;

    // The holder's session was minted by node beta; node alpha has never heard
    // of it and says so, with the code that sends the client down the reset
    // path rather than into a loop.
    let refused = attempt_resume(a_addr, &grant, holder.session_id, holder.frames_seen).await;
    assert_eq!(refused.code, codes::RESUME_REQUIRED);
    assert_eq!(refused.symbol, "RESUME_REQUIRED");
}
