//! Direct-message federation, driven at the client-visible seam: a direct
//! conversation whose two participants are connected to *different* nodes, each
//! message crossing the mesh between them as a sealed envelope nobody on the
//! way opens.
//!
//! This is the scenario section 170 spent its whole honest tail admitting was
//! missing: two full nodes, a store per node (the tools/2node shape, not the
//! shared-store one `cross_node_resume.rs` builds), and a participant on the
//! far node whom a publish that stopped at the sender's hub would simply never
//! reach — there is no row in the far store to sync from. The conversation tier
//! is what closes that gap: the conversation row names a home node
//! (`home_region`, stamped at creation), the far node asks the home node to
//! watch the conversation once per process (`FED_CONVERSATION_SUBSCRIBE`), and
//! every publish is handed to the node that owns the fan-out — one federated
//! copy per watching node (`FED_CONVERSATION_EVENT`), the inner event frame
//! sealed exactly as a local session would have received it.
//!
//! The topology is deliberately the *harder* of the two section 170 shapes.
//! Two `App`s over one shared store would federate nothing worth testing: the
//! far node would already hold every row, and a message would reach the far
//! participant through the local hub of whichever node received the send. A
//! store per node means the far participant is unreachable without the mesh —
//! the account, device, and session rows authentication reads are copied
//! verbatim the way the replication stand-in (nnode) copies them, which is the
//! honest stand-in for the account-to-node map section 170 still lists as a
//! gap, and the conversation row crosses the same way so membership
//! authorisation answers on both nodes — and so does the friendship, because
//! the far node's privacy gate is fail-closed and can only answer for a
//! recipient whose profile and friendship edge it holds.
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
//! Determinism is the client-seam house style: every exchange is bounded by a
//! step budget, and the one place timing genuinely races — alice's first send
//! may leave node alpha before node beta's watch registration lands — is
//! handled the way a real client handles it, by retrying with *fresh* message
//! ids (section 156: a duplicate id produces no fanout, so a retry that reuses
//! one would be a silent no-op), bounded by attempts, never by sleeping.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::config::{MeshPeer, StoreConfig};
use migo_core::{Clock, Config, Id, OsRandom, Secret, SystemClock};
use migo_crypto::NodeSecret;
use migo_protocol::{
    from_frame, to_frame, ConversationCreateRequest, ConversationKind, ConversationSummary, Decode,
    Encode, Frame, Hello, MessageAccepted, MessageEvent, MessageKind, MessageSend, Opcode,
    Platform, RelationshipKind, SubscribeRequest, SubscribeResponse, Topic, TopicKind, Welcome,
    PROTOCOL_VERSION,
};
use migo_ratelimit::TrustTier;
use migo_social::Caller;
use migo_store::model::{NewAccount, NewDevice, NewSession};
use migo_store::SharedStore;
use migod::App;

/// How long any single exchange may take before the test declares a node stuck.
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

/// Makes the two accounts friends, because a direct conversation's privacy
/// accepts friends only.
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

/// Copies one account's rows verbatim to the far node's store: account, device,
/// and the session the access token names.
///
/// This is the replication stand-in's job in a real fleet — the
/// account-to-node map section 170 lists as a gap is what would route this —
/// and it is done by hand here so the test says exactly what the far node
/// needs: nothing more than the rows authentication reads. The token itself
/// verifies anywhere because both nodes share the token key, the way a
/// deployment must.
async fn copy_identity_to(from: &SharedStore, to: &SharedStore, grant: &Grant) {
    copy_account_to(from, to, grant.account_id).await;

    let device = from
        .device_by_id(grant.device_id)
        .await
        .expect("the source store reads")
        .expect("the device exists");
    to.register_device(NewDevice {
        device_id: device.device_id,
        account_id: device.account_id,
        platform: device.platform,
        display_name: device.display_name,
        app_version: device.app_version,
        os_version: device.os_version,
        device_model: device.device_model,
        status: device.status,
        public_credential: device.public_credential,
        created_at: device.created_at,
    })
    .await
    .expect("the far store takes the device row verbatim");

    let session = from
        .session_by_id(grant.session_id)
        .await
        .expect("the source store reads")
        .expect("the session exists");
    to.create_session(NewSession {
        session_id: session.session_id,
        account_id: session.account_id,
        device_id: session.device_id,
        family_id: session.family_id,
        refresh_hash: session.refresh_hash,
        generation: session.generation,
        created_at: session.created_at,
        authenticated_at: session.authenticated_at,
        access_expires_at: session.access_expires_at,
        refresh_expires_at: session.refresh_expires_at,
        ip_class: session.ip_class,
        user_agent: session.user_agent,
    })
    .await
    .expect("the far store takes the session row verbatim");
}

/// Copies a conversation row and its membership to the far node's store,
/// verbatim — including the `home_region` label, which is the one fact every
/// node holding a copy of the row must read the same answer from: both stores
/// say the conversation is homed on alpha, so both nodes agree on who owns the
/// fan-out.
async fn copy_conversation_to(from: &SharedStore, to: &SharedStore, conversation_id: Id) {
    let conversation = from
        .conversation(conversation_id)
        .await
        .expect("the source store reads")
        .expect("the conversation exists on the node that created it");
    let members = from
        .members(conversation_id)
        .await
        .expect("the member rows read");
    assert_eq!(members.len(), 2, "a direct conversation seats exactly two");
    to.create_conversation(
        conversation,
        members.iter().map(|member| member.account_id).collect(),
    )
    .await
    .expect("the far store takes the conversation verbatim, label included");
}

/// Copies one account row verbatim — the root every other row the stand-in
/// carries hangs from. The profile's foreign key resolves to it, and the far
/// store refuses a profile whose account it does not hold, so this crosses
/// first wherever a profile follows.
async fn copy_account_to(from: &SharedStore, to: &SharedStore, account_id: Id) {
    let account = from
        .account_by_id(account_id)
        .await
        .expect("the source store reads")
        .expect("the account exists on the node that registered it");
    to.create_account(NewAccount {
        account_id: account.account_id,
        username: account.username,
        email: account.email,
        phone: account.phone,
        passphrase_hash: account.passphrase_hash,
        locale: account.locale,
        country: account.country,
        created_at: account.created_at,
    })
    .await
    .expect("the far store takes the account row verbatim");
}

/// Copies the social rows the far node's privacy gate reads for a direct
/// send: both profiles, and the friendship's two edges, verbatim.
///
/// A direct send enforces the recipient's `who_can_message` on every message,
/// at whichever node takes the send — so bob's reply is gated on node beta,
/// which was not there when the friendship was made on alpha. The gate is
/// deliberately fail-closed: a node that cannot see the recipient's profile
/// cannot answer the privacy question, and a node that cannot answer refuses
/// — the same posture that keeps a stranger's `Nobody` setting meaningful. So
/// the replication stand-in carries the answer with the passenger: alice's
/// profile row (the setting itself) and the caller-side friendship edge (the
/// `Friends` verdict), copied verbatim, nothing derived. The pair arrives
/// whole rather than as only the rows this one send reads, because that is
/// the shape the store keeps — a friendship is two edges or it is the "we are
/// friends but you are not in my list" bug the store's own acceptance path
/// refuses to write.
///
/// The account row crosses ahead of the profile, in the store's dependency
/// order: the profile's foreign key must resolve on the far side before the
/// profile can land. Bob's account is already there — his identity crossed
/// when he did — so only alice's is written, and the check keeps the helper
/// idempotent rather than assuming the caller remembers who crossed first.
async fn copy_friendship_to(from: &SharedStore, to: &SharedStore, left: Id, right: Id) {
    for account_id in [left, right] {
        if to
            .account_by_id(account_id)
            .await
            .expect("the far store reads")
            .is_none()
        {
            copy_account_to(from, to, account_id).await;
        }
        let profile = from
            .profile(account_id)
            .await
            .expect("the source store reads")
            .expect("a registered account has a profile");
        to.create_profile(profile)
            .await
            .expect("the far store takes the profile row verbatim");
    }
    for (owner, peer) in [(left, right), (right, left)] {
        let edge = from
            .relationship(owner, peer, RelationshipKind::Friend)
            .await
            .expect("the source store reads")
            .expect("the friendship exists in both directions");
        to.put_relationship(edge)
            .await
            .expect("the far store takes the friendship edge verbatim");
    }
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
            "the inline token authenticated the session — the far node read the copied rows"
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
}

/// The full scenario: a direct conversation created on one node, its other
/// participant seated on another node with another store, and the sealed
/// messages crossing both directions over the mesh — the home node fanning
/// out, the far node handing its reply to the home node, and neither ever
/// holding anything but an opaque envelope.
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

    // Two accounts, made friends on the node both were registered through,
    // because the conversation's privacy accepts friends only.
    let alice = registered_grant(&alpha, "dmfedalice").await;
    let bob = registered_grant(&alpha, "dmfedbob").await;
    befriend(&alpha, &alice, &bob).await;

    // Bob's identity crosses to node beta the way the replication stand-in
    // would carry it: the rows authentication reads, verbatim, nothing more.
    copy_identity_to(&store_alpha, &store_beta, &bob).await;

    // Alice, seated on the home node, opens the conversation through her own
    // wire session. The row node alpha writes carries the home label "alpha",
    // stamped at creation and never derived again.
    let mut alice_client = Client::connect_fresh(a_addr, &alice).await;
    assert_eq!(alice_client.node, "node-alpha");
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

    // The conversation crosses to node beta the same way, label included, so
    // both stores agree on who owns the fan-out and beta's membership check
    // can answer for bob.
    copy_conversation_to(&store_alpha, &store_beta, conversation).await;
    let row = store_alpha
        .conversation(conversation)
        .await
        .expect("the home store reads")
        .expect("the conversation row exists");
    assert_eq!(
        row.home_region, "alpha",
        "the creating node stamped itself as the conversation's home"
    );

    // The friendship's rows cross too, the same stand-in's cargo: bob's reply
    // is a send like any other, and it is node beta that must enforce alice's
    // `who_can_message` for it — fail-closed until her profile and the edge
    // are there to answer.
    copy_friendship_to(&store_alpha, &store_beta, alice.account_id, bob.account_id).await;

    // Both participants subscribe their side. Alice's subscribe needs no ask —
    // her node is the home node. Bob's subscribe is the subscribe half of the
    // tier: node beta asks node alpha to watch the conversation, once.
    alice_client
        .subscribe(vec![Topic {
            kind: TopicKind::Conversation,
            id: conversation,
        }])
        .await;
    let mut bob_client = Client::connect_fresh(b_addr, &bob).await;
    assert_eq!(
        bob_client.node, "node-beta",
        "bob is genuinely seated on the other node"
    );
    assert_eq!(bob_client.region, "beta");
    bob_client
        .subscribe(vec![Topic {
            kind: TopicKind::Conversation,
            id: conversation,
        }])
        .await;

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

    // Bob answers from the far node. His send lands in beta's store (its own
    // seq — a private message needs no global order, section 170), and the
    // forward half hands it to the conversation's home node, because beta
    // holds no watch table and cannot tier it itself.
    let (_, bob_accepted) = bob_client.send_message(conversation, BOB_SEALED).await;
    assert_eq!(
        bob_accepted.seq, 1,
        "beta's own store numbers its own sends"
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
    // what keeps the tier's fan-out authority in one place.
    for store in [&store_alpha, &store_beta] {
        let row = store
            .conversation(conversation)
            .await
            .expect("the store reads")
            .expect("the conversation row exists");
        assert_eq!(row.home_region, "alpha");
    }
}
