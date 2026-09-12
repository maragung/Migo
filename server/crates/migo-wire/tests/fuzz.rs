//! Bounded fuzz suites for the MWP/1 codec — brief section 172's fuzz contract.
//!
//! Section 172 mandates fuzzing the frame decoder with random bytes and with
//! mutations of the valid vector corpus, and it names the cases the corpus MUST
//! contain. What it does not mandate is the engine: `cargo-fuzz` needs a nightly
//! toolchain, and this repository's CI lane is stable by policy (see the
//! property tests in `src/frame.rs`). So the same contract is delivered as
//! bounded, seeded suites: every run is reproducible (`SIM_SEED` selects the
//! seed, the same variable the deterministic simulator reads), every loop has a
//! fixed iteration count, and CI therefore terminates deterministically.
//! Unbounded continuous fuzzing stays outside CI by design.
//!
//! The ceiling every case asserts is the one section 172 states: no panic, no
//! unbounded allocation, no unending loop — for any input. Where a case is a
//! *rejection* case (a length prefix claiming four gigabytes, a list count of
//! two billion) the assertion is stronger than "no panic": the codec must
//! refuse the input *before* allocating what the header asked for, which is the
//! difference between a fuzz finding and an outage.
//!
//! The corpus bytes are built by hand from the framing rules in `src/frame.rs`'s
//! module docs, never by running this crate's own encoder — a corpus case the
//! codec under test had produced itself would only test the codec against its
//! own idea of the format.

use std::path::PathBuf;

use bytes::Bytes;
use migo_core::{Random, SeededRandom};
use migo_wire::error::WireError;
use migo_wire::{decode_batch, encode_batch, from_bytes, varint, Frame, FrameHeader, Reader};
use migo_wire::{Decode, Encode, Writer, PROTOCOL_VERSION};
use serde_json::Value;

// --- the hand-written corpus of section 172 ---------------------------------

/// Encodes a frame header's fixed prefix by hand: version, flags, then the
/// opcode and correlation as raw varints, so a case can choose non-canonical or
/// over-long encodings the real encoder would never emit.
fn header_bytes(flags: u8, opcode: &[u8], correlation: &[u8]) -> Vec<u8> {
    let mut out = vec![PROTOCOL_VERSION, flags];
    out.extend_from_slice(opcode);
    out.extend_from_slice(correlation);
    out
}

#[test]
fn a_non_canonical_varint_in_the_header_is_refused() {
    // Opcode 1 as `81 00`: decodes to one, but is two bytes and the final
    // group is zero — padded, therefore not canonical. The codec is strict
    // because mesh frames are signed over the bytes (crate docs).
    let bytes = header_bytes(0, &[0x81, 0x00], &[0x01]);
    assert_eq!(
        Frame::decode(Bytes::from(bytes)),
        Err(WireError::NonMinimalVarint { offset: 2 })
    );
}

#[test]
fn an_eleven_byte_varint_is_refused_at_ten() {
    // Eleven continuation bytes: the decoder must stop at MAX_VARINT_BYTES
    // rather than spin, and must not read past the buffer to find an end.
    let bytes = header_bytes(0, [0x80u8; 11].as_slice(), &[0x01]);
    assert_eq!(
        Frame::decode(Bytes::from(bytes)),
        Err(WireError::VarintTooLong { offset: 2, max: 10 })
    );
}

#[test]
fn a_length_claim_past_the_limit_is_refused_before_allocating() {
    // A string or list that claims u32::MAX items with two bytes present. The
    // claim is refused against the schema's ceiling before any caller sizes a
    // buffer from it — otherwise a six-byte field is a remote out-of-memory
    // primitive (limits.rs).
    let mut raw = Vec::new();
    varint::encode_u64(u64::from(u32::MAX), &mut raw);
    raw.extend_from_slice(b"xy");
    let mut reader = Reader::new(Bytes::from(raw.clone()));
    match reader.read_string() {
        Err(WireError::StringTooLong { .. }) => {}
        other => panic!("a u32::MAX string length must be refused, got {other:?}"),
    }
    let mut reader = Reader::new(Bytes::from(raw));
    match reader.read_list_len() {
        Err(WireError::ListTooLong { .. }) => {}
        other => panic!("a u32::MAX list length must be refused, got {other:?}"),
    }
}

#[test]
fn a_list_count_of_two_billion_is_refused_before_the_vec_is_built() {
    // 2_000_000_000 as a varint, then nothing. The generated decoders size
    // their Vecs from this number, so the refusal has to happen inside
    // `read_list_len` — before the Vec exists.
    let mut raw = Vec::new();
    varint::encode_u64(2_000_000_000, &mut raw);
    let mut reader = Reader::new(Bytes::from(raw));
    match reader.read_list_len() {
        Err(WireError::ListTooLong { .. }) => {}
        other => panic!("a two-billion-item list must be refused, got {other:?}"),
    }
}

#[test]
fn seventeen_levels_of_nesting_are_refused_at_sixteen() {
    // MAX_NESTING_DEPTH is the bound that keeps recursive decoding off the
    // stack. Nothing on the wire marks a level — the depth is the decoder's
    // own count — so the honest test drives the count directly: sixteen
    // `enter`s succeed, the seventeenth is refused.
    let mut reader = Reader::new(Bytes::from(vec![0u8; 64]));
    for _ in 0..16 {
        reader.enter().expect("sixteen levels are legal");
    }
    assert!(matches!(
        reader.enter(),
        Err(WireError::DepthExceeded { .. })
    ));
}

#[test]
fn invalid_utf8_in_a_string_is_refused_not_repaired() {
    // A two-byte string whose bytes are not UTF-8. The limit checks pass; the
    // validity check is what fires, and it must fire rather than hand the
    // caller a lossy conversion.
    let mut raw = Vec::new();
    varint::encode_u64(2, &mut raw);
    raw.extend_from_slice(&[0xFF, 0xFE]);
    let mut reader = Reader::new(Bytes::from(raw));
    assert_eq!(reader.read_string(), Err(WireError::InvalidUtf8));
}

#[test]
fn a_trailing_byte_after_a_complete_value_is_refused() {
    // Section 172's "trailing byte": a value that ends cleanly and then one
    // more byte. Trailing bytes mean the two sides disagree about the shape,
    // and continuing on that basis is how a parsing bug becomes a security bug
    // (crate docs, `from_bytes`).
    let mut w = Writer::new();
    w.enter().expect("depth");
    w.write_u64(7);
    w.write_u32(0); // no optional fields
    w.leave();
    let mut encoded = w.finish().expect("finishes").to_vec();
    encoded.push(0);
    assert_eq!(
        from_bytes::<U64WithOptional>(Bytes::from(encoded)),
        Err(WireError::TrailingBytes { count: 1 })
    );
}

#[test]
fn a_reserved_flag_bit_is_refused_not_ignored() {
    // FLAGS_EXT (bit 7) is the one bit a MWP/1 receiver must reject, because a
    // receiver that ignored it would already be accepting frames whose meaning
    // it cannot see (flags.rs).
    let bytes = header_bytes(0x80, &[0x01], &[0x01]);
    assert!(matches!(
        Frame::decode(Bytes::from(bytes)),
        Err(WireError::ReservedFlags { .. })
    ));
}

#[test]
fn a_fragment_with_total_zero_is_refused() {
    // FRAGMENT flag set, then index 0 and total 0. A total of zero can never
    // reassemble; the codec refuses the pair rather than optimistically
    // buffering a slice that will never complete.
    let mut bytes = header_bytes(0x20, &[0x01], &[0x01]); // 0x20 = FRAGMENT
    varint::encode_u64(0, &mut bytes);
    varint::encode_u64(0, &mut bytes);
    assert!(matches!(
        Frame::decode(Bytes::from(bytes)),
        Err(WireError::InvalidFragment { .. })
    ));
}

#[test]
fn opcode_zero_and_a_huge_correlation_are_legal_and_round_trip() {
    // Opcode zero is the BATCH envelope's number and is not, in the codec, a
    // reserved value — the codec does not know opcodes, the layers above do.
    // A u32::MAX correlation is likewise legal. The corpus case exists because
    // these two must be *handled*: a clean decode, not a panic and not a wrap.
    let header = FrameHeader::new(0, u32::MAX);
    let frame = Frame::new(header, Bytes::from_static(&[0x00]));
    let encoded = frame.encode().expect("encodes");
    let decoded = Frame::decode(encoded).expect("opcode zero and a full correlation decode");
    assert_eq!(decoded.header.opcode, 0);
    assert_eq!(decoded.header.correlation, u32::MAX);
}

#[test]
fn a_batch_inside_a_batch_is_refused_both_ways() {
    // The exponential-expansion guard: a small frame must not be able to
    // describe an arbitrarily large one. Encoding refuses to nest; decoding
    // refuses a nested payload, so the check does not depend on the sender
    // having used this crate to build the bomb.
    let inner = Frame::new(FrameHeader::new(2, 0), Bytes::from_static(&[]));
    let mut batched = inner.clone();
    batched.header.flags |= migo_wire::flags::BATCH;
    assert_eq!(
        encode_batch(&[batched.clone(), inner.clone()]),
        Err(WireError::NestedBatch)
    );

    // And the decode side: hand-build the nested payload the encoder refuses
    // to produce, and the decoder must refuse it too.
    let mut payload = Vec::new();
    varint::encode_u64(1, &mut payload); // one element
    let element = batched.encode().expect("a batched frame encodes alone");
    varint::encode_u64(element.len() as u64, &mut payload);
    payload.extend_from_slice(&element);
    let mut envelope_header = FrameHeader::new(0, 0);
    envelope_header.flags |= migo_wire::flags::BATCH;
    let envelope = Frame::new(envelope_header, Bytes::from(payload));
    assert_eq!(
        decode_batch(&envelope).map(|frames| frames.len()),
        Err(WireError::NestedBatch)
    );
}

#[test]
fn an_oversize_buffer_is_refused_before_the_header_is_parsed() {
    // One byte past MAX_FRAME_BYTES of anything. The size check runs before
    // the first varint is read, so the content is irrelevant — which is the
    // property under test: the refusal cannot depend on parsing the bomb.
    let bomb = vec![0u8; migo_wire::limits::MAX_FRAME_BYTES + 1];
    assert!(matches!(
        Frame::decode(Bytes::from(bomb)),
        Err(WireError::FrameTooLarge { .. })
    ));
}

#[test]
fn a_trace_block_shorter_than_twenty_four_bytes_is_refused() {
    // TRACED demands 16 + 8 bytes. A frame that stops early must be refused
    // with the offset it stopped at, not padded and not read past.
    let bytes = header_bytes(0x02, &[0x01], &[0x01]); // TRACED
    assert!(matches!(
        Frame::decode(Bytes::from(bytes)),
        Err(WireError::UnexpectedEnd { .. })
    ));
}

#[test]
fn a_metadata_block_claiming_more_than_the_frame_is_carried_not_acted_on() {
    // The METADATA block's payload_len is a claim about bytes that may not be
    // there. The block itself always decodes (it is three varints); the claim
    // is checked where it is used — and the decode must not read past the end
    // of the frame to satisfy it.
    let mut bytes = header_bytes(0x40, &[0x01], &[0x01]); // METADATA
    varint::encode_u64(1, &mut bytes); // frame_seq
    varint::encode_u64(0, &mut bytes); // sent_at_delta
    varint::encode_u64(u64::from(u32::MAX), &mut bytes); // payload_len claim
    let frame =
        Frame::decode(Bytes::from(bytes)).expect("a metadata block with a huge claim still parses");
    let metadata = frame.header.metadata.expect("the block is present");
    assert_eq!(metadata.payload_len, Some(u32::MAX));
}

// --- seeded random buffers ---------------------------------------------------

/// How many random buffers each size class contributes. Fixed, not a duration:
/// a fuzz budget stated in seconds is a test that runs longer on a slow runner
/// and "terminates" only until the runner is slower than the budget.
const RANDOM_CASES_PER_SIZE: u64 = 32;

#[test]
fn seeded_random_buffers_never_panic_the_decoder() {
    let mut rng = SeededRandom::from_env();
    // Every size class that guards a boundary in the codec: the header
    // minimum, the varint stride, the compression threshold, the string
    // ceiling, and beyond.
    for size in [
        0usize, 1, 2, 3, 4, 5, 9, 10, 11, 64, 511, 512, 513, 4096, 65_536,
    ] {
        for _ in 0..RANDOM_CASES_PER_SIZE {
            let mut buffer = vec![0u8; size];
            rng.fill_bytes(&mut buffer);
            // Bare and length-prefixed both: they are the two entry points
            // every transport uses. Any return value is acceptable; the
            // assertion is the absence of a panic.
            let _ = Frame::decode(Bytes::from(buffer.clone()));
            let _ = Frame::decode_length_prefixed(&Bytes::from(buffer));
        }
    }
}

#[test]
fn seeded_random_buffers_with_a_valid_header_prefix_never_panic_the_decoder() {
    // Random bytes that begin the way a real frame does — version 1, a known
    // flags byte — reach deeper into the decoder than uniformly random ones,
    // which almost always die on the version byte. One flags value per known
    // bit, plus the reserved bit, so each optional-block parser gets driven.
    let mut rng = SeededRandom::from_env();
    for flag_bits in [
        0x00u8, // bare
        0x01,   // COMPRESSED — random payload hits the bomb guard
        0x02,   // TRACED — 24 bytes of trace, then payload
        0x04,   // BATCH — random payload hits the batch parser
        0x20,   // FRAGMENT
        0x40,   // METADATA
        0x80,   // reserved, refused at the header
    ] {
        for _ in 0..RANDOM_CASES_PER_SIZE {
            let mut buffer = header_bytes(flag_bits, &[0x01], &[0x01]);
            let tail = (rng.next_u64() % 512) as usize;
            let mut noise = vec![0u8; tail];
            rng.fill_bytes(&mut noise);
            buffer.extend_from_slice(&noise);
            let _ = Frame::decode(Bytes::from(buffer.clone()));
            let _ = Frame::decode_length_prefixed(&Bytes::from(buffer));
        }
    }
}

// --- mutations of the valid vector corpus ------------------------------------

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../shared/protocol/vectors/wire")
}

/// Collects every `hex` field from every section of every wire vector file —
/// valid and invalid cases both. The invalid ones are part of the corpus on
/// purpose: they are already known-bad inputs, and mutations of a known-bad
/// input are how a decoder's error paths are explored.
fn every_vector_payload() -> Vec<Vec<u8>> {
    let mut payloads = Vec::new();
    for file in ["varint.json", "frames.json", "mse.json"] {
        let path = vectors_dir().join(file);
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
        let parsed: Value =
            serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {path:?}: {error}"));
        for (section, cases) in parsed.as_object().expect("a vector file is a JSON object") {
            let Some(cases) = cases.as_array() else {
                continue;
            };
            for case in cases {
                if let Some(hex) = case.get("hex").and_then(Value::as_str) {
                    payloads.push(
                        hex::decode(hex)
                            .unwrap_or_else(|error| panic!("{file}/{section}: bad hex: {error}")),
                    );
                }
            }
        }
    }
    assert!(
        payloads.len() >= 80,
        "the wire corpus has shrunk to {} payloads; a generator stopped emitting cases",
        payloads.len()
    );
    payloads
}

#[test]
fn every_vector_payload_survives_every_truncation() {
    // Every strict prefix of every corpus payload — valid or invalid — must
    // come back as an error or a value, never a panic. A transport hands the
    // codec partial bytes whenever a read boundary lands mid-frame.
    for payload in every_vector_payload() {
        for cut in 0..payload.len() {
            let prefix = Bytes::from(payload[..cut].to_vec());
            let _ = Frame::decode(prefix.clone());
            let _ = Frame::decode_length_prefixed(&prefix);
            let mut reader = Reader::new(prefix);
            let _ = reader.read_list_len();
            let _ = reader.read_string();
        }
        // The complete payload too, so a case that only panics whole is caught.
        let _ = Frame::decode(Bytes::from(payload.clone()));
        let _ = Frame::decode_length_prefixed(&Bytes::from(payload));
    }
}

#[test]
fn every_vector_payload_survives_seeded_bit_flips() {
    // Eight seeded single-bit flips per payload, at positions the seed picks.
    // Every cut above plus eight flips keeps the run bounded while still
    // walking past the first byte, where a uniformly random flip almost always
    // lands on the version check and tests nothing deeper.
    let mut rng = SeededRandom::from_env();
    for payload in every_vector_payload() {
        for _ in 0..8 {
            if payload.is_empty() {
                break;
            }
            let position = (rng.next_u64() as usize) % payload.len();
            let bit = 1u8 << (rng.next_u64() % 8);
            let mut mutated = payload.clone();
            mutated[position] ^= bit;
            let _ = Frame::decode(Bytes::from(mutated.clone()));
            let _ = Frame::decode_length_prefixed(&Bytes::from(mutated));
        }
    }
}

// --- a decode target for the trailing-byte case ------------------------------

/// The minimal shape the trailing-byte case needs: one positional field, then
/// the optional-field count. Hand-written, like the corpus bytes around it,
/// because a generated struct would pull the whole protocol schema into this
/// test's subject.
#[derive(Debug, PartialEq)]
struct U64WithOptional {
    value: u64,
}

impl Encode for U64WithOptional {
    fn encode(&self, w: &mut Writer) -> migo_wire::Result<()> {
        w.enter()?;
        w.write_u64(self.value);
        w.write_u32(0);
        w.leave();
        Ok(())
    }
}

impl Decode for U64WithOptional {
    fn decode(r: &mut Reader) -> migo_wire::Result<Self> {
        r.enter()?;
        let value = r.read_u64()?;
        let count = r.read_u32()?;
        for _ in 0..count {
            let _field = r.read_optional()?;
        }
        r.leave();
        Ok(Self { value })
    }
}
