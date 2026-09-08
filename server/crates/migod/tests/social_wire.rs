//! The SOCIAL opcodes answered on the wire, where the handler layer lives.
//!
//! Two behaviours only the dispatcher can get wrong, because the service under it is
//! correct in isolation:
//!
//! * **A full friends page must not hide the request behind it.** The combined
//!   relationship listing once truncated the concatenated answer to the caller's
//!   limit, so a user whose friends filled the page read "no pending requests"
//!   while requests sat unanswered — the requests view and its badge rendered an
//!   empty graph over a list that was never fully read. The listing now bounds
//!   each kind separately; the first test drives `RELATIONSHIP_LIST` over TCP with
//!   a one-entry limit and asserts the friend and the waiting request are both on
//!   the answer.
//! * **A crossing request is an acceptance, and must say so.** When B asks A while
//!   A's request to B is still waiting, the two become friends — and the account
//!   that asked first is told with the `state` hint `FRIEND_EVENT` carries. The
//!   handler once labelled every request-path notice `request`, so the original
//!   asker heard "request" for an event that was an acceptance. The second test
//!   drives the crossing over TCP and asserts the label.
//!
//! Both tests use the reply rule as their clock: every frame they wait for is one
//! the server owes somebody, so the timeout is the assertion.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Clock, Config, Secret};
use migo_protocol::{
    from_frame, to_frame, Acknowledged, Encode, Frame, FriendEvent, FriendRespond, FriendTarget,
    Opcode, RelationshipList, RelationshipListReq, SubscribeRequest, SubscribeResponse, Topic,
    TopicKind, PROTOCOL_VERSION,
};
use migod::App;

/// How long any single exchange may take before the test declares the server
/// stuck — silence being the bug class both tests exist to catch.
const STEP: Duration = Duration::from_secs(5);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development app with the TCP listener bound, as the listener tests build
/// it: the in-memory backends, the real services over them, and the one
/// environment pair that turns the native transport on.
async fn build_app() -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
            // The paged-walk test opens six sessions against one listener, and the
            // pre-auth HELLO bucket is shared per peer IP. The production refill
            // still answers a reconnect storm; this suite's concern is the protocol,
            // so the bucket is widened for the test app rather than each session
            // sleeping the bucket back open.
            (
                "MIGO_RATE_LIMIT__ANONYMOUS_BURST".to_string(),
                "80".to_string(),
            ),
        ],
    )
    .expect("configuration should parse");
    App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends")
}

/// Registers one account through the front door, stamped with the node's own
/// clock so the inline token is not born expired.
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
                device: DeviceClaim::new(migo_protocol::Platform::Web, "social wire test"),
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
    Frame::decode(Bytes::from(body)).expect("the frame decodes")
}

/// A live authenticated TCP session, subscribed to its own user topic — the
/// topic every friend event and notification is published to.
struct LiveSession {
    stream: tokio::net::TcpStream,
}

impl LiveSession {
    /// The pre-auth HELLO is charged against the peer IP at the anonymous tier, and a
    /// development endpoint bucket holds two hellos and refills 2.5 tokens a second —
    /// so a test that opens several sessions spaces them, the same consideration a
    /// client's reconnect backoff has, instead of racing the refill.
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting does not stall")
            .expect("the connection is accepted");

        let hello = migo_protocol::Hello {
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
                let confirmation: SubscribeResponse =
                    from_frame(&frame).expect("the SUBSCRIBE reply decodes");
                assert_eq!(
                    confirmation.accepted.len(),
                    1,
                    "the own user topic is accepted"
                );
                return Self { stream };
            }
        }
    }

    /// Sends a request that must succeed, returning the decoded reply.
    async fn ask<M: Encode, R: migo_protocol::Decode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        message: &M,
    ) -> R {
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

    /// Asks for friendship and waits for the acknowledgement.
    async fn friend_request(&mut self, correlation: u32, target: migo_core::Id) {
        let _: Acknowledged = self
            .ask(
                Opcode::FriendRequest,
                correlation,
                &FriendTarget { user_id: target },
            )
            .await;
    }
}

/// Reads from a session until a frame of the wanted opcode arrives, skipping
/// the unrelated events a subscribed session receives.
async fn next_event_of(stream: &mut tokio::net::TcpStream, want: Opcode) -> Frame {
    loop {
        let frame = recv_within(stream, STEP).await;
        if Opcode::from_wire(frame.header.opcode) == Some(want) {
            return frame;
        }
    }
}

/// The limit bounds each kind, never the concatenated answer: a one-entry page
/// still carries the settled friend and the request waiting behind it.
#[tokio::test]
async fn a_full_friends_page_does_not_hide_the_request_behind_it() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let alice_grant = registered_grant(&app, "alice").await;
    let bob_grant = registered_grant(&app, "bob").await;
    let carol_grant = registered_grant(&app, "carol").await;

    let mut alice = LiveSession::connect(addr, &alice_grant).await;
    let mut bob = LiveSession::connect(addr, &bob_grant).await;
    let mut carol = LiveSession::connect(addr, &carol_grant).await;

    // Alice and Bob become friends; Carol's request to Alice then waits — the
    // entry a combined cap would drop once the friend fills the page.
    alice.friend_request(10, bob_grant.account_id).await;
    let _: Acknowledged = bob
        .ask(
            Opcode::FriendRespond,
            11,
            &FriendRespond {
                user_id: alice_grant.account_id,
                accept: true,
            },
        )
        .await;
    carol.friend_request(12, alice_grant.account_id).await;

    let listing: RelationshipList = alice
        .ask(
            Opcode::RelationshipList,
            13,
            &RelationshipListReq {
                limit: 1,
                kind: None,
                cursor: None,
            },
        )
        .await;
    let kinds: Vec<(migo_core::Id, u32)> = listing
        .entries
        .iter()
        .map(|entry| (entry.user_id, entry.kind))
        .collect();
    assert!(
        kinds.contains(&(
            bob_grant.account_id,
            migo_protocol::RelationshipKind::Friend.to_wire()
        )),
        "the settled friend is on the page: {kinds:?}"
    );
    assert!(
        kinds.contains(&(
            carol_grant.account_id,
            migo_protocol::RelationshipKind::PendingIncoming.to_wire()
        )),
        "the waiting request is on its own page, not starved behind the friend: {kinds:?}"
    );
}

/// A crossing request — B asks A while A's request to B waits — accepts the one
/// already waiting, and the account that asked first hears `accepted`, not
/// `request`, for the event that made them friends.
#[tokio::test]
async fn a_crossing_request_is_announced_as_an_acceptance() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let alice_grant = registered_grant(&app, "alice").await;
    let bob_grant = registered_grant(&app, "bob").await;

    let mut alice = LiveSession::connect(addr, &alice_grant).await;
    let mut bob = LiveSession::connect(addr, &bob_grant).await;

    // Alice asks first: Bob hears a request, which is what the label said before
    // and must still say now.
    alice.friend_request(10, bob_grant.account_id).await;
    let request_frame = next_event_of(&mut bob.stream, Opcode::FriendEvent).await;
    let request: FriendEvent = from_frame(&request_frame).expect("the event decodes");
    assert_eq!(request.user_id, alice_grant.account_id);
    assert_eq!(request.state, "request", "a first ask is a request");

    // Bob asks back: the two become friends, and Alice — the original asker, the
    // notice's audience — must hear an acceptance.
    bob.friend_request(11, alice_grant.account_id).await;
    let accepted_frame = next_event_of(&mut alice.stream, Opcode::FriendEvent).await;
    let accepted: FriendEvent = from_frame(&accepted_frame).expect("the event decodes");
    assert_eq!(accepted.user_id, bob_grant.account_id);
    assert_eq!(
        accepted.state, "accepted",
        "a crossing request is an acceptance to the account that asked first"
    );
}

/// A paged walk of one kind over TCP reaches the end of a friends list longer than
/// a page, following the server's cursor between pages.
///
/// The combined listing bounds each kind at the server's page, so a caller with
/// more friends than that has no reachable end through it — this is the path that
/// reaches it, driven here over the same TCP frames a real client sends.
#[tokio::test]
async fn a_paged_walk_over_tcp_reaches_friends_beyond_the_first_page() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let alice_grant = registered_grant(&app, "alice").await;
    let mut alice = LiveSession::connect(addr, &alice_grant).await;

    // Five friends for Alice: one more than the four-row page the walk asks for, so
    // the walk crosses exactly one page boundary and then reads the tail.
    let mut correlation = 10u32;
    for n in 0..5u32 {
        let peer_grant = registered_grant(&app, &format!("peer{n}")).await;
        alice
            .friend_request(correlation, peer_grant.account_id)
            .await;
        correlation += 1;
        // The peer accepts from their own session, which is the only way an
        // acceptance can happen: the answer belongs to the account asked.
        let mut peer = LiveSession::connect(addr, &peer_grant).await;
        let _: Acknowledged = peer
            .ask(
                Opcode::FriendRespond,
                correlation,
                &FriendRespond {
                    user_id: alice_grant.account_id,
                    accept: true,
                },
            )
            .await;
        correlation += 1;
    }

    let kind = migo_protocol::RelationshipKind::Friend.to_wire();
    let mut seen: Vec<migo_core::Id> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let listing: RelationshipList = alice
            .ask(
                Opcode::RelationshipList,
                correlation,
                &RelationshipListReq {
                    limit: 4,
                    kind: Some(kind),
                    cursor: cursor.clone(),
                },
            )
            .await;
        correlation += 1;
        assert!(listing.entries.len() <= 4, "the limit is a ceiling");
        seen.extend(listing.entries.iter().map(|entry| entry.user_id));
        match listing.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(
        seen.len(),
        5,
        "every friend is reached exactly once: {seen:?}"
    );
}
