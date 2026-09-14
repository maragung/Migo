//! Direct-message federation, driven at the client-visible seam: a direct
//! conversation whose two participants were *registered on different nodes*,
//! each message crossing the mesh between them as a sealed envelope nobody on
//! the way opens.
//!
//! This is the scenario section 170 spent its whole honest tail admitting was
//! missing, and the gap it admitted is now closed for the rows this scenario
//! needs. Two full nodes, a store per node (the tools/2node shape, not the
//! shared-store one `cross_node_resume.rs` builds) — and no hand-copying at
//! all: bob's account, profile, and social rows exist *only* on the node he
//! registered through, and alice's only on hers. The conversation row is
//! created on alice's node and reaches bob's the same way. What carries them
//! is the row-replication tier (section 170's account-to-node routing map): a
//! gate that fail-closes on a missing row asks the mesh which node holds it
//! (`FED_ACCOUNT_QUERY` / `FED_CONVERSATION_QUERY`), the owner answers with
//! the rows verbatim (`FED_ACCOUNT_ROWS` / `FED_CONVERSATION_ROWS`), and the
//! gate asks its question again — fail-closed all the way, so an unreachable
//! owner changes nothing.
//!
//! The tiered fan-out is what carries the messages themselves: the
//! conversation row names a home node (`home_region`, stamped at creation),
//! the far node asks the home node to watch the conversation once per process
//! (`FED_CONVERSATION_SUBSCRIBE`), and every publish is handed to the node
//! that owns the fan-out — one federated copy per watching node
//! (`FED_CONVERSATION_EVENT`), the inner event frame sealed exactly as a
//! local session would have received it.
//!
//! And since the message-row tier, the crossing is not push-only: the ingest
//! path seats every `MessageEvent` that crosses as a *row* in the receiving
//! node's store — idempotently, never over a row the node already holds, with
//! the sending node's seq verbatim — so a sync asked on the far node answers
//! from real rows rather than an empty transcript. This scenario drives that
//! half too: each participant syncs from their own node for a message the
//! *other* node accepted, and an edit and a deletion cross the same way and
//! land on the far row, because the wire's one event shape carries all three.
//!
//! The sealed envelope is the whole security claim under test. Direct messages
//! are sealed client-side (section 170's standing rule); the frames this
//! scenario pushes across the mesh carry those bytes, and the test asserts
//! byte-for-byte equality on both ends — what alice's device sealed is what
//! bob's device decrypts, and the two nodes in between (one of which is the
//! conversation's home node and fans it out) see a conversation id and an
//! opaque payload, which is everything they need to route and nothing they
//! could read.
//!
//! What is *still* seeded by hand, and honestly so: the friend handshake
//! itself does not federate. A pending friend request is per-node state — the
//! request and its acceptance are writes against the graph of the node that
//! took them — so this test seeds each node with the one edge its own account
//! owns (alice's `alice → bob` edge on alpha, bob's `bob → alice` edge on
//! beta), which is exactly the state the store's own acceptance path would
//! leave behind and exactly the state a friend-request federation tier will
//! produce when it exists. The *cross* edges are not seeded: bob's edge toward
//! alice crosses to alpha inside the row-replication answer, and alice's
//! toward bob crosses to beta the same way, and the test asserts both arrived
//! — the replication of edges is proven, only their creation stays local.
//!
//! Determinism is the client-seam house style: every exchange is bounded by a
//! step budget, and the one place timing genuinely races — alice's first send
//! may leave node alpha before beta's watch registration lands — is handled
//! the way a real client handles it, by retrying with *fresh* message ids
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
    Encode, Frame, Hello, MessageAccepted, MessageDelete, MessageEdit, MessageEvent, MessageKind,
    MessageSend, Opcode, Platform, ProfileUpdate, SubscribeRequest, SubscribeResponse, SyncRequest,
    SyncResponse, Topic, TopicKind, Welcome, PROTOCOL_VERSION,
};
use migo_store::model::{Relationship, Visibility};
use migo_store::SharedStore;
use migod::App;

/// How long any single exchange may take before the test declares a node stuck.
///
/// The row-replication pulls ride inside ordinary requests — a create, a
/// subscribe, a send — and each costs one mesh round trip (the outbox runner
/// drains every half second, so a question and its answer fit inside a second
/// or two) on top of the request's own work. Five seconds bounds a request
/// *plus* one pull with margin, and the pull's own wait is capped at three
/// seconds inside the relay, so a stuck node still trips this budget rather
/// than hanging the test.
const STEP: Duration = Duration::from_secs(5);

/// How long one delivery attempt waits for the far participant's event before
/// the sender retries with a fresh message id. Deliberately shorter than STEP:
/// this is the poll inside the retry loop, not the bug-catching budget.
const DELIVERY_WAIT: Duration = Duration::from_secs(2);

/// How many fresh-id sends the retry loop may make. Each attempt is bounded by
/// `DELIVERY_WAIT`, so the whole loop is bounded by attempts × wait — the
/// mesh's runner tick (half a second) and one dial fit inside the first or
/// second attempt; the rest is margin.
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

/// What alice's device sealed for bob. The bytes are opaque to both nodes on
/// the way; the test asserts they arrive exactly as they left.
const ALICE_SEALED: &[u8] = b"sealed-by-alices-device-for-bob";

/// What bob's device sealed back for alice, crossing the other direction: the
/// reply leaves the non-home node, is carried to the home node, and is served
/// from the home node's own hub to alice's session there.
const BOB_SEALED: &[u8] = b"sealed-by-bobs-device-for-alice";

/// What alice's device sealed as the replacement for her original message.
/// The edit crosses as the same event shape the original send did, so the
/// assertion is on sealed bytes here too, not on a flag.
const ALICE_EDITED: &[u8] = b"sealed-by-alices-device-for-bob-edited";

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
        .expect("a development node must build over its own store")
}

/// Names each node to the other, both directions, the way two configuration
/// documents would: the canonical node id, the base64 public key, the address
/// the peer's mesh listener actually bound, and the region label the peer
/// itself claims — which is how the conversation's `home_region` resolves.
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
                device: DeviceClaim::new(Platform::Web, "dm federation test"),
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
/// to — the *remaining* stand-in, and the only one this test still makes.
///
/// The friend handshake does not federate: a pending request and its
/// acceptance are writes against the graph of the node that took them, so
/// there is no cross-node path today that would leave these rows behind. What
/// is seeded is exactly what the store's own acceptance path writes on one
/// node — an accepted edge owned by the account this node registered — and
/// nothing more. The cross edges (bob's toward alice on alpha, alice's toward
/// bob on beta) are *not* seeded: they cross inside the row-replication
/// answers, and the test asserts they arrived.
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

    /// Syncs forward from zero and hands back the row this node holds for one
    /// message id, if it holds one yet.
    ///
    /// This is the poll primitive for the message-row tier's assertions,
    /// because the live push and the row seat are two halves of one delivery
    /// and the push can arrive first: the hub copy is published before the
    /// ingest path seats the row, so a sync asked the instant the push lands
    /// can honestly answer from a store that has not written yet. The caller
    /// bounds the polling, exactly the way the send retry loop does.
    async fn held_row(&mut self, conversation: Id, message_id: Id) -> Option<MessageEvent> {
        let page: SyncResponse = self
            .ask(
                Opcode::Sync,
                &SyncRequest {
                    conversation_id: conversation,
                    have_seq: 0,
                    limit: 50,
                    to_seq: None,
                    backwards: None,
                },
            )
            .await;
        page.messages
            .into_iter()
            .find(|message| message.message_id == message_id)
    }

    /// Reads until the message event for one specific message arrives.
    ///
    /// The send retry loop's bounded waits can leave a straggler behind — a
    /// late attempt's event is a perfectly legal `MessageEvent` that is
    /// simply not the one being asked for — so matching on the message id
    /// rather than the opcode alone is what keeps the later edit and
    /// tombstone assertions honest.
    async fn next_message_event_for(&mut self, message_id: Id) -> MessageEvent {
        loop {
            let frame = self.next_frame().await;
            if Opcode::from_wire(frame.header.opcode) == Some(Opcode::MessageEvent) {
                if let Ok(event) = from_frame::<MessageEvent>(&frame) {
                    if event.message_id == message_id {
                        return event;
                    }
                }
            }
        }
    }
}

/// The full scenario: two accounts registered on two different nodes, a
/// direct conversation created on one and subscribed on the other, and the
/// sealed messages crossing both directions over the mesh — the home node
/// fanning out, the far node handing its reply to the home node, and neither
/// ever holding anything but an opaque envelope. No row is copied by hand:
/// every account, profile, edge, and conversation row that crosses, crosses
/// over the mesh.
#[tokio::test]
async fn a_direct_conversations_sealed_messages_reach_a_peer_on_another_node() {
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
    // node. Bob's rows never exist anywhere but beta; alice's never anywhere
    // but alpha. The assertion is a precondition, not a formality: it is what
    // makes every later row on the far store a row the mesh carried.
    let alice = registered_grant(&alpha, "dmfedalice").await;
    let bob = registered_grant(&beta, "dmfedbob").await;
    assert!(
        store_alpha
            .account_by_id(bob.account_id)
            .await
            .expect("the home store reads")
            .is_none(),
        "bob's account does not exist on node alpha before the mesh carries it"
    );
    assert!(
        store_beta
            .account_by_id(alice.account_id)
            .await
            .expect("the home store reads")
            .is_none(),
        "alice's account does not exist on node beta before the mesh carries it"
    );

    // Each participant narrows their own messaging privacy to friends —
    // through the front door, the PROFILE_UPDATE every client sends — so the
    // gate has a real setting to enforce and the friendship edges have a
    // verdict to decide. Both settings are facts of the node that owns the
    // account, which is exactly what the pull must carry across.
    let mut alice_client = Client::connect_fresh(a_addr, &alice).await;
    assert_eq!(alice_client.node, "node-alpha");
    let mut bob_client = Client::connect_fresh(b_addr, &bob).await;
    assert_eq!(
        bob_client.node, "node-beta",
        "bob is genuinely seated on the other node"
    );
    assert_eq!(bob_client.region, "beta");
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

    // The friendship, one edge each: alice's own edge on her node, bob's own
    // edge on his. This is the one remaining hand-seeded stand-in — the friend
    // handshake does not federate — and it seeds only what each node's own
    // acceptance path would have written. The cross edges are not seeded; the
    // assertions at the end prove the replication answer carried them.
    let friend_at = alpha.clock.now();
    seed_own_friend_edge(&store_alpha, alice.account_id, bob.account_id, friend_at).await;
    seed_own_friend_edge(&store_beta, bob.account_id, alice.account_id, friend_at).await;

    // Alice opens the conversation through her own wire session. The create's
    // privacy gate fail-closed on a recipient whose profile node alpha does
    // not hold — bob registered on beta — and the row-replication tier pulled
    // his account, profile, and his edge toward alice from beta before the
    // gate asked again. The reply arriving at all is the proof the pull
    // answered; the row it wrote is checked right below.
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

    // Bob's rows crossed as a *replica*, verbatim: the account, the profile
    // with the privacy setting he set through beta's front door, and the edge
    // he owns toward alice. Nothing was derived and nothing was hand-copied —
    // the assertion reads the far store, which only the mesh could have
    // filled.
    let bob_replica = store_alpha
        .account_by_id(bob.account_id)
        .await
        .expect("the far store reads")
        .expect("bob's account crossed the mesh inside the create's pull");
    assert_eq!(bob_replica.username, "dmfedbob");
    let bob_profile_replica = store_alpha
        .profile(bob.account_id)
        .await
        .expect("the far store reads")
        .expect("bob's profile crossed with the account");
    assert_eq!(
        bob_profile_replica.who_can_message,
        Visibility::Friends,
        "the privacy setting bob set on beta crossed verbatim"
    );
    let bob_edge_replica = store_alpha
        .relationship(
            bob.account_id,
            alice.account_id,
            migo_protocol::RelationshipKind::Friend,
        )
        .await
        .expect("the far store reads")
        .expect("bob's own-side friendship edge crossed inside the answer");
    assert!(
        bob_edge_replica.accepted_at.is_some(),
        "the accepted edge crossed whole, timestamps and all"
    );

    // Both participants subscribe their side. Alice's subscribe needs no ask —
    // her node is the home node. Bob's subscribe is where the conversation's
    // row crosses: beta's membership read came back empty (the row lives on
    // alpha), the row-replication tier pulled it from the home node, and the
    // subscription was granted against the replica — then the conversation
    // tier asked alpha to watch it, once.
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
    let beta_row = store_beta
        .conversation(conversation)
        .await
        .expect("the far store reads")
        .expect("the conversation row crossed the mesh inside bob's subscribe");
    assert_eq!(
        beta_row.home_region, "alpha",
        "the replica kept the home label, so both nodes agree on who owns the fan-out"
    );

    // Alice sends, retrying with fresh message ids until the delivery lands.
    // The race is real: her first send may leave node alpha before beta's
    // watch registration arrives, and that send is simply not fanned out —
    // there was no watcher to fan it out to. A retry that reused the message
    // id would be a no-op (section 156's idempotency), so every attempt mints
    // a fresh one. The sealed bytes are the same on every attempt, so whatever
    // attempt crosses, bob decrypts these bytes.
    let mut delivered: Option<MessageEvent> = None;
    for attempt in 0..SEND_ATTEMPTS {
        let (_, accepted) = alice_client.send_message(conversation, ALICE_SEALED).await;
        assert_eq!(
            accepted.seq,
            attempt as u64 + 1,
            "each fresh-id send is a new message on the home node's store"
        );
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
            delivered = Some(event);
            break;
        }
    }
    let alice_to_bob =
        delivered.expect("a sealed message crossed the mesh within the retry budget");
    assert_eq!(
        alice_to_bob.envelope, ALICE_SEALED,
        "the sealed envelope bob's device holds is byte for byte what alice's device sealed — \
         two nodes carried it and neither opened it"
    );

    // Bob answers from the far node. His send is gated on beta — alice's
    // `who_can_message` is a fact of alpha, and beta holds none of her rows
    // yet — so the gate's pull runs here too: alice's account, profile, and
    // her edge toward bob cross from alpha inside this very request, and the
    // send proceeds against rows the mesh just delivered. The send itself
    // lands in beta's store, and its number is the honest one for a node that
    // now *holds* the conversation's rows: the seated replica of alice's
    // message advanced beta's high-water mark to her seq or past it (a late
    // straggler from the retry loop may have advanced it further by the time
    // the append runs), so beta's own send is numbered beyond the mark —
    // still beta sequencing its own sends (section 170), just no longer from
    // a counter that pretends the replica never arrived — and the forward
    // half hands it to the conversation's home node, because beta holds no
    // watch table and cannot tier it itself.
    let (_, bob_accepted) = bob_client.send_message(conversation, BOB_SEALED).await;
    assert!(
        bob_accepted.seq > alice_to_bob.seq,
        "beta numbers its own send past the replica's high-water mark ({} > {})",
        bob_accepted.seq,
        alice_to_bob.seq
    );

    // And the pull inside that send left a replica behind, the mirror of the
    // one the create left on alpha: alice's account, her privacy setting, and
    // her own-side edge — carried by the mesh, never by the test.
    let alice_profile_replica = store_beta
        .profile(alice.account_id)
        .await
        .expect("the far store reads")
        .expect("alice's profile crossed the mesh inside bob's send");
    assert_eq!(alice_profile_replica.who_can_message, Visibility::Friends);
    assert!(
        store_beta
            .relationship(
                alice.account_id,
                bob.account_id,
                migo_protocol::RelationshipKind::Friend
            )
            .await
            .expect("the far store reads")
            .expect("alice's own-side friendship edge crossed inside the answer")
            .accepted_at
            .is_some(),
        "the cross edge the test never seeded is there — the answer carried it"
    );

    // Alice receives the reply from her own node's hub: the home node
    // published what beta forwarded, sealed as beta's hub had it.
    let reply_frame = alice_client.next_event(Opcode::MessageEvent).await;
    let reply: MessageEvent = from_frame(&reply_frame).expect("the reply decodes");
    assert_eq!(reply.sender_id, bob.account_id);
    assert_eq!(reply.conversation_id, conversation);
    assert_eq!(
        reply.envelope, BOB_SEALED,
        "the reply crossed the far node, the home node, and arrived sealed exactly as bob's \
         device left it"
    );

    // And the label held: both stores still name the same home node, which is
    // what keeps the tier's fan-out authority in one place — and the member
    // rows the replica seated are the pair the conversation was created with.
    for store in [&store_alpha, &store_beta] {
        let row = store
            .conversation(conversation)
            .await
            .expect("the store reads")
            .expect("the conversation row exists");
        assert_eq!(row.home_region, "alpha");
        assert_eq!(
            store
                .members(conversation)
                .await
                .expect("the store reads")
                .len(),
            2,
            "both participants are seated wherever the row is"
        );
    }

    // --- the message-row tier, driven at the same seam -------------------
    //
    // Everything above proved the push; what follows proves the row. A node
    // that watches a conversation now also holds its messages, so sync on
    // either node answers from real rows. The polls are bounded the way the
    // send retry loop is, because the live push and the row seat are two
    // halves of one delivery and the push can arrive first.

    // Bob syncs from beta — the node that never accepted alice's send — and
    // the page holds her message as a row, sealed bytes and all, with the seq
    // alpha assigned it verbatim. Before the tier this page was empty, which
    // was the whole gap: a reconnect or a fresh device on the far node lost
    // the transcript the push had already shown.
    let mut alice_row = None;
    for _ in 0..SEND_ATTEMPTS {
        if let Some(row) = bob_client
            .held_row(conversation, alice_to_bob.message_id)
            .await
        {
            alice_row = Some(row);
            break;
        }
    }
    let alice_row = alice_row.expect("the crossed message is seated as a row on the far store");
    assert_eq!(
        alice_row.envelope, ALICE_SEALED,
        "the row the far node holds carries the sealed bytes unopened"
    );
    assert_eq!(
        alice_row.seq, alice_to_bob.seq,
        "the replica carries the sending node's seq verbatim"
    );
    assert_eq!(alice_row.sender_id, alice.account_id);

    // And the mirror: alice syncs from alpha — the node that never accepted
    // bob's reply — and the page holds it, because the forwarded event seated
    // the replica on the home node too. Bob's reply is found by sender rather
    // than by id because alpha holds every retry attempt's row as well, all
    // of them alice's own sends, and only one row there is bob's.
    let mut bob_row = None;
    for _ in 0..SEND_ATTEMPTS {
        let page: SyncResponse = alice_client
            .ask(
                Opcode::Sync,
                &SyncRequest {
                    conversation_id: conversation,
                    have_seq: 0,
                    limit: 50,
                    to_seq: None,
                    backwards: None,
                },
            )
            .await;
        if let Some(row) = page
            .messages
            .into_iter()
            .find(|message| message.sender_id == bob.account_id)
        {
            bob_row = Some(row);
            break;
        }
    }
    let bob_row = bob_row.expect("the forwarded reply is seated as a row on the home store");
    assert_eq!(
        bob_row.envelope, BOB_SEALED,
        "the reply's row crossed back sealed exactly as bob's device left it"
    );

    // An edit crosses the same way: the wire's one event shape carries it,
    // the far node's live push delivers it, and the far row carries it —
    // envelope and edit stamp both, applied to the row the tier seated.
    alice_client
        .expect_ok(
            Opcode::MessageEdit,
            &MessageEdit {
                message_id: alice_to_bob.message_id,
                conversation_id: conversation,
                envelope: ALICE_EDITED.to_vec(),
            },
        )
        .await;
    let edited_push = bob_client
        .next_message_event_for(alice_to_bob.message_id)
        .await;
    assert_eq!(edited_push.envelope, ALICE_EDITED);
    assert!(
        edited_push.edited_at.is_some(),
        "the crossing edit carries its stamp"
    );
    assert_eq!(edited_push.deleted, None);
    let mut edited_row = None;
    for _ in 0..SEND_ATTEMPTS {
        let row = bob_client
            .held_row(conversation, alice_to_bob.message_id)
            .await
            .expect("the row the edit applies to is held");
        if row.edited_at.is_some() {
            edited_row = Some(row);
            break;
        }
    }
    let edited_row = edited_row.expect("the far row carries the edit the event brought");
    assert_eq!(edited_row.envelope, ALICE_EDITED);
    assert!(edited_row.edited_at.is_some());
    assert_eq!(edited_row.deleted, None);

    // And a deletion crosses as a tombstone: the payload goes with it on the
    // far node exactly as it went on the origin, so a fresh device on beta
    // reads the same deletion bob's live session was pushed.
    alice_client
        .expect_ok(
            Opcode::MessageDelete,
            &MessageDelete {
                message_id: alice_to_bob.message_id,
                conversation_id: conversation,
                for_everyone: true,
            },
        )
        .await;
    let tombstone_push = bob_client
        .next_message_event_for(alice_to_bob.message_id)
        .await;
    assert_eq!(
        tombstone_push.deleted,
        Some(true),
        "the crossing tombstone says deleted"
    );
    assert!(
        tombstone_push.envelope.is_empty(),
        "the tombstone carries no payload"
    );
    let mut gone_row = None;
    for _ in 0..SEND_ATTEMPTS {
        let row = bob_client
            .held_row(conversation, alice_to_bob.message_id)
            .await
            .expect("the tombstoned row is still held");
        if row.deleted == Some(true) {
            gone_row = Some(row);
            break;
        }
    }
    let gone_row = gone_row.expect("the far row carries the tombstone the event brought");
    assert!(
        gone_row.envelope.is_empty(),
        "a replica deletion that kept the ciphertext deleted nothing"
    );

    // The row the far store holds is the store's own shape too: the wire
    // carries no expiry, so a replica is seated without one — the recorded
    // gap, asserted here so it stays a documented fact rather than a
    // surprise.
    let stored = store_beta
        .message(conversation, alice_to_bob.message_id)
        .await
        .expect("the far store reads")
        .expect("the replica row is in the far store");
    assert_eq!(stored.expires_at, None);
    assert_eq!(
        stored.deleted_by, None,
        "the wire carries no deleter, so the replica tombstone records none"
    );
    assert_eq!(stored.envelope, Vec::<u8>::new());
}
