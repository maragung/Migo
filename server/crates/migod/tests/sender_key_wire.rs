//! Section 163 answered on the wire: the frames a group's key redistribution
//! rides when its membership moves, and the frames a mid-call joiner's key
//! request rides.
//!
//! The server's role in both is routing, never keys: it cannot read what it
//! relays, so what these tests pin is that the *paths* exist and stay sealed —
//! that a membership change stamps the generation on the member event, that
//! the distribution relay carries a real AEAD envelope to the member it names
//! on that member's own user topic, that a distribution aimed at somebody a
//! removal already dropped is refused, and that a mid-call joiner has a
//! request path to the call's key holder and a sealed reply path back.
//!
//! * **The trigger is visible on the wire.** The kick's member event carries
//!   `group_key_epoch: 2` — the number a client that missed the
//!   redistribution compares against the generation its keys carry.
//! * **The relay carries sealed bytes it cannot name.** Each distribution is
//!   a real sender-key distribution, sealed under the pairwise session the
//!   target device shares with the distributor; the frame arrives byte for
//!   byte on the target's user topic and opens under that session and no
//!   other.
//! * **Adoption obeys the crypto's rules, not the server's.** A receiver
//!   adopts the post-rotation distribution, refuses the stale pre-rotation
//!   one with `KeyAlreadyUsed`, and decrypts a message sealed under the new
//!   chain.
//! * **The departed hold nothing further.** A distribution aimed at the
//!   removed member is refused with `PERMISSION_DENIED` — the wire's whole
//!   contribution, because the server cannot unseal what it refuses.
//! * **A mid-call joiner has something to talk to.** The joiner's sealed
//!   request rides the addressed roster relay as a `CALL_RENEGOTIATE`, the
//!   holder's sealed reply rides it as a `CALL_SDP`, and the joiner installs
//!   the current epoch from the reply: current media opens, pre-join media
//!   does not, and a session the reply was not sealed for cannot open it.
//!
//! Every frame waited for is one the server owes somebody, so the timeout is
//! the assertion — the same clock the SFU wire tests run on.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Clock, Config, Id, SeededRandom};
use migo_crypto::aead::{self, SymmetricKey};
use migo_crypto::kdf;
use migo_crypto::{
    CallKeyState, CryptoError, IdentityPublic, IdentitySecret, ReceiverKeyState, SenderKeyHeader,
    SenderKeyMessage, SenderKeyState,
};
use migo_protocol::{
    from_frame, to_frame, Acknowledged, CallInvite, CallRenegotiate, CallSdp,
    ConversationCreateRequest, ConversationKickRequest, ConversationKind, ConversationMemberEvent,
    Encode, Frame, GroupKeyDistribution, Hello, MemberChange, MessageEvent, MessageKind,
    MessageSend, Opcode, Platform, SubscribeRequest, SubscribeResponse, Topic, TopicKind, Welcome,
    PROTOCOL_VERSION,
};
use migod::App;

/// How long any single exchange may take before the test declares the server
/// stuck — silence being the bug class these tests exist to catch.
const STEP: Duration = Duration::from_secs(5);

/// A distribution on the wire: 4 + 4 + 4 + 32 + 64 bytes.
const DISTRIBUTION_LEN: usize = 44 + 64;

/// A sender-key message on the wire: header, length-prefixed ciphertext,
/// signature.
const SIGNATURE_LEN: usize = 64;

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development app with the TCP listener bound, as the listener tests build it.
async fn build_app() -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
        ],
    )
    .expect("configuration should parse");
    App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends")
}

/// Registers one account through the front door, stamped with the node's own clock.
async fn registered_grant(app: &App, username: &str) -> Grant {
    app.auth
        .register(
            Registration {
                username: username.to_string(),
                email: None,
                phone: None,
                passphrase: migo_core::Secret::new("correct-horse-battery-staple"),
                locale: "en-US".to_string(),
                country: None,
                gender: None,
                device: DeviceClaim::new(Platform::Web, "sender key wire test"),
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

/// A live authenticated TCP session on its own user topic, plus whichever
/// conversation topics the test subscribes it to.
struct LiveSession {
    stream: tokio::net::TcpStream,
}

impl LiveSession {
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        // A third handshake from one IP inside the limiter's window is refused
        // with a retry-after — the server talking, not the server broken —
        // so the session waits exactly as long as it is told and tries again,
        // the way a client with manners does.
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
            access_token: Some(grant.access_token.clone()),
            device_id: Some(grant.device_id),
            ..Default::default()
        };
        send(&mut stream, Opcode::Hello, 1, &hello).await;

        let welcome_frame = recv_within(&mut stream, STEP).await;
        if Opcode::from_wire(welcome_frame.header.opcode) == Some(Opcode::Hello)
            && !welcome_frame.header.is_error()
        {
            let welcome: Welcome = from_frame(&welcome_frame).expect("the WELCOME decodes");
            assert_eq!(welcome.authenticated_user, Some(grant.account_id));
        } else {
            // Any other answer is the server refusing the session; only a
            // rate-limit refusal is worth retrying, because only it names a
            // time at which the answer changes.
            let refusal: migo_protocol::Error =
                from_frame(&welcome_frame).expect("the refusal decodes");
            assert!(
                refusal.code == migo_protocol::codes::RATE_LIMITED,
                "the handshake is refused outright: {refusal:?}"
            );
            return Err(u64::from(refusal.retry_after_ms.unwrap_or(1000)));
        }

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
                return Ok(Self { stream });
            }
        }
    }

    /// Subscribes to a conversation's topic, as a member's client does once
    /// it has loaded the conversation.
    async fn subscribe_conversation(&mut self, conversation: Id, correlation: u32) {
        send(
            &mut self.stream,
            Opcode::Subscribe,
            correlation,
            &SubscribeRequest {
                topics: vec![Topic {
                    kind: TopicKind::Conversation,
                    id: conversation,
                }],
            },
        )
        .await;
        loop {
            let frame = recv_within(&mut self.stream, STEP).await;
            if frame.header.correlation == correlation {
                assert!(
                    !frame.header.is_error(),
                    "the conversation subscription is accepted: {:?}",
                    from_frame::<migo_protocol::Error>(&frame)
                );
                return;
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

    /// Sends a request that must fail, returning the error frame.
    async fn ask_error<M: Encode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        message: &M,
    ) -> migo_protocol::Error {
        send(&mut self.stream, opcode, correlation, message).await;
        loop {
            let frame = recv_within(&mut self.stream, STEP).await;
            if frame.header.correlation == correlation {
                assert!(frame.header.is_error(), "the request was expected to fail");
                return from_frame(&frame).expect("the error decodes");
            }
        }
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

/// The join frame a scripted client sends: the `CallInvite` shape the registry
/// froze for the opcode, with group semantics — the roster is the audience,
/// so `callee_id` is a placeholder the server never reads.
fn sfu_join(call_id: Id, conversation_id: Id) -> CallInvite {
    CallInvite {
        call_id,
        conversation_id,
        callee_id: Id::from(0u128),
        media_kind: 0,
        caller_device: Id::from(0u128),
        capabilities: 0,
        sealed_offer: b"sealed-group-offer".to_vec(),
    }
}

// --- the wire shape of a sender-key distribution, as this test's clients encode it ---

/// Encodes a distribution: epoch, chain, position, the chain key, the sender's
/// identity. There is no canonical client encoder in `migo-crypto` — each
/// client owns its own — so this test carries one, and what it pins is that
/// the server relays the bytes without ever needing to read them.
fn distribution_bytes(distribution: &migo_crypto::SenderKeyDistribution) -> Vec<u8> {
    let mut out = Vec::with_capacity(DISTRIBUTION_LEN);
    out.extend_from_slice(&distribution.group_key_epoch.to_be_bytes());
    out.extend_from_slice(&distribution.chain_id.to_be_bytes());
    out.extend_from_slice(&distribution.message_number.to_be_bytes());
    out.extend_from_slice(&distribution.chain_key);
    out.extend_from_slice(&distribution.identity.to_bytes());
    out
}

/// Decodes a distribution back, or names the byte that is wrong.
fn parse_distribution(bytes: &[u8]) -> migo_crypto::SenderKeyDistribution {
    assert_eq!(
        bytes.len(),
        DISTRIBUTION_LEN,
        "a distribution is {DISTRIBUTION_LEN} bytes"
    );
    migo_crypto::SenderKeyDistribution {
        group_key_epoch: u32::from_be_bytes(bytes[0..4].try_into().expect("four bytes")),
        chain_id: u32::from_be_bytes(bytes[4..8].try_into().expect("four bytes")),
        message_number: u32::from_be_bytes(bytes[8..12].try_into().expect("four bytes")),
        chain_key: bytes[12..44].try_into().expect("thirty-two bytes"),
        identity: IdentityPublic::parse(&bytes[44..]).expect("the identity parses"),
    }
}

/// Seals a distribution for one member device's pairwise session with the
/// distributor, bound to the conversation it belongs to.
fn seal_distribution(
    session_secret: [u8; 32],
    conversation_id: Id,
    distribution: &[u8],
    random: &mut SeededRandom,
) -> Vec<u8> {
    aead::seal(
        &SymmetricKey::from_bytes(session_secret),
        conversation_id.as_bytes(),
        distribution,
        random,
    )
    .expect("the distribution seals")
}

/// Opens a distribution under one member device's pairwise session — and no
/// other, which the assertions below rely on.
fn open_distribution(session_secret: [u8; 32], conversation_id: Id, sealed: &[u8]) -> Vec<u8> {
    aead::open(
        &SymmetricKey::from_bytes(session_secret),
        conversation_id.as_bytes(),
        sealed,
    )
    .expect("the member's own session opens their distribution")
}

/// Encodes a sender-key message for a `MESSAGE_SEND` envelope.
fn message_bytes(message: &SenderKeyMessage) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 4 + message.ciphertext.len() + SIGNATURE_LEN);
    out.extend_from_slice(&message.header.to_bytes());
    out.extend_from_slice(&(message.ciphertext.len() as u32).to_be_bytes());
    out.extend_from_slice(&message.ciphertext);
    out.extend_from_slice(&message.signature);
    out
}

/// Decodes a sender-key message from a `MESSAGE_SEND` envelope.
fn parse_message(bytes: &[u8]) -> SenderKeyMessage {
    let header = SenderKeyHeader::parse(&bytes[..8]).expect("the header parses");
    let len = u32::from_be_bytes(bytes[8..12].try_into().expect("four bytes")) as usize;
    assert_eq!(
        bytes.len(),
        12 + len + SIGNATURE_LEN,
        "the envelope is header, length-prefixed ciphertext, signature"
    );
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(&bytes[12 + len..]);
    SenderKeyMessage {
        header,
        ciphertext: bytes[12..12 + len].to_vec(),
        signature,
    }
}

#[tokio::test]
async fn a_membership_change_carries_a_new_distribution_to_every_remaining_member() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let founder = registered_grant(&app, "skwfounder").await;
    let kept = registered_grant(&app, "skwkept").await;
    let removed = registered_grant(&app, "skwremoved").await;

    let mut founder_session = LiveSession::connect(addr, &founder).await;
    let mut kept_session = LiveSession::connect(addr, &kept).await;
    let mut removed_session = LiveSession::connect(addr, &removed).await;

    let summary: migo_protocol::ConversationSummary = founder_session
        .ask(
            Opcode::ConversationCreate,
            11,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![kept.account_id, removed.account_id],
                title: Some("The Redistribution Group".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    founder_session
        .subscribe_conversation(conversation_id, 12)
        .await;
    kept_session
        .subscribe_conversation(conversation_id, 13)
        .await;
    removed_session
        .subscribe_conversation(conversation_id, 14)
        .await;

    // The founder's sending half: an identity, and the epoch-1 chain the
    // group starts under. The pairwise sessions are the ones each member
    // device shares with the founder — different secrets per device, which is
    // what seals each member's copy for them and nobody else, this node
    // included.
    let founder_identity = IdentitySecret::from_seeds([0x11; 32], [0x12; 32]);
    let founder_kept_secret = [0x0au8; 32];
    let founder_removed_secret = [0x0bu8; 32];
    let mut random = SeededRandom::new(0x1631);
    let mut sender = SenderKeyState::create(1, 1, &mut random);

    // The first distribution rides the dedicated relay: one frame per member
    // device, sealed under that device's own session with the distributor.
    let first = distribution_bytes(&sender.distribution(&founder_identity));
    let ack: Acknowledged = founder_session
        .ask(
            Opcode::GroupKeyDistribute,
            15,
            &GroupKeyDistribution {
                conversation_id,
                from_device: founder.device_id,
                to_account: kept.account_id,
                to_device: kept.device_id,
                sealed_distribution: seal_distribution(
                    founder_kept_secret,
                    conversation_id,
                    &first,
                    &mut random,
                ),
            },
        )
        .await;
    assert!(
        ack.ok,
        "the first distribution to the kept member is relayed"
    );
    let ack: Acknowledged = founder_session
        .ask(
            Opcode::GroupKeyDistribute,
            16,
            &GroupKeyDistribution {
                conversation_id,
                from_device: founder.device_id,
                to_account: removed.account_id,
                to_device: removed.device_id,
                sealed_distribution: seal_distribution(
                    founder_removed_secret,
                    conversation_id,
                    &first,
                    &mut random,
                ),
            },
        )
        .await;
    assert!(
        ack.ok,
        "the first distribution to the removed member is relayed"
    );

    // Each member receives theirs on their own user topic, byte for byte, and
    // adopts the baseline: the first distribution is the receiver's starting
    // state, exactly as the first pairwise ratchet is.
    let frame = next_event_of(&mut kept_session.stream, Opcode::GroupKeyDistribute).await;
    let heard: GroupKeyDistribution = from_frame(&frame).expect("the relayed distribution decodes");
    assert_eq!(heard.conversation_id, conversation_id);
    assert_eq!(heard.to_device, kept.device_id);
    assert_eq!(heard.from_device, founder.device_id);
    let mut kept_receiver = ReceiverKeyState::accept(&parse_distribution(&open_distribution(
        founder_kept_secret,
        conversation_id,
        &heard.sealed_distribution,
    )));
    assert_eq!(kept_receiver.group_key_epoch(), 1);

    let frame = next_event_of(&mut removed_session.stream, Opcode::GroupKeyDistribute).await;
    let heard: GroupKeyDistribution = from_frame(&frame).expect("the relayed distribution decodes");
    let _removed_receiver = ReceiverKeyState::accept(&parse_distribution(&open_distribution(
        founder_removed_secret,
        conversation_id,
        &heard.sealed_distribution,
    )));

    // A group message under the epoch-1 chain, so the chain has something to
    // lose when the membership moves.
    let epoch_one_message = sender
        .encrypt(
            &founder_identity,
            conversation_id.as_bytes(),
            b"sealed under the first chain",
        )
        .expect("the first chain seals");
    let _: migo_protocol::MessageAccepted = founder_session
        .ask(
            Opcode::MessageSend,
            17,
            &MessageSend {
                message_id: Id::from(0x7001u128),
                conversation_id,
                kind: MessageKind::Text,
                envelope: message_bytes(&epoch_one_message),
                ..Default::default()
            },
        )
        .await;
    let frame = next_event_of(&mut kept_session.stream, Opcode::MessageEvent).await;
    let event: MessageEvent = from_frame(&frame).expect("the message event decodes");
    assert_eq!(
        kept_receiver
            .decrypt(conversation_id.as_bytes(), &parse_message(&event.envelope))
            .expect("the kept member opens the first chain's message"),
        b"sealed under the first chain"
    );

    // The membership change: the founder removes one member. The member event
    // on the conversation topic carries the generation this change produced —
    // the trigger, visible on the wire, that tells every remaining client a
    // redistribution is owed.
    let _: Acknowledged = founder_session
        .ask(
            Opcode::ConversationKick,
            18,
            &ConversationKickRequest {
                conversation_id,
                target_id: removed.account_id,
            },
        )
        .await;
    let frame = next_event_of(&mut kept_session.stream, Opcode::ConversationMemberEvent).await;
    let event: ConversationMemberEvent = from_frame(&frame).expect("the member event decodes");
    assert_eq!(event.change, MemberChange::Kicked);
    assert_eq!(event.user_id, removed.account_id);
    assert_eq!(
        event.group_key_epoch,
        Some(2),
        "the removal carries the membership generation on the wire"
    );

    // The redistribution: the founder rotates and pushes the epoch-2
    // distribution to every remaining member. One sealed copy per device,
    // because each device's pairwise session is its own.
    let epoch = sender.rotate(2, &mut random);
    assert_eq!(epoch, 2, "the rotation raises the epoch");
    let second = distribution_bytes(&sender.distribution(&founder_identity));
    let ack: Acknowledged = founder_session
        .ask(
            Opcode::GroupKeyDistribute,
            19,
            &GroupKeyDistribution {
                conversation_id,
                from_device: founder.device_id,
                to_account: kept.account_id,
                to_device: kept.device_id,
                sealed_distribution: seal_distribution(
                    founder_kept_secret,
                    conversation_id,
                    &second,
                    &mut random,
                ),
            },
        )
        .await;
    assert!(ack.ok, "the redistribution to the kept member is relayed");

    let frame = next_event_of(&mut kept_session.stream, Opcode::GroupKeyDistribute).await;
    let heard: GroupKeyDistribution = from_frame(&frame).expect("the relayed distribution decodes");
    kept_receiver
        .adopt(&parse_distribution(&open_distribution(
            founder_kept_secret,
            conversation_id,
            &heard.sealed_distribution,
        )))
        .expect("the new distribution installs");
    assert_eq!(kept_receiver.group_key_epoch(), 2);

    // A stale distribution is refused by the receiver's own rules, not by the
    // server: the epoch-1 copy re-sent cannot strand a peer on a dead chain.
    assert_eq!(
        kept_receiver.adopt(&parse_distribution(&first)),
        Err(CryptoError::KeyAlreadyUsed),
        "a stale distribution must not install"
    );

    // A message under the new chain opens with the adopted state — the whole
    // point of the redistribution.
    let epoch_two_message = sender
        .encrypt(
            &founder_identity,
            conversation_id.as_bytes(),
            b"sealed under the rotated chain",
        )
        .expect("the rotated chain seals");
    let _: migo_protocol::MessageAccepted = founder_session
        .ask(
            Opcode::MessageSend,
            20,
            &MessageSend {
                message_id: Id::from(0x7002u128),
                conversation_id,
                kind: MessageKind::Text,
                envelope: message_bytes(&epoch_two_message),
                ..Default::default()
            },
        )
        .await;
    let frame = next_event_of(&mut kept_session.stream, Opcode::MessageEvent).await;
    let event: MessageEvent = from_frame(&frame).expect("the message event decodes");
    assert_eq!(
        kept_receiver
            .decrypt(conversation_id.as_bytes(), &parse_message(&event.envelope))
            .expect("the kept member opens the rotated chain's message"),
        b"sealed under the rotated chain"
    );

    // The removed member is owed nothing further, and the relay says so: a
    // distribution aimed at them is refused, sealed bytes unread. This is the
    // server's whole contribution to "the departed hold nothing further" — it
    // cannot unseal what it refused, but it can keep the wire from carrying it.
    let error = founder_session
        .ask_error(
            Opcode::GroupKeyDistribute,
            21,
            &GroupKeyDistribution {
                conversation_id,
                from_device: founder.device_id,
                to_account: removed.account_id,
                to_device: removed.device_id,
                sealed_distribution: seal_distribution(
                    founder_removed_secret,
                    conversation_id,
                    &second,
                    &mut random,
                ),
            },
        )
        .await;
    assert_eq!(error.code, migo_protocol::codes::PERMISSION_DENIED);
}

/// The key that wraps a joiner's frames for one call: the same derivation the
/// call key's own join distribution uses, so the request and the reply seal
/// under one purpose-built key per (session, call) pair.
fn join_key(session_secret: &[u8], call_id: Id) -> SymmetricKey {
    SymmetricKey::from_bytes(kdf::derive::<32>(
        session_secret,
        Some(call_id.as_bytes()),
        kdf::LABEL_CALL_JOIN,
    ))
}

#[tokio::test]
async fn a_mid_call_joiner_asks_the_holder_and_receives_the_key_sealed_for_them() {
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let holder = registered_grant(&app, "skwholder").await;
    let second = registered_grant(&app, "skwsecond").await;
    let joiner = registered_grant(&app, "skwjoiner").await;

    let mut holder_session = LiveSession::connect(addr, &holder).await;
    let mut second_session = LiveSession::connect(addr, &second).await;
    let mut joiner_session = LiveSession::connect(addr, &joiner).await;

    let summary: migo_protocol::ConversationSummary = holder_session
        .ask(
            Opcode::ConversationCreate,
            11,
            &ConversationCreateRequest {
                kind: ConversationKind::Group,
                members: vec![second.account_id, joiner.account_id],
                title: Some("The Mid-Call Group".to_string()),
            },
        )
        .await;
    let conversation_id = summary.conversation_id;

    holder_session
        .subscribe_conversation(conversation_id, 12)
        .await;
    second_session
        .subscribe_conversation(conversation_id, 13)
        .await;

    let call_id = Id::from(0x5f08u128);

    // The call's key as the holder's device holds it: derived from the pairwise
    // session with the seated participant, already rotated once, with media on
    // the wire under epoch 1. The joiner's pairwise session with the holder is
    // a *different* secret — that is what seals the joiner's request and reply
    // for them and nobody else, server included.
    let holder_second_secret = [0x0au8; 32];
    let holder_joiner_secret = [0x0bu8; 32];
    let mut random = SeededRandom::new(0x1632);
    let mut call_key = CallKeyState::from_session(&holder_second_secret, call_id);
    call_key.rotate(&mut random).expect("the call rotates");
    let pre_join = call_key
        .seal_frame(b"media from before the join", &mut random)
        .expect("seals");

    // The call in progress: two seats.
    let _: migo_protocol::CallTurnResponse = holder_session
        .ask(Opcode::CallSfuJoin, 14, &sfu_join(call_id, conversation_id))
        .await;
    let _ = next_event_of(&mut holder_session.stream, Opcode::CallSfuEvent).await;
    let _: migo_protocol::CallTurnResponse = second_session
        .ask(Opcode::CallSfuJoin, 15, &sfu_join(call_id, conversation_id))
        .await;
    let _ = next_event_of(&mut second_session.stream, Opcode::CallSfuEvent).await;
    let _ = next_event_of(&mut holder_session.stream, Opcode::CallSfuEvent).await;

    // The join, mid-call. The joiner's roster snapshot lands on their own user
    // topic; the holder hears the announcement on the conversation topic —
    // the announcement is the last fact the join produces, so waiting for it
    // is what makes the seat-recorded relays below deterministic.
    let _: migo_protocol::CallTurnResponse = joiner_session
        .ask(Opcode::CallSfuJoin, 16, &sfu_join(call_id, conversation_id))
        .await;
    let _ = next_event_of(&mut joiner_session.stream, Opcode::CallSfuEvent).await;
    let _ = next_event_of(&mut holder_session.stream, Opcode::CallSfuEvent).await;

    // The joiner's request: sealed under the joiner's own pairwise session
    // with the holder, bound to the call. It rides the addressed roster relay
    // the call already provides, in a `CALL_RENEGOTIATE`'s sealed payload —
    // from the server's side a key request is indistinguishable from a codec
    // renegotiation, which is the property that keeps the request's content
    // off the wire's metadata: a separate opcode would name it.
    let request = b"the current key of this call, please";
    let sealed_request = aead::seal(
        &join_key(&holder_joiner_secret, call_id),
        call_id.as_bytes(),
        request,
        &mut random,
    )
    .expect("the request seals");
    let ack: Acknowledged = joiner_session
        .ask(
            Opcode::CallRenegotiate,
            17,
            &CallRenegotiate {
                call_id,
                from_device: joiner.device_id,
                to_device: holder.device_id,
                sealed_sdp: sealed_request.clone(),
            },
        )
        .await;
    assert!(ack.ok, "the sealed request is relayed to the holder");

    // The holder receives the request unchanged — the group relay projects a
    // renegotiation to `CALL_SDP` for the target, as the registry freezes it —
    // and opens it with the session this holder shares with this joiner.
    let frame = next_event_of(&mut holder_session.stream, Opcode::CallSdp).await;
    let heard: CallSdp = from_frame(&frame).expect("the holder's request frame decodes");
    assert_eq!(
        heard.sealed_sdp, sealed_request,
        "the sealed request arrives byte for byte"
    );
    assert_eq!(heard.from_device, joiner.device_id);
    assert_eq!(
        aead::open(
            &join_key(&holder_joiner_secret, call_id),
            call_id.as_bytes(),
            &heard.sealed_sdp,
        )
        .expect("the holder's own session with the joiner opens the request"),
        request,
        "a key request is client semantics inside a sealed renegotiation"
    );

    // The holder rotates on the join — the joiner's first key must be one that
    // did not exist while they were outside the call — and seals the epoch-2
    // key under the joiner's own pairwise session. The reply rides the same
    // addressed relay, in a `CALL_SDP`'s sealed payload.
    call_key.rotate(&mut random).expect("the join rotates");
    let sealed_reply = call_key
        .sealed_join_distribution(&holder_joiner_secret, &mut random)
        .expect("the join distribution seals");
    let ack: Acknowledged = holder_session
        .ask(
            Opcode::CallSdp,
            18,
            &CallSdp {
                call_id,
                from_device: holder.device_id,
                to_device: joiner.device_id,
                sealed_sdp: sealed_reply.clone(),
            },
        )
        .await;
    assert!(ack.ok, "the sealed first key is relayed");

    // The joiner receives the bytes unchanged and installs the current epoch:
    // current media opens, pre-join media does not, and neither the seated
    // participant's session secret nor anyone else's opens the joiner's blob.
    let frame = next_event_of(&mut joiner_session.stream, Opcode::CallSdp).await;
    let heard: CallSdp = from_frame(&frame).expect("the joiner's key frame decodes");
    assert_eq!(
        heard.sealed_sdp, sealed_reply,
        "the sealed first key arrives byte for byte"
    );
    let joiner_key =
        CallKeyState::from_join_distribution(&holder_joiner_secret, call_id, &heard.sealed_sdp)
            .expect("the joiner's own session opens their first key");
    assert_eq!(joiner_key.epoch(), 2);
    let current = call_key
        .seal_frame(b"media after the join", &mut random)
        .expect("seals");
    assert_eq!(
        joiner_key
            .open_frame(&current)
            .expect("the joiner hears the call they joined"),
        b"media after the join"
    );
    assert_eq!(
        joiner_key.open_frame(&pre_join),
        Err(CryptoError::DecryptionFailed),
        "the joiner opened media from before they joined"
    );
    assert!(
        CallKeyState::from_join_distribution(&holder_second_secret, call_id, &heard.sealed_sdp)
            .is_err(),
        "a device with a different session opened the joiner's first key"
    );
    assert!(
        aead::open(
            &join_key(&holder_second_secret, call_id),
            call_id.as_bytes(),
            &sealed_request,
        )
        .is_err(),
        "a session other than the joiner's opened the joiner's request"
    );
}
