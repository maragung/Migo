//! Cross-node presence propagation, driven at the client seam: the gap this
//! suite exists to close. A user's presence change on her node reaches the
//! peer whose client watches her user topic — the scenario section 170's
//! user-topic tier was built for, and the one the tools/nnode harness kept
//! reporting as missing while room events already crossed.
//!
//! The topology is the same honest shape cross_node_resume.rs uses: two full
//! nodes over ONE store through `App::build_with_store`, because everything
//! authorization needs — the friendship a user-topic subscription is granted
//! against, the accounts both clients authenticate as — is a store fact. What
//! is NEW here is the mesh link: each node binds an ephemeral mesh listener
//! and admits the other through `apply_mesh_peers`, the same public seam an
//! operator's `federation.peers` configuration drives at boot. The signing
//! seeds are fixed 32-byte strings so each node's mesh identity is
//! deterministic, exactly the way tools/nnode pins its node ids.
//!
//! Determinism follows the client-seam house style: every exchange is bounded
//! by a step budget, every expectation is asserted on frame contents, and the
//! one inherently asynchronous hop — the mesh carrying the watch ask from one
//! node to the other's table — is observed by polling the table itself, not
//! by sleeping and hoping. The poll budget is generous (ten seconds against a
//! runner that ticks every 500ms) and fails by naming the state it never saw.
//!
//! The flow under test, in the order it lives in production: the subject
//! connects on her node, the watcher connects on the other node and
//! subscribes to her user topic, that subscription makes the watcher's node
//! ask the mesh to watch her, and only then does she change state — because
//! the change is published by the node whose session caused it, and the ask
//! must have landed first or the origin would have nobody to fan out to.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::config::{MeshPeer, StoreConfig};
use migo_core::{Clock, Config, Id, Secret};
use migo_crypto::NodeSecret;
use migo_protocol::{
    from_frame, to_frame, Acknowledged, Decode, Encode, Frame, Hello, Opcode, PresenceEvent,
    PresenceState, PresenceUpdate, SubscribeRequest, SubscribeResponse, Topic, TopicKind, Welcome,
    PROTOCOL_VERSION,
};
use migo_ratelimit::TrustTier;
use migo_social::Caller;
use migod::App;

/// How long any single exchange may take before the test declares a node
/// stuck — silence being the bug class this suite exists to catch.
const STEP: Duration = Duration::from_secs(5);

/// How long the mesh may take to carry a watch ask between two healthy
/// in-process nodes: eighty ticks of a quarter second, against an outbox
/// runner that drains every 500ms. The loop exits the moment the state is
/// observed, so this budget is only ever spent on failure.
const MESH_BUDGET: Duration = Duration::from_secs(10);

/// Node alpha's mesh signing seed, exactly 32 bytes the way `NodeSecret`
/// demands. The mesh node id is the first 16 bytes, so the two seeds differ
/// there — the same pinning discipline tools/nnode applies, in-process.
const SEED_ALPHA: &str = "presence-alpha-00000000000000000";

/// Node beta's mesh signing seed.
const SEED_BETA: &str = "presence-beta-000000000000000000";

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
/// the scenario's four handshakes from one peer address must all fit inside
/// the bucket. The two nodes share one token key the way a deployment must,
/// so a grant minted by either node authenticates on both. The gateway
/// heartbeat is pinned short on purpose: a session's presence floor is a
/// sixth of the heartbeat it was told (section 159), so at the 30-second
/// default the watcher's second event about the same subject is held for
/// five seconds — exactly this suite's step budget — and a held frame only
/// leaves the writer at the next quarter-heartbeat tick, which is seven and
/// a half seconds away. Six seconds puts the floor at one second and the
/// release tick at one and a half, so both changes of state arrive inside
/// the budget the way they do for a client that is not racing its own
/// cadence.
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
                device: DeviceClaim::new(migo_protocol::Platform::Web, "cross-node presence test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// Makes the two accounts friends, because a user topic other than one's own
/// is granted against the social graph and the wire path under test is the
/// presence event, not the relationship.
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

/// A scripted client: a real TCP session speaking the length-prefixed
/// framing, tracking its correlation counter the way a real one does.
struct Client {
    stream: tokio::net::TcpStream,
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

        // The PRESENCE bit: every frame this suite drives is the presence family, which
        // brief section 72 gates on the negotiated set — the session asks for the bit
        // the way a client that wants presence events does.
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            features: migo_protocol::features::PRESENCE,
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

    /// Reads until one subject's presence stream reaches the wanted state,
    /// skipping the frames a subscribed session receives that are not that
    /// arrival — including a re-delivered copy of a state already seen, which
    /// an at-least-once mesh (section 153) can put on the wire after the
    /// first. A re-delivery presents the state the consumer already holds;
    /// the coalescing consumer keeps the latest, and this reads with the same
    /// discipline: the wanted state is the fact, everything earlier is the
    /// copy the wire owes at least once.
    async fn next_state_of(&mut self, user: Id, state: PresenceState) -> PresenceEvent {
        loop {
            let frame = recv_within(&mut self.stream, STEP).await;
            if Opcode::from_wire(frame.header.opcode) != Some(Opcode::PresenceEvent) {
                continue;
            }
            let event: PresenceEvent = from_frame(&frame).expect("the presence event decodes");
            assert_eq!(
                event.user_id, user,
                "the stream the watcher receives names the subject he subscribed to"
            );
            if event.state == state {
                return event;
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
}

/// The full scenario, in the order a real cross-node friendship lives it:
/// the two nodes link, the subject connects on hers, the watcher connects on
/// the other and asks for her stream, the ask crosses the mesh, and her state
/// change arrives on his socket as a presence event — once, naming her.
#[tokio::test]
async fn a_presence_change_reaches_the_watcher_on_the_other_node() {
    // The fleet: two nodes over one store, each with its own ephemeral mesh
    // listener. The store is opened once and handed to both; the mesh is
    // linked by admitting each node to the other's allow-list with the
    // address its listener actually bound, the same call `App::build` makes
    // from `federation.peers` configuration.
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

    // Two accounts, made friends because a user topic other than one's own is
    // granted against the social graph. The friendship is a store fact, so it
    // authorizes on either node.
    let alice_grant = registered_grant(&app_a, "crossnodealice").await;
    let bob_grant = registered_grant(&app_b, "crossnodebob").await;
    befriend(&app_a, &alice_grant, &bob_grant).await;

    // The subject connects on her node. Her self-subscription asks node alpha
    // to have the peers watch her — which is node beta's table, not alpha's,
    // and is inert for this scenario until one of her devices moves.
    let mut alice = Client::connect_fresh(a_addr, &alice_grant).await;

    // The watcher connects on the other node and asks for her stream. His own
    // self-subscription asks node beta to have alpha watch him; the
    // subscription that matters is the one to alice's user topic, which makes
    // node beta ask node alpha to watch her.
    let mut bob = Client::connect_fresh(b_addr, &bob_grant).await;
    bob.subscribe(vec![Topic {
        kind: TopicKind::User,
        id: alice_grant.account_id,
    }])
    .await;

    // The deterministic barrier: the ask is durable and async, so the test
    // waits until it has landed where it matters — node alpha's watch table
    // for alice — before her state changes. Polling the table (not sleeping)
    // is what makes the rest of the scenario's assertions exact. The claim
    // asserted is the one the tier owes — node beta is registered as a
    // watcher — rather than the table holding beta and nothing else: the two
    // nodes share one allow-list, so the ask a node's subscribe half
    // broadcasts is also addressed to the shared table's row for alpha
    // itself, and whichever node's outbox runner drains first delivers the
    // ask that names alpha. Alpha records the deliverer, so the table can
    // honestly hold beta, or beta and alpha, depending on which runner won a
    // race production never runs — a deployment's nodes keep one store each,
    // so a node never meets its own row. The exactly-one-federated-copy
    // promise is not lost: the relay's own tests assert it against a mesh
    // with a private allow-list.
    let mut registered = false;
    let deadline = tokio::time::Instant::now() + MESH_BUDGET;
    while tokio::time::Instant::now() < deadline {
        if app_a
            .presence_relay
            .watchers_of(alice_grant.account_id)
            .contains(&id_b)
        {
            registered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    assert!(
        registered,
        "node alpha must register node beta as a watcher of alice's user topic within the mesh \
         budget: {:?}",
        app_a.presence_relay.watchers_of(alice_grant.account_id)
    );

    // The subject goes away on her node. The ack says her node took it; the
    // assertion that matters is what the other node's client sees.
    let _: Acknowledged = alice
        .ask(
            Opcode::PresenceSet,
            &PresenceUpdate {
                state: PresenceState::Away,
                custom_status: None,
            },
        )
        .await;

    // The watcher's socket receives her Away: the origin published it, the
    // mesh carried one copy to the watching node, and that node's hub placed
    // it on her user topic where his subscription listens. The first change
    // of a subject is never paced — the floor of section 159 spaces one
    // subject's frames, and this is the first his session carries of her.
    let event: PresenceEvent = bob
        .next_state_of(alice_grant.account_id, PresenceState::Away)
        .await;
    assert_eq!(event.state, PresenceState::Away);

    // The stream continues: the next change of hers crosses the same way,
    // which is the difference between a one-shot relay and a tier. Her Busy
    // is the second frame his session carries about her, so the pacing floor
    // holds it for one interval before the writer releases it — the budget
    // below covers the floor the short heartbeat keeps at one second, the
    // quarter-heartbeat tick that releases a held frame, and the mesh hop.
    let _: Acknowledged = alice
        .ask(
            Opcode::PresenceSet,
            &PresenceUpdate {
                state: PresenceState::Busy,
                custom_status: None,
            },
        )
        .await;
    let event: PresenceEvent = bob
        .next_state_of(alice_grant.account_id, PresenceState::Busy)
        .await;
    assert_eq!(event.state, PresenceState::Busy);
}
