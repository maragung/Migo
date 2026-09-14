//! The federation tier's repair after a home node restarts, driven at the
//! client-visible seam: a direct conversation whose far subscriber keeps
//! hearing events after the conversation's home node died and came back with
//! the same node key over the same store — which is what a deployment
//! restart of one node in a fleet actually is.
//!
//! The gap this scenario pins down is the seam between the two halves of the
//! tiered fan-out (section 170). The far node's subscribe cache says "this
//! process already asked the home node to watch this conversation" — once
//! per process, because the outbox owes the peer one ask, not one per
//! subscribing session. The watch table that ask fills is memory on the
//! *home* node's side. A home node that restarts comes back holding an empty
//! table while the far node's cache still says every ask was answered, and
//! no client `SUBSCRIBE` is coming to re-ask — the sessions that wanted the
//! topics are already granted them. The tier is then down silently: a
//! publish on the home node finds no watchers and fans out to nobody, which
//! is indistinguishable on the wire from a conversation that simply went
//! quiet.
//!
//! The re-anchor is the repair: a timer in the composition root
//! (`federation.reanchor_interval_ms`) re-sends the asks the caches
//! remember, and the home node's `register_watcher` is idempotent — a home
//! node that never restarted re-inserts what it already holds, and a
//! restarted one rebuilds its table from the asks that arrive. This test
//! starts the task by hand against an app that never serves, exactly as the
//! sweeper tests start theirs, with the interval shrunk through the same
//! environment variable an operator would set.
//!
//! The in-process death is the cooperative shutdown, the same approximation
//! `cross_node_resume.rs` makes: the gateway drains its sessions and stops,
//! and the rebuilt node binds fresh ports that the relink repoints the peer
//! at — same node key, so same node id, which is what the allow-list and
//! the conversation's `home_region` label key on. What the far side sees is
//! a home node that came back with an empty watch table, which is precisely
//! the state under test.
//!
//! Determinism is the client-seam house style: every exchange is bounded by
//! a step budget, and the one place timing genuinely races — the re-anchor
//! ask landing before the next send leaves the home node — is handled the
//! way a real client handles it, by retrying with *fresh* message ids
//! (section 156: a duplicate id produces no fanout, so a retry that reuses
//! one would be a silent no-op), bounded by attempts, never by sleeping.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::config::{MeshPeer, StoreConfig};
use migo_core::{Clock, Config, Id, OsRandom, Secret, SystemClock, Timestamp};
use migo_crypto::NodeSecret;
use migo_protocol::{
    from_frame, to_frame, ConversationCreateRequest, ConversationKind, ConversationSummary, Decode,
    Encode, Frame, Hello, MessageAccepted, MessageEvent, MessageKind, MessageSend, Opcode,
    Platform, ProfileUpdate, SubscribeRequest, SubscribeResponse, Topic, TopicKind, Welcome,
    PROTOCOL_VERSION,
};
use migo_store::model::{Relationship, Visibility};
use migo_store::SharedStore;
use migod::App;

/// How long any single exchange may take before the test declares a node stuck.
///
/// The re-anchor ask rides the same outbox every federated event rides (the
/// runner drains every half second), so one ask and its ingestion fit inside
/// a second or two; five seconds bounds a request plus the ask with margin.
const STEP: Duration = Duration::from_secs(5);

/// How long one delivery attempt waits for the far participant's event before
/// the sender retries with a fresh message id. Deliberately shorter than STEP:
/// this is the poll inside the retry loop, not the bug-catching budget.
const DELIVERY_WAIT: Duration = Duration::from_secs(2);

/// How many fresh-id sends the retry loop may make. Each attempt is bounded
/// by `DELIVERY_WAIT`, so the whole loop is bounded by attempts × wait — the
/// re-anchor tick and one mesh round trip fit inside the first or second
/// attempt; the rest is margin.
const SEND_ATTEMPTS: usize = 10;

/// The node signing keys, exactly 32 bytes each so the mesh identity derives
/// from them the way a production node's does: the node id is the first 16
/// bytes and the Ed25519 key pair is the whole string. The compile-time check
/// keeps them that way — `NodeSecret::from_seed` demands exactly 32 bytes, and
/// a literal one byte short would otherwise only surface at test boot.
const ALPHA_KEY: &str = "alpha-node-mesh-key-000000000000";
const BETA_KEY: &str = "beta-node-mesh-key-0000000000000";
const _: () = assert!(ALPHA_KEY.len() == 32);
const _: () = assert!(BETA_KEY.len() == 32);

/// What alice's device sealed for bob before the restart, proving the tier
/// worked when it was first anchored. The bytes are opaque to both nodes on
/// the way; the test asserts they arrive exactly as they left.
const BASELINE_SEALED: &[u8] = b"sealed-before-the-home-node-restarted";

/// What alice's device seals for bob after the home node came back — the
/// bytes that must cross the *repaired* tier, and different from the baseline
/// so the assertion cannot be satisfied by a stale or replayed event.
const POST_RESTART_SEALED: &[u8] = b"sealed-after-the-home-node-restarted";

/// The re-anchor interval the test runs with, in milliseconds: the production
/// default is a minute, which is the repair latency an operator tolerates,
/// but a test needs the repair inside its own clock. Both nodes get it
/// because the helper has one shape; only the subscriber node's tick matters
/// here — the home node's caches hold nothing to re-send for a conversation
/// it homes.
const TEST_REANCHOR_MS: u64 = 200;

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// The mesh identity a signing key implies: the node id (first 16 bytes) and
/// the base64 public key an operator would paste into the peer's allow-list.
/// The same derivation `App::build` performs internally, read here so the two
/// nodes can be named to each other exactly the way configuration would.
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

/// Builds one full node over its own store: its own TCP listener for the
/// clients, its own mesh listener for the peer, and its own node identity.
///
/// `federation.enabled` stays off on purpose. The flag exists for
/// configuration validation (peers and a signing key must accompany it), and
/// dummy peer entries at build time would only buy refused dials and delivery
/// backoff; the peers are admitted after both listeners have bound, through
/// the same `apply_mesh_peers` a deployment's restart runs, so the addresses
/// the entries name are the real ones.
///
/// The re-anchor interval comes in through the environment — the same
/// `MIGO_FEDERATION__REANCHOR_INTERVAL_MS` an operator would set — because
/// that is the whole point of the knob: the repair cadence is a deployment
/// concern, not a code constant, and the test shrinks it the only way it can.
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
            (
                "MIGO_FEDERATION__REANCHOR_INTERVAL_MS".to_string(),
                TEST_REANCHOR_MS.to_string(),
            ),
        ],
    )
    .expect("configuration should parse");
    App::build_with_store(&config, store.clone())
        .await
        .expect("a development node must build over its own store")
}

/// Names each node to the other, both directions, the way two configuration
/// documents would: the canonical node id, the base64 public key, the address
/// the peer's mesh listener actually bound, and the region label the peer
/// itself claims — which is how the conversation's `home_region` resolves.
/// Called again after the home node's rebuild, which is the deployment's
/// restart shape: the entry's address is brought to what changed, and nothing
/// else is.
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

/// Registers one account through the front door of a node.
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
                device: DeviceClaim::new(Platform::Web, "relay reanchor test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// Seeds the one friend edge an account owns on the node that account belongs
/// to — the same stand-in `dm_federation.rs` makes, for the same reason: the
/// friend handshake does not federate, so what is seeded is exactly what the
/// store's own acceptance path writes on one node, and nothing more. The
/// cross edges are not seeded; they cross inside the row-replication answers.
async fn seed_own_friend_edge(store: &SharedStore, owner: Id, peer: Id, at: Timestamp) {
    store
        .put_relationship(Relationship {
            account_id: owner,
            other_id: peer,
            kind: migo_protocol::RelationshipKind::Friend,
            created_at: at,
            accepted_at: Some(at),
        })
        .await
        .expect("the node that owns the account takes its own friendship edge");
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

/// Reads one length-prefixed frame if it arrives within `limit`, `None` if
/// nothing did. The retry loop's poll: a delivery that has not landed yet is
/// an expected state, not a stuck node.
async fn try_recv_within(stream: &mut tokio::net::TcpStream, limit: Duration) -> Option<Frame> {
    let mut head = [0u8; 4];
    match tokio::time::timeout(limit, stream.read_exact(&mut head)).await {
        Err(_) => return None,
        Ok(Err(error)) => panic!("unexpected read error while polling: {error}"),
        Ok(Ok(_)) => {}
    }
    let mut body = vec![0u8; u32::from_be_bytes(head) as usize];
    tokio::time::timeout(STEP, stream.read_exact(&mut body))
        .await
        .expect("a frame whose head arrived does not stall mid-body")
        .expect("the polled frame's body arrives");
    Some(Frame::decode(body.into()).expect("the polled frame decodes"))
}

/// A scripted client: a real TCP session speaking the length-prefixed framing.
struct Client {
    stream: tokio::net::TcpStream,
    node: String,
    region: String,
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
            "the inline token authenticated the session on the node that registered it"
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
                    node: welcome.node.node_id,
                    region: welcome.node.region,
                    correlation: 2,
                };
            }
        }
    }

    /// Reads one frame.
    async fn next_frame(&mut self) -> Frame {
        recv_within(&mut self.stream, STEP).await
    }

    /// Sends a request that must succeed, returning the decoded reply. Frames
    /// that are not the reply are read and kept, never discarded silently.
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

    /// Sends a request that must succeed, keeping only the fact that it did.
    /// The profile update's reply is the caller's refreshed card, and this
    /// test cares about the *far* node's copy of that card, not the near one.
    async fn expect_ok<M: Encode>(&mut self, opcode: Opcode, message: &M) {
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
                return;
            }
        }
    }

    /// Reads until a frame of the wanted opcode arrives or `limit` passes with
    /// nothing, whichever comes first — the retry loop's poll.
    async fn try_next_event(&mut self, want: Opcode, limit: Duration) -> Option<Frame> {
        loop {
            let frame = try_recv_within(&mut self.stream, limit).await?;
            if Opcode::from_wire(frame.header.opcode) == Some(want) {
                return Some(frame);
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
    async fn send_message(&mut self, conversation: Id, envelope: &[u8]) -> MessageAccepted {
        let message_id = Id::generate(SystemClock.now().as_unix_ms().max(0) as u64, &mut OsRandom);
        self.ask(
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
        .await
    }
}

/// The full scenario: a direct conversation homed on one node with its far
/// subscriber on another, the home node dying and coming back over the same
/// store with the same node key, and the far subscriber hearing events again
/// — because the subscriber node re-sent the watch ask its cache remembered,
/// not because anybody re-subscribed.
#[tokio::test]
async fn a_far_subscriber_hears_events_again_after_the_home_node_restarts() {
    // The fleet: two full nodes, a store each — the shape where the far
    // participant is unreachable without the mesh, which is the point.
    let store_alpha = migo_store::open(&StoreConfig::default())
        .await
        .expect("the in-memory backend opens");
    let store_beta = migo_store::open(&StoreConfig::default())
        .await
        .expect("the in-memory backend opens");
    let alpha = build_node(&store_alpha, "node-alpha", "alpha", ALPHA_KEY).await;
    let beta = build_node(&store_beta, "node-beta", "beta", BETA_KEY).await;
    let a_addr = alpha.tcp_bind.expect("node alpha binds its TCP listener");
    let b_addr = beta.tcp_bind.expect("node beta binds its TCP listener");
    link_peers(&alpha, &beta).await;

    // Two accounts, each registered through the front door of a *different*
    // node, exactly as `dm_federation.rs` seats them: bob's rows never exist
    // anywhere but beta, alice's never anywhere but alpha, and every row that
    // crosses does so over the mesh.
    let alice = registered_grant(&alpha, "reanchoralice").await;
    let bob = registered_grant(&beta, "reanchorbob").await;
    let mut alice_client = Client::connect_fresh(a_addr, &alice).await;
    assert_eq!(alice_client.node, "node-alpha");
    let mut bob_client = Client::connect_fresh(b_addr, &bob).await;
    assert_eq!(bob_client.node, "node-beta");
    assert_eq!(bob_client.region, "beta");

    // Each participant narrows their own messaging privacy to friends, so the
    // conversation create's gate has a real setting to enforce and the
    // friendship edges have a verdict to decide — the same run-up the
    // federation harness performs, kept verbatim so the only thing this test
    // changes about the world is the home node's restart.
    alice_client
        .expect_ok(
            Opcode::ProfileUpdate,
            &ProfileUpdate {
                who_can_message: Some(u32::from(Visibility::Friends.to_i16() as u16)),
                ..Default::default()
            },
        )
        .await;
    bob_client
        .expect_ok(
            Opcode::ProfileUpdate,
            &ProfileUpdate {
                who_can_message: Some(u32::from(Visibility::Friends.to_i16() as u16)),
                ..Default::default()
            },
        )
        .await;
    let friend_at = alpha.clock.now();
    seed_own_friend_edge(&store_alpha, alice.account_id, bob.account_id, friend_at).await;
    seed_own_friend_edge(&store_beta, bob.account_id, alice.account_id, friend_at).await;

    // Alice opens the conversation through her own wire session, on her own
    // node — the node the row stamps as the conversation's home.
    let summary: ConversationSummary = alice_client
        .ask(
            Opcode::ConversationCreate,
            &ConversationCreateRequest {
                kind: ConversationKind::Direct,
                members: vec![bob.account_id],
                title: None,
            },
        )
        .await;
    let conversation = summary.conversation_id;
    let row = store_alpha
        .conversation(conversation)
        .await
        .expect("the home store reads")
        .expect("the conversation row exists");
    assert_eq!(
        row.home_region, "alpha",
        "the creating node stamped itself as the conversation's home"
    );

    // Both participants subscribe their side. Alice's subscribe needs no ask
    // — her node is the home node. Bob's subscribe is where the tier is
    // anchored: beta's relay asks alpha to watch the conversation, once, and
    // alpha's watch table gains its one entry.
    alice_client
        .subscribe(vec![Topic {
            kind: TopicKind::Conversation,
            id: conversation,
        }])
        .await;
    bob_client
        .subscribe(vec![Topic {
            kind: TopicKind::Conversation,
            id: conversation,
        }])
        .await;

    // The baseline: the tier works while the anchor holds. Alice sends,
    // retrying with fresh message ids until the delivery lands — the same
    // race `dm_federation.rs` retries around, that her first send may leave
    // before beta's watch registration arrives.
    let mut baseline_seq = 0;
    let mut delivered: Option<MessageEvent> = None;
    for attempt in 0..SEND_ATTEMPTS {
        let accepted = alice_client
            .send_message(conversation, BASELINE_SEALED)
            .await;
        assert_eq!(
            accepted.seq,
            attempt as u64 + 1,
            "each fresh-id send is a new message on the home node's store"
        );
        baseline_seq = accepted.seq;
        if let Some(frame) = bob_client
            .try_next_event(Opcode::MessageEvent, DELIVERY_WAIT)
            .await
        {
            let event: MessageEvent = from_frame(&frame).expect("the crossing message decodes");
            assert_eq!(
                event.sender_id, alice.account_id,
                "the event that crossed is alice's, not bob's own echo"
            );
            assert_eq!(event.conversation_id, conversation);
            assert_eq!(
                event.envelope, BASELINE_SEALED,
                "the sealed envelope bob's device holds is byte for byte what alice's device \
                 sealed — two nodes carried it and neither opened it"
            );
            delivered = Some(event);
            break;
        }
    }
    delivered.expect("the tier worked before the restart — a sealed message crossed");

    // Node alpha dies. In-process that is the cooperative shutdown: the
    // gateway drains each session with a RECONNECT_HINT and closes, and the
    // client's side of the death is just the connection ending. The client
    // object is dropped unread — what the drain sends on the way out is
    // `cross_node_resume.rs`'s concern, not this test's.
    alpha.shutdown.trigger();
    drop(alice_client);

    // And comes back: same node key, so same node id, over the same store —
    // the deployment's restart shape. The rebuilt node binds fresh ports,
    // holds a fresh watch table with nothing in it, and its own subscribe
    // caches are empty too, because a node that homes the conversation never
    // asks anybody to watch it. The relink repoints beta's entry for the same
    // node id at the new mesh address, exactly as the configuration-driven
    // `apply_mesh_peers` a real restart runs.
    let alpha = build_node(&store_alpha, "node-alpha", "alpha", ALPHA_KEY).await;
    let a_addr = alpha
        .tcp_bind
        .expect("the rebuilt node binds its TCP listener");
    link_peers(&alpha, &beta).await;

    // Alice returns as a fresh session on the rebuilt home node. Her grant
    // still verifies — same token key, same store — and her subscribe is
    // granted from the row that survived the restart.
    let mut alice_client = Client::connect_fresh(a_addr, &alice).await;
    assert_eq!(
        alice_client.node, "node-alpha",
        "the rebuilt node kept its identity"
    );
    alice_client
        .subscribe(vec![Topic {
            kind: TopicKind::Conversation,
            id: conversation,
        }])
        .await;

    // Beta's re-anchor, started by hand exactly as the sweeper tests start
    // theirs. Production starts it in `serve` for the whole serve; here it
    // starts at the moment the scenario needs it, running with the interval
    // the environment shrank. Without it, this is the silent outage: beta's
    // cache says the ask was answered, alpha's replacement holds an empty
    // watch table, no client SUBSCRIBE is coming to re-ask, and everything
    // alice sends below fans out to nobody — the loop under it exhausts its
    // budget and the test fails on the silence.
    let reanchor = beta.spawn_relay_reanchor();

    // The proof: alice keeps sending, bob keeps polling. The re-anchor's ask
    // lands on the rebuilt home node between one attempt and the next, the
    // watch table gains its entry back, and the sends that follow fan out to
    // the one node that holds the far subscriber. The seq assertion is the
    // restart made visible: the home node's store survived its own death, so
    // the numbering continues from where the baseline left it.
    let mut resumed: Option<MessageEvent> = None;
    for attempt in 0..SEND_ATTEMPTS {
        let accepted = alice_client
            .send_message(conversation, POST_RESTART_SEALED)
            .await;
        assert_eq!(
            accepted.seq,
            baseline_seq + attempt as u64 + 1,
            "the home node's store survived its own restart; the numbering continues"
        );
        if let Some(frame) = bob_client
            .try_next_event(Opcode::MessageEvent, DELIVERY_WAIT)
            .await
        {
            let event: MessageEvent = from_frame(&frame).expect("the resumed message decodes");
            assert_eq!(
                event.sender_id, alice.account_id,
                "the event that crossed is alice's, not bob's own echo"
            );
            assert_eq!(event.conversation_id, conversation);
            resumed = Some(event);
            break;
        }
    }
    let resumed = resumed.expect(
        "the re-anchor repaired the tier within the retry budget — a sealed \
                        message crossed the restarted home node to the far subscriber",
    );
    assert_eq!(
        resumed.envelope, POST_RESTART_SEALED,
        "the sealed envelope bob's device holds is byte for byte what alice's device sealed \
         after the restart — the repaired tier carried it and neither node opened it"
    );

    // The task dies with its node: the cooperative shutdown ends the loop the
    // serve would have ended, so the test leaves no task behind it.
    beta.shutdown.trigger();
    let _ = reanchor.await;
    alpha.shutdown.trigger();
}
