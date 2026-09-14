//! Cross-node room presence: the reconnect grace and the online count, driven at the
//! client seam. The gap this suite exists to close is that both were per-node facts —
//! a member whose socket dropped on one node and came back on another was invisible to
//! the first node's generation check, so its two-minute grace fired anyway and removed
//! an online member from every room they were in, and an online count served anywhere
//! but the member's own node undercounted by everyone connected elsewhere.
//!
//! The topology is the same honest shape the other cross-node suites use: two full nodes
//! over ONE store through `App::build_with_store`, linked by `apply_mesh_peers` with
//! fixed 32-byte signing seeds, because membership is a store fact and the mesh link is
//! the wire path under test. The room is created on node alpha, which homes it and so
//! holds its watch table; carol's room-topic subscription on node beta is what makes
//! beta a watcher — without a watcher on the far side, a member event published on alpha
//! has nobody to fan out to, which is the deployment reality this scenario mirrors: the
//! node a member reconnects through is a node somebody else on it already watches the
//! room from.
//!
//! The flow under test, in the order it lives in production: bob holds his one session
//! on alpha and drops it; alpha announces `Disconnected` and arms its grace; the
//! announcement crosses to beta, whose tally records the room as owed — arming nothing,
//! because the node that saw the drop owns the timeout; bob's next session comes up on
//! beta, and the `Reconnected` beta owes the room crosses back to alpha, where it both
//! cancels alpha's grace (the same generation bump a local reconnect carries) and puts
//! bob back in the online count alpha serves. Alice, still on alpha, is the witness the
//! old code failed: her socket receives the `Reconnected`, and her room listing counts
//! every member — herself locally, and bob and carol by the member events that crossed.
//!
//! Determinism follows the client-seam house style: every exchange is bounded by a step
//! budget, every expectation is asserted on frame contents, and the one inherently
//! asynchronous hop — beta's watch ask landing in alpha's table — is observed by polling
//! the table itself (`App::room_relay`), never by sleeping and hoping.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::config::{MeshPeer, StoreConfig};
use migo_core::{Clock, Config, Id, Secret};
use migo_crypto::NodeSecret;
use migo_protocol::{
    from_frame, to_frame, Decode, Encode, Frame, Hello, MemberChange, Opcode, RoomCreate,
    RoomJoinRequest, RoomJoinResponse, RoomListRequest, RoomListResponse, RoomMemberEvent,
    SubscribeRequest, SubscribeResponse, Topic, TopicKind, Welcome, PROTOCOL_VERSION,
};
use migod::App;

/// How long any single exchange may take before the test declares a node stuck —
/// silence being the bug class this suite exists to catch.
const STEP: Duration = Duration::from_secs(5);

/// How long the mesh may take to carry a watch ask between two healthy in-process
/// nodes: eighty quarter-second polls, against an outbox runner that drains every
/// 500ms. The loop exits the moment the state is observed, so this budget is only
/// ever spent on failure.
const MESH_BUDGET: Duration = Duration::from_secs(10);

/// Node alpha's mesh signing seed, exactly 32 bytes the way `NodeSecret` demands. The
/// mesh node id is the first 16 bytes, so the two seeds differ there.
const SEED_ALPHA: &str = "room-presence-alpha-0000000000";

/// Node beta's mesh signing seed.
const SEED_BETA: &str = "room-presence-beta-00000000000";

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// The mesh node id a signing seed derives: the first 16 bytes of the seed, the
/// derivation `App::build_with_store` applies when it opens the mesh.
fn mesh_node_id(seed: &str) -> Id {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&seed.as_bytes()[..16]);
    Id::from_bytes(bytes)
}

/// The `federation.peers` entry naming one node to the other: its derived node id,
/// the public half of its mesh signing key, the ephemeral address its listener
/// actually bound, and its region label.
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

/// Builds one node of the fleet: TCP and mesh listeners on ephemeral loopback ports, a
/// fixed mesh signing seed, and the anonymous burst raised because the scenario's
/// handshakes from one peer address must all fit inside the bucket. The two nodes share
/// one token key the way a deployment must, so a grant minted by either node
/// authenticates on both.
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
            ("MIGO_GATEWAY__HEARTBEAT_MS".to_string(), "6000".to_string()),
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

/// Registers one account through the front door of a node, stamped with that node's
/// own clock so the inline token is not born expired.
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
                device: DeviceClaim::new(migo_protocol::Platform::Web, "cross-node rooms test"),
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

/// A scripted client: a real TCP session speaking the length-prefixed framing,
/// tracking its correlation counter the way a real one does.
struct Client {
    stream: tokio::net::TcpStream,
    correlation: u32,
}

impl Client {
    /// Connects and opens a fresh, authenticated session: HELLO with the grant's
    /// inline token, then the self-subscription every client makes at its handshake.
    async fn connect_fresh(addr: SocketAddr, grant: &Grant) -> Self {
        let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting does not stall")
            .expect("the connection is accepted");

        // The ROOMS bit: every frame this suite drives is the room family, which brief
        // section 72 gates on the negotiated set — the session asks for the bit the way
        // a client that wants room events does.
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

    /// Sends a request that must succeed, returning the decoded reply. Frames that are
    /// not the reply — the events a subscribed session receives — are read and kept,
    /// never discarded silently.
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

    /// Reads until the room's stream carries this member and this change, skipping the
    /// frames a subscribed session receives that are not that arrival — state deltas,
    /// other members' events, and a re-delivered copy of a state already seen, which an
    /// at-least-once mesh (section 153) can put on the wire after the first.
    async fn next_member_event_of(&mut self, member: Id, change: MemberChange) -> RoomMemberEvent {
        loop {
            let frame = recv_within(&mut self.stream, STEP).await;
            if Opcode::from_wire(frame.header.opcode) != Some(Opcode::RoomMemberEvent) {
                continue;
            }
            let event: RoomMemberEvent = from_frame(&frame).expect("the member event decodes");
            if event.user_id == member && event.change == Some(change) {
                return event;
            }
        }
    }
}

/// The full scenario, in the order a real cross-node reconnect lives it: the room's
/// home node arms a grace for a member whose socket dropped, the member comes back on
/// the other node, and the `Reconnected` that owes the room crosses back and cancels
/// the grace — while the online count the home node serves counts the members it holds
/// no session for, because their member events crossed.
#[tokio::test]
async fn a_reconnect_on_the_other_node_cancels_the_grace_and_counts_the_member() {
    // The fleet: two nodes over one store, each with its own ephemeral mesh listener.
    // The room will be created on alpha, which homes it and so holds its watch table.
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

    // Three accounts: alice stays on alpha and witnesses; bob starts on alpha and
    // reconnects on beta; carol holds the beta-side seat whose room-topic subscription
    // makes beta a watcher of the room — without her, a member event published on
    // alpha has nobody on beta to fan out to.
    let alice_grant = registered_grant(&app_a, "roomalice").await;
    let bob_grant = registered_grant(&app_a, "roombob").await;
    let carol_grant = registered_grant(&app_b, "roomcarol").await;

    // Alice opens the room on her node, which homes it, and watches its topic.
    let mut alice = Client::connect_fresh(a_addr, &alice_grant).await;
    let created: RoomJoinResponse = alice
        .ask(
            Opcode::RoomCreate,
            &RoomCreate {
                slug: "cross-node-lobby".to_string(),
                name: "Cross-node lobby".to_string(),
                kind: migo_protocol::RoomKind::Public.to_wire(),
                topic: None,
                max_members: None,
            },
        )
        .await;
    let room_id = created.room.room_id;
    let room_topic = Topic {
        kind: TopicKind::Room,
        id: room_id,
    };
    alice.subscribe(vec![room_topic.clone()]).await;

    // Bob joins from alpha and watches the topic; his is the session that will drop.
    let mut bob = Client::connect_fresh(a_addr, &bob_grant).await;
    let _: RoomJoinResponse = bob
        .ask(
            Opcode::RoomJoin,
            &RoomJoinRequest {
                room_id,
                invite_code: None,
            },
        )
        .await;
    bob.subscribe(vec![room_topic.clone()]).await;

    // Carol joins from beta and watches the topic there. Her join's member event
    // crosses to the home node, so alice's socket sees her arrive — the beta-to-alpha
    // half of the room tier, which carries the reconnect back later.
    let mut carol = Client::connect_fresh(b_addr, &carol_grant).await;
    let _: RoomJoinResponse = carol
        .ask(
            Opcode::RoomJoin,
            &RoomJoinRequest {
                room_id,
                invite_code: None,
            },
        )
        .await;
    carol.subscribe(vec![room_topic]).await;
    let joined = alice
        .next_member_event_of(carol_grant.account_id, MemberChange::Joined)
        .await;
    assert_eq!(joined.member_count, Some(3), "three members are seated");

    // The deterministic barrier: carol's subscription asked the home node to register
    // beta as a watcher of the room, and the ask is durable and async, so the test
    // waits until it has landed in alpha's table — polling the table, not sleeping —
    // before bob's disconnect publishes anything that depends on it.
    let mut registered = false;
    let deadline = tokio::time::Instant::now() + MESH_BUDGET;
    while tokio::time::Instant::now() < deadline {
        if app_a.room_relay.watchers_of(room_id).contains(&id_b) {
            registered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    assert!(
        registered,
        "node alpha must register node beta as a watcher of the room within the mesh budget: \
         {:?}",
        app_a.room_relay.watchers_of(room_id)
    );

    // Bob's one socket goes down on alpha. Alpha announces the `Disconnected` and arms
    // its two-minute grace; carol's socket on beta receives the announcement, which is
    // the proof the crossing happened — and beta's tally has recorded the room as owed
    // by the time the frame is on her wire, because the ingest path records it in the
    // same task step that published it, with no await between the two.
    drop(bob);
    let dark = carol
        .next_member_event_of(bob_grant.account_id, MemberChange::Disconnected)
        .await;
    assert!(!dark.joined, "a disconnect is not a join");

    // Bob comes back — on beta, the node carol watches from. Beta owes the room a
    // `Reconnected` because its tally recorded the drop, and the copy crosses to the
    // home node: alice's socket receives it, which is the frame the old code never
    // sent, because no node but the one that saw the drop ever learned of it. The
    // session is bound to an underscore name on purpose: it must live until the end
    // of the test, because the socket is the fact — dropping it would end the very
    // session whose reconnect is under assertion.
    let _bob_elsewhere = Client::connect_fresh(b_addr, &bob_grant).await;
    let back = alice
        .next_member_event_of(bob_grant.account_id, MemberChange::Reconnected)
        .await;
    assert_eq!(back.user_id, bob_grant.account_id);
    assert!(back.joined, "a reconnect reads as present");
    assert_eq!(back.member_count, Some(3), "nobody lost their seat");

    // Carol sees the same reconnect locally on beta, because beta published it.
    let back_beta = carol
        .next_member_event_of(bob_grant.account_id, MemberChange::Reconnected)
        .await;
    assert_eq!(back_beta.user_id, bob_grant.account_id);

    // The count the home node serves: alice by her local session, bob by the
    // reconnect that crossed, carol by the join that crossed — three of three, where
    // the per-node tally the old code served would have said one.
    let listed: RoomListResponse = alice
        .ask(
            Opcode::RoomList,
            &RoomListRequest {
                limit: 50,
                query: None,
                category: None,
                language: None,
                country: None,
                cursor: None,
            },
        )
        .await;
    let summary = listed
        .rooms
        .iter()
        .find(|room| room.room_id == room_id)
        .expect("the room is in its creator's listing");
    assert_eq!(summary.member_count, 3, "the roster is intact");
    assert_eq!(
        summary.online_count, 3,
        "the home node counts the members whose sessions live on the other node"
    );
}
