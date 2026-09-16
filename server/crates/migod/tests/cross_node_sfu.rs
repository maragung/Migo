//! Cross-node SFU announcements: the group call's membership events, driven
//! at the client seam. The gap this suite exists to close is that the join
//! and departure frames a group call publishes to the *conversation's* topic
//! never crossed a node boundary — the conversation tier (section 170) was
//! widened to carry only messaging fanouts, and a member whose socket sat on
//! another node never learned a call was running at all, while the roster to
//! the joiner's own user topic did cross (`FED_CALL_RELAY`, the suite in
//! `cross_node_calls.rs`). The tier now carries the announcement frames too,
//! as sealed blobs no intermediate node opens, placed on the far node's
//! conversation topic under the same no-coalescing rule the origin's own
//! publish keeps.
//!
//! The topology is the honest shape the other cross-node suites use: two full
//! nodes over ONE store through `App::build_with_store`, linked by
//! `apply_mesh_peers` with fixed 32-byte signing seeds. The conversation is a
//! group chat — a conversation no room owns, homed on node alpha because
//! alice created it there — and the joiner sits on node beta, so the join
//! announcement rides the tier's full two hops: beta publishes locally,
//! hands one copy to the home node alpha, and alpha places it on the
//! conversation topic its own subscribers sit on.
//!
//! Exactly once is the assertion the suite is named for, and it is structural
//! rather than lucky: the home node's onward fan-out excludes the origin, so
//! beta never receives back what it sent, and the roster crosses on the
//! user-topic tier to a different topic entirely — the joiner's own session
//! must hear the roster and *no* announcement, while the far subscriber must
//! hear the announcement exactly once. A quiet window after each expected
//! frame is what pins it: a second copy would be indistinguishable from the
//! first, so the absence of one is asserted, not assumed.
//!
//! Determinism follows the client-seam house style: every exchange is bounded
//! by a step budget, every expectation is asserted on frame contents, and the
//! one inherently asynchronous hop — beta's watch ask landing in alpha's
//! table — is observed by polling the table itself (`App::conversation_relay`),
//! never by sleeping and hoping.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::config::{MeshPeer, StoreConfig};
use migo_core::{Clock, Config, Id, Secret};
use migo_crypto::NodeSecret;
use migo_protocol::{
    from_frame, to_frame, CallEnd, CallInvite, CallStateEvent, CallTurnResponse,
    ConversationCreateRequest, ConversationKind, ConversationSummary, Decode, Encode, Frame, Hello,
    Opcode, Platform, SubscribeRequest, SubscribeResponse, Topic, TopicKind, Welcome,
    PROTOCOL_VERSION,
};
use migod::App;

/// How long any single exchange may take before the test declares a node stuck —
/// silence being the bug class this suite exists to catch.
const STEP: Duration = Duration::from_secs(5);

/// How long the mesh may take to carry a frame between two healthy in-process
/// nodes, and the quiet window that proves no second copy follows the first:
/// the outbox runner drains every 500ms, so a duplicate would ride the very
/// next drain after the frame it duplicates. The loop exits the moment the
/// expected state is observed, so this budget is only ever spent waiting for
/// nothing to arrive.
const MESH_BUDGET: Duration = Duration::from_secs(10);

/// The quiet window: three outbox drain cycles with no frame of the watched
/// opcode is the absence this suite asserts on, because a duplicate would be
/// enqueued in the same cycle as the copy it duplicates.
const QUIET: Duration = Duration::from_millis(1500);

/// Node alpha's mesh signing seed, exactly 32 bytes the way `NodeSecret` demands. The
/// mesh node id is the first 16 bytes, so the two seeds differ there.
const SEED_ALPHA: &str = "cross-sfu-alpha-0000000000000000";

/// Node beta's mesh signing seed.
const SEED_BETA: &str = "cross-sfu-beta-00000000000000000";

/// The joiner's sealed media description, the blob the whole tier must carry
/// unread. Distinctive bytes, so a frame that arrived altered would fail the
/// equality the way a frame that never arrived fails the wait.
const SEALED_OFFER: &[u8] = b"sealed-cross-node-group-offer";

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

/// The same node with a seat grace short enough to observe inside one test
/// budget: the retirement scenario below waits out the grace window plus one
/// sweep tick, and the production default's thirty seconds belongs to an
/// operator's patience, not a test's. Only the roster-holding node needs it —
/// the sweeper that retires the dead seat is the one whose group store holds
/// it.
async fn build_node_with_short_seat_grace(
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
            ("MIGO_CALLS__SEAT_GRACE_MS".to_string(), "1500".to_string()),
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
                device: DeviceClaim::new(Platform::Web, "cross-node sfu test"),
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
    try_recv_within(stream, limit)
        .await
        .expect("the frame does not stall — silence here is the bug these tests exist to catch")
}

/// Reads one length-prefixed frame the way [`recv_within`] does, but reports the
/// budget running out as `None` instead of panicking, so a quiet window can be the
/// one to name a duplicate — with everything it saw on the way, which a panic
/// inside the read would have taken with it.
async fn try_recv_within(stream: &mut tokio::net::TcpStream, limit: Duration) -> Option<Frame> {
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
    .ok()?;
    Some(Frame::decode(body.into()).expect("the frame decodes"))
}

/// A scripted client: a real TCP session speaking the length-prefixed framing,
/// tracking its correlation counter the way a real one does.
struct Client {
    stream: tokio::net::TcpStream,
    correlation: u32,
    /// Frames read while waiting for another arrival, kept for the wait that wants
    /// them. The nodes send their events in their own order, so a wait that skips
    /// past a frame must not eat it.
    held: Vec<Frame>,
}

impl Client {
    /// Connects and opens a fresh, authenticated session: HELLO with the grant's
    /// inline token, then the self-subscription every client makes at its handshake.
    async fn connect_fresh(addr: SocketAddr, grant: &Grant) -> Self {
        let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting does not stall")
            .expect("the connection is accepted");

        // The CALLS bit: the leave this suite drives is the call family, which
        // brief section 72 gates on the negotiated set — the SFU opcodes
        // themselves stay ungated (the decision section 165 records), but the
        // `CALL_END` that carries the departure is not SFU.
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

        let wanted = Topic {
            kind: TopicKind::User,
            id: grant.account_id,
        };
        send(
            &mut stream,
            Opcode::Subscribe,
            2,
            &SubscribeRequest {
                topics: vec![wanted.clone()],
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
                let reply: SubscribeResponse = from_frame(&frame).expect("the reply decodes");
                assert!(
                    reply.accepted.contains(&wanted),
                    "the self-subscription takes the user topic (rejected: {:?})",
                    reply.rejected
                );
                return Self {
                    stream,
                    correlation: 2,
                    held: Vec::new(),
                };
            }
        }
    }

    /// Subscribes to a conversation's topic, as a member's client does once it
    /// has loaded the conversation — the door that asks the far node to watch.
    async fn subscribe_conversation(&mut self, conversation: Id) {
        let wanted = Topic {
            kind: TopicKind::Conversation,
            id: conversation,
        };
        let reply: SubscribeResponse = self
            .ask(
                Opcode::Subscribe,
                &SubscribeRequest {
                    topics: vec![wanted.clone()],
                },
            )
            .await;
        assert!(
            reply.accepted.contains(&wanted),
            "the conversation subscription is accepted (rejected: {:?})",
            reply.rejected
        );
    }

    /// Sends a request that must succeed, returning the decoded reply. Frames
    /// that are not the reply — the events a subscribed session receives —
    /// are read and kept for a later wait, never discarded: the duplicate
    /// assertions read what every earlier wait passed over.
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
            self.held.push(frame);
        }
    }

    /// Reads until this session's stream carries a frame of the wanted opcode within
    /// `limit`, keeping the frames it passes over so a later wait can take them —
    /// the roster and the announcements arrive in each node's own order, so
    /// order-tolerance is a requirement, not a courtesy.
    ///
    /// A timeout inside the read returns `None` rather than panicking, so the
    /// deadline assert here is what fires when nothing arrives — and its message
    /// carries the whole of the evidence: what this wait saw on the socket, what
    /// earlier waits held, and the caller's `evidence` of the far node's own
    /// delivery counters. A far node that ingested the event but delivered
    /// nothing leaves no other trace.
    async fn next_event_of(
        &mut self,
        want: Opcode,
        limit: Duration,
        evidence: &dyn Fn() -> String,
    ) -> Frame {
        if let Some(position) = self
            .held
            .iter()
            .position(|frame| Opcode::from_wire(frame.header.opcode) == Some(want))
        {
            return self.held.remove(position);
        }
        let deadline = tokio::time::Instant::now() + limit;
        let mut seen: Vec<Opcode> = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "the {:?} frame never arrived within {:?}; this wait saw {:?} on the \
                 socket, earlier waits held {:?}, and the far node reports: {}",
                want,
                limit,
                seen,
                self.held
                    .iter()
                    .filter_map(|frame| Opcode::from_wire(frame.header.opcode))
                    .collect::<Vec<_>>(),
                evidence()
            );
            let Some(frame) = try_recv_within(&mut self.stream, remaining).await else {
                continue;
            };
            if let Some(opcode) = Opcode::from_wire(frame.header.opcode) {
                if opcode == want {
                    return frame;
                }
                seen.push(opcode);
            }
            self.held.push(frame);
        }
    }

    /// Asserts that no further frame of the wanted opcode arrives within the quiet
    /// window, holding everything else it reads for a later wait. This is the
    /// exactly-once half of the suite: a second copy of an announcement would be
    /// indistinguishable from the first, so its absence is asserted on the socket,
    /// with the evidence a duplicate would have left in `held` and `seen`.
    async fn assert_quiet_for(&mut self, watch: Opcode, evidence: &dyn Fn() -> String) {
        // Held frames count as arrivals too: a duplicate an earlier wait read
        // past is as much a duplicate as one this window reads off the socket.
        assert!(
            !self
                .held
                .iter()
                .any(|frame| Opcode::from_wire(frame.header.opcode) == Some(watch)),
            "a second {:?} frame was already held from an earlier wait — the \
             duplicate the tier must not deliver; earlier waits held {:?}, and \
             the far node reports: {}",
            watch,
            self.held
                .iter()
                .filter_map(|frame| Opcode::from_wire(frame.header.opcode))
                .collect::<Vec<_>>(),
            evidence()
        );
        let deadline = tokio::time::Instant::now() + QUIET;
        let mut seen: Vec<Opcode> = Vec::new();
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if let Some(frame) = try_recv_within(&mut self.stream, remaining).await {
                match Opcode::from_wire(frame.header.opcode) {
                    Some(opcode) if opcode == watch => panic!(
                        "a second {:?} frame arrived within {:?} of the last one — the \
                         duplicate the tier must not deliver; this window also saw {:?}, \
                         earlier waits held {:?}, and the far node reports: {}",
                        watch,
                        QUIET,
                        seen,
                        self.held
                            .iter()
                            .filter_map(|frame| Opcode::from_wire(frame.header.opcode))
                            .collect::<Vec<_>>(),
                        evidence()
                    ),
                    Some(opcode) => seen.push(opcode),
                    None => {}
                }
                self.held.push(frame);
            }
        }
    }
}

/// The full scenario, in the order a real cross-node group call lives it: the
/// conversation's home node holds the fan-out table, the joiner sits on the
/// other node, and the announcements cross so the far subscriber learns the
/// call is running and then learns it emptied — each exactly once, with the
/// joiner's own session proving no copy bounces back.
#[tokio::test]
async fn a_group_calls_announcements_reach_a_far_subscriber_exactly_once() {
    // Diagnostics: the federated half's failures are warn-level logs on nodes whose
    // sockets stay silent, so the suite captures what both nodes logged while it ran.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "migod=debug,migo_federation=info,migo_gateway=warn",
        ))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();

    // The fleet: two nodes over one store, each with its own ephemeral mesh
    // listener. The conversation will be homed on alpha, the joiner seated on
    // beta, so the announcements ride the tier's full two hops.
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

    // The parties: alice stays on alpha, subscribes to the conversation's
    // topic, and is the far subscriber the announcements must reach; bob
    // sits on beta and is the joiner whose seat the announcements name. Both
    // accounts live in the one store both nodes read.
    let alice_grant = registered_grant(&app_a, "sfualice").await;
    let bob_grant = registered_grant(&app_a, "sfubob").await;

    let mut alice = Client::connect_fresh(a_addr, &alice_grant).await;
    let mut bob = Client::connect_fresh(b_addr, &bob_grant).await;

    // The conversation: a group chat alice opens with bob, through her own
    // wire session, so the creating node — alpha — stamps itself as the home
    // the tier's fan-out question reads.
    let summary: ConversationSummary = alice
        .ask(
            Opcode::ConversationCreate,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![bob_grant.account_id],
                title: Some("The Cross-Node Call".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;
    let row = store
        .conversation(conversation_id)
        .await
        .expect("the shared store reads")
        .expect("the conversation row exists");
    assert_eq!(
        row.home_region, "alpha",
        "the creating node stamped itself as the conversation's home"
    );
    assert!(
        row.room_id.is_none(),
        "a group chat is no room's conversation, so the conversation tier owns its fan-out"
    );

    // Both sides subscribe. Alice's is a home-node subscription — no ask. Bob's
    // is the door the tier hangs on: beta asks alpha to watch the
    // conversation, and the ask is durable and async, so the test waits until
    // it has landed in alpha's table — polling the table, not sleeping —
    // before the join publishes anything that depends on it.
    alice.subscribe_conversation(conversation_id).await;
    bob.subscribe_conversation(conversation_id).await;
    let mut registered = false;
    let deadline = tokio::time::Instant::now() + MESH_BUDGET;
    while tokio::time::Instant::now() < deadline {
        if app_a
            .conversation_relay
            .watchers_of(conversation_id)
            .contains(&id_b)
        {
            registered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    assert!(
        registered,
        "node alpha must register node beta as a watcher of the conversation within the \
         mesh budget: {:?}",
        app_a.conversation_relay.watchers_of(conversation_id)
    );

    // The failure evidence: the frames this suite waits for are ones whose
    // loss leaves no log anywhere, so every wait's failure message carries
    // both nodes' own delivery counters.
    let evidence = || {
        format!(
            "alpha's gateway wrote {} frames and dropped {:?}; beta's gateway wrote {} \
             frames and dropped {:?}",
            app_a.gateway.frames_out_total(),
            app_a.gateway.dropped_frames_total(),
            app_b.gateway.frames_out_total(),
            app_b.gateway.dropped_frames_total(),
        )
    };

    // The join: bob's seat lands on beta's group store, the roster returns to
    // his own user topic (the local half every join already had), and the
    // join announcement must cross to alpha's conversation topic — the frame
    // the old code never sent, because the conversation tier carried only
    // messaging fanouts.
    let call_id = Id::from_bytes([0x5F; 16]);
    let _: CallTurnResponse = bob
        .ask(
            Opcode::CallSfuJoin,
            &CallInvite {
                call_id,
                conversation_id,
                callee_id: Id::from(0u128),
                media_kind: 0,
                caller_device: Id::from(0u128),
                capabilities: 0,
                sealed_offer: SEALED_OFFER.to_vec(),
            },
        )
        .await;

    // The joiner's own roster first: the one frame this crossing must not
    // disturb, published to bob's user topic by the node his socket sits on.
    let roster_frame = bob
        .next_event_of(Opcode::CallSfuEvent, STEP, &evidence)
        .await;
    let roster: CallStateEvent = from_frame(&roster_frame).expect("the roster event decodes");
    assert_eq!(roster.call_id, call_id, "the roster names the call");
    assert_eq!(roster.user_id, Some(bob_grant.account_id));
    let seats = roster.participants.expect("the roster is attached");
    assert_eq!(seats.len(), 1, "one seat, the joiner's own");
    assert_eq!(seats[0].sealed_offer, SEALED_OFFER, "the offer is bob's");

    // The far subscriber hears the join: the announcement crossed beta's
    // local publish, the copy to the home node, and alpha's placement onto
    // its own conversation topic — sealed offer intact, exactly as a local
    // subscriber on beta would have received it.
    let join_frame = alice
        .next_event_of(Opcode::CallSfuEvent, MESH_BUDGET, &evidence)
        .await;
    let join: CallStateEvent = from_frame(&join_frame).expect("the join announcement decodes");
    assert_eq!(join.call_id, call_id, "the announcement names the call");
    assert_eq!(
        join.conversation_id,
        Some(conversation_id),
        "the announcement names the conversation it belongs to"
    );
    assert_eq!(
        join.state,
        migo_calls::group_store::GROUP_STATE_CONNECTED,
        "the join announcement is the Connected state"
    );
    assert_eq!(
        join.user_id,
        Some(bob_grant.account_id),
        "the announcement names the joiner"
    );
    assert_eq!(join.participant_count, Some(1), "the roster holds one seat");
    assert_eq!(
        join.sealed_offer,
        Some(SEALED_OFFER.to_vec()),
        "the sealed offer crossed every node on the way unopened"
    );
    assert!(
        join.participants.is_none(),
        "an announcement is not the roster — the two frames stay distinct on the wire"
    );

    // Exactly once, both ends. The home node's onward fan-out excludes the
    // origin, so bob must hear nothing further on any topic his session
    // holds, and alpha must not deliver alice a second copy through a second
    // envelope — the room tier was asked the same question and answered that
    // the conversation is not a room's.
    alice
        .assert_quiet_for(Opcode::CallSfuEvent, &evidence)
        .await;
    bob.assert_quiet_for(Opcode::CallSfuEvent, &evidence).await;

    // The departure: bob leaves, and the departure announcement crosses the
    // same two hops so the far subscriber's call note retires — the frame
    // that says the call emptied, participant count zero.
    let _: migo_protocol::Acknowledged = bob
        .ask(
            Opcode::CallEnd,
            &CallEnd {
                call_id,
                reason: migo_calls::EndReason::ByCaller.to_wire(),
            },
        )
        .await;

    let left_frame = alice
        .next_event_of(Opcode::CallSfuEvent, MESH_BUDGET, &evidence)
        .await;
    let left: CallStateEvent = from_frame(&left_frame).expect("the departure decodes");
    assert_eq!(left.call_id, call_id, "the departure names the call");
    assert_eq!(
        left.state,
        migo_calls::group_store::GROUP_STATE_ENDED,
        "the departure announcement is the Ended state"
    );
    assert_eq!(
        left.user_id,
        Some(bob_grant.account_id),
        "the departure names the leaver"
    );
    assert_eq!(
        left.participant_count,
        Some(0),
        "the roster is empty after the departure"
    );
    assert_eq!(
        left.reason,
        Some(migo_calls::EndReason::ByCaller.to_wire()),
        "the departure carries the leaver's own reason"
    );

    // And exactly once again, both ends: the departure is the last frame the
    // call owes anybody, so both sockets fall quiet on the announcement
    // opcode.
    alice
        .assert_quiet_for(Opcode::CallSfuEvent, &evidence)
        .await;
    bob.assert_quiet_for(Opcode::CallSfuEvent, &evidence).await;
}

/// A dead seat's retirement crosses the same two hops the join crossed — the
/// frame the *sweeper* owes a far roster. The departure above was asked for:
/// bob's client sent the `CALL_END` whose handler published and forwarded in
/// the same breath. A seat whose session died sends nothing, so the crossing
/// hangs on the roster-holding node's sweeper publishing the retirement out
/// of band and handing it to the same fan-out tier a request handler uses —
/// the half that would silently not exist if the sweeper owed only its own
/// node's subscribers. The scenario keeps the first test's shape — alice the
/// far subscriber on the conversation's home node, bob the joiner on the
/// other node — and replaces the leave with the one honest signal a dead
/// client ever sends: a socket that closes with no frame at all. The grace
/// window is shortened on bob's node so the retirement lands inside a test
/// budget; everything after the drop is the production timing, grace plus one
/// sweep tick plus the tier's own drain.
#[tokio::test]
async fn a_dead_seat_s_retirement_reaches_the_far_subscriber() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "migod=debug,migo_federation=info,migo_gateway=warn",
        ))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();

    // The fleet: the same two nodes, with bob's holding the short seat grace
    // because his seat is the one that will die.
    let store = migo_store::open(&StoreConfig::default())
        .await
        .expect("the in-memory backend opens");
    let app_a = build_node(&store, "node-alpha", "alpha", SEED_ALPHA).await;
    let app_b = build_node_with_short_seat_grace(&store, "node-beta", "beta", SEED_BETA).await;
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

    let alice_grant = registered_grant(&app_a, "sfudeadalice").await;
    let bob_grant = registered_grant(&app_a, "sfudeadbob").await;

    let mut alice = Client::connect_fresh(a_addr, &alice_grant).await;
    let mut bob = Client::connect_fresh(b_addr, &bob_grant).await;

    // The conversation, homed on alpha by the node alice created it through —
    // the same home the tier's fan-out question will read after the death.
    let summary: ConversationSummary = alice
        .ask(
            Opcode::ConversationCreate,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![bob_grant.account_id],
                title: Some("The Dead Seat's Call".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    // Both sides subscribed, and the watch ask waited for by table — so the
    // tier is proven up *before* the death, and a silent far socket after the
    // drop is the retirement's silence, not the subscription's.
    alice.subscribe_conversation(conversation_id).await;
    bob.subscribe_conversation(conversation_id).await;
    let mut registered = false;
    let deadline = tokio::time::Instant::now() + MESH_BUDGET;
    while tokio::time::Instant::now() < deadline {
        if app_a
            .conversation_relay
            .watchers_of(conversation_id)
            .contains(&id_b)
        {
            registered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    assert!(
        registered,
        "node alpha must register node beta as a watcher of the conversation within the \
         mesh budget: {:?}",
        app_a.conversation_relay.watchers_of(conversation_id)
    );

    let evidence = || {
        format!(
            "alpha's gateway wrote {} frames and dropped {:?}; beta's gateway wrote {} \
             frames and dropped {:?}",
            app_a.gateway.frames_out_total(),
            app_a.gateway.dropped_frames_total(),
            app_b.gateway.frames_out_total(),
            app_b.gateway.dropped_frames_total(),
        )
    };

    // The join: bob's seat lands on beta's group store, the roster returns to
    // his own user topic, and the join announcement crosses to alice — the
    // frames the first test already proved, kept here so the retirement is
    // the only thing the death changes.
    let call_id = Id::from_bytes([0xD5; 16]);
    let _: CallTurnResponse = bob
        .ask(
            Opcode::CallSfuJoin,
            &CallInvite {
                call_id,
                conversation_id,
                callee_id: Id::from(0u128),
                media_kind: 0,
                caller_device: Id::from(0u128),
                capabilities: 0,
                sealed_offer: SEALED_OFFER.to_vec(),
            },
        )
        .await;
    let roster_frame = bob
        .next_event_of(Opcode::CallSfuEvent, STEP, &evidence)
        .await;
    let roster: CallStateEvent = from_frame(&roster_frame).expect("the roster event decodes");
    assert_eq!(roster.call_id, call_id, "the roster names the call");
    let join_frame = alice
        .next_event_of(Opcode::CallSfuEvent, MESH_BUDGET, &evidence)
        .await;
    let join: CallStateEvent = from_frame(&join_frame).expect("the join announcement decodes");
    assert_eq!(join.call_id, call_id, "the announcement names the call");
    assert_eq!(
        join.state,
        migo_calls::group_store::GROUP_STATE_CONNECTED,
        "the join announcement is the Connected state"
    );
    alice
        .assert_quiet_for(Opcode::CallSfuEvent, &evidence)
        .await;
    bob.assert_quiet_for(Opcode::CallSfuEvent, &evidence).await;

    // The sweeper `App::serve` spawns in production on the roster-holding
    // node, started by hand because neither node serves — the retirement it
    // publishes is the whole point.
    let _sweeper = app_b.spawn_call_sweeper();

    // The death: bob's socket closes with no `CALL_END`, the only honest
    // signal a dead client ever sends. Beta stamps the seat gone on the
    // session edge, and the sweeper owes the retirement once the shortened
    // grace passes.
    drop(bob);

    // The retirement the far subscriber must hear: the sweeper's departure,
    // published out of band on the roster-holding node and carried to the
    // home node's conversation topic — `Ended`, the dead member named, the
    // roster emptied, and the reason telling the truth about a departure
    // nobody chose to send.
    let retired_frame = alice
        .next_event_of(Opcode::CallSfuEvent, MESH_BUDGET, &evidence)
        .await;
    let retired: CallStateEvent = from_frame(&retired_frame).expect("the retirement decodes");
    assert_eq!(retired.call_id, call_id, "the retirement names the call");
    assert_eq!(
        retired.conversation_id,
        Some(conversation_id),
        "the retirement names the conversation it belongs to"
    );
    assert_eq!(
        retired.state,
        migo_calls::group_store::GROUP_STATE_ENDED,
        "the retirement is the Ended state"
    );
    assert_eq!(
        retired.user_id,
        Some(bob_grant.account_id),
        "the retirement names the member whose session died"
    );
    assert_eq!(
        retired.device_id,
        Some(bob_grant.device_id),
        "the retirement names the dead member's device"
    );
    assert_eq!(
        retired.participant_count,
        Some(0),
        "the roster is empty after the retirement"
    );
    assert_eq!(
        retired.reason,
        Some(migo_calls::EndReason::Network.to_wire()),
        "Network: the session died, nobody withdrew"
    );

    // And exactly once: the retirement is the last frame the call owes
    // anybody, so the far socket falls quiet on the announcement opcode.
    alice
        .assert_quiet_for(Opcode::CallSfuEvent, &evidence)
        .await;
}
