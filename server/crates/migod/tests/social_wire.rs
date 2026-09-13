//! The SOCIAL opcodes answered on the wire, where the handler layer lives.
//!
//! Six behaviours only the dispatcher can get wrong, because the service under it is
//! correct in isolation:
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
//! * **A custom status belongs to the session that negotiated RICH_PRESENCE.** The
//!   field is the feature bit's own surface (section 148): a session that asked for
//!   the bit sets and reads back its status, and a session that did not is answered
//!   FEATURE_NOT_NEGOTIATED for the field — while the rest of PROFILE_UPDATE keeps
//!   working, because every deployed client sends that opcode without the bit. One of
//!   the tests below drives the pair the way a client uses it: the bit set a stock
//!   client offers, a status written through the profile patch, and the value read
//!   back off the card by a different session — the half the other two cannot see.
//! * **A feature bit is a switch on the wire, not an advertisement.** The PRESENCE
//!   bit gates the family's frames in both directions (section 72): the inbound half
//!   is the FEATURE_NOT_NEGOTIATED refusal, and the outbound half is the withholding
//!   — a session that did not ask for the bit is not sent the family's events at
//!   all. The last test drives the outbound half with two sessions of one account
//!   watching a friend's topic, one with the bit and one without, and reads the
//!   bitless session through a PING to make the missing frame a deterministic fact
//!   rather than a timeout that proves nothing.
//! * **Every graph move reaches every device that holds a stale copy.** The fan-out
//!   behind `FRIEND_EVENT` has two halves, and for a long time only one of them
//!   existed: the other party heard the move, while the actor's *other* devices —
//!   and, for declines and blocks, the other party entirely — sat on a stale friends
//!   list until somebody refreshed by hand. The tests after the crossing one
//!   drive each mutation with both parties live on two devices each, over real TCP
//!   sockets, and assert who hears what: an acceptance reaches the acceptor's other
//!   device (and not the session that answered), a request reaches the asker's other
//!   device, a decline moves the graph for both parties without a bell, a block
//!   tears a friendship down live on both sides, and blocking a stranger publishes
//!   nothing to the stranger at all — the privacy half of the same fan-out. A mute
//!   is the quietest member of the family: the muter's other device hears the
//!   switch flip both ways, and the muted account hears nothing, because a volume
//!   control is not a verdict.
//!
//! Every test uses the reply rule as its clock: every frame it waits for is one
//! the server owes somebody, so the timeout is the assertion.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext, SignIn};
use migo_core::{Clock, Config, Secret};
use migo_protocol::{
    codes, from_frame, to_frame, Acknowledged, Encode, Frame, FriendEvent, FriendRespond,
    FriendTarget, MuteSet, NotificationEvent, NotificationKind, Opcode, PresenceEvent,
    PresenceState, PresenceUpdate, ProfileRequest, ProfileResponse, ProfileUpdate,
    RelationshipList, RelationshipListReq, SubscribeRequest, SubscribeResponse, Topic, TopicKind,
    UserProfile, PROTOCOL_VERSION,
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

/// Signs an existing account in on a second device, the way the answered-elsewhere
/// fan-outs already do it: the bit test below needs one account watching from two
/// sessions at once, one that asked for a feature bit and one that did not.
async fn second_device_grant(app: &App, username: &str) -> Grant {
    app.auth
        .sign_in(
            SignIn {
                identifier: username.to_string(),
                passphrase: Secret::new("correct-horse-battery-staple"),
                device: DeviceClaim::new(migo_protocol::Platform::Web, "the other device"),
                captcha: None,
                server: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a fresh second sign-in needs no captcha and succeeds")
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
    /// A session that negotiated no feature bits — the honest set of every
    /// deployed client, which sends no bit it does not know.
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        Self::connect_with_features(addr, grant, 0).await
    }

    /// A live session whose HELLO asked for the given feature bits. The node
    /// advertises RICH_PRESENCE, so a client that asks for it negotiates it and
    /// the session carries the bit for its whole lifetime (section 148).
    ///
    /// The pre-auth HELLO is charged against the peer IP at the anonymous tier, and a
    /// development endpoint bucket holds two hellos and refills 2.5 tokens a second —
    /// so a test that opens several sessions spaces them, the same consideration a
    /// client's reconnect backoff has, instead of racing the refill.
    async fn connect_with_features(addr: SocketAddr, grant: &Grant, features: u64) -> Self {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting does not stall")
            .expect("the connection is accepted");

        let hello = migo_protocol::Hello {
            protocol_version: PROTOCOL_VERSION,
            access_token: Some(grant.access_token.clone()),
            device_id: Some(grant.device_id),
            features,
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

    /// Subscribes to a peer's user topic, the one every presence event for that
    /// account is published to. The self-subscription the constructor performs is
    /// the same call with the session's own id; this is the second one a friend's
    /// client makes, and its acceptance is what authorizes the events below.
    async fn subscribe_to_user(&mut self, correlation: u32, user: migo_core::Id) {
        let confirmation: SubscribeResponse = self
            .ask(
                Opcode::Subscribe,
                correlation,
                &SubscribeRequest {
                    topics: vec![Topic {
                        kind: TopicKind::User,
                        id: user,
                    }],
                },
            )
            .await;
        assert_eq!(
            confirmation.accepted.len(),
            1,
            "the peer's user topic is accepted for a friend"
        );
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

#[tokio::test]
async fn a_friend_request_rings_exactly_one_bell() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let alice_grant = registered_grant(&app, "bellalice").await;
    let bob_grant = registered_grant(&app, "bellbob").await;

    let mut alice = LiveSession::connect(addr, &alice_grant).await;
    let mut bob = LiveSession::connect(addr, &bob_grant).await;

    // The ask. Bob is owed two frames for it — the hint and the bell — and the bell
    // used to be published by the dispatcher's own hand while the row went through the
    // notifier; both halves now leave through the notifier's one seam, and this test
    // pins that the fold did not double the frame.
    alice.friend_request(20, bob_grant.account_id).await;
    let request_frame = next_event_of(&mut bob.stream, Opcode::FriendEvent).await;
    let request: FriendEvent = from_frame(&request_frame).expect("the event decodes");
    assert_eq!(request.user_id, alice_grant.account_id);
    assert_eq!(request.state, "request");

    let bell_frame = next_event_of(&mut bob.stream, Opcode::NotificationEvent).await;
    let bell: NotificationEvent = from_frame(&bell_frame).expect("the bell decodes");
    assert_eq!(bell.kind, NotificationKind::FriendRequest);
    assert_eq!(bell.actor_id, Some(alice_grant.account_id));
    assert_eq!(bell.title, None, "the client writes the sentence");
    assert_eq!(bell.body, None, "the client writes the sentence");

    // Exactly one. A PING is the next frame Bob's session is owed; a second bell
    // queued ahead of the PONG would be read here.
    send(
        &mut bob.stream,
        Opcode::Ping,
        21,
        &migo_protocol::Ping {
            client_time: migo_core::Timestamp::from_millis(0),
        },
    )
    .await;
    let pong = loop {
        let frame = recv_within(&mut bob.stream, STEP).await;
        assert_ne!(
            Opcode::from_wire(frame.header.opcode),
            Some(Opcode::NotificationEvent),
            "one ask rings one bell"
        );
        if frame.header.correlation == 21 {
            break frame;
        }
    };
    assert!(!pong.header.is_error());

    // Alice asked, so nobody rings hers: the session that acted is answered by the
    // acknowledgement alone. The echo hint is owed to her *other* devices (section
    // 156 keeps it off the session that just acted), so the assertion here is an
    // absence: a PING, and no FRIEND_EVENT may arrive in front of its PONG.
    send(
        &mut alice.stream,
        Opcode::Ping,
        22,
        &migo_protocol::Ping {
            client_time: migo_core::Timestamp::from_millis(0),
        },
    )
    .await;
    let pong = loop {
        let frame = recv_within(&mut alice.stream, STEP).await;
        let opcode = Opcode::from_wire(frame.header.opcode);
        assert_ne!(
            opcode,
            Some(Opcode::FriendEvent),
            "the session that asked is not handed the echo it was already answered with"
        );
        assert_ne!(
            opcode,
            Some(Opcode::NotificationEvent),
            "the asker's own devices are not an audience of her own ask"
        );
        if frame.header.correlation == 22 {
            break frame;
        }
    };
    assert!(!pong.header.is_error());
}

/// The acceptance's other half: the acceptor's own second device.
///
/// The session that pressed the button was answered by its `Acknowledged` and re-read
/// the graph itself, but an account's *other* device holds a friends list that just
/// grew, and before the caller-echo fan-out it sat stale until somebody refreshed by
/// hand — the exact shape of the "accepted friend requests do not appear immediately"
/// report. The echo must also skip the session that acted (section 156): it already
/// knows, and a second hint for a change it just made is a frame with no news in it.
#[tokio::test]
async fn an_acceptance_reaches_the_acceptors_other_device() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let alice_grant = registered_grant(&app, "fiona").await;
    let bob_grant = registered_grant(&app, "gael").await;
    let bob_laptop_grant = second_device_grant(&app, "gael").await;

    let mut alice = LiveSession::connect(addr, &alice_grant).await;
    let mut bob = LiveSession::connect(addr, &bob_grant).await;
    let mut bob_laptop = LiveSession::connect(addr, &bob_laptop_grant).await;

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

    // The asker hears the acceptance: the notice's audience, as it always was.
    let accepted: FriendEvent =
        from_frame(&next_event_of(&mut alice.stream, Opcode::FriendEvent).await)
            .expect("the asker's event decodes");
    assert_eq!(accepted.user_id, bob_grant.account_id);
    assert_eq!(accepted.state, "accepted");

    // The acceptor's other device hears the same move as an echo on the acceptor's
    // own topic, naming the new friend. It has been subscribed since before the ask,
    // so its queue holds the whole story in order: first the incoming request —
    // the other device learns of the ask itself here — and then the acceptance the
    // sibling session just made.
    let asked: FriendEvent =
        from_frame(&next_event_of(&mut bob_laptop.stream, Opcode::FriendEvent).await)
            .expect("the acceptor's other device heard the ask");
    assert_eq!(asked.user_id, alice_grant.account_id);
    assert_eq!(asked.state, "request");

    let echo: FriendEvent =
        from_frame(&next_event_of(&mut bob_laptop.stream, Opcode::FriendEvent).await)
            .expect("the acceptor's other device gets an event");
    assert_eq!(echo.user_id, alice_grant.account_id);
    assert_eq!(
        echo.state, "accepted",
        "the acceptor's other device learns the graph grew a friend"
    );

    // The session that answered does not. A PING, and a read to its PONG: the echo —
    // if the fan-out had leaked it to the actor — is queued ahead of the reply this
    // PING earns, so finding the PONG without a FRIEND_EVENT in front of it is the
    // assertion, exactly as the feature-bit test reads its bitless session.
    send(
        &mut bob.stream,
        Opcode::Ping,
        12,
        &migo_protocol::Ping {
            client_time: migo_core::Timestamp::from_millis(0),
        },
    )
    .await;
    let pong = loop {
        let frame = recv_within(&mut bob.stream, STEP).await;
        assert_ne!(
            Opcode::from_wire(frame.header.opcode),
            Some(Opcode::FriendEvent),
            "the session that answered is not handed the echo it was already answered with"
        );
        if frame.header.correlation == 12 {
            break frame;
        }
    };
    assert!(
        !pong.header.is_error(),
        "the acting session keeps serving the frames it did ask for"
    );
}

/// A request is a row in the asker's own graph too: the outgoing list the asker's
/// other device shows must grow without a manual refresh, the same way the recipient's
/// incoming list does.
#[tokio::test]
async fn a_request_reaches_the_askers_other_device() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let alice_grant = registered_grant(&app, "hana").await;
    let alice_laptop_grant = second_device_grant(&app, "hana").await;
    let bob_grant = registered_grant(&app, "ivan").await;

    let mut alice = LiveSession::connect(addr, &alice_grant).await;
    let mut alice_laptop = LiveSession::connect(addr, &alice_laptop_grant).await;
    let mut bob = LiveSession::connect(addr, &bob_grant).await;

    alice.friend_request(10, bob_grant.account_id).await;

    // The recipient hears the request, as they always did.
    let request: FriendEvent =
        from_frame(&next_event_of(&mut bob.stream, Opcode::FriendEvent).await)
            .expect("the recipient's event decodes");
    assert_eq!(request.user_id, alice_grant.account_id);
    assert_eq!(request.state, "request");

    // The asker's other device hears the echo: the outgoing list it renders gained a
    // row, and this is the only frame that tells it so.
    let echo: FriendEvent =
        from_frame(&next_event_of(&mut alice_laptop.stream, Opcode::FriendEvent).await)
            .expect("the asker's other device gets an event");
    assert_eq!(echo.user_id, bob_grant.account_id);
    assert_eq!(
        echo.state, "request",
        "the asker's other device learns the request it watched leave"
    );
}

/// A mute is a page of the caller's own graph, and the echo is what keeps it honest on
/// the caller's other devices: a mute tapped on the phone leaves the tablet rendering
/// a stranger's messages until somebody refreshes by hand. The switch flips both ways —
/// `muted` on, `unmuted` off — and the muted account hears neither, because a volume
/// control is not a verdict.
#[tokio::test]
async fn a_mute_reaches_the_muters_other_device() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let alice_grant = registered_grant(&app, "jena").await;
    let alice_laptop_grant = second_device_grant(&app, "jena").await;
    let bob_grant = registered_grant(&app, "kyle").await;

    let mut alice = LiveSession::connect(addr, &alice_grant).await;
    let mut alice_laptop = LiveSession::connect(addr, &alice_laptop_grant).await;
    let mut bob = LiveSession::connect(addr, &bob_grant).await;

    // The mute itself. The two accounts need no relationship at all — a mute is the
    // caller's own business — so the switch is flipped on a stranger.
    let _: Acknowledged = alice
        .ask(
            Opcode::MuteSet,
            10,
            &MuteSet {
                user_id: bob_grant.account_id,
                on: true,
            },
        )
        .await;

    // The muter's other device hears the echo: the mute list it renders gained a
    // row, and this is the only frame that tells it so.
    let muted: FriendEvent =
        from_frame(&next_event_of(&mut alice_laptop.stream, Opcode::FriendEvent).await)
            .expect("the muter's other device gets an event");
    assert_eq!(muted.user_id, bob_grant.account_id);
    assert_eq!(
        muted.state, "muted",
        "the muter's other device learns the mute list grew"
    );

    // And back off again: the unmute is the same page moving the other way.
    let _: Acknowledged = alice
        .ask(
            Opcode::MuteSet,
            11,
            &MuteSet {
                user_id: bob_grant.account_id,
                on: false,
            },
        )
        .await;
    let unmuted: FriendEvent =
        from_frame(&next_event_of(&mut alice_laptop.stream, Opcode::FriendEvent).await)
            .expect("the muter's other device gets the second event");
    assert_eq!(unmuted.user_id, bob_grant.account_id);
    assert_eq!(
        unmuted.state, "unmuted",
        "the muter's other device learns the mute was lifted"
    );

    // The muted account hears nothing at all. A PING, and a read to its PONG: the
    // echo — had the fan-out leaked it across accounts — is queued ahead of the
    // reply this PING earns, so finding the PONG without a FRIEND_EVENT in front of
    // it is the assertion.
    send(
        &mut bob.stream,
        Opcode::Ping,
        12,
        &migo_protocol::Ping {
            client_time: migo_core::Timestamp::from_millis(0),
        },
    )
    .await;
    let pong = loop {
        let frame = recv_within(&mut bob.stream, STEP).await;
        assert_ne!(
            Opcode::from_wire(frame.header.opcode),
            Some(Opcode::FriendEvent),
            "the muted account is not told it was muted — a volume control is not a verdict"
        );
        if frame.header.correlation == 12 {
            break frame;
        }
    };
    assert!(
        !pong.header.is_error(),
        "the muted account keeps serving the frames it did ask for"
    );
}

/// A decline moves the graph for both parties, and neither of them hears a bell.
///
/// The service's rule is that a decline is the responder's own business — no notice,
/// no inbox row — and that rule stands. But the *graph* moved on both sides: the
/// asker's outgoing list and the decliner's incoming list each hold a row that is
/// gone. The `FRIEND_EVENT` hint carries no verdict, only that the edge moved, so the
/// asker learns exactly what their own next listing would have told them and nothing
/// more.
#[tokio::test]
async fn a_decline_moves_the_graph_for_both_parties() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let alice_grant = registered_grant(&app, "juno").await;
    let bob_grant = registered_grant(&app, "kira").await;
    let bob_laptop_grant = second_device_grant(&app, "kira").await;

    let mut alice = LiveSession::connect(addr, &alice_grant).await;
    let mut bob = LiveSession::connect(addr, &bob_grant).await;
    let mut bob_laptop = LiveSession::connect(addr, &bob_laptop_grant).await;

    alice.friend_request(10, bob_grant.account_id).await;
    let _: Acknowledged = bob
        .ask(
            Opcode::FriendRespond,
            11,
            &FriendRespond {
                user_id: alice_grant.account_id,
                accept: false,
            },
        )
        .await;

    // The asker hears the edge is gone — the hint, not a verdict.
    let removed: FriendEvent =
        from_frame(&next_event_of(&mut alice.stream, Opcode::FriendEvent).await)
            .expect("the asker's event decodes");
    assert_eq!(removed.user_id, bob_grant.account_id);
    assert_eq!(
        removed.state, "removed",
        "a decline tells the asker the edge is gone, and nothing about why"
    );

    // The decliner's other device: the incoming list it renders lost the same row.
    // It has held a subscription since before the ask, so its queue carries the story
    // in order — the incoming request first, then the decline that removed it.
    let asked: FriendEvent =
        from_frame(&next_event_of(&mut bob_laptop.stream, Opcode::FriendEvent).await)
            .expect("the decliner's other device heard the ask");
    assert_eq!(asked.user_id, alice_grant.account_id);
    assert_eq!(asked.state, "request");

    let echo: FriendEvent =
        from_frame(&next_event_of(&mut bob_laptop.stream, Opcode::FriendEvent).await)
            .expect("the decliner's other device gets an event");
    assert_eq!(echo.user_id, alice_grant.account_id);
    assert_eq!(echo.state, "removed");

    // And the asker's own listing agrees: no outgoing request survives the decline.
    let listing: RelationshipList = alice
        .ask(
            Opcode::RelationshipList,
            12,
            &RelationshipListReq {
                limit: 0,
                kind: None,
                cursor: None,
            },
        )
        .await;
    assert!(
        !listing.entries.iter().any(|entry| {
            entry.user_id == bob_grant.account_id
                && entry.kind == migo_protocol::RelationshipKind::PendingOutgoing.to_wire()
        }),
        "the declined request is gone from the asker's graph: {:?}",
        listing.entries
    );
}

/// A block tears a friendship down, and both sides' devices watch it happen live.
///
/// The blocked account hears the same `removed` hint an un-friend would carry — the
/// two must stay indistinguishable — on every device but the blocker's acting session
/// (section 156). The blocker's other devices hear the block itself, because their
/// block list and friends list both moved.
#[tokio::test]
async fn a_block_tears_the_friendship_down_live_on_both_sides() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let alice_grant = registered_grant(&app, "lena").await;
    let alice_laptop_grant = second_device_grant(&app, "lena").await;
    let bob_grant = registered_grant(&app, "omar").await;
    let bob_laptop_grant = second_device_grant(&app, "omar").await;

    let mut alice = LiveSession::connect(addr, &alice_grant).await;
    let mut alice_laptop = LiveSession::connect(addr, &alice_laptop_grant).await;
    let mut bob = LiveSession::connect(addr, &bob_grant).await;
    let mut bob_laptop = LiveSession::connect(addr, &bob_laptop_grant).await;

    // The friendship, established the ordinary way. Each device's expected events are
    // drained as they arrive, so the block below is read against a quiet stream.
    alice.friend_request(10, bob_grant.account_id).await;
    let request: FriendEvent =
        from_frame(&next_event_of(&mut bob.stream, Opcode::FriendEvent).await)
            .expect("the recipient's event decodes");
    assert_eq!(request.state, "request");
    let asker_echo: FriendEvent =
        from_frame(&next_event_of(&mut alice_laptop.stream, Opcode::FriendEvent).await)
            .expect("the asker's other device gets the request echo");
    assert_eq!(asker_echo.user_id, bob_grant.account_id);

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
    // Bob's other device was subscribed through the ask as well, so its queue holds
    // the incoming request ahead of the acceptance; both are drained in order.
    let bob_device_asked: FriendEvent =
        from_frame(&next_event_of(&mut bob_laptop.stream, Opcode::FriendEvent).await)
            .expect("the acceptor's other device heard the ask");
    assert_eq!(bob_device_asked.user_id, alice_grant.account_id);
    assert_eq!(bob_device_asked.state, "request");
    let accepted: FriendEvent =
        from_frame(&next_event_of(&mut alice.stream, Opcode::FriendEvent).await)
            .expect("the asker's acceptance decodes");
    assert_eq!(accepted.state, "accepted");
    let acceptor_echo: FriendEvent =
        from_frame(&next_event_of(&mut bob_laptop.stream, Opcode::FriendEvent).await)
            .expect("the acceptor's other device gets the acceptance echo");
    assert_eq!(acceptor_echo.state, "accepted");
    // The asker's other device hears the acceptance too — the notice's topic is the
    // asker's own, and both her sessions are subscribed to it — so it is drained here
    // to keep the block below read against a quiet stream.
    let asker_device_accepted: FriendEvent =
        from_frame(&next_event_of(&mut alice_laptop.stream, Opcode::FriendEvent).await)
            .expect("the asker's other device gets the acceptance");
    assert_eq!(asker_device_accepted.state, "accepted");

    // Bob blocks Alice. Every device of both accounts but the acting one holds a
    // stale copy of the graph now.
    let _: Acknowledged = bob
        .ask(
            Opcode::BlockSet,
            12,
            &FriendTarget {
                user_id: alice_grant.account_id,
            },
        )
        .await;

    // The blocked account, on both her devices: the friendship is gone, and the hint
    // is the word an un-friend would have carried.
    for stream in [&mut alice.stream, &mut alice_laptop.stream] {
        let removed: FriendEvent = from_frame(&next_event_of(stream, Opcode::FriendEvent).await)
            .expect("the blocked account's device gets an event");
        assert_eq!(removed.user_id, bob_grant.account_id);
        assert_eq!(
            removed.state, "removed",
            "the block's teardown is indistinguishable from an un-friend"
        );
    }

    // The blocker's other device: the block itself, which moved the block list and
    // the friends list it renders.
    let blocker_echo: FriendEvent =
        from_frame(&next_event_of(&mut bob_laptop.stream, Opcode::FriendEvent).await)
            .expect("the blocker's other device gets an event");
    assert_eq!(blocker_echo.user_id, alice_grant.account_id);
    assert_eq!(blocker_echo.state, "blocked");

    // The acting session is excluded, and the graphs agree with the frames: no
    // friendship survives on either side, and the block is where the blocker left it.
    send(
        &mut bob.stream,
        Opcode::Ping,
        13,
        &migo_protocol::Ping {
            client_time: migo_core::Timestamp::from_millis(0),
        },
    )
    .await;
    let pong = loop {
        let frame = recv_within(&mut bob.stream, STEP).await;
        assert_ne!(
            Opcode::from_wire(frame.header.opcode),
            Some(Opcode::FriendEvent),
            "the session that blocked is not handed the echo it was already answered with"
        );
        if frame.header.correlation == 13 {
            break frame;
        }
    };
    assert!(!pong.header.is_error());

    let listing: RelationshipList = alice
        .ask(
            Opcode::RelationshipList,
            14,
            &RelationshipListReq {
                limit: 0,
                kind: None,
                cursor: None,
            },
        )
        .await;
    assert!(
        !listing
            .entries
            .iter()
            .any(|entry| entry.kind == migo_protocol::RelationshipKind::Friend.to_wire()),
        "no friendship survives the block on the blocked side: {:?}",
        listing.entries
    );
}

/// The privacy half of the block fan-out: blocking a stranger publishes nothing to
/// the stranger.
///
/// A "the graph moved" hint delivered to an account whose graph did not visibly move
/// would name the blocker to somebody the wire otherwise tells nothing — most blocks
/// are of strangers, and a stranger who receives an event naming the person who just
/// blocked them has been told exactly that. The blocker's own other devices still
/// hear the block, because the block list they render did move.
#[tokio::test]
async fn blocking_a_stranger_publishes_nothing_to_the_stranger() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let alice_grant = registered_grant(&app, "paula").await;
    let alice_laptop_grant = second_device_grant(&app, "paula").await;
    let carol_grant = registered_grant(&app, "quinn").await;

    let mut alice = LiveSession::connect(addr, &alice_grant).await;
    let mut alice_laptop = LiveSession::connect(addr, &alice_laptop_grant).await;
    let mut carol = LiveSession::connect(addr, &carol_grant).await;

    let _: Acknowledged = alice
        .ask(
            Opcode::BlockSet,
            10,
            &FriendTarget {
                user_id: carol_grant.account_id,
            },
        )
        .await;

    // The blocker's other device hears the block: the block list it renders grew.
    let echo: FriendEvent =
        from_frame(&next_event_of(&mut alice_laptop.stream, Opcode::FriendEvent).await)
            .expect("the blocker's other device gets an event");
    assert_eq!(echo.user_id, carol_grant.account_id);
    assert_eq!(echo.state, "blocked");

    // The stranger hears nothing. A PING, and a read to its PONG: any leaked hint
    // would be queued ahead of the reply, so finding the PONG without a FRIEND_EVENT
    // in front of it is the assertion.
    send(
        &mut carol.stream,
        Opcode::Ping,
        11,
        &migo_protocol::Ping {
            client_time: migo_core::Timestamp::from_millis(0),
        },
    )
    .await;
    let pong = loop {
        let frame = recv_within(&mut carol.stream, STEP).await;
        assert_ne!(
            Opcode::from_wire(frame.header.opcode),
            Some(Opcode::FriendEvent),
            "a stranger the block removed nothing from is told nothing"
        );
        if frame.header.correlation == 11 {
            break frame;
        }
    };
    assert!(
        !pong.header.is_error(),
        "the stranger's session keeps serving the frames it did ask for"
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

/// The RICH_PRESENCE bit's own surface, end to end: a session that asked for the bit
/// sets a custom status through PROFILE_UPDATE and the reply reads it back, and a
/// later patch that names other fields keeps the status — the wire's "absent means
/// leave alone" extended to the new field.
#[tokio::test]
async fn a_rich_presence_session_sets_and_keeps_its_custom_status() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let grant = registered_grant(&app, "carol").await;
    let mut session =
        LiveSession::connect_with_features(addr, &grant, migo_protocol::features::RICH_PRESENCE)
            .await;

    let reply: UserProfile = session
        .ask(
            Opcode::ProfileUpdate,
            10,
            &ProfileUpdate {
                custom_status: Some("sedang di jalan".to_string()),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        reply.custom_status.as_deref(),
        Some("sedang di jalan"),
        "the reply is the caller's refreshed card, and the card carries the status"
    );
    assert_eq!(reply.user_id, grant.account_id);

    // A patch that names only the display name must keep the status: the field is
    // optional on the wire, and absent means leave alone.
    let kept: UserProfile = session
        .ask(
            Opcode::ProfileUpdate,
            11,
            &ProfileUpdate {
                display_name: Some("Carol Kota".to_string()),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        kept.custom_status.as_deref(),
        Some("sedang di jalan"),
        "a patch that does not name the status leaves it alone"
    );
    assert_eq!(kept.display_name, "Carol Kota");
}

/// The pair from the client's end: a session shaped like a stock client — the bit set
/// `packages/sdk/src/transport.ts` offers by default, which the web client's HELLO
/// carries — negotiates RICH_PRESENCE, writes a status, and a *different* session
/// reads that status back off the profile card.
///
/// This is the shape the two half-tests around it cannot pin. The bit being advertised
/// is proved elsewhere; the field round-tripping through PROFILE_UPDATE's own reply is
/// proved above. What neither covers is the pair as a client actually uses it: the
/// stock bit set negotiating at all, and the value being readable by somebody else —
/// the read side the friend list and the profile panel render from. A bit no shipped
/// client offers, or a status only its author can see, is the same dead end one step
/// further along.
#[tokio::test]
async fn a_stock_client_session_writes_a_status_a_peer_reads_back() {
    // The bits `DEFAULT_CLIENT_FEATURES` states, by name. Kept as a literal rather than
    // derived: the point is that a client offering an arbitrary set of *known* bits
    // negotiates the one it needs, and a helper that computed the set from the crates
    // would agree with the server by construction and prove nothing.
    const STOCK_CLIENT_FEATURES: u64 = migo_protocol::features::COMPRESSION
        | migo_protocol::features::BATCHING
        | migo_protocol::features::E2E_V1
        | migo_protocol::features::GROUP_E2E_V1
        | migo_protocol::features::PRESENCE
        | migo_protocol::features::TYPING
        | migo_protocol::features::ROOMS
        | migo_protocol::features::RESUME
        | migo_protocol::features::VOICE_MESSAGE
        | migo_protocol::features::RICH_PRESENCE;

    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let author_grant = registered_grant(&app, "erin").await;
    let reader_grant = registered_grant(&app, "frank").await;

    let mut author =
        LiveSession::connect_with_features(addr, &author_grant, STOCK_CLIENT_FEATURES).await;

    let written: UserProfile = author
        .ask(
            Opcode::ProfileUpdate,
            10,
            &ProfileUpdate {
                custom_status: Some("di dapur".to_string()),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        written.custom_status.as_deref(),
        Some("di dapur"),
        "the stock bit set negotiates RICH_PRESENCE, so the field is admitted"
    );

    // A reader that negotiated nothing at all — an older client, which knows no bit for
    // this — still gets the status on the card: reading the field has never needed the
    // bit, and gating the read would be a second, invisible dead end.
    let mut reader = LiveSession::connect(addr, &reader_grant).await;
    let card: ProfileResponse = reader
        .ask(
            Opcode::ProfileFetch,
            10,
            &ProfileRequest {
                user_ids: vec![author_grant.account_id],
            },
        )
        .await;
    let seen = card
        .profiles
        .iter()
        .find(|profile| profile.user_id == author_grant.account_id)
        .expect("the author's card is served to a peer");
    assert_eq!(
        seen.custom_status.as_deref(),
        Some("di dapur"),
        "the status a stock client wrote is what another session renders"
    );
}

/// The gate on the other side of the bit: a session that did not negotiate
/// RICH_PRESENCE is answered FEATURE_NOT_NEGOTIATED for the field — and the session,
/// and every other field of the opcode, keep working, which is what keeps every
/// deployed client (none of which send the field) unaffected.
#[tokio::test]
async fn a_custom_status_without_the_rich_presence_bit_is_refused_but_the_session_is_not() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let grant = registered_grant(&app, "dave").await;
    let mut session = LiveSession::connect(addr, &grant).await;

    send(
        &mut session.stream,
        Opcode::ProfileUpdate,
        10,
        &ProfileUpdate {
            custom_status: Some("tidak diizinkan".to_string()),
            ..Default::default()
        },
    )
    .await;
    loop {
        let frame = recv_within(&mut session.stream, STEP).await;
        if frame.header.correlation == 10 {
            assert!(
                frame.header.is_error(),
                "the custom status is refused on a session without the bit"
            );
            let error: migo_protocol::Error = from_frame(&frame).expect("the error frame decodes");
            assert_eq!(
                error.code,
                codes::FEATURE_NOT_NEGOTIATED,
                "section 148's answer for a feature the session did not negotiate"
            );
            break;
        }
    }

    // The refusal was answered, not fatal: the same session goes on serving the rest
    // of the opcode, so a client that merely asked for a field it never negotiated
    // loses that one frame and nothing else.
    let still_working: UserProfile = session
        .ask(
            Opcode::ProfileUpdate,
            11,
            &ProfileUpdate {
                display_name: Some("Dave Kota".to_string()),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(still_working.display_name, "Dave Kota");
    assert_eq!(
        still_working.custom_status, None,
        "the refused field was never written"
    );
}

/// The PRESENCE bit's own surface, end to end: a session that did not ask for the
/// bit does not receive the family's frames (brief section 72 — the server must not
/// send a frame for a feature the client did not advertise).
///
/// One account watches a friend from two devices at once: a session whose HELLO
/// asked for PRESENCE, and a session whose HELLO asked for nothing. Both subscribe
/// to the friend's user topic, the friend changes state, and the two halves of the
/// intersection see the one fan-out differently — the bitted session is handed the
/// PRESENCE_EVENT, the bitless session is handed nothing. The bitless half is read
/// through a PING: the fan-out has already been delivered to its twin when the PING
/// is sent, and replies share the session's one outbound queue with broadcasts, so
/// any presence frame the gate failed to withhold would be sitting in front of the
/// PONG. Reading to the PONG and finding no PRESENCE_EVENT is therefore a
/// deterministic negative, not a race won by timing.
#[tokio::test]
async fn a_presence_frame_is_withheld_from_a_session_that_did_not_ask_for_the_bit() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let author_grant = registered_grant(&app, "gina").await;
    let watcher_grant = registered_grant(&app, "hank").await;
    let other_device_grant = second_device_grant(&app, "hank").await;

    // The author changes state later; the watcher listens from two sessions of one
    // account, one per side of the bit. Every session takes the 1500ms spacing the
    // constructor gives the shared pre-auth bucket.
    let mut author =
        LiveSession::connect_with_features(addr, &author_grant, migo_protocol::features::PRESENCE)
            .await;
    let mut watching =
        LiveSession::connect_with_features(addr, &watcher_grant, migo_protocol::features::PRESENCE)
            .await;
    let mut bitless = LiveSession::connect_with_features(addr, &other_device_grant, 0).await;

    // Friendship, by crossing requests: the peer's user topic is only served to a
    // friend, so the subscriptions below need the relationship settled first.
    author.friend_request(10, watcher_grant.account_id).await;
    watching.friend_request(11, author_grant.account_id).await;

    // Both of the watcher's sessions subscribe to the author's topic — the
    // subscription itself is not the feature; the frames it carries are.
    watching
        .subscribe_to_user(20, author_grant.account_id)
        .await;
    bitless.subscribe_to_user(21, author_grant.account_id).await;

    // The author goes Away. Online would collide with the state session-start
    // already stamped, and an unchanged state publishes nothing — Away is a change
    // the fan-out cannot skip.
    let acknowledged: Acknowledged = author
        .ask(
            Opcode::PresenceSet,
            30,
            &PresenceUpdate {
                state: PresenceState::Away,
                custom_status: None,
            },
        )
        .await;
    assert!(acknowledged.ok, "the author's own session holds the bit");

    // The bitted session is handed the event. Presence frames about the watcher's
    // own account may have arrived earlier — the other device's session-start — so
    // the wait is for the author's event in particular, and the state it carries is
    // part of the assertion.
    let event = loop {
        let frame = recv_within(&mut watching.stream, STEP).await;
        if Opcode::from_wire(frame.header.opcode) == Some(Opcode::PresenceEvent) {
            let event: PresenceEvent = from_frame(&frame).expect("the event decodes");
            if event.user_id == author_grant.account_id {
                break event;
            }
        }
    };
    assert_eq!(
        event.state,
        PresenceState::Away,
        "the bitted session sees the state change it was promised"
    );

    // The bitless session: a PING, and a read to its PONG. The fan-out above has
    // already been delivered to the bitted twin, so the withheld frame — if the
    // gate had leaked it — is queued ahead of the reply this PING earns. Finding
    // the PONG without a PRESENCE_EVENT in front of it is the assertion; the
    // session being alive to answer at all is the second one.
    send(
        &mut bitless.stream,
        Opcode::Ping,
        31,
        &migo_protocol::Ping {
            client_time: migo_core::Timestamp::from_millis(0),
        },
    )
    .await;
    let pong = loop {
        let frame = recv_within(&mut bitless.stream, STEP).await;
        assert_ne!(
            Opcode::from_wire(frame.header.opcode),
            Some(Opcode::PresenceEvent),
            "a session without the PRESENCE bit is never handed a presence frame"
        );
        if frame.header.correlation == 31 {
            break frame;
        }
    };
    assert!(
        !pong.header.is_error(),
        "the bitless session keeps serving the frames it did negotiate"
    );
}
