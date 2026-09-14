//! Cross-node calls: the ring and its state events, driven at the client seam.
//! The gap this suite exists to close is that every call event was published
//! only to the local user topics — a callee whose socket sat on another node
//! never heard the ring at all, because `FED_CALL_RELAY` (opcode 220) had an
//! ingest arm that logged the envelope and dropped it, and no publisher
//! anywhere. The invite reached the far node's store-side preconditions (the
//! accounts and the conversation live in the shared rows) but not the far
//! node's hub, so the phone on the other node simply did not ring.
//!
//! The topology is the same honest shape the other cross-node suites use: two
//! full nodes over ONE store through `App::build_with_store`, linked by
//! `apply_mesh_peers` with fixed 32-byte signing seeds. The call row itself
//! lives in the inviting node's in-process call store — that is the flagged
//! boundary of this tier, and the test stays deliberately inside it: alice on
//! node alpha places the ring and cancels it, both of which the row-holding
//! node can do, and bob on node beta is the far-side phone that has to hear
//! both frames for the call to be real.
//!
//! The flow under test, in the order it lives in production: bob's
//! self-subscription on beta asks the mesh to watch his user topic; alice's
//! `CALL_INVITE` publishes the invite event to bob's topic on alpha and
//! forwards one copy per watching node inside the `FED_CALL_RELAY` envelope;
//! beta ingests it and places the frame on bob's topic exactly as alpha's own
//! hub would have. Then alice cancels, and the `Ended(ByCaller)` state event
//! crosses the same way — the frame the old code never sent, because no node
//! but the publisher's own ever saw a call event at all.
//!
//! Determinism follows the client-seam house style: every exchange is bounded
//! by a step budget, every expectation is asserted on frame contents, and the
//! one inherently asynchronous hop — beta's watch ask landing in alpha's table
//! — is observed by polling the table itself (`App::presence_relay`), never by
//! sleeping and hoping.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_calls::{CallState, EndReason};
use migo_core::config::{MeshPeer, StoreConfig};
use migo_core::{Clock, Config, Id, Secret, Timestamp};
use migo_crypto::NodeSecret;
use migo_protocol::{
    from_frame, to_frame, CallCancel, CallInvite, CallInviteEvent, CallInviteResult,
    CallStateEvent, Decode, Encode, Frame, Hello, Opcode, RoomJoinRequest, RoomKind,
    SubscribeRequest, Topic, TopicKind, Welcome, PROTOCOL_VERSION,
};
use migo_ratelimit::TrustTier;
use migo_rooms::{Caller as RoomCaller, NewRoomRequest};
use migo_social::Caller as SocialCaller;
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
const SEED_ALPHA: &str = "cross-calls-alpha-00000000000000";

/// Node beta's mesh signing seed.
const SEED_BETA: &str = "cross-calls-beta-000000000000000";

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
                device: DeviceClaim::new(migo_protocol::Platform::Web, "cross-node calls test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// One call's preconditions, set in-process against the shared store exactly the way
/// the single-node wire suite sets them: a friendship (the default call policy is
/// friends-only) and a room both accounts are members of, whose conversation the
/// invite will name. The rows land in the one store both nodes read, so the far
/// node's gate answers the same membership question the near one does.
async fn a_room_between(app: &App, caller: &Grant, callee: &Grant, slug: &str) -> Id {
    let now = Timestamp::from_millis(1);
    let as_caller = SocialCaller::new(
        caller.account_id,
        caller.device_id,
        TrustTier::Established,
        now,
    );
    let as_callee = SocialCaller::new(
        callee.account_id,
        callee.device_id,
        TrustTier::Established,
        now,
    );
    app.social
        .request_friend(&as_caller, callee.account_id)
        .await
        .expect("the friend request is sent");
    app.social
        .respond_friend(&as_callee, caller.account_id, true)
        .await
        .expect("the friend request is accepted");

    let as_owner = RoomCaller::new(
        caller.account_id,
        caller.device_id,
        TrustTier::Established,
        now,
    );
    let as_member = RoomCaller::new(
        callee.account_id,
        callee.device_id,
        TrustTier::Established,
        now,
    );
    let room = app
        .rooms
        .create(
            &as_owner,
            NewRoomRequest {
                slug: slug.to_string(),
                name: "The Ring Room".to_string(),
                topic: None,
                kind: RoomKind::Public,
                max_members: None,
            },
        )
        .await
        .expect("the caller founds the room");
    let (joined, _) = app
        .rooms
        .join(
            &as_member,
            RoomJoinRequest {
                room_id: room.room_id,
                invite_code: None,
            },
        )
        .await
        .expect("the callee joins the room");
    joined.conversation_id
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

        // The CALLS bit: every request this suite drives is the call family, which
        // brief section 72 gates on the negotiated set — the session asks for the
        // bit the way a client that wants to place calls does.
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            features: migo_protocol::features::CALLS,
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

    /// Reads until this session's stream carries a frame of the wanted opcode,
    /// skipping the unrelated events a subscribed session receives — the bell's
    /// notification rides the same user topic the ring does, and presence may speak
    /// too. The timeout is the assertion: a ring the far node never hears is the bug.
    async fn next_event_of(&mut self, want: Opcode) -> Frame {
        loop {
            let frame = recv_within(&mut self.stream, STEP).await;
            if Opcode::from_wire(frame.header.opcode) == Some(want) {
                return frame;
            }
        }
    }
}

/// The full scenario, in the order a real cross-node ring lives it: the caller's
/// node holds the call row and publishes the invite event, the callee's node holds
/// the callee's socket and owes them the frame, and the `FED_CALL_RELAY` tier is
/// what carries it across — then carries the cancellation back the same way.
#[tokio::test]
async fn a_ring_places_and_cancels_across_nodes() {
    // The fleet: two nodes over one store, each with its own ephemeral mesh listener.
    // The call will be placed on alpha, which holds the call row its own store built.
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

    // The parties: alice stays on alpha and places the call; bob sits on beta and is
    // the far-side phone the ring has to reach. Both accounts and their friendship
    // live in the one store both nodes read.
    let alice_grant = registered_grant(&app_a, "callalice").await;
    let bob_grant = registered_grant(&app_a, "callbob").await;
    let conversation_id = a_room_between(&app_a, &alice_grant, &bob_grant, "ring-room").await;

    let mut alice = Client::connect_fresh(a_addr, &alice_grant).await;
    let mut bob = Client::connect_fresh(b_addr, &bob_grant).await;

    // The deterministic barrier: bob's self-subscription on beta asked the mesh to
    // watch his user topic, and the ask is durable and async, so the test waits until
    // it has landed in alpha's table — polling the table, not sleeping — before the
    // invite publishes anything that depends on it.
    let mut registered = false;
    let deadline = tokio::time::Instant::now() + MESH_BUDGET;
    while tokio::time::Instant::now() < deadline {
        if app_a
            .presence_relay
            .watchers_of(bob_grant.account_id)
            .contains(&id_b)
        {
            registered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    assert!(
        registered,
        "node alpha must register node beta as a watcher of bob's user topic within the \
         mesh budget: {:?}",
        app_a.presence_relay.watchers_of(bob_grant.account_id)
    );

    // The ring: alice's invite is accepted by the row-holding node, and the invite
    // event it publishes to bob's user topic must cross to the node holding bob's
    // socket — the frame the old code dropped, because no publisher fed the
    // FED_CALL_RELAY tier and its ingest arm only logged.
    let call_id = Id::from_bytes([0xC7; 16]);
    let result: CallInviteResult = alice
        .ask(
            Opcode::CallInvite,
            &CallInvite {
                call_id,
                conversation_id,
                callee_id: bob_grant.account_id,
                media_kind: 0,
                caller_device: alice_grant.device_id,
                capabilities: 0,
                sealed_offer: vec![0x11; 48],
            },
        )
        .await;
    assert_eq!(result.status, 0, "the invite is accepted as a ring");
    assert_eq!(result.call_id, call_id);

    let invite_frame = bob.next_event_of(Opcode::CallInviteEvent).await;
    let invite: CallInviteEvent = from_frame(&invite_frame).expect("the invite event decodes");
    assert_eq!(
        invite.call_id, call_id,
        "the far node rings the call placed"
    );
    assert_eq!(invite.caller_id, alice_grant.account_id);
    assert_eq!(
        invite.sealed_offer,
        vec![0x11; 48],
        "the sealed offer crosses unopened, exactly as it would on one node"
    );

    // The cancellation: the row-holding node retires its own ring and publishes the
    // `Ended(ByCaller)` state event, which crosses the same tier so the far phone
    // stops ringing without anybody on beta having to decline a call they cannot
    // even name — the store there holds no row for it.
    let _: migo_protocol::Acknowledged =
        alice.ask(Opcode::CallCancel, &CallCancel { call_id }).await;

    let ended_frame = bob.next_event_of(Opcode::CallStateEvent).await;
    let ended: CallStateEvent = from_frame(&ended_frame).expect("the end event decodes");
    assert_eq!(ended.call_id, call_id, "the cancellation names the ring");
    assert_eq!(ended.state, CallState::Ended.to_wire());
    assert_eq!(
        ended.reason,
        Some(EndReason::ByCaller.to_wire()),
        "a ring the caller withdrew ends as by-caller on the far node too"
    );
}
