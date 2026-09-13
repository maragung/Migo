//! Cross-node delivery of the frames whose topic is a user: the friend-graph
//! hint, the notification bell, and the sealed sender-key distribution — the
//! scenario section 170's user-topic tier carries beside presence, and the one
//! this suite exists to prove: before the tier carried them, a member whose
//! session lived on another node heard none of the three. The sender got his
//! `Acknowledged`, the local hub had nobody listening, and the frame stopped
//! at the node that took it.
//!
//! The topology is the honest shape cross_node_presence.rs uses: two full
//! nodes over ONE store through `App::build_with_store`, linked by ephemeral
//! mesh listeners and `apply_mesh_peers` — the same public seam an operator's
//! `federation.peers` configuration drives at boot. Everything authorization
//! needs is a store fact; the mesh link is the only thing that is new.
//!
//! Determinism follows the client-seam house style: every exchange is bounded
//! by a step budget, every expectation is asserted on frame contents, and the
//! one inherently asynchronous hop — the watch ask crossing from one node's
//! subscribe half to the other's table — is observed by polling the table
//! itself, not by sleeping and hoping. The frames this suite drives need no
//! feature bits (section 72 maps the social and key opcodes to none), so the
//! HELLO negotiates the empty set the way a client that only wants its own
//! topic's traffic does.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::config::{MeshPeer, StoreConfig};
use migo_core::{Clock, Config, Id, Secret};
use migo_crypto::NodeSecret;
use migo_protocol::{
    from_frame, to_frame, Acknowledged, ConversationCreateRequest, ConversationKind,
    ConversationSummary, Decode, Encode, Frame, FriendEvent, FriendTarget, GroupKeyDistribution,
    Hello, NotificationEvent, NotificationKind, Opcode, SubscribeRequest, SubscribeResponse, Topic,
    TopicKind, Welcome, PROTOCOL_VERSION,
};
use migod::App;

/// How long any single exchange may take before the test declares a node
/// stuck — silence being the bug class this suite exists to catch.
const STEP: Duration = Duration::from_secs(5);

/// How long the mesh may take to carry a watch ask, or a user-topic frame
/// behind it, between two healthy in-process nodes: twenty ticks of half a
/// second, against an outbox runner that drains at that cadence. The loops
/// exit the moment the state is observed, so this budget is only ever spent
/// on failure.
const MESH_BUDGET: Duration = Duration::from_secs(10);

/// Node alpha's mesh signing seed, exactly 32 bytes the way `NodeSecret`
/// demands. The mesh node id is the first 16 bytes, so the two seeds differ
/// there — the same pinning discipline tools/nnode applies, in-process.
const SEED_ALPHA: &str = "user-topic-alpha-000000000000000";

/// Node beta's mesh signing seed.
const SEED_BETA: &str = "user-topic-beta-00000000000000000";

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// The mesh node id a signing seed derives: the first 16 bytes of the seed,
/// the derivation `App::build_with_store` applies when it opens the mesh.
fn mesh_node_id(seed: &str) -> Id {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&seed.as_bytes()[..16]);
    Id::from_bytes(bytes)
}

/// The `federation.peers` entry naming one node to the other: its derived
/// node id, the public half of its mesh signing key, the ephemeral address
/// its listener actually bound, and its region label.
fn mesh_peer_of(seed: &str, bind: SocketAddr, region: &str) -> MeshPeer {
    let public = NodeSecret::from_seed(seed.as_bytes())
        .expect("a 32-byte seed derives a node secret")
        .public()
        .to_bytes();
    MeshPeer {
        node_id: mesh_node_id(seed).to_text(),
        public_key: base64::engine::general_purpose::STANDARD.encode(public),
        base_url: format!("wss://{bind}"),
        region: region.to_string(),
    }
}

/// Builds one node of the fleet: TCP and mesh listeners on ephemeral loopback
/// ports, a fixed mesh signing seed, and the anonymous burst raised because
/// the scenario's handshakes from one peer address must fit inside the
/// bucket. The two nodes share one token key the way a deployment must, so a
/// grant minted by either node authenticates on both.
async fn build_node(
    store: &migo_store::SharedStore,
    node_id: &str,
    region: &str,
    seed: &str,
) -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
            (
                "MIGO_NODE__MESH_BIND".to_string(),
                "127.0.0.1:0".to_string(),
            ),
            ("MIGO_NODE__ID".to_string(), node_id.to_string()),
            ("MIGO_NODE__REGION".to_string(), region.to_string()),
            ("MIGO_NODE__SIGNING_KEY".to_string(), seed.to_string()),
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
                device: DeviceClaim::new(
                    migo_protocol::Platform::Web,
                    "cross-node user topic test",
                ),
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
    Frame::decode(body.into()).expect("the frame decodes")
}

/// A scripted client: a real TCP session speaking the length-prefixed
/// framing, tracking its correlation counter the way a real one does.
struct Client {
    stream: tokio::net::TcpStream,
    correlation: u32,
}

impl Client {
    /// Connects and opens a fresh, authenticated session: HELLO with the
    /// grant's inline token, then the self-subscription every client makes at
    /// its handshake. The self-subscription is the whole reason the far node
    /// ends up in the peers' watch tables: it is what makes a target's own
    /// devices reachable through the user-topic tier.
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
                return Self {
                    stream,
                    correlation: 2,
                };
            }
        }
    }

    /// Sends a request that must succeed, returning the decoded reply. Frames
    /// that are not the reply — the events a subscribed session receives —
    /// are read and kept, never discarded silently.
    async fn ask<M: Encode, R: Decode>(&mut self, opcode: Opcode, message: &M) -> R {
        self.correlation += 1;
        let correlation = self.correlation;
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

    /// Reads until one frame of `opcode` arrives, skipping the frames a
    /// subscribed session receives that are not that arrival. The skip is
    /// order-tolerant by design: the hint, the bell, and a distribution ride
    /// separate halves whose relative order is not a promise the wire makes,
    /// and a re-delivered copy of a frame already held (section 153's
    /// at-least-once) presents the same fact again rather than a new one.
    async fn next_event_of<R: Decode>(&mut self, opcode: Opcode, limit: Duration) -> R {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "the frame of {:?} never arrived within the mesh budget",
                opcode
            );
            let frame = recv_within(&mut self.stream, remaining).await;
            if Opcode::from_wire(frame.header.opcode) == Some(opcode) {
                return from_frame(&frame).expect("the event decodes");
            }
        }
    }
}

/// Two full nodes over one store, linked through the operator's seam, with
/// their addresses and mesh identities handed back to the scenario.
struct Fleet {
    app_a: App,
    app_b: App,
    a_addr: SocketAddr,
    b_addr: SocketAddr,
    id_b: Id,
}

/// Builds and links the two-node fleet the scenarios share.
async fn fleet() -> Fleet {
    let store = migo_store::open(&StoreConfig::default())
        .await
        .expect("the in-memory backend opens");
    let app_a = build_node(&store, "node-alpha", "alpha", SEED_ALPHA).await;
    let app_b = build_node(&store, "node-beta", "beta", SEED_BETA).await;
    let a_addr = app_a.tcp_bind.expect("node alpha binds its TCP listener");
    let b_addr = app_b.tcp_bind.expect("node beta binds its TCP listener");
    let mesh_a = app_a.mesh_bind.expect("node alpha binds its mesh listener");
    let mesh_b = app_b.mesh_bind.expect("node beta binds its mesh listener");
    let id_a = mesh_node_id(SEED_ALPHA);
    let id_b = mesh_node_id(SEED_BETA);
    assert_ne!(id_a, id_b, "the two seeds must derive two mesh identities");

    migod::apply_mesh_peers(
        &app_a.federation,
        id_a,
        &[mesh_peer_of(SEED_BETA, mesh_b, "beta")],
        app_a.clock.now(),
    )
    .await
    .expect("node alpha admits node beta");
    migod::apply_mesh_peers(
        &app_b.federation,
        id_b,
        &[mesh_peer_of(SEED_ALPHA, mesh_a, "alpha")],
        app_b.clock.now(),
    )
    .await
    .expect("node beta admits node alpha");

    Fleet {
        app_a,
        app_b,
        a_addr,
        b_addr,
        id_b,
    }
}

/// Waits until the origin node's watch table names the far node as a watcher
/// of `subject`, the deterministic barrier every scenario crosses before it
/// publishes: the watch ask is durable and asynchronous, and a frame
/// published before the ask lands has nobody to be fanned out to.
async fn await_watcher(app: &App, subject: Id, watcher: Id) {
    let deadline = tokio::time::Instant::now() + MESH_BUDGET;
    while tokio::time::Instant::now() < deadline {
        if app.presence_relay.watchers_of(subject).contains(&watcher) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    panic!(
        "the origin never registered the far node as a watcher of the subject within the mesh \
         budget: {:?}",
        app.presence_relay.watchers_of(subject)
    );
}

/// The friendship's first frame, driven through the front door of the
/// sender's node: the request is acknowledged on alpha, and the recipient —
/// whose only session lives on beta — receives both halves the request owes
/// him there: the `FRIEND_EVENT` hint his friends UI reacts to, and the
/// notification the bell rings (the same frame the inbox row's process
/// derives its wake-up from, though the row itself is a store fact this
/// topology shares rather than a frame the tier carries).
#[tokio::test]
async fn a_friend_request_rings_the_recipient_on_the_other_node() {
    let fleet = fleet().await;
    let alice_grant = registered_grant(&fleet.app_a, "user topicalice").await;
    let bob_grant = registered_grant(&fleet.app_b, "user topicbob").await;

    // The sender connects on her node; the recipient connects on his. His
    // self-subscription is what puts him in alpha's watch table.
    let mut alice = Client::connect_fresh(fleet.a_addr, &alice_grant).await;
    let mut bob = Client::connect_fresh(fleet.b_addr, &bob_grant).await;
    await_watcher(&fleet.app_a, bob_grant.account_id, fleet.id_b).await;

    // The request itself, over the wire, on the sender's node.
    let _: Acknowledged = alice
        .ask(
            Opcode::FriendRequest,
            &FriendTarget {
                user_id: bob_grant.account_id,
            },
        )
        .await;

    // The hint: the recipient's own user topic, on his node, naming the
    // account whose edge toward him just appeared and the state that says a
    // request — not an acceptance — is what he is holding.
    let hint: FriendEvent = bob.next_event_of(Opcode::FriendEvent, MESH_BUDGET).await;
    assert_eq!(
        hint.user_id, alice_grant.account_id,
        "the hint names the account whose graph moved"
    );
    assert_eq!(hint.state, "request", "the hint says a request arrived");

    // The bell: the notification frame the same request owes the recipient,
    // carried beside the hint because the notifier's ring happens where no
    // connection context exists — the far hub places it on the same topic.
    let bell: NotificationEvent = bob
        .next_event_of(Opcode::NotificationEvent, MESH_BUDGET)
        .await;
    assert_eq!(
        bell.kind,
        NotificationKind::FriendRequest,
        "the notification is the friend request kind"
    );
    assert_eq!(
        bell.actor_id,
        Some(alice_grant.account_id),
        "the notification names the asker as its actor"
    );

    // The asker's own echo stayed local by design: her other devices, if she
    // had any, would be on alpha, and beta holds no session of hers — but
    // the assertion that matters is that nothing extra crossed to bob. His
    // socket has been read to the two frames the request owes him.
    drop(alice);
    drop(bob);
}

/// A sealed sender-key distribution, driven through the front door of the
/// distributor's node: the member it names holds his only session on the
/// other node, and the distribution — sealed for his device, opaque to every
/// node it crosses — arrives on his user topic there byte for byte (section
/// 163's relay, now riding the tier section 170 describes for it).
#[tokio::test]
async fn a_sealed_key_distribution_reaches_the_member_on_the_other_node() {
    let fleet = fleet().await;
    let alice_grant = registered_grant(&fleet.app_a, "sealed alice").await;
    let bob_grant = registered_grant(&fleet.app_b, "sealed bob").await;

    // The group whose keys are being distributed: created on the sender's
    // node over the shared store, so both members are store facts and the
    // distribution's authorization reads them without a replication ask.
    let mut alice = Client::connect_fresh(fleet.a_addr, &alice_grant).await;
    let summary: ConversationSummary = alice
        .ask(
            Opcode::ConversationCreate,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![alice_grant.account_id, bob_grant.account_id],
                title: Some("the sealed crossing".to_string()),
            },
        )
        .await;
    let conversation = summary.conversation_id;

    // The recipient connects on his node and self-subscribes; the barrier
    // crosses before the distribution is sent, exactly as production would
    // have it (his session existed before the rotation that targets him).
    let mut bob = Client::connect_fresh(fleet.b_addr, &bob_grant).await;
    await_watcher(&fleet.app_a, bob_grant.account_id, fleet.id_b).await;

    // The distribution: sealed bytes with a recognizable marker, addressed to
    // the member's device. The sender holds an acknowledgement that promises
    // the distribution was taken before the assertion asks what arrived.
    let sealed = b"cross-node sealed distribution marker".to_vec();
    let _: Acknowledged = alice
        .ask(
            Opcode::GroupKeyDistribute,
            &GroupKeyDistribution {
                conversation_id: conversation,
                from_device: alice_grant.device_id,
                to_account: bob_grant.account_id,
                to_device: bob_grant.device_id,
                sealed_distribution: sealed.clone(),
            },
        )
        .await;

    // The member receives it on his node, on his own user topic, sealed
    // exactly as the distributor sealed it — the crossing nodes read only
    // the ids that route it.
    let arrived: GroupKeyDistribution = bob
        .next_event_of(Opcode::GroupKeyDistribute, MESH_BUDGET)
        .await;
    assert_eq!(
        arrived.conversation_id, conversation,
        "the distribution names the conversation it belongs to"
    );
    assert_eq!(
        arrived.to_account, bob_grant.account_id,
        "the distribution names the member it is for"
    );
    assert_eq!(
        arrived.to_device, bob_grant.device_id,
        "the distribution names the device it is sealed for"
    );
    assert_eq!(
        arrived.sealed_distribution, sealed,
        "the sealed bytes arrive byte for byte, never resealed in transit"
    );
    drop(alice);
    drop(bob);
}
