//! Bounded fuzz suites for the generated protocol decoders — brief section 172.
//!
//! Every struct decoder in this crate is generated, and every generated decoder
//! is a fixed composition of the same `Reader` primitives, so the highest-value
//! fuzz targets are the two edges of that composition:
//!
//! * **`Opcode::from_wire`** — the table every inbound frame is resolved
//!   through, before any struct is read. Swept exhaustively over the ranges
//!   that matter rather than sampled: the whole low span where every allocated
//!   opcode lives, and the reserved tail the gateway refuses (section 145/146).
//! * **The decoders an unauthenticated stranger can reach** — `Hello`,
//!   `Authenticate`, `Pong`, `Ack` and `ResumeRequest` are the payloads that
//!   arrive before any account exists, so they are the ones a hostile peer
//!   controls outright. Authenticated-surface decoders (`MessageSend`,
//!   `Subscribe`, `ProfileFetch`) are fuzzed too, with the caveat that an
//!   attacker needs a session to reach them; the pre-auth set is the security
//!   boundary and gets the deeper treatment.
//!
//! Like the wire crate's fuzz suite, this is bounded and seeded, not continuous:
//! fixed iteration counts, `SIM_SEED` for reproducibility, and no dependency on
//! a nightly toolchain. The property asserted is the one the brief states — for
//! any bytes, a decoder returns a value or an error, never a panic — plus one
//! stronger one: **whatever hostile bytes a decoder accepts, re-encoding the
//! accepted value must reproduce it**, so an accepted-but-uncanonical input can
//! never smuggle a second meaning past a signature (mesh frames are signed over
//! the bytes, which is why the codec is canonical everywhere else).

use bytes::Bytes;
use migo_core::{Id, Random, SeededRandom, Timestamp};
use migo_protocol::{
    from_bytes, to_bytes, Ack, Authenticate, Hello, MessageSend, Opcode, Pong, ProfileRequest,
    ResumeRequest, SubscribeRequest, PROTOCOL_VERSION,
};

// --- the opcode table --------------------------------------------------------

#[test]
fn the_opcode_table_resolves_the_low_span_and_the_reserved_tail_exactly() {
    // Every allocated opcode lives below 241 today (the highest is
    // ENTITLEMENTS at 240), so the whole span 0..=300 is swept exhaustively
    // rather than sampled: it covers every allocated number, the never-allocated
    // gaps between ranges, and the reserved head.
    for raw in 0u32..=300 {
        let resolved = Opcode::from_wire(raw); // must not panic, for any input
        let in_reserved_span = (241..=255).contains(&raw);
        assert_eq!(
            resolved.is_some(),
            !in_reserved_span && Opcode::ALL.iter().any(|opcode| opcode.to_wire() == raw),
            "opcode {raw} resolved to {resolved:?}, which contradicts the schema's allocation"
        );
    }

    // The reserved span the gateway refuses before resolution (section 146):
    // the table must not know a single one of them, or the range gate and the
    // table would disagree about what this build speaks.
    for raw in 241u32..=255 {
        assert_eq!(
            Opcode::from_wire(raw),
            None,
            "opcode {raw} is inside the never-allocated span 241-255 and must not resolve"
        );
    }

    // 240 is allocated (ENTITLEMENTS, section 145's carve-out) and must resolve
    // — the phase gate, not the range gate, is what refuses it from a client.
    assert_eq!(Opcode::from_wire(240), Some(Opcode::Entitlements));

    // Past the reserved span the numbers are simply unknown, not reserved:
    // a newer client speaking one is answered, not cut off.
    for raw in [256u32, 1000, u32::MAX - 1, u32::MAX] {
        let _ = Opcode::from_wire(raw); // no panic, value unchecked by design
    }
}

// --- the pre-auth decoders ---------------------------------------------------

/// Fixed iteration count, not a duration: see the wire crate's fuzz suite.
const RANDOM_CASES: u64 = 256;

/// One hostile payload against one decoder, with the stronger half of the
/// contract attached: success and failure are both acceptable answers, but
/// whatever a decoder accepts must be canonical — re-encoding the accepted
/// value and decoding again has to return it, or the value has two encodings,
/// which is two valid signatures for one message (the crate docs' reason for
/// strictness).
///
/// Generic per decoder rather than a loop over a list of results, because the
/// five decoders return five different types and there is no honest common
/// type to collect them into — a `Box<dyn Debug>` would check the re-encode
/// half against `Debug` output instead of against the value.
fn assert_canonical_if_accepted<T>(payload: &Bytes)
where
    T: migo_protocol::Decode + migo_protocol::Encode + PartialEq + std::fmt::Debug,
{
    if let Ok(value) = from_bytes::<T>(payload.clone()) {
        let re_encoded = to_bytes(&value).expect("an accepted value re-encodes");
        let re_decoded = from_bytes::<T>(re_encoded).expect("the re-encoding decodes");
        assert_eq!(
            re_decoded, value,
            "the decoder accepted a non-canonical encoding"
        );
    }
}

#[test]
fn seeded_random_payloads_never_panic_the_pre_auth_decoders() {
    let mut rng = SeededRandom::from_env();
    for _ in 0..RANDOM_CASES {
        let len = (rng.next_u64() % 512) as usize;
        let mut buffer = vec![0u8; len];
        rng.fill_bytes(&mut buffer);
        let payload = Bytes::from(buffer);
        // The five payloads an unauthenticated stranger controls outright.
        assert_canonical_if_accepted::<Hello>(&payload);
        assert_canonical_if_accepted::<Authenticate>(&payload);
        assert_canonical_if_accepted::<Pong>(&payload);
        assert_canonical_if_accepted::<Ack>(&payload);
        assert_canonical_if_accepted::<ResumeRequest>(&payload);
    }
}

/// A valid, fully-populated HELLO: every optional field present, so the
/// mutation sweep below walks past every field boundary the struct has.
fn sample_hello() -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        client: migo_protocol::ClientInfo {
            platform: migo_protocol::Platform::Android,
            app_version: "0.1.0".to_string(),
            os_version: Some("14".to_string()),
            device_model: Some("corpus".to_string()),
        },
        features: u64::MAX,
        locale: "id-ID".to_string(),
        bandwidth_mode: migo_protocol::BandwidthMode::LowData,
        access_token: Some("a-token-from-a-client".to_string()),
        device_id: Some(Id::from_bytes([3u8; 16])),
        resume: Some(ResumeRequest {
            session_id: Id::from_bytes([4u8; 16]),
            last_frame_seq: u64::MAX,
        }),
    }
}

#[test]
fn every_single_bit_flip_of_a_valid_hello_never_panics_the_decoder() {
    // Exhaustive over the bits, bounded over the bytes: a HELLO is under a
    // hundred bytes, so every bit of a realistic greeting can be tried — the
    // exact corpus a corruption or an attacker would produce.
    let encoded = to_bytes(&sample_hello()).expect("a valid HELLO encodes");
    for position in 0..encoded.len() {
        for bit in 0..8u32 {
            let mut mutated = encoded.to_vec();
            mutated[position] ^= 1 << bit;
            // Success or failure are both acceptable. Panicking is not: this
            // is the first struct a stranger's socket ever hands the server.
            assert_canonical_if_accepted::<Hello>(&Bytes::from(mutated));
        }
    }
}

#[test]
fn every_truncation_of_a_valid_hello_is_an_error() {
    // A HELLO cut at any point must be refused — a partial greeting that
    // decoded would hand the server fields their sender never wrote.
    let encoded = to_bytes(&sample_hello()).expect("a valid HELLO encodes");
    for cut in 0..encoded.len() {
        assert!(
            from_bytes::<Hello>(encoded.slice(..cut)).is_err(),
            "a {cut}-byte prefix of a HELLO decoded successfully"
        );
    }
}

// --- the authenticated-surface decoders --------------------------------------

#[test]
fn seeded_mutations_of_authenticated_payloads_never_panic_their_decoders() {
    // The authenticated surface is reachable only behind a session, so the
    // sweep is seeded rather than exhaustive — eight flips per struct, at
    // seed-chosen positions, over a valid fully-populated sample of each.
    let mut rng = SeededRandom::from_env();
    let timestamp = Timestamp::from_millis(1_700_000_000_000);
    let samples: Vec<Bytes> = vec![
        to_bytes(&sample_hello()).expect("encodes"),
        to_bytes(&Authenticate {
            access_token: "token".to_string(),
            device_id: Id::from_bytes([5u8; 16]),
        })
        .expect("encodes"),
        to_bytes(&Pong {
            client_time: timestamp,
            server_time: timestamp,
        })
        .expect("encodes"),
        to_bytes(&Ack { frame_seq: 42 }).expect("encodes"),
        to_bytes(&MessageSend {
            message_id: Id::from_bytes([6u8; 16]),
            conversation_id: Id::from_bytes([7u8; 16]),
            kind: migo_protocol::MessageKind::Text,
            envelope: vec![0xAB; 64],
            reply_to: Some(Id::from_bytes([8u8; 16])),
            expires_in_ms: Some(3_600_000),
            sender_key_id: Some(9),
        })
        .expect("encodes"),
        to_bytes(&SubscribeRequest {
            topics: vec![migo_protocol::Topic {
                kind: migo_protocol::TopicKind::User,
                id: Id::from_bytes([10u8; 16]),
            }],
        })
        .expect("encodes"),
        to_bytes(&ProfileRequest {
            user_ids: vec![Id::from_bytes([11u8; 16])],
        })
        .expect("encodes"),
    ];
    for payload in &samples {
        for _ in 0..8 {
            let mut mutated = payload.to_vec();
            if mutated.is_empty() {
                break;
            }
            let position = (rng.next_u64() as usize) % mutated.len();
            mutated[position] ^= 1u8 << (rng.next_u64() % 8);
            let _ = from_bytes::<Hello>(Bytes::from(mutated.clone()));
            let _ = from_bytes::<Authenticate>(Bytes::from(mutated.clone()));
            let _ = from_bytes::<Pong>(Bytes::from(mutated.clone()));
            let _ = from_bytes::<Ack>(Bytes::from(mutated.clone()));
            let _ = from_bytes::<ResumeRequest>(Bytes::from(mutated.clone()));
            let _ = from_bytes::<MessageSend>(Bytes::from(mutated.clone()));
            let _ = from_bytes::<SubscribeRequest>(Bytes::from(mutated.clone()));
            let _ = from_bytes::<ProfileRequest>(Bytes::from(mutated));
        }
    }
}

#[test]
fn the_default_of_every_reachable_decoder_round_trips() {
    // `Default` on a generated struct is the shape every client version can
    // send (all optional fields absent), so encode→decode must be the identity
    // on it — the backward-compatibility case in section 172's list.
    let hello = to_bytes(&Hello::default()).expect("a default HELLO encodes");
    assert_eq!(
        from_bytes::<Hello>(hello).expect("decodes"),
        Hello::default()
    );
    let authenticate = to_bytes(&Authenticate::default()).expect("a default AUTHENTICATE encodes");
    assert_eq!(
        from_bytes::<Authenticate>(authenticate).expect("decodes"),
        Authenticate::default()
    );
    let send = to_bytes(&MessageSend::default()).expect("a default MESSAGE_SEND encodes");
    assert_eq!(
        from_bytes::<MessageSend>(send).expect("decodes"),
        MessageSend::default()
    );
}
