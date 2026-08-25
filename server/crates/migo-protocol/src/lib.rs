//! MWP/1 message types, opcodes, and error codes.
//!
//! Everything in this crate is generated. The single source of truth is
//! `shared/protocol/schema/` — one set of JSON files describing structs, enums,
//! opcodes, and error codes — from which `tools/protocol-codegen` emits both the
//! Rust in [`generated`] and the TypeScript in `packages/protocol`.
//!
//! # Why generate, and why commit the output
//!
//! A wire protocol implemented twice is a wire protocol implemented wrong. The
//! failure is never dramatic: someone adds a field to the server struct, the
//! client's hand-written decoder reads the next field at the old offset, and the
//! bug shows up a week later as garbled text for users on one app version.
//! Generating both sides from one schema makes that class of bug impossible to
//! introduce, rather than merely unlikely.
//!
//! The generated files are committed rather than produced by a build script
//! (ADR-0010). That is a deliberate trade:
//!
//! * A reviewer sees the wire format change in the diff. A schema edit that
//!   accidentally reorders required fields is a breaking change, and it should be
//!   visible in review rather than buried in a build artifact.
//! * `cargo build` and `next build` need no Node.js toolchain, so a Rust-only
//!   contributor and a TypeScript-only contributor can each build without the
//!   other's environment.
//! * `git blame` works on the protocol.
//!
//! The cost is drift, and drift is handled by making it a build failure: `make
//! protocol-check` regenerates into memory and exits non-zero if the result
//! differs from what is committed. CI runs it, so a stale file cannot merge.
//!
//! # What generation guarantees
//!
//! The generator refuses to emit anything unless the schema is complete:
//!
//! * Every opcode declares a rate-limit `cost` and a delivery class. There is no
//!   default, because a defaulted cost is a free operation, and a free operation
//!   is a denial-of-service primitive (ADR-0006).
//! * Every opcode declares a direction and an auth level, which is what makes
//!   [`Opcode::accepts_from_client`] a fact from the schema rather than a
//!   condition somebody remembered to write in a handler.
//! * Required fields keep their schema order in both languages, so the
//!   positional prefix that MSE depends on cannot silently change.
//!
//! # Layering
//!
//! This crate holds *shapes and constants*, not behaviour. It depends on
//! `migo-wire` for the codec and on `migo-core` for [`migo_core::Id`] and
//! [`migo_core::Timestamp`], and on nothing else — no tokio, no database, no
//! HTTP. Every layer above can therefore speak about protocol messages without
//! pulling in a runtime.
//!
//! ```
//! use migo_protocol::{codes, Opcode};
//!
//! // Costs come from the schema, so the rate limiter cannot forget one.
//! assert_eq!(Opcode::MessageSend.cost(), 1);
//! assert_eq!(Opcode::Ack.cost(), 0);
//!
//! // A client may not send a server-to-client opcode.
//! assert!(Opcode::MessageSend.accepts_from_client());
//! assert!(!Opcode::MessageEvent.accepts_from_client());
//!
//! // Error codes carry their own retry semantics.
//! assert_eq!(migo_protocol::error_symbol(codes::RATE_LIMITED), Some("RATE_LIMITED"));
//! ```

#![forbid(unsafe_code)]
#![warn(clippy::all)]

pub mod fault;
pub mod generated;

pub use crate::fault::{error as fault_error, kind_of as error_kind_of};
pub use crate::generated::*;

/// Re-exported so callers can encode and decode protocol types without also
/// depending on `migo-wire` directly.
pub use migo_wire::{
    decode_batch, encode_batch, from_bytes, from_frame, to_bytes, to_frame, Decode, Encode, Frame,
    FrameHeader, Reader, Result as WireResult, WireError, Writer,
};

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::{Id, Timestamp};

    #[test]
    fn the_protocol_version_matches_the_framing_version() {
        // Two constants, one from the schema and one from the codec. They are
        // allowed to be separate only as long as they agree.
        assert_eq!(PROTOCOL_VERSION, u32::from(migo_wire::PROTOCOL_VERSION));
    }

    #[test]
    fn the_epoch_matches_core() {
        assert_eq!(EPOCH_MS, migo_core::MIGO_EPOCH_MS);
    }

    #[test]
    fn every_opcode_round_trips_through_the_wire() {
        for &opcode in Opcode::ALL {
            assert_eq!(
                Opcode::from_wire(opcode.to_wire()),
                Some(opcode),
                "{}",
                opcode.name()
            );
        }
    }

    #[test]
    fn an_unknown_opcode_is_none_rather_than_a_panic() {
        assert_eq!(Opcode::from_wire(0xFFFF), None);
    }

    #[test]
    fn opcode_numbers_are_unique() {
        let mut seen: Vec<u32> = Opcode::ALL.iter().map(|o| o.to_wire()).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two opcodes share a wire number");
    }

    #[test]
    fn opcode_names_are_unique() {
        let mut seen: Vec<&str> = Opcode::ALL.iter().map(|o| o.name()).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two opcodes share a name");
    }

    #[test]
    fn free_opcodes_are_only_the_ones_that_must_be_free() {
        // A zero-cost opcode is exempt from rate limiting, which makes it a
        // potential flood vector. The exemption list is small and deliberate:
        // acknowledgements and server-originated frames. If this test fails
        // because a new opcode was given cost 0, that is the review question.
        for &opcode in Opcode::ALL {
            if opcode.cost() == 0 {
                assert!(
                    !opcode.accepts_from_client() || matches!(opcode, Opcode::Ack),
                    "{} is free and client-sendable",
                    opcode.name()
                );
            }
        }
    }

    #[test]
    fn server_to_client_opcodes_are_refused_from_clients() {
        assert!(!Opcode::MessageEvent.accepts_from_client());
        assert!(!Opcode::PresenceEvent.accepts_from_client());
        assert!(Opcode::MessageSend.accepts_from_client());
        assert!(Opcode::Hello.accepts_from_client());
    }

    #[test]
    fn error_codes_resolve_to_symbols_and_statuses() {
        assert_eq!(error_symbol(codes::RATE_LIMITED), Some("RATE_LIMITED"));
        assert_eq!(error_http_status(codes::RATE_LIMITED), 429);
        assert_eq!(error_symbol(0), None, "code 0 is not an error");
    }

    #[test]
    fn a_message_send_round_trips_through_a_frame() {
        let original = MessageSend {
            message_id: Id::from_bytes([3u8; 16]),
            conversation_id: Id::from_bytes([4u8; 16]),
            kind: MessageKind::Text,
            // The server never sees plaintext for a private conversation; what
            // travels here is ciphertext produced on the device.
            envelope: b"ciphertext".to_vec(),
            reply_to: Some(Id::from_bytes([5u8; 16])),
            expires_in_ms: Some(86_400_000),
            ..Default::default()
        };
        let frame = to_frame(Opcode::MessageSend.to_wire(), 1, &original).expect("encodes");
        let received = Frame::decode(frame.encode().expect("encodes")).expect("decodes");
        assert_eq!(received.header.opcode, Opcode::MessageSend.to_wire());
        let decoded: MessageSend = from_frame(&received).expect("decodes");
        assert_eq!(decoded, original);
    }

    #[test]
    fn an_absent_optional_field_stays_absent() {
        let original = MessageSend {
            message_id: Id::from_bytes([1u8; 16]),
            envelope: b"c".to_vec(),
            ..Default::default()
        };
        assert!(original.reply_to.is_none());
        assert!(original.expires_in_ms.is_none());
        let bytes = to_bytes(&original).expect("encodes");
        let decoded: MessageSend = from_bytes(bytes).expect("decodes");
        assert!(decoded.reply_to.is_none());
        assert!(decoded.expires_in_ms.is_none());
        assert_eq!(decoded, original);
    }

    #[test]
    fn absent_optionals_cost_only_the_count_byte() {
        // The point of MSE: a message with no optional fields set pays one byte
        // for the optional count and nothing per absent field.
        let bare = MessageSend {
            message_id: Id::from_bytes([1u8; 16]),
            conversation_id: Id::from_bytes([2u8; 16]),
            envelope: b"c".to_vec(),
            ..Default::default()
        };
        let with_reply = MessageSend {
            reply_to: Some(Id::from_bytes([3u8; 16])),
            ..bare.clone()
        };
        let bare_len = to_bytes(&bare).expect("encodes").len();
        let reply_len = to_bytes(&with_reply).expect("encodes").len();
        // 16 bytes of id plus a field id and a length varint.
        assert_eq!(reply_len, bare_len + 18);
    }

    #[test]
    fn a_hello_round_trips_with_its_nested_struct() {
        let original = Hello {
            protocol_version: PROTOCOL_VERSION,
            client: ClientInfo {
                platform: Platform::Web,
                app_version: "0.1.0".into(),
                ..Default::default()
            },
            locale: "id-ID".into(),
            ..Default::default()
        };
        let bytes = to_bytes(&original).expect("encodes");
        let decoded: Hello = from_bytes(bytes).expect("decodes");
        assert_eq!(decoded, original);
    }

    #[test]
    fn a_truncated_protocol_frame_never_panics() {
        let original = Hello {
            protocol_version: PROTOCOL_VERSION,
            locale: "en-US".into(),
            ..Default::default()
        };
        let bytes = to_bytes(&original).expect("encodes");
        for cut in 0..bytes.len() {
            let _ = from_bytes::<Hello>(bytes.slice(..cut));
        }
    }

    #[test]
    fn timestamps_survive_the_epoch_conversion() {
        let now = Timestamp::from_unix_ms(1_800_000_000_000);
        let original = Ping { client_time: now };
        let decoded: Ping = from_bytes(to_bytes(&original).expect("encodes")).expect("decodes");
        assert_eq!(decoded.client_time, now);
        assert_eq!(decoded.client_time.as_unix_ms(), 1_800_000_000_000);
    }
}
