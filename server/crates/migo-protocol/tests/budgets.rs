//! The protocol budget gate (brief sections 56 and 171).
//!
//! The bandwidth targets in the brief are budgets, not hopes: a change that
//! exceeds one needs a justification in review. Until now that rule was
//! enforced by reading — the numbers were measurable from the encoder, but
//! nothing measured them. This file is the automation: every budget that can
//! be derived from the wire alone is measured here from the *real* encoder,
//! and a frame that outgrows its budget fails CI (`make budget-check`) with
//! the budget it broke named in the assertion message.
//!
//! Three things this gate deliberately is not:
//!
//! * It is not a runtime measurement. Bytes per session on a live node are
//!   the gateway metrics' job (section 171's other half, still SPEC).
//! * It is not compressed. Frames are measured as the encoder emits them;
//!   batching and compression only ever shrink what goes on the wire, so the
//!   uncompressed frame is the ceiling.
//! * It is not a property test. The values fed in are *typical* frames, not
//!   adversarial ones — the budget rule in the brief asks for "ukuran frame
//!   tipikalnya", and every sample value below says where it comes from.
//!
//! Where a budget in the brief could not be met by the frozen MWP/1 layout
//! (a 16-byte id is a required positional field and cannot shrink to fit a
//! 12-byte typing budget), the brief was updated to the measured truth and
//! the history of the old number is recorded in the brief itself, not here.

use migo_core::{Id, SeededRandom, Timestamp};
use migo_crypto::aead::{self, SymmetricKey, NONCE_LEN};
use migo_crypto::kdf;
use migo_protocol::{
    to_frame, Ack, Acknowledged, Authenticate, Authenticated, CallAnswer, CallIce, CallInvite,
    CallInviteResult, CallStateEvent, CallStats, ClientInfo, ConversationKind,
    ConversationListRequest, ConversationListResponse, ConversationSummary, EncryptionMode, FedAck,
    FedForward, Frame, Hello, Limits, MessageEvent, MessageKind, MessageReceipt, MessageSend,
    NodeInfo, Opcode, Ping, Pong, PresenceEvent, PresenceState, PresenceUpdate, ReceiptKind,
    ResumeRequest, RoomStateEvent, SyncResponse, SyncStatus, TypingEvent, TypingState, Welcome,
};

/// Migo-epoch milliseconds for a September 2026 instant: 6 varint bytes on the
/// wire, which is what every timestamp in these frames costs for years to come.
const NOW: Timestamp = Timestamp::from_millis(84_932_800_000);

/// The gateway heartbeat default (`gateway.heartbeat_ms`, migo-core config).
/// A client that honours it sends one PING per interval and eats one PONG.
const HEARTBEAT_MS: u64 = 30_000;

/// Access tokens are 174 characters of base64url text (migo-auth TOKEN_TEXT_LEN).
/// Any 174-character string measures identically; this one just reads like one.
const TOKEN_LEN: usize = 174;

/// A voice-note waveform is 64 buckets (brief section 179) — one byte per
/// bucket, so the waveform is 64 bytes inside the sealed envelope.
const WAVEFORM_BUCKETS: usize = 64;

// Sample media sizes for the call-signaling budget. These are the honest
// judgment calls in this file, so they are named and justified: a browser's
// audio-plus-video offer with trickle ICE lands around 1.5–2 KB, the answer
// around 1 KB, and a trickled batch of candidates around 250 bytes. The
// sealed forms carry a 24-byte nonce and a 16-byte tag on top (migo-crypto
// XChaCha20-Poly1305). The 8 KB signaling budget has room for roughly double
// these sizes, which is the headroom a renegotiation or an ICE restart needs.
const SAMPLE_SDP_OFFER_LEN: usize = 1600;
const SAMPLE_SDP_ANSWER_LEN: usize = 1200;
const SAMPLE_ICE_BATCH_LEN: usize = 250;
const ICE_BATCHES_PER_DIRECTION: usize = 4;

/// Encodes one frame the way the WebSocket transport sends it: header plus
/// payload, no length prefix (the TCP record adds 4 bytes, which every budget
/// here already has room for).
fn frame_size<T: migo_protocol::Encode>(opcode: Opcode, correlation: u32, value: &T) -> usize {
    let frame: Frame = to_frame(opcode.to_wire(), correlation, value)
        .unwrap_or_else(|e| panic!("{opcode:?} must encode: {e}"));
    frame
        .encode()
        .unwrap_or_else(|e| panic!("{opcode:?} frame must encode: {e}"))
        .len()
}

/// Seals with a nonce derived from the key, returning only the wire half.
///
/// The double ratchet derives its nonce from the message key, which is why the
/// envelope layout in the brief has no nonce field: both sides re-derive it.
/// `aead::seal_with_nonce` returns `nonce || ciphertext || tag`; the envelope
/// carries the last two, so the nonce is drained here rather than transmitted.
fn seal_with_derived_nonce(
    key: &SymmetricKey,
    associated_data: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    let nonce = kdf::derive::<NONCE_LEN>(key.expose(), None, b"budget gate nonce");
    let mut sealed = aead::seal_with_nonce(key, &nonce, associated_data, plaintext)
        .unwrap_or_else(|e| panic!("sealing with a valid key cannot fail: {e}"));
    let wire_only = sealed.split_off(NONCE_LEN);
    wire_only
}

/// Builds the 1-on-1 cryptographic envelope exactly as the brief's E2E MESSAGE
/// FORMAT section lays it out: version, scheme, sender_key_id, ratchet public
/// key, message counter, previous chain length, then ciphertext and tag. The
/// AEAD inside is real; only the ratchet state around it is frozen at "message
/// 5 of the first chain", which is a typical mid-conversation position.
fn one_on_one_envelope(key: &SymmetricKey, associated_data: &[u8], plaintext: &[u8]) -> Vec<u8> {
    use migo_wire::varint;

    let mut ratchet_key = [0u8; 32];
    ratchet_key.copy_from_slice(&kdf::derive::<32>(
        key.expose(),
        None,
        b"budget gate ratchet",
    ));
    let sealed = seal_with_derived_nonce(key, associated_data, plaintext);

    let mut out = Vec::with_capacity(37 + sealed.len());
    out.push(1); // envelope_version
    out.push(1); // scheme: Double Ratchet 1-on-1
    varint::encode_u64(3, &mut out); // sender_key_id
    out.extend_from_slice(&ratchet_key);
    varint::encode_u64(5, &mut out); // message_counter
    varint::encode_u64(0, &mut out); // previous_chain_length
    out.extend_from_slice(&sealed);
    out
}

/// Sample bytes that are not compressible by accident (a run of one byte would
/// deflate; these do not), for sealed blobs whose content is sample data.
fn sample_bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// The plaintext of a text message: content_type, then the body's varint
/// length and the body itself.
fn text_plaintext(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 2);
    out.push(1); // content_type: Text
    out.push(len as u8);
    out.extend(sample_bytes(len));
    out
}

/// A fresh identity and key material for one test. Seeded, so a failing budget
/// reproduces on every run — a flaky size gate would be worse than none.
struct Fixture {
    user: Id,
    peer: Id,
    device: Id,
    conversation: Id,
    key: SymmetricKey,
}

impl Fixture {
    fn new(seed: u64) -> Self {
        let mut rng = SeededRandom::new(seed);
        let unix_ms = u64::try_from(NOW.as_unix_ms()).unwrap_or(0);
        Self {
            user: Id::generate(unix_ms, &mut rng),
            peer: Id::generate(unix_ms, &mut rng),
            device: Id::generate(unix_ms, &mut rng),
            conversation: Id::generate(unix_ms, &mut rng),
            key: SymmetricKey::generate(&mut rng),
        }
    }

    /// A HELLO the web client actually sends: platform, version, the feature
    /// bits a web session negotiates, locale, bandwidth mode, and the inline
    /// token plus device that authenticate in one round trip.
    fn hello(&self, resume: Option<ResumeRequest>) -> Hello {
        Hello {
            protocol_version: 1,
            client: ClientInfo {
                platform: migo_protocol::Platform::Web,
                app_version: "0.24.8".to_string(),
                os_version: None,
                device_model: None,
            },
            features: 0xFF,
            locale: "en-US".to_string(),
            bandwidth_mode: migo_protocol::BandwidthMode::Normal,
            access_token: Some("A".repeat(TOKEN_LEN)),
            device_id: Some(self.device),
            resume,
        }
    }

    /// The WELCOME the gateway answers with: node identity from the example
    /// config ("migo-local-1"/"local"/"ID"), the negotiated feature cut, the
    /// limits it actually sends, and the authenticated user on the inline path.
    fn welcome(&self, resumed: Option<bool>, resume_from_seq: Option<u64>) -> Welcome {
        Welcome {
            session_id: self.conversation,
            node: NodeInfo {
                node_id: "migo-local-1".to_string(),
                region: "local".to_string(),
                country: "ID".to_string(),
            },
            features: 0xFF,
            server_time: NOW,
            limits: Limits {
                max_frame_bytes: 1_048_576,
                max_batch_items: 256,
                max_subscriptions: 512,
                heartbeat_ms: u32::try_from(HEARTBEAT_MS).unwrap_or(30_000),
            },
            resumed,
            resume_from_seq,
            authenticated_user: Some(self.user),
        }
    }
}

#[test]
fn per_event_frames_fit_their_budgets() {
    let f = Fixture::new(1);

    // PING/Pong: 6-byte budget in the brief assumed a bare header; a Migo
    // timestamp is a 6-byte varint, so the brief now budgets 16 and 24. A PONG
    // travels on the PING opcode as its correlated reply — there is no separate
    // PONG opcode — so the two frames differ only in what they carry.
    let ping = frame_size(Opcode::Ping, 1, &Ping { client_time: NOW });
    assert!(ping <= 16, "PING is {ping} bytes, budget 16 (section 56)");
    let pong = frame_size(
        Opcode::Ping,
        1,
        &Pong {
            client_time: NOW,
            server_time: NOW,
        },
    );
    assert!(pong <= 24, "Pong is {pong} bytes, budget 24 (section 56)");

    // ACK and Acknowledged: the cumulative watermark, one frame retiring
    // hundreds. seq mid-five-figures is a long-lived session's watermark.
    let ack = frame_size(Opcode::Ack, 1, &Ack { frame_seq: 5000 });
    assert!(ack <= 10, "ACK is {ack} bytes, budget 10 (section 56)");
    let acknowledged = frame_size(Opcode::MessageEdit, 1, &Acknowledged { ok: true });
    assert!(
        acknowledged <= 10,
        "Acknowledged is {acknowledged} bytes, budget 10 (section 56)"
    );

    // Message receipt as the client sends it: conversation, kind, watermark
    // seq, nothing else. 24 bytes is exactly this frame — the budget has no
    // headroom by design, because the optional user id belongs to the
    // fan-out form below.
    let receipt = frame_size(
        Opcode::MessageReceipt,
        1,
        &MessageReceipt {
            conversation_id: f.conversation,
            kind: ReceiptKind::Read,
            seq: 10_000,
            user_id: None,
            at: None,
        },
    );
    assert!(
        receipt <= 24,
        "watermark receipt is {receipt} bytes, budget 24 (section 56)"
    );

    // Typing as published to the other devices: the conversation id is a
    // required positional field, so the old 12-byte budget was unmeetable;
    // the brief now budgets 48 for the full fan-out form.
    let typing = frame_size(
        Opcode::Typing,
        1,
        &TypingEvent {
            conversation_id: f.conversation,
            state: TypingState::Stop,
            user_id: Some(f.user),
        },
    );
    assert!(
        typing <= 48,
        "typing fan-out is {typing} bytes, budget 48 (section 56)"
    );

    // Presence: the client's own update is two payload bytes; the fan-out
    // event carries the user's 16-byte id, so its budget is 32.
    let presence_update = frame_size(
        Opcode::PresenceSet,
        1,
        &PresenceUpdate {
            state: PresenceState::Online,
            custom_status: None,
        },
    );
    assert!(
        presence_update <= 16,
        "presence update is {presence_update} bytes, budget 16 (section 56)"
    );
    let presence_event = frame_size(
        Opcode::PresenceEvent,
        1,
        &PresenceEvent {
            user_id: f.user,
            state: PresenceState::Online,
            custom_status: None,
            last_seen: None,
        },
    );
    assert!(
        presence_event <= 32,
        "presence fan-out is {presence_event} bytes, budget 32 (section 56)"
    );

    // Room member count as the coalesced delta: RoomStateEvent with only the
    // count optional set. The room id is required, hence 32 and not the
    // brief's original 10.
    let member_count = frame_size(
        Opcode::RoomStateEvent,
        1,
        &RoomStateEvent {
            room_id: f.conversation,
            online_count: None,
            member_count: Some(1234),
            topic: None,
            slow_mode_ms: None,
            max_members: None,
        },
    );
    assert!(
        member_count <= 32,
        "member-count delta is {member_count} bytes, budget 32 (section 56)"
    );

    // Sync response header: an empty first page. Everything past these bytes
    // is message content, which is not the header's to pay for.
    let sync_header = frame_size(
        Opcode::Sync,
        1,
        &SyncResponse {
            conversation_id: f.conversation,
            status: SyncStatus::Ok,
            from_seq: 100,
            to_seq: 200,
            more: false,
            messages: Vec::new(),
        },
    );
    assert!(
        sync_header <= 32,
        "sync response header is {sync_header} bytes, budget 32 (section 56)"
    );

    println!(
        "per-event bytes: ping {ping}, pong {pong}, ack {ack}, acknowledged {acknowledged}, \
         receipt {receipt}, typing {typing}, presence {presence_update}/{presence_event}, \
         member count {member_count}, sync header {sync_header}"
    );
}

#[test]
fn text_message_overhead_stays_under_96_bytes() {
    let f = Fixture::new(2);

    // A 120-character message, the brief's reference size. The overhead is
    // everything the frame spends that is not ciphertext: frame header, the
    // two required ids, the kind, the envelope's own header fields, and the
    // authentication tag. The 96-byte budget has room for a reply_to optional
    // or a sender_key_id, not both — which is the budget working as intended.
    let body = text_plaintext(120);
    let envelope = one_on_one_envelope(&f.key, &[], &body);
    let sealed_len = envelope.len() - 37; // header fields ahead of the ciphertext
    let frame = frame_size(
        Opcode::MessageSend,
        1,
        &MessageSend {
            message_id: f.user,
            conversation_id: f.conversation,
            kind: MessageKind::Text,
            envelope,
            reply_to: None,
            expires_in_ms: None,
            sender_key_id: None,
        },
    );
    let overhead = frame - sealed_len;
    assert!(
        overhead <= 96,
        "120-char text costs {overhead} bytes of overhead, budget 96 (section 56)"
    );
    println!("text message: {frame} bytes total, {overhead} overhead + {sealed_len} sealed");
}

#[test]
fn handshake_and_reconnect_fit_their_session_budgets() {
    let f = Fixture::new(3);

    // The inline-auth handshake: HELLO carries the token, WELCOME answers
    // authenticated. 512 bytes is the budget for the whole exchange.
    let hello = frame_size(Opcode::Hello, 1, &f.hello(None));
    let welcome = frame_size(Opcode::Hello, 1, &f.welcome(Some(true), None));
    let inline = hello + welcome;
    assert!(
        inline <= 512,
        "inline-auth handshake is {inline} bytes, budget 512 (section 56)"
    );

    // The legacy three-frame form still exists for clients that authenticate
    // separately; its token rides AUTHENTICATE instead, so it costs roughly
    // the same total. Measured, not gated separately — one form is the budget.
    let bare_hello = {
        let mut hello = f.hello(None);
        hello.access_token = None;
        hello.device_id = None;
        frame_size(Opcode::Hello, 1, &hello)
    };
    let authenticate = frame_size(
        Opcode::Authenticate,
        2,
        &Authenticate {
            access_token: "A".repeat(TOKEN_LEN),
            device_id: f.device,
        },
    );
    let authenticated = frame_size(
        Opcode::Authenticate,
        2,
        &Authenticated {
            user_id: f.user,
            device_id: f.device,
            capabilities: 0xFF,
            profile: None,
        },
    );
    let legacy = bare_hello + welcome + authenticate + authenticated;
    assert!(
        legacy <= 512,
        "legacy AUTHENTICATE handshake is {legacy} bytes, budget 512 (section 56)"
    );

    // Reconnect with resume and nothing missed: HELLO carries session, token,
    // device and resume; WELCOME answers with the resume point. 400 bytes.
    let resume = frame_size(
        Opcode::Hello,
        1,
        &f.hello(Some(ResumeRequest {
            session_id: f.conversation,
            last_frame_seq: 1200,
        })),
    );
    let resumed_welcome = frame_size(Opcode::Hello, 1, &f.welcome(Some(true), Some(1200)));
    let reconnect = resume + resumed_welcome;
    assert!(
        reconnect <= 400,
        "resume reconnect is {reconnect} bytes, budget 400 (section 56)"
    );

    println!(
        "sessions: inline handshake {inline}, legacy handshake {legacy}, reconnect {reconnect}"
    );
}

#[test]
fn cold_start_and_idle_hour_fit_their_session_budgets() {
    let f = Fixture::new(4);

    // Cold start: the chat list the first screen renders — twenty
    // conversations, each a summary with a title, a two-member roster and a
    // last-message preview sealed under a typical short envelope. The profile
    // and unread frames ride the same batched window with the remaining
    // headroom; the list is the part whose size can regress silently.
    let mut conversations = Vec::new();
    for index in 0..20u32 {
        let preview = MessageEvent {
            message_id: f.user,
            conversation_id: f.conversation,
            seq: u64::from(500 - index),
            sender_id: f.peer,
            sender_device: f.device,
            kind: MessageKind::Text,
            envelope: one_on_one_envelope(&f.key, &[], &text_plaintext(100)),
            created_at: NOW,
            reply_to: None,
            edited_at: None,
            deleted: None,
            sender_key_id: None,
        };
        conversations.push(ConversationSummary {
            conversation_id: f.conversation,
            kind: ConversationKind::Direct,
            encryption: EncryptionMode::EndToEnd,
            last_seq: 500 - u64::from(index),
            read_seq: 490,
            title: Some("project chat".to_string()),
            avatar_url: None,
            members: Some(vec![f.user, f.peer]),
            last_message: Some(preview),
            muted_until: None,
            pinned: Some(index % 4 == 0),
            archived: None,
        });
    }
    let welcome = frame_size(Opcode::Hello, 1, &f.welcome(Some(true), None));
    let list_request = frame_size(
        Opcode::ConversationList,
        1,
        &ConversationListRequest {
            limit: 20,
            cursor: None,
        },
    );
    let list_response = frame_size(
        Opcode::ConversationList,
        1,
        &ConversationListResponse {
            conversations,
            next_cursor: None,
        },
    );
    let cold_start = welcome + list_request + list_response;
    assert!(
        cold_start <= 24 * 1024,
        "cold start is {cold_start} bytes, budget 24 KB (section 56)"
    );

    // Idle hour: nothing but heartbeats at the server-dictated interval.
    // Both directions are the session's bytes to pay for.
    let ping = frame_size(Opcode::Ping, 1, &Ping { client_time: NOW });
    let pong = frame_size(
        Opcode::Ping,
        1,
        &Pong {
            client_time: NOW,
            server_time: NOW,
        },
    );
    let beats = 3_600_000 / HEARTBEAT_MS;
    let idle_hour = usize::try_from(beats).unwrap_or(0) * (ping + pong);
    assert!(
        idle_hour <= 8 * 1024,
        "an idle hour costs {idle_hour} bytes, budget 8 KB (section 56)"
    );

    println!(
        "sessions: cold start {cold_start} bytes, idle hour {idle_hour} bytes ({beats} beats)"
    );
}

#[test]
fn voice_note_protocol_overhead_fits_256_bytes() {
    let f = Fixture::new(5);

    // Everything the chat path spends to point at a voice note, outside the
    // audio bytes themselves: the MessageSend frame and the sealed envelope
    // carrying media id, media key, duration and the 64-bucket waveform. The
    // audio rides the HTTP media plane, which this budget does not cover.
    let mut body = Vec::with_capacity(120);
    body.extend_from_slice(f.conversation.as_bytes()); // media_id
    body.extend_from_slice(f.key.expose()); // media key
    body.extend_from_slice(&30000u64.to_le_bytes()); // duration_ms
    body.push(WAVEFORM_BUCKETS as u8); // waveform length varint
    body.extend_from_slice(&sample_bytes(WAVEFORM_BUCKETS));
    let mut plaintext = Vec::with_capacity(body.len() + 1);
    plaintext.push(4); // content_type: VoiceNoteRef
    plaintext.extend_from_slice(&body);

    let envelope = one_on_one_envelope(&f.key, &[], &plaintext);
    let frame = frame_size(
        Opcode::MessageSend,
        1,
        &MessageSend {
            message_id: f.user,
            conversation_id: f.conversation,
            kind: MessageKind::Voice,
            envelope,
            reply_to: None,
            expires_in_ms: None,
            sender_key_id: None,
        },
    );
    assert!(
        frame <= 256,
        "voice note costs {frame} bytes of protocol overhead, budget 256 (section 171)"
    );
    println!("voice note: {frame} bytes of protocol overhead");
}

#[test]
fn call_signaling_fits_8kb_and_batches_fit_1kb() {
    let f = Fixture::new(6);
    let mut rng = SeededRandom::new(60);
    let unix_ms = u64::try_from(NOW.as_unix_ms()).unwrap_or(0);
    let call_id = Id::generate(unix_ms, &mut rng);

    // Sealed media-plane blobs use the app-level seal, which transmits its
    // random nonce (24 bytes) alongside the ciphertext and tag.
    let sealed_offer = aead::seal(&f.key, &[], &sample_bytes(SAMPLE_SDP_OFFER_LEN), &mut rng)
        .unwrap_or_else(|e| panic!("sealing cannot fail: {e}"));
    let sealed_answer = aead::seal(&f.key, &[], &sample_bytes(SAMPLE_SDP_ANSWER_LEN), &mut rng)
        .unwrap_or_else(|e| panic!("sealing cannot fail: {e}"));
    let sealed_candidates = aead::seal(&f.key, &[], &sample_bytes(SAMPLE_ICE_BATCH_LEN), &mut rng)
        .unwrap_or_else(|e| panic!("sealing cannot fail: {e}"));

    let invite = frame_size(
        Opcode::CallInvite,
        1,
        &CallInvite {
            call_id,
            conversation_id: f.conversation,
            callee_id: f.peer,
            media_kind: 1,
            caller_device: f.device,
            capabilities: 0,
            sealed_offer,
        },
    );
    let invite_result = frame_size(
        Opcode::CallInvite,
        1,
        &CallInviteResult {
            call_id,
            status: 1,
            expires_at: NOW,
        },
    );
    let answer = frame_size(
        Opcode::CallAnswer,
        2,
        &CallAnswer {
            call_id,
            callee_device: f.device,
            sealed_answer,
        },
    );
    let answer_ack = frame_size(Opcode::CallAnswer, 2, &Acknowledged { ok: true });

    // One ICE batch, both directions, with its Acknowledged reply. The batch
    // budget is its own line in section 171.
    let ice = frame_size(
        Opcode::CallIce,
        3,
        &CallIce {
            call_id,
            from_device: f.device,
            to_device: f.peer,
            sealed_candidates,
        },
    );
    assert!(
        ice <= 1024,
        "an ICE batch is {ice} bytes, budget 1 KB (section 171)"
    );
    let ice_ack = frame_size(Opcode::CallIce, 3, &Acknowledged { ok: true });

    let connected = frame_size(
        Opcode::CallStateEvent,
        4,
        &CallStateEvent {
            call_id,
            state: 2, // Connected
            reason: None,
            conversation_id: Some(f.conversation),
            user_id: None,
            device_id: None,
            participant_count: None,
            sealed_offer: None,
            participants: None,
        },
    );

    let both_directions = 2 * ICE_BATCHES_PER_DIRECTION;
    let total = invite
        + invite_result
        + answer
        + answer_ack
        + both_directions * (ice + ice_ack)
        + connected;
    assert!(
        total <= 8 * 1024,
        "full call signaling is {total} bytes, budget 8 KB (section 171)"
    );

    // CALL_STATS is Droppable and priced at 128 bytes: five optional quality
    // numbers beside the call id.
    let stats = frame_size(
        Opcode::CallStats,
        5,
        &CallStats {
            call_id,
            setup_ms: Some(150),
            rtt_ms: Some(45),
            packet_loss: Some(120), // per-mille
            jitter_ms: Some(12),
            used_turn: Some(false),
        },
    );
    assert!(
        stats <= 128,
        "CALL_STATS is {stats} bytes, budget 128 (section 171)"
    );

    println!(
        "call: invite {invite} + result {invite_result} + answer {answer} + {both_directions} ice \
         batches {ice} each + connected {connected} = {total}; stats {stats}"
    );
}

#[test]
fn federation_frames_fit_their_budgets() {
    let f = Fixture::new(7);

    // FED_FORWARD's overhead is everything around the payload: the frame
    // header, the two region names, the payload length varint and the
    // optional-count byte. The payload itself is an inner frame, built here
    // with the real encoder — a message event with a short sealed envelope.
    let inner = to_frame(
        Opcode::MessageEvent.to_wire(),
        1,
        &MessageEvent {
            message_id: f.user,
            conversation_id: f.conversation,
            seq: 500,
            sender_id: f.peer,
            sender_device: f.device,
            kind: MessageKind::Text,
            envelope: one_on_one_envelope(&f.key, &[], &text_plaintext(100)),
            created_at: NOW,
            reply_to: None,
            edited_at: None,
            deleted: None,
            sender_key_id: None,
        },
    )
    .unwrap_or_else(|e| panic!("the inner frame must encode: {e}"));
    let inner_bytes = inner
        .encode()
        .unwrap_or_else(|e| panic!("the inner frame must encode: {e}"))
        .to_vec();

    let forward = FedForward {
        from: "local".to_string(),
        to: "eu-1".to_string(),
        payload: inner_bytes.clone(),
    };
    let forward_frame = frame_size(Opcode::FedForward, 1, &forward);
    let forward_overhead = forward_frame - inner_bytes.len();
    assert!(
        forward_overhead <= 64,
        "FED_FORWARD overhead is {forward_overhead} bytes, budget 64 (section 171)"
    );

    // FED_ACK names the peer by its 26-character text id, which is why the
    // brief's original 16-byte budget became 48: the watermark is three
    // bytes, the rest is the name.
    let ack = frame_size(
        Opcode::FedAck,
        2,
        &FedAck {
            node_id: f.device.to_text(),
            seq: 5000,
        },
    );
    assert!(ack <= 48, "FED_ACK is {ack} bytes, budget 48 (section 171)");

    println!(
        "federation: forward overhead {forward_overhead} bytes around a {}-byte payload, ack {ack}",
        inner_bytes.len()
    );
}
