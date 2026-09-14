//! A kick vote's tally, answered across nodes: the one-question rule a room
//! promises, proven on the deployment shape where it used to be a lie.
//!
//! "One vote runs per room" is a promise the rooms service makes on every
//! `VOTE_ALREADY_OPEN` it answers, and until the tally moved into the store
//! that promise was only true per *process*: the registry was a
//! `Mutex<HashMap>` inside each node's rooms service, so two nodes over one
//! database — the single-store deployment section 170 names as a first-class
//! shape — each held their own map, each answered their own question, and a
//! vote opened through node A was invisible to node B. Two factions on two
//! nodes could run two tallies against the same room at the same time, each
//! reach its own threshold, and each kick out a member the other tally never
//! agreed to remove.
//!
//! The tally is now a store fact (the `kick_vote` tables, migration 0015), and
//! this suite drives the whole rule at the client seam over real TCP sockets:
//! two full nodes built through `App::build_with_store` over ONE store, the
//! same composition seam `cross_node_resume.rs` uses, with no mesh link
//! between them because the path under test never touches federation — both
//! nodes serve the same room from the same rows, which is exactly the gap the
//! old registry fell into. The scenario, in the order the bug lived it:
//!
//! * a member on node alpha opens a vote, and the reply says one voice of two;
//! * the *target*, on node beta, tries to open a counter-vote against a
//!   different member — and is refused `VOTE_ALREADY_OPEN`, because beta reads
//!   the same tally alpha wrote. On the old code this is exactly where the lie
//!   showed: beta's empty registry would have opened a second question;
//! * a member on node beta then adds the carrying voice to *alpha's* tally —
//!   two of two, the vote passes through beta, and the kick lands where both
//!   nodes can see it, in the shared membership rows the roster reads.
//!
//! Every exchange is bounded by a step budget and every expectation is asserted
//! on frame contents, the client-seam house style, because a full `App` serves
//! real sockets on a real clock.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::config::StoreConfig;
use migo_core::{Clock, Config, Id, Secret};
use migo_protocol::{
    codes, from_frame, to_frame, Decode, Encode, Frame, Hello, Opcode, Platform, RoomCreate,
    RoomJoinRequest, RoomJoinResponse, RoomVoteKick, RoomVoteKickResponse, RosterReq,
    RosterResponse, SubscribeRequest, SubscribeResponse, Topic, TopicKind, Welcome,
    PROTOCOL_VERSION,
};
use migod::App;

/// How long any single exchange may take before the test declares a node stuck
/// — silence being the bug class this suite exists to catch.
const STEP: Duration = Duration::from_secs(5);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// One node of the two, built over the shared store the way a single-database
/// deployment builds every node it runs. The limiter ceilings are raised the
/// way the cross-node resume suite raises them: three scripted clients driving
/// a burst of handshakes, joins, and votes from one peer address are a test,
/// not a stranger, and the configuration keeps every charge inside the burst so
/// refill never enters the picture. One token key for both nodes, because a
/// grant minted by one must verify on the other or the scenario dies at the
/// handshake.
async fn build_node(store: &migo_store::SharedStore, node_id: &str, region: &str) -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
            ("MIGO_NODE__ID".to_string(), node_id.to_string()),
            ("MIGO_NODE__REGION".to_string(), region.to_string()),
            (
                "MIGO_RATE_LIMIT__ANONYMOUS_BURST".to_string(),
                "100".to_string(),
            ),
            ("MIGO_RATE_LIMIT__USER_BURST".to_string(), "100".to_string()),
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
                device: DeviceClaim::new(Platform::Web, "cross-node kick vote test"),
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
/// negotiating the rooms family (brief section 72 — every frame this suite
/// drives belongs to it, and a session that did not ask for the bit is refused
/// `FEATURE_NOT_NEGOTIATED` before the first `ROOM_CREATE`).
struct Client {
    stream: tokio::net::TcpStream,
    correlation: u32,
}

impl Client {
    /// Connects and opens a fresh, authenticated session, subscribing to the
    /// caller's own user topic the way every client does at its handshake. A
    /// rate-limited refusal is waited out and retried, exactly as told.
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        let mut backoff = 0u64;
        for _ in 0..5 {
            if backoff > 0 {
                tokio::time::sleep(Duration::from_millis(backoff + 100)).await;
            }
            match Self::handshake(addr, grant).await {
                Ok(session) => return session,
                Err(retry_after_ms) => backoff = retry_after_ms,
            }
        }
        panic!("the handshake never succeeds even after backing off as instructed");
    }

    /// One full connection attempt, returning the retry-after the server asked
    /// for when it refuses the handshake as rate-limited.
    async fn handshake(addr: SocketAddr, grant: &Grant) -> Result<Self, u64> {
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
        if welcome_frame.header.is_error() {
            let refusal: migo_protocol::Error =
                from_frame(&welcome_frame).expect("the refusal decodes");
            assert_eq!(
                refusal.code,
                codes::RATE_LIMITED,
                "the handshake is refused outright: {refusal:?}"
            );
            return Err(u64::from(refusal.retry_after_ms.unwrap_or(1000)));
        }
        let welcome: Welcome = from_frame(&welcome_frame).expect("the WELCOME decodes");
        assert_eq!(
            welcome.authenticated_user,
            Some(grant.account_id),
            "the inline token authenticated the session"
        );

        let mut session = Self {
            stream,
            correlation: 1,
        };
        let confirmation: SubscribeResponse = session
            .ask(
                Opcode::Subscribe,
                &SubscribeRequest {
                    topics: vec![Topic {
                        kind: TopicKind::User,
                        id: grant.account_id,
                    }],
                },
            )
            .await;
        assert_eq!(
            confirmation.accepted.len(),
            1,
            "the own user topic is accepted"
        );
        Ok(session)
    }

    /// Reads one frame from the session.
    async fn next_frame(&mut self) -> Frame {
        recv_within(&mut self.stream, STEP).await
    }

    /// Sends a request that must succeed, returning the decoded reply. Frames
    /// that are not the reply — the events a subscribed session receives — are
    /// read past, never discarded silently. A rate-limited refusal is waited
    /// out and retried, exactly as told.
    async fn ask<M: Encode, R: Decode>(&mut self, opcode: Opcode, message: &M) -> R {
        self.correlation += 1;
        let correlation = self.correlation;
        let mut backoff = 0u64;
        for _ in 0..5 {
            if backoff > 0 {
                tokio::time::sleep(Duration::from_millis(backoff + 100)).await;
            }
            send(&mut self.stream, opcode, correlation, message).await;
            loop {
                let frame = self.next_frame().await;
                if frame.header.correlation != correlation {
                    continue;
                }
                if !frame.header.is_error() {
                    return from_frame(&frame).expect("the reply decodes");
                }
                let refusal: migo_protocol::Error =
                    from_frame(&frame).expect("the refusal decodes");
                assert_eq!(
                    refusal.code,
                    codes::RATE_LIMITED,
                    "the request was refused outright: {refusal:?}"
                );
                backoff = u64::from(refusal.retry_after_ms.unwrap_or(1000));
                break;
            }
        }
        panic!("the request never succeeds even after backing off as instructed");
    }

    /// Sends a request that must be refused, returning the error it was refused
    /// with. The rate limiter's refusal is not one of these: it is the server
    /// asking for patience, so it is waited out rather than returned.
    async fn refused<M: Encode>(&mut self, opcode: Opcode, message: &M) -> migo_protocol::Error {
        self.correlation += 1;
        let correlation = self.correlation;
        let mut backoff = 0u64;
        for _ in 0..5 {
            if backoff > 0 {
                tokio::time::sleep(Duration::from_millis(backoff + 100)).await;
            }
            send(&mut self.stream, opcode, correlation, message).await;
            loop {
                let frame = self.next_frame().await;
                if frame.header.correlation != correlation {
                    continue;
                }
                if !frame.header.is_error() {
                    panic!("the request was answered with success: {frame:?}");
                }
                let refusal: migo_protocol::Error =
                    from_frame(&frame).expect("the refusal decodes");
                if refusal.code == codes::RATE_LIMITED {
                    backoff = u64::from(refusal.retry_after_ms.unwrap_or(1000));
                    break;
                }
                return refusal;
            }
        }
        panic!("the request is never answered outside the rate limiter");
    }

    /// Joins a room (or re-joins it — the call is idempotent).
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

    /// Casts one voice of a kick vote.
    async fn vote_kick(&mut self, room_id: Id, target_id: Id) -> RoomVoteKickResponse {
        self.ask(Opcode::RoomVoteKick, &RoomVoteKick { room_id, target_id })
            .await
    }
}

/// The scenario in the order the bug lived it: a question opened on one node
/// is the question on both, because the tally is a store fact and not a
/// per-process map.
#[tokio::test]
async fn one_kick_vote_question_at_a_time_is_a_fact_both_nodes_see() {
    // The fleet: two nodes over one store, the one-database shape of section
    // 170. No mesh link joins them, on purpose: the client path under test is
    // the rooms RPC both nodes serve from the same rows, which is exactly the
    // half of the deployment the old per-process registry got wrong.
    let store = migo_store::open(&StoreConfig::default())
        .await
        .expect("the in-memory backend opens");
    let alpha = build_node(&store, "node-alpha", "alpha").await;
    let beta = build_node(&store, "node-beta", "beta").await;
    let a_addr = alpha.tcp_bind.expect("node alpha binds its TCP listener");
    let b_addr = beta.tcp_bind.expect("node beta binds its TCP listener");

    // Four accounts, registered through node alpha's front door; the store is
    // shared, so beta authenticates the same grants. The fourth is a spare
    // target: a room's owner is beyond every vote, so proving that a new
    // question may open after one passes needs a member who is not one.
    let owner = registered_grant(&alpha, "kickvoteowner").await;
    let member = registered_grant(&alpha, "kickvotemember").await;
    let target = registered_grant(&alpha, "kickvotetarget").await;
    let spare = registered_grant(&alpha, "kickvotespare").await;

    // The owner on alpha founds a public room; the member joins through alpha
    // and the target and the spare through *beta*, which is the first thing
    // this scenario proves beyond the vote itself — beta serves a room it did
    // not create, because every fact it needs is a store fact.
    let mut owner_session = Client::connect(a_addr, &owner).await;
    let mut member_session = Client::connect(a_addr, &member).await;
    let mut target_session = Client::connect(b_addr, &target).await;
    let mut spare_session = Client::connect(b_addr, &spare).await;

    let created: RoomJoinResponse = owner_session
        .ask(
            Opcode::RoomCreate,
            &RoomCreate {
                slug: "cross-node-kick-vote".to_string(),
                name: "The one question".to_string(),
                kind: 1, // RoomKind::Public
                topic: None,
                max_members: None,
            },
        )
        .await;
    let room = created.room.room_id;
    member_session.join_room(room).await;
    target_session.join_room(room).await;
    spare_session.join_room(room).await;

    // A room of four needs two voices. The member, on alpha, opens the
    // question against the target.
    let opened = member_session.vote_kick(room, target.account_id).await;
    assert_eq!(opened.votes, 1, "the opener's voice is the first");
    assert_eq!(opened.needed, 2, "half of four");
    assert_eq!(opened.member_count, 4);
    assert!(opened.open, "one voice of two does not pass");

    // The target, on beta, tries to open a counter-question against the
    // member. This is the assertion the old code fails: beta's registry was
    // empty, so beta would have opened a second tally against the same room,
    // and the two factions would each have been counting their own question.
    // The store-backed tally makes the refusal the same on both nodes.
    let refusal = target_session
        .refused(
            Opcode::RoomVoteKick,
            &RoomVoteKick {
                room_id: room,
                target_id: member.account_id,
            },
        )
        .await;
    assert_eq!(
        refusal.code,
        codes::VOTE_ALREADY_OPEN,
        "the question alpha opened is the question beta sees"
    );

    // And the refusal disturbed nothing: the member's own tally is still
    // running, still at one voice, and a retried voice from the opener is the
    // same answer the first call got.
    let retried = member_session.vote_kick(room, target.account_id).await;
    assert_eq!(retried.votes, 1, "the tally did not move for a retry");
    assert!(retried.open);

    // The target cannot carry their own defence alone, but the owner can end
    // the question: a voice cast on *beta* — the node that refused to open a
    // second one — joins alpha's tally, reaches two of two, and the kick lands
    // in the membership rows both nodes read.
    let mut owner_on_beta = Client::connect(b_addr, &owner).await;
    let carried = owner_on_beta.vote_kick(room, target.account_id).await;
    assert_eq!(
        carried.votes, 2,
        "the voice cast on beta joined alpha's tally"
    );
    assert_eq!(carried.needed, 2);
    assert!(
        !carried.open,
        "two of two is the tally, wherever the second voice came from"
    );

    // The kick landed where both nodes can see it: the roster read back on
    // alpha no longer seats the target, and the room's count is the three who
    // remain.
    let roster: RosterResponse = owner_session
        .ask(
            Opcode::RoomRoster,
            &RosterReq {
                room_id: room,
                limit: None,
                after: None,
            },
        )
        .await;
    assert_eq!(
        roster.members.len(),
        3,
        "the vote's kick emptied the seat, in the rows both nodes read"
    );
    assert!(
        roster
            .members
            .iter()
            .all(|entry| entry.account_id != target.account_id),
        "the target is gone from the roster"
    );

    // And with the question answered, the room may be asked another: the tally
    // that passed is closed, so a new one opens instead of being refused.
    let reopened = member_session.vote_kick(room, spare.account_id).await;
    assert!(reopened.open, "a passed tally leaves no question behind");
    assert_eq!(reopened.votes, 1);
}
