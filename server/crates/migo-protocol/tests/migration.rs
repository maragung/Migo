//! The protocol migration harness — brief section 176.
//!
//! Section 176 is the migration path spec, and its promise has two halves this
//! file pins from the test side only — no schema file, no generated code, and
//! no production version constant is touched anywhere in it:
//!
//! * **Migration inside MWP/1 is additive.** A newer build must accept
//!   everything an older one ever wrote, and the version byte on the frame is
//!   what an old build writes and a new build reads. The first two tests
//!   round-trip every message kind the schema allocates at that version,
//!   through every optional header block, on both transports.
//! * **A future MWP/2 must be refusable gracefully.** When the framing breaks,
//!   the version byte is what decides which parser runs — "byte version pada
//!   frame yang menentukan parser mana yang dipakai. Tidak ada penebakan" — so
//!   a version this build does not speak is refused with the named error
//!   before any other byte is examined, and the refusal reaches the peer as
//!   `PROTOCOL_VERSION_UNSUPPORTED`, the exact code section 176 prescribes for
//!   a refused version.
//!
//! Two things this harness deliberately does not re-prove, because they are
//! already pinned where they live: the schema's `PROTOCOL_VERSION` and the
//! codec's are held equal by `the_protocol_version_matches_the_framing_version`
//! in this crate's unit tests, and the other half of within-MWP/1 additivity —
//! an unknown optional field being skipped by length so an old decoder
//! survives a new field — is proven in `migo-wire`'s reader tests and by
//! scenario 8 of `migod`'s node_failures suite. This file covers the version
//! envelope those properties travel in.
//!
//! "Message kind" here means the opcode: the opcode table is generated
//! complete from the schema, so [`Opcode::ALL`] is every message kind the
//! schema defines, and the frame — not the payload — is the only place a
//! version exists on the wire.

use bytes::Bytes;
use migo_protocol::{codes, fault, Frame, FrameHeader, Opcode, WireError};
use migo_wire::frame::MetadataBlock;
use migo_wire::{Fragment, TraceContext, PROTOCOL_VERSION};

/// The version a hypothetical MWP/2 build would write.
///
/// This is the section 176 future simulated test-side only: the production
/// constant stays exactly where it is, and the bump exists to prove that the
/// *current* parser — the one every deployed build runs — refuses the next
/// version deterministically rather than guessing at it. A refusal here is
/// what makes dual-speak possible later: when MWP/2 arrives and the constant
/// genuinely moves to 2, the parser that must keep serving v1 for a
/// deprecation cycle is this same gate, widened by exactly one value.
const FUTURE_VERSION: u8 = PROTOCOL_VERSION + 1;

/// A payload that is deterministic per kind and never empty, so the frames
/// below carry real bytes rather than a degenerate zero-length payload that
/// only exercises the header path.
fn payload_for(opcode: Opcode) -> Bytes {
    let number = opcode.to_wire();
    Bytes::from(vec![
        0xA5,
        (number & 0xFF) as u8,
        ((number >> 8) & 0x7F) as u8 | 0x80,
        0x5A,
    ])
}

#[test]
fn every_message_kind_round_trips_at_the_current_version() {
    // The additive half of section 176: whatever a newer build must still
    // accept, it accepts today, because today's build is the "newer build"
    // relative to every build that has ever shipped. Every kind the schema
    // allocates, in three header shapes — minimal, flag-carrying, and fully
    // loaded with every optional block the header can hold — must survive its
    // own encoder.
    for &opcode in Opcode::ALL {
        let payload = payload_for(opcode);
        let shapes = [
            Frame::simple(opcode.to_wire(), 7, payload.clone()),
            Frame::new(
                FrameHeader::new(opcode.to_wire(), 8).error().ack_required(),
                payload.clone(),
            ),
            Frame::new(
                FrameHeader::new(opcode.to_wire(), 9)
                    .with_trace(TraceContext {
                        trace_id: [0x0F; 16],
                        span_id: [0xF0; 8],
                    })
                    .with_fragment(Fragment { index: 1, total: 3 })
                    .with_metadata(MetadataBlock {
                        frame_seq: 4,
                        sent_at_delta: 1_000,
                        payload_len: Some(4),
                    }),
                payload,
            ),
        ];
        for frame in shapes {
            let encoded = frame
                .encode()
                .unwrap_or_else(|e| panic!("{} must encode: {e}", opcode.name()));
            let decoded = Frame::decode(encoded)
                .unwrap_or_else(|e| panic!("{} must decode its own encoding: {e}", opcode.name()));
            assert_eq!(
                decoded.header.version,
                PROTOCOL_VERSION,
                "{}",
                opcode.name()
            );
            assert_eq!(decoded, frame, "{}", opcode.name());
        }
    }
}

#[test]
fn the_version_every_deployed_build_writes_still_decodes_on_both_transports() {
    // MWP/1 has exactly one released version, so "old-version envelope" and
    // "current-version envelope" are the same bytes today — and the assertion
    // that matters is that this can never silently stop being true: a frame
    // written the way every deployed build has written it decodes on both
    // transports, including the length-prefixed record path a stream transport
    // uses, where a version-gate regression would otherwise hide until a TCP
    // client upgraded.
    for &opcode in Opcode::ALL {
        let frame = Frame::new(
            FrameHeader::new(opcode.to_wire(), 11)
                .with_trace(TraceContext {
                    trace_id: [0x11; 16],
                    span_id: [0x22; 8],
                })
                .with_metadata(MetadataBlock {
                    frame_seq: 6,
                    sent_at_delta: 42,
                    payload_len: None,
                }),
            payload_for(opcode),
        );

        // The WebSocket/QUIC path: no prefix, the transport frames it.
        let bare = frame
            .encode()
            .unwrap_or_else(|e| panic!("{} must encode: {e}", opcode.name()));
        assert_eq!(
            Frame::decode(bare).expect("a bare frame the encoder just wrote decodes"),
            frame,
            "{}",
            opcode.name()
        );

        // The stream path: u32 length prefix, the record a TCP transport reads.
        let record = frame
            .encode_length_prefixed()
            .unwrap_or_else(|e| panic!("{} must encode a record: {e}", opcode.name()));
        let (decoded, used) = Frame::decode_length_prefixed(&record)
            .expect("the record must parse")
            .expect("a whole record must be present");
        assert_eq!(used, record.len(), "{}", opcode.name());
        assert_eq!(decoded, frame, "{}", opcode.name());
    }
}

#[test]
fn a_future_mwp2_version_is_refused_for_every_kind_before_any_other_byte() {
    // The graceful-refusal half of section 176. FUTURE_VERSION is the
    // test-side bump of the wire constant — an MWP/2 build's first byte — and
    // the current parser must answer it with the named error, not a panic, not
    // a silent acceptance, and not a different error that would leave the
    // peer guessing which half of its frame was wrong.
    for &opcode in Opcode::ALL {
        let encoded = Frame::simple(opcode.to_wire(), 0, payload_for(opcode))
            .encode()
            .unwrap_or_else(|e| panic!("{} must encode: {e}", opcode.name()));
        let mut future = encoded.to_vec();
        future[0] = FUTURE_VERSION;
        assert_eq!(
            Frame::decode(Bytes::from(future)),
            Err(WireError::UnsupportedVersion {
                found: FUTURE_VERSION,
                supported: PROTOCOL_VERSION,
            }),
            "{} at version {FUTURE_VERSION} must be refused with the version error",
            opcode.name()
        );
    }

    // The gate fires before any other byte is examined: a frame whose second
    // byte sets reserved flag bits is still answered with the version error,
    // because the version byte is what picks the parser — no parsing happens
    // past it, so nothing about a future frame's body can change the verdict.
    assert_eq!(
        Frame::decode(Bytes::from(vec![FUTURE_VERSION, 0xFF])),
        Err(WireError::UnsupportedVersion {
            found: FUTURE_VERSION,
            supported: PROTOCOL_VERSION,
        })
    );
}

#[test]
fn no_version_byte_other_than_the_current_one_is_ever_accepted() {
    // The exhaustive version of the refusal: not "some future version" but
    // every byte the version field can hold, for every kind. Exactly one
    // value decodes and every other value is refused with the same named
    // error — a sweep like this is what "never accepted silently, never a
    // panic" actually means, and it covers the versions below 1 that were
    // never released just as much as the ones above it that do not exist yet.
    for &opcode in Opcode::ALL {
        let encoded = Frame::simple(opcode.to_wire(), 0, Bytes::new())
            .encode()
            .unwrap_or_else(|e| panic!("{} must encode: {e}", opcode.name()));
        for version in 0u8..=255 {
            let mut bytes = encoded.to_vec();
            bytes[0] = version;
            let verdict = Frame::decode(Bytes::from(bytes)); // no panic, for any byte
            if version == PROTOCOL_VERSION {
                assert!(
                    verdict.is_ok(),
                    "{} at its own version must decode",
                    opcode.name()
                );
            } else {
                assert_eq!(
                    verdict,
                    Err(WireError::UnsupportedVersion {
                        found: version,
                        supported: PROTOCOL_VERSION,
                    }),
                    "{} at version {version} must be refused with the version error",
                    opcode.name()
                );
            }
        }
    }
}

#[test]
fn a_refused_version_answers_with_the_code_section_176_names() {
    // Section 176 says a version past its end-of-support date is "dijawab
    // PROTOCOL_VERSION_UNSUPPORTED" — answered with that code, not with an
    // ad-hoc string. The same translation answers any version this build
    // cannot speak, so the refusal a nightly MWP/2 simulation observes here is
    // wire-identical to the one a deprecated client will one day see.
    let error = fault::from_wire(WireError::UnsupportedVersion {
        found: FUTURE_VERSION,
        supported: PROTOCOL_VERSION,
    });
    assert_eq!(error.code(), codes::PROTOCOL_VERSION_UNSUPPORTED);
    assert_eq!(error.symbol(), "PROTOCOL_VERSION_UNSUPPORTED");
}
