//! The leave→re-join race answered on the wire, across two nodes: a member
//! whose departure crossed the mesh must not have the room taken away from
//! them after they came back.
//!
//! The revocation that follows a room departure is a fan-out, and a fan-out
//! can arrive late. A member who leaves from one node and immediately comes
//! back through another holds a subscription the fresh join granted — and the
//! federated copy of their own departure, still in the outbox, names them for
//! revocation on the node they reconnected through. The ingest used to honor
//! that name unconditionally: the stale event took the room and its
//! conversation away from an active member, and the next room event they were
//! entitled to never reached them. The fix re-reads membership at the last
//! moment, under the removal's own feet: every room membership write mirrors
//! the account's standing into the room conversation's member rows, so one
//! `is_member` read answers for both topics, and a departure that was
//! overtaken by a re-join revokes nothing.
//!
//! The topology is the one-database shape of section 170, the same
//! `cross_node_resume.rs` builds, with the mesh link `dm_federation.rs`
//! builds joined on top: two full nodes over ONE store, so the join, the
//! leave, and the re-join are store facts both nodes can serve, and the mesh
//! carries only the fan-out. The room's home is node alpha (its owner created
//! it there); the member under test lives on node beta, whose watch
//! registration makes beta the node the stale departure comes home to.
//!
//! The race itself is the outbox runner's half-second tick against a re-join
//! that completes in milliseconds, so the ordering holds on the first round
//! almost always — but "almost" is not a guarantee, so the scenario repeats:
//! each round is one leave→re-join overtaken by its own federated echo, and a
//! round whose echo was ingested before the re-join landed simply passes
//! (nothing stale to honor by then) and lets the next round try again. The
//! assertion that matters is the probe: a fresh message from the owner must
//! reach the re-joined member on the far node, on the topics the stale
//! departure would have taken away.
//!
//! Determinism is the client-seam house style: every exchange is bounded by a
//! step budget, every expectation is asserted on frame contents, and the one
//! place timing genuinely races is retried with fresh message ids (section
//! 156: a duplicate id produces no fanout), never by sleeping past it.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext, SignIn};
use migo_core::config::{MeshPeer, StoreConfig};
use migo_core::{Clock, Config, Id, Secret, Timestamp};
use migo_crypto::NodeSecret;
use migo_protocol::{
    from_frame, to_frame, Acknowledged, Decode, Encode, Frame, Hello, MemberChange,
    MessageAccepted, MessageEvent, MessageKind, MessageSend, Opcode, Platform, RoomCreate,
    RoomJoinRequest, RoomJoinResponse, RoomLeaveRequest, RoomMemberEvent, SubscribeRequest,
    SubscribeResponse, Topic, TopicKind, Welcome, PROTOCOL_VERSION,
};
use migo_store::SharedStore;
use migod::App;

/// How long any single exchange may take before the test declares a node stuck
/// — silence being the bug class this suite exists to catch.
const STEP: Duration = Duration::from_secs(5);

/// How long one round may wait for the stale departure to come home over the
/// mesh. The outbox runner drains every half second, so the federated copy of
/// a leave lands inside a second or two; three seconds bounds it with margin
/// while still being shorter than the step budget that catches a stuck node.
const FED_WAIT: Duration = Duration::from_secs(3);

/// How many fresh-id baseline sends the warm-up may make before declaring the
/// mesh unproven. The first send may leave alpha before beta's watch
/// registration lands, exactly as `dm_federation.rs` describes; each attempt
/// is bounded by `FED_WAIT`, so the loop is bounded, never a sleep.
const BASELINE_ATTEMPTS: usize = 5;

/// How many leave→re-join rounds the scenario runs. The outbox tick makes the
/// re-join win the race almost every time; the rounds exist for the rare one
/// it does not, where the echo was ingested before the re-join landed and the
/// round passes for reasons that prove nothing.
const ROUNDS: usize = 3;

/// The node signing keys, exactly 32 bytes each so the mesh identity derives
/// from them the way a production node's does. The compile-time check keeps
/// them that way — `NodeSecret::from_seed` demands exactly 32 bytes.
const ALPHA_KEY: &str = "alpha-node-mesh-key-000000000000";
const BETA_KEY: &str = "beta-node-mesh-key-0000000000000";
const _: () = assert!(ALPHA_KEY.len() == 32);
const _: () = assert!(BETA_KEY.len() == 32);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// The mesh identity a signing key implies: the node id (first 16 bytes) and
/// the base64 public key an operator would paste into the peer's allow-list.
fn fed_identity(key: &str) -> (Id, String) {
    let seed = key.as_bytes();
    assert_eq!(seed.len(), 32, "the test keys are 32-byte seeds");
    let mut node_id = [0u8; 16];
    node_id.copy_from_slice(&seed[..16]);
    let public = NodeSecret::from_seed(seed)
        .expect("a 32-byte seed builds a node key")
        .public();
    (
        Id::from_bytes(node_id),
        base64::engine::general_purpose::STANDARD.encode(public.to_bytes()),
    )
}

/// Builds one full node over the shared store: its own TCP listener for the
/// clients, its own mesh listener for the peer, and its own node identity.
/// `federation.enabled` stays off on purpose — the peers are admitted after
/// both listeners have bound, through the same `apply_mesh_peers` a
/// deployment's restart runs. The anonymous handshake budget is raised because
/// the scenario is a burst of HELLOs from one peer address, and the default
/// endpoint bucket is sized for strangers.
async fn build_node(store: &SharedStore, node_id: &str, region: &str, key: &str) -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
            ("MIGO_NODE__ID".to_string(), node_id.to_string()),
            ("MIGO_NODE__REGION".to_string(), region.to_string()),
            ("MIGO_NODE__SIGNING_KEY".to_string(), key.to_string()),
            (
                "MIGO_NODE__MESH_BIND".to_string(),
                "127.0.0.1:0".to_string(),
            ),
            (
                "MIGO_RATE_LIMIT__ANONYMOUS_BURST".to_string(),
                "100".to_string(),
            ),
        ],
    )
    .expect("configuration should parse");
    App::build_with_store(&config, store.clone())
        .await
        .expect("a development node must build over the shared store")
}

/// Names each node to the other, both directions, the way two configuration
/// documents would, exactly as `dm_federation.rs` links its pair.
async fn link_peers(alpha: &App, beta: &App) {
    let (a_id, a_public) = fed_identity(ALPHA_KEY);
    let (b_id, b_public) = fed_identity(BETA_KEY);
    let a_mesh = alpha.mesh_bind.expect("node alpha binds its mesh listener");
    let b_mesh = beta.mesh_bind.expect("node beta binds its mesh listener");
    migod::apply_mesh_peers(
        &alpha.federation,
        a_id,
        &[MeshPeer {
            node_id: b_id.to_text(),
            public_key: b_public,
            base_url: format!("wss://{b_mesh}"),
            region: "beta".to_string(),
        }],
        alpha.clock.now(),
    )
    .await
    .expect("node alpha admits node beta");
    migod::apply_mesh_peers(
        &beta.federation,
        b_id,
        &[MeshPeer {
            node_id: a_id.to_text(),
            public_key: a_public,
            base_url: format!("wss://{a_mesh}"),
            region: "alpha".to_string(),
        }],
        beta.clock.now(),
    )
    .await
    .expect("node beta admits node alpha");
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
                device: DeviceClaim::new(Platform::Web, "room rejoin race test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// Signs an existing account in on a second device — through the *other* node,
/// over the shared store — so the same member can leave from the home node
/// while their main session lives on the watching one.
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

/// A live authenticated TCP session: HELLO with the grant's inline token (and
/// the ROOMS bit, section 72, because every frame this suite drives belongs to
/// the rooms family), then the self-subscription every client makes at its
/// handshake.
struct LiveSession {
    stream: tokio::net::TcpStream,
    correlation: u32,
    /// Events read while an `ask` was waiting for its reply, in arrival order.
    ///
    /// The race this suite exists to catch is decided in exactly this window: the
    /// federated echo of a leave lands on the outbox runner's half-second tick, which
    /// is regularly *while* the re-join's own asks are in flight — and a frame read
    /// off the socket is gone for good once read. An `ask` that dropped the frames it
    /// did not ask for was eating the very event the assertions wait on, making a
    /// healthy tier look like a dead one. Parking them and replaying them in arrival
    /// order keeps every reader — reply waits and event waits alike — seeing every
    /// frame exactly once.
    parked: VecDeque<Frame>,
}

impl LiveSession {
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting does not stall")
            .expect("the connection is accepted");

        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            features: migo_protocol::features::ROOMS,
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

        let mut session = Self {
            stream,
            correlation: 1,
            parked: VecDeque::new(),
        };
        session
            .subscribe(&[Topic {
                kind: TopicKind::User,
                id: grant.account_id,
            }])
            .await;
        session
    }

    /// Reads one frame off the session — a parked one first, in arrival order,
    /// and only then the socket.
    async fn next_frame(&mut self) -> Frame {
        if let Some(frame) = self.parked.pop_front() {
            return frame;
        }
        recv_within(&mut self.stream, STEP).await
    }

    /// Sends a request that must succeed, returning the decoded reply. Frames
    /// that are not the reply — the events a subscribed session receives — are
    /// parked and replayed to the next reader in arrival order, never
    /// discarded: the race this suite stages is regularly decided while an
    /// `ask` is in flight, and the echo a leave enqueues must survive the
    /// re-join's own asks to be observable at all. A rate-limited refusal is
    /// waited out and retried, exactly as told: the scenario is a burst of
    /// joins and subscribes inside the limiter's window, and "retry in N ms"
    /// is the server talking, not the server broken.
    ///
    /// The frames read past are held locally and re-parked only once the
    /// reply lands. Parking them on the session queue mid-wait would spin
    /// instead of wait: `next_frame` serves the queue before the socket, so a
    /// frame pushed back while it is being drained comes around again and
    /// again and the socket — where the reply sits, and where the step budget
    /// lives — is never reached. The hold keeps every iteration draining
    /// towards the socket, and the step budget around the whole wait turns a
    /// reply that never comes into a failure naming it, not a hang.
    async fn ask<M: Encode, R: Decode>(&mut self, opcode: Opcode, message: &M) -> R {
        self.correlation += 1;
        let correlation = self.correlation;
        let mut backoff = 0u64;
        for _ in 0..5 {
            if backoff > 0 {
                tokio::time::sleep(Duration::from_millis(backoff + 100)).await;
            }
            send(&mut self.stream, opcode, correlation, message).await;
            let mut held: Vec<Frame> = Vec::new();
            let reply = tokio::time::timeout(STEP, async {
                loop {
                    let frame = self.next_frame().await;
                    if frame.header.correlation == correlation {
                        return frame;
                    }
                    held.push(frame);
                }
            })
            .await;
            for frame in held {
                self.parked.push_back(frame);
            }
            let reply = reply.unwrap_or_else(|| {
                panic!(
                    "the {:?} reply for correlation {correlation} never arrived inside \
                     the step budget; frames parked while waiting: {:?}",
                    opcode,
                    self.parked
                        .iter()
                        .map(|frame| Opcode::from_wire(frame.header.opcode))
                        .collect::<Vec<_>>()
                )
            });
            if !reply.header.is_error() {
                return from_frame(&reply).expect("the reply decodes");
            }
            let refusal: migo_protocol::Error = from_frame(&reply).expect("the refusal decodes");
            assert!(
                refusal.code == migo_protocol::codes::RATE_LIMITED,
                "the request was refused outright: {refusal:?}"
            );
            backoff = u64::from(refusal.retry_after_ms.unwrap_or(1000));
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
            "every asked topic was granted to a member: {:?}",
            confirmation
        );
    }

    /// Joins a room (or re-joins it — the call is idempotent), returning the
    /// handle the wire answers a join with.
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

    /// Sends one sealed message into a room's conversation, with a fresh id —
    /// section 156: a duplicate id produces no fanout, so a retry that reuses
    /// one would be a silent no-op.
    async fn send_room_message(&mut self, conversation_id: Id, nonce: u128) -> Id {
        let message_id = Id::from(nonce);
        let accepted: MessageAccepted = self
            .ask(
                Opcode::MessageSend,
                &MessageSend {
                    message_id,
                    conversation_id,
                    kind: MessageKind::Text,
                    envelope: b"sealed-for-the-race".to_vec(),
                    reply_to: None,
                    expires_in_ms: None,
                    sender_key_id: None,
                },
            )
            .await;
        assert_eq!(accepted.message_id, message_id);
        message_id
    }
}

/// Reads from a session for `window`, looking for the stale departure of
/// `member` from `room` — the federated `Left` the mesh carries home — skipping
/// everything else a subscribed session hears. `None` means the window closed
/// without it, which is the silence this suite exists to catch, so the frame
/// read is bounded here the same way [`try_message_of`] bounds its own: the
/// caller turns the silence into a report, not a hang.
async fn try_departure_of(
    session: &mut LiveSession,
    room_id: Id,
    member: Id,
    window: Duration,
) -> Option<RoomMemberEvent> {
    let outcome = tokio::time::timeout(window, async {
        loop {
            let frame = session.next_frame().await;
            if Opcode::from_wire(frame.header.opcode) == Some(Opcode::RoomMemberEvent) {
                let event: RoomMemberEvent = from_frame(&frame).expect("the member event decodes");
                if event.room_id == room_id
                    && event.user_id == member
                    && event.change == Some(MemberChange::Left)
                {
                    return Some(event);
                }
            }
        }
    })
    .await;
    outcome.unwrap_or(None)
}

/// Reads from a session for `window`, looking for the message with the given
/// id, skipping everything else a subscribed session hears — other messages
/// included, so a warm-up straggler is never mistaken for the probe that
/// follows it. `None` means the window closed without it: the warm-up's retry
/// loop treats that as an answer to retry past, while the probe treats it as
/// the bug itself, and both read through the same bounded window rather than
/// a hang. The window bounds the whole read, frame included, the same shape
/// `assert_quiet` uses for its stray-frame watch.
async fn try_message_of(
    session: &mut LiveSession,
    message_id: Id,
    window: Duration,
) -> Option<MessageEvent> {
    let outcome = tokio::time::timeout(window, async {
        loop {
            let frame = session.next_frame().await;
            if Opcode::from_wire(frame.header.opcode) == Some(Opcode::MessageEvent) {
                let event: MessageEvent = from_frame(&frame).expect("the message event decodes");
                if event.message_id == message_id {
                    return Some(event);
                }
            }
        }
    })
    .await;
    outcome.unwrap_or(None)
}

/// The scenario: a member leaves a room from the home node and immediately
/// comes back through the watching one, and the departure's own federated echo
/// must not take the room away from the member who came back.
#[tokio::test]
async fn a_stale_departure_does_not_revoke_a_rejoined_member() {
    // Diagnostics: the federated half's failures are warn-level logs on nodes
    // whose sockets stay silent, so the suite captures what both nodes logged
    // while it ran. A second `try_init` failure would mean something else
    // already installed a global subscriber; nothing else in this binary does,
    // so silence is fine here.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "migod=debug,migo_federation=info,migo_gateway=warn",
        ))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
    // The fleet: two nodes over one store, the one-database shape of section
    // 170, joined by the mesh link dm_federation.rs builds.
    let store = migo_store::open(&StoreConfig::default())
        .await
        .expect("the in-memory backend opens");
    let alpha = build_node(&store, "node-alpha", "alpha", ALPHA_KEY).await;
    let beta = build_node(&store, "node-beta", "beta", BETA_KEY).await;
    let a_addr = alpha.tcp_bind.expect("node alpha binds its TCP listener");
    let b_addr = beta.tcp_bind.expect("node beta binds its TCP listener");
    link_peers(&alpha, &beta).await;

    // The owner lives on alpha; the member under test lives on beta, with a
    // second device signed in on alpha — the one that will perform the leave
    // from the room's home node while the member's main session stays put on
    // the watching one.
    let owner = registered_grant(&alpha, "raceowner").await;
    let member = registered_grant(&beta, "racemember").await;
    let member_alpha = second_device_grant(&alpha, "racemember").await;

    let mut owner_session = LiveSession::connect(a_addr, &owner).await;
    let mut member_beta = LiveSession::connect(b_addr, &member).await;
    let mut member_alpha = LiveSession::connect(a_addr, &member_alpha).await;

    // The room, created through the owner's own wire session on alpha — the
    // home node, the one every federated copy of its fan-out comes home to.
    let created: RoomJoinResponse = owner_session
        .ask(
            Opcode::RoomCreate,
            &RoomCreate {
                slug: "rejoin-race".to_string(),
                name: "The Rejoin Race".to_string(),
                kind: 1, // RoomKind::Public
                topic: None,
                max_members: None,
            },
        )
        .await;
    let room_id = created.room.room_id;
    let conversation_id = created.conversation_id;
    let room_topics = vec![
        Topic {
            kind: TopicKind::Room,
            id: room_id,
        },
        Topic {
            kind: TopicKind::Conversation,
            id: conversation_id,
        },
    ];

    // The member joins from beta and holds the room's two topics there — the
    // subscription whose survival is the subject of the test.
    let _: RoomJoinResponse = member_beta.join_room(room_id).await;
    member_beta.subscribe(&room_topics).await;

    // The deterministic barrier for the room's own tier: the warm-up below
    // proves the conversation watch, but the departure the race waits on rides
    // the ROOM tier, whose watch ask is a separate row on the outbox. The
    // leave's federated half is a plain no-op against an empty watcher table —
    // no error, no warn, nothing crossing — so the table is polled, not
    // assumed, before the race starts: the ask is durable and lands on the
    // outbox runner's half-second tick.
    let beta_id = fed_identity(BETA_KEY).0;
    let mut registered = false;
    let deadline = tokio::time::Instant::now() + FED_WAIT;
    while tokio::time::Instant::now() < deadline {
        if alpha.room_relay.watchers_of(room_id).contains(&beta_id) {
            registered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    assert!(
        registered,
        "node alpha must register node beta as a watcher of the room inside the \
         mesh budget: {:?}",
        alpha.room_relay.watchers_of(room_id)
    );

    // The warm-up: the owner sends until the member hears one, which proves
    // the whole federated path — beta's watch registration on alpha, the mesh
    // link, the fan-out — before the race starts. Retrying with fresh ids is
    // the dm_federation convention for the one ordering the mesh does not
    // promise: the first send may leave alpha before the watch lands.
    let mut nonce: u128 = 1;
    let mut warmed = false;
    for _ in 0..BASELINE_ATTEMPTS {
        let message_id = owner_session
            .send_room_message(conversation_id, nonce)
            .await;
        nonce += 1;
        if let Some(event) = try_message_of(&mut member_beta, message_id, FED_WAIT).await {
            assert_eq!(
                event.message_id, message_id,
                "the baseline message that arrived is the one that was sent"
            );
            warmed = true;
            break;
        }
    }
    assert!(
        warmed,
        "the owner's baseline message crossed the mesh inside the retry budget"
    );

    // The race, repeated. Each round: a leave from the home node, a re-join
    // from the watching one that lands in milliseconds, and the departure's
    // federated echo arriving on the outbox runner's half-second tick — after
    // the member is already back. The echo is published to the room topic the
    // member still holds (publish first, revoke second is the ingest's own
    // order), so waiting for it makes the stale arrival observable; the probe
    // after it is the assertion.
    for round in 0..ROUNDS {
        // The leave, from the member's other device on the home node. Alpha
        // revokes its own sessions for the member and enqueues the federated
        // copy for the watchers; the acknowledgement is the moment the echo is
        // in the outbox, which is the starting gun for the re-join.
        let acknowledged: Acknowledged = member_alpha
            .ask(Opcode::RoomLeave, &RoomLeaveRequest { room_id })
            .await;
        assert!(acknowledged.ok, "round {round}: the leave is acknowledged");

        // The re-join, immediately, from beta — the store write and the fresh
        // subscriptions land long before the outbox tick can carry the
        // departure back.
        let _: RoomJoinResponse = member_beta.join_room(room_id).await;
        member_beta.subscribe(&room_topics).await;

        // The stale departure comes home and is published to the room topic
        // the member holds. Its arrival is the race resolved: the member is
        // already back, and the echo names them anyway. The wait refuses to be
        // a silent hang: on the step budget's expiry it reports the three
        // facts a delivery fault and an ingest fault do NOT share — the leave
        // row still sitting in the shared outbox means the delivery tier on
        // the home node stalled (a settle that logs nothing, a drain that
        // never dialed), while a delivered row with nothing written to the
        // member's socket means the ingest or publish tier on the watching
        // node dropped it — so the failure names its own side of the mesh.
        let stale = match try_departure_of(&mut member_beta, room_id, member.account_id, STEP).await
        {
            Some(event) => event,
            None => panic!(
                "round {round}: the departure's federated echo never came home inside \
                 the step budget. alpha watchers {:?}, beta watchers {:?}, \
                 undelivered outbox [{}], beta frames_out {} dropped {:?}",
                alpha.room_relay.watchers_of(room_id),
                beta.room_relay.watchers_of(room_id),
                store
                    .due_events(Timestamp::from_millis(i64::MAX / 2), 200)
                    .await
                    .expect("the outbox is readable")
                    .iter()
                    .map(|row| {
                        format!(
                            "target {} opcode {} attempts {} created {} next {} error {:?}",
                            row.target_node,
                            row.opcode,
                            row.attempts,
                            row.created_at.as_millis(),
                            row.next_attempt_at.as_millis(),
                            row.last_error
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
                beta.gateway.frames_out_total(),
                beta.gateway.dropped_frames_total()
            ),
        };
        assert_eq!(
            stale.change,
            Some(MemberChange::Left),
            "round {round}: the echo that came home is the departure itself"
        );

        // The probe: a fresh message from the owner must reach the re-joined
        // member on beta, on the very topics the stale departure would have
        // taken away. The wait matches the probe's own id — a warm-up
        // straggler is skipped, not answered for — and stays honest the same
        // way every other wait here does: had the ingest honored the echo, the
        // step budget closes the window and the failure names the message
        // that never came home.
        let probe_id = owner_session
            .send_room_message(conversation_id, nonce)
            .await;
        nonce += 1;
        assert!(
            try_message_of(&mut member_beta, probe_id, STEP)
                .await
                .is_some(),
            "round {round}: the probe message {} never reached the re-joined \
             member inside the step budget — the stale departure took the room \
             away from the member who came back to it",
            probe_id.to_text()
        );
    }
}
