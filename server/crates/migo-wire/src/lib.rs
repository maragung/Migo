//! MWP/1 framing and Migo Struct Encoding — the binary codec.
//!
//! This crate is layer 1: it depends on `migo-core` and on nothing else of ours.
//! It knows how to turn bytes into structs and back, and it knows nothing at all
//! about chat, rooms, or users. Every generated protocol type in `migo-protocol`
//! is built from the two traits defined here, which is why the code generator
//! never has to emit byte manipulation.
//!
//! # Why not JSON, protobuf, or MessagePack
//!
//! Migo targets phones on 3G in places where a megabyte costs real money, and
//! the app is a chat app: thousands of small messages, not a few large ones. At
//! that shape the per-message overhead *is* the bandwidth bill.
//!
//! JSON re-transmits every field name on every message and needs 20 characters
//! for a timestamp. Protobuf tags every field, which is the right trade for a
//! schema that evolves freely but the wrong one when the first eight fields are
//! present in literally every message. MessagePack is JSON's shape with shorter
//! syntax.
//!
//! MSE splits the difference:
//!
//! * **Required fields are positional.** They appear in schema order with no tag
//!   and no length — zero framing overhead. The price is that the required
//!   prefix of a struct is frozen for the life of a protocol version, which the
//!   code generator enforces.
//! * **Optional fields are tagged**, preceded by a count, each carrying a field
//!   id and a byte length. An unknown field id is skipped by its length, so a
//!   1.2 client and a 1.7 server interoperate with no negotiation step at all.
//!
//! On a typical `MessageDelivered` that is roughly a third of the equivalent
//! JSON, and the decode is a forward scan with no string hashing.
//!
//! # Rules this crate does not bend
//!
//! * **Every limit is checked before the allocation it bounds.** A length prefix
//!   from the network is an allocation request from a stranger. Each one is
//!   validated against both its configured maximum and the bytes actually
//!   present, so five bytes on the wire can never ask for four gigabytes of
//!   heap.
//! * **Encodings are canonical.** Non-minimal varints are rejected. Mesh frames
//!   are signed and deduplicated by hash, so two byte sequences that mean the
//!   same value would mean two valid signatures for one message.
//! * **Unknown is not the same as reserved.** Unknown *optional fields* are
//!   skipped, because that is the forward-compatibility mechanism. Reserved
//!   *flag bits* are rejected, because a receiver that ignores them makes it
//!   impossible to ever assign them a meaning.
//! * **Errors never quote payload bytes.** A decode failure reports offsets and
//!   lengths. Message text belongs in neither logs nor error strings.
//!
//! # Layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`varint`] | LEB128, strict and canonical |
//! | [`writer`] | MSE encoder |
//! | [`reader`] | MSE decoder |
//! | [`frame`] | MWP/1 header and frame |
//! | [`compress`] | raw DEFLATE with a policy and a bomb guard |
//! | [`batch`] | many frames in one transport message |
//! | [`limits`] | generated size bounds |
//! | [`flags`] | generated frame flag bits |
//!
//! `limits` and `flags` are generated from `shared/protocol/schema/` by
//! `tools/protocol-codegen`. Editing them by hand is pointless: `make
//! protocol-check` fails the build when they drift from the schema.
//!
//! # Example
//!
//! ```
//! use bytes::Bytes;
//! use migo_wire::{Decode, Encode, Frame, Reader, Result, Writer};
//!
//! /// The shape a code generator would emit.
//! #[derive(Debug, PartialEq)]
//! struct Ping {
//!     sequence: u64,
//!     note: Option<String>,
//! }
//!
//! impl Encode for Ping {
//!     fn encode(&self, w: &mut Writer) -> Result<()> {
//!         w.enter()?;
//!         w.write_u64(self.sequence);
//!         w.write_u32(u32::from(self.note.is_some()));
//!         if let Some(v) = &self.note {
//!             w.optional(1, |w| { w.write_str(v)?; Ok(()) })?;
//!         }
//!         w.leave();
//!         Ok(())
//!     }
//! }
//!
//! impl Decode for Ping {
//!     fn decode(r: &mut Reader) -> Result<Self> {
//!         r.enter()?;
//!         let sequence = r.read_u64()?;
//!         let mut note = None;
//!         let count = r.read_u32()?;
//!         for _ in 0..count {
//!             let (field_id, mut owned) = r.read_optional()?;
//!             let sub = &mut owned;
//!             match field_id {
//!                 1 => note = Some(sub.read_string()?),
//!                 _ => {} // Forward compatibility: a newer peer's field.
//!             }
//!         }
//!         r.leave();
//!         Ok(Ping { sequence, note })
//!     }
//! }
//!
//! # fn main() -> migo_wire::Result<()> {
//! let ping = Ping { sequence: 42, note: Some("hello".into()) };
//! let frame = Frame::simple(0x01, 1, migo_wire::to_bytes(&ping)?);
//! let received = Frame::decode(frame.encode()?)?;
//! assert_eq!(migo_wire::from_frame::<Ping>(&received)?, ping);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod batch;
pub mod compress;
pub mod error;
pub mod flags;
pub mod frame;
pub mod limits;
pub mod reader;
pub mod varint;
pub mod writer;

use bytes::Bytes;

pub use crate::batch::{decode_batch, encode_batch, BATCH_OPCODE};
pub use crate::compress::{deflate_raw, inflate_raw, maybe_deflate};
pub use crate::error::{Result, WireError};
pub use crate::frame::{Fragment, Frame, FrameHeader, TraceContext, PROTOCOL_VERSION};
pub use crate::reader::Reader;
pub use crate::writer::Writer;

/// A type that can be written in MSE.
///
/// Implementations are generated from the IDL. Hand-written ones exist only in
/// tests and in this crate's own documentation, because a hand-written encoder
/// that disagrees with its decoder is exactly the bug that a single source of
/// truth is there to prevent.
pub trait Encode {
    /// Appends `self` to `w`.
    ///
    /// Implementations must call [`Writer::enter`] first and [`Writer::leave`]
    /// last, so nesting depth stays bounded.
    fn encode(&self, w: &mut Writer) -> Result<()>;
}

/// A type that can be read from MSE.
pub trait Decode: Sized {
    /// Reads one value from `r`.
    ///
    /// Implementations must call [`Reader::enter`] first and [`Reader::leave`]
    /// last, and must skip unknown optional field ids rather than failing on
    /// them — that is what makes a rolling upgrade possible.
    fn decode(r: &mut Reader) -> Result<Self>;
}

/// Encodes a value to bytes.
pub fn to_bytes<T: Encode>(value: &T) -> Result<Bytes> {
    let mut writer = Writer::new();
    value.encode(&mut writer)?;
    writer.finish()
}

/// Decodes a value from bytes, requiring that nothing is left over.
///
/// Trailing bytes are an error rather than a curiosity. They mean the sender and
/// receiver disagree about the shape of the message, and continuing on that basis
/// is how a parsing bug becomes a security bug.
pub fn from_bytes<T: Decode>(input: Bytes) -> Result<T> {
    let mut reader = Reader::new(input);
    let value = T::decode(&mut reader)?;
    reader.finish()?;
    Ok(value)
}

/// Decodes a value from a frame's payload, inflating it first if needed.
pub fn from_frame<T: Decode>(frame: &Frame) -> Result<T> {
    from_bytes(frame.payload_inflated()?)
}

/// Encodes a value into a frame, compressing when the policy says it pays.
pub fn to_frame<T: Encode>(opcode: u32, correlation: u32, value: &T) -> Result<Frame> {
    let payload = to_bytes(value)?;
    Ok(Frame::compressing(
        FrameHeader::new(opcode, correlation),
        payload,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::{Id, Timestamp};

    /// Stands in for a generated struct: required fields positional, optional
    /// fields tagged, nested struct, list, and an id.
    #[derive(Debug, Clone, PartialEq)]
    struct Envelope {
        id: Id,
        sent_at: Timestamp,
        body: String,
        tags: Vec<String>,
        reply_to: Option<Id>,
    }

    impl Encode for Envelope {
        fn encode(&self, w: &mut Writer) -> Result<()> {
            w.enter()?;
            w.write_id(&self.id);
            w.write_timestamp(self.sent_at);
            w.write_str(&self.body)?;
            w.list_len(self.tags.len())?;
            for item in &self.tags {
                w.write_str(item)?;
            }
            w.write_u32(u32::from(self.reply_to.is_some()));
            if let Some(v) = &self.reply_to {
                w.optional(1, |w| {
                    w.write_id(v);
                    Ok(())
                })?;
            }
            w.leave();
            Ok(())
        }
    }

    impl Decode for Envelope {
        fn decode(r: &mut Reader) -> Result<Self> {
            r.enter()?;
            let id = r.read_id()?;
            let sent_at = r.read_timestamp()?;
            let body = r.read_string()?;
            let count = r.read_list_len()?;
            let mut tags = Vec::with_capacity(count);
            for _ in 0..count {
                tags.push(r.read_string()?);
            }
            let mut reply_to = None;
            let optional_count = r.read_u32()?;
            for _ in 0..optional_count {
                let (field_id, mut owned) = r.read_optional()?;
                let sub = &mut owned;
                if field_id == 1 {
                    reply_to = Some(sub.read_id()?);
                }
            }
            r.leave();
            Ok(Envelope {
                id,
                sent_at,
                body,
                tags,
                reply_to,
            })
        }
    }

    fn sample() -> Envelope {
        Envelope {
            id: Id::from_bytes([9u8; 16]),
            sent_at: Timestamp::from_millis(1_234_567),
            body: "selamat sore".into(),
            tags: vec!["a".into(), "bb".into()],
            reply_to: Some(Id::from_bytes([1u8; 16])),
        }
    }

    #[test]
    fn round_trips_through_bytes() {
        let encoded = to_bytes(&sample()).expect("encodes");
        assert_eq!(from_bytes::<Envelope>(encoded).expect("decodes"), sample());
    }

    #[test]
    fn round_trips_through_a_frame() {
        let frame = to_frame(0x21, 5, &sample()).expect("encodes");
        let received = Frame::decode(frame.encode().expect("encodes")).expect("decodes");
        assert_eq!(
            from_frame::<Envelope>(&received).expect("decodes"),
            sample()
        );
    }

    #[test]
    fn a_large_value_is_compressed_on_the_way_into_a_frame() {
        let mut big = sample();
        big.body = "berita panjang ".repeat(200);
        let frame = to_frame(0x21, 1, &big).expect("encodes");
        assert!(frame.header.is_compressed());
        assert_eq!(from_frame::<Envelope>(&frame).expect("decodes"), big);
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut encoded = to_bytes(&sample()).expect("encodes").to_vec();
        encoded.push(0);
        assert_eq!(
            from_bytes::<Envelope>(Bytes::from(encoded)),
            Err(WireError::TrailingBytes { count: 1 })
        );
    }

    #[test]
    fn a_newer_peers_unknown_field_is_skipped() {
        // Encode by hand with two optional fields, one of which this build has
        // never heard of. This is the rolling-upgrade case, and it must decode.
        let value = sample();
        let mut w = Writer::new();
        w.enter().expect("depth");
        w.write_id(&value.id);
        w.write_timestamp(value.sent_at);
        w.write_str(&value.body).expect("fits");
        w.list_len(value.tags.len()).expect("fits");
        for tag in &value.tags {
            w.write_str(tag).expect("fits");
        }
        w.write_u32(2);
        w.optional(1, |w| {
            w.write_id(value.reply_to.as_ref().expect("present"));
            Ok(())
        })
        .expect("fits");
        w.optional(9_999, |w| {
            w.write_str("a field from the future").expect("fits");
            w.write_u64(u64::MAX);
            Ok(())
        })
        .expect("fits");
        w.leave();

        let decoded = from_bytes::<Envelope>(w.finish().expect("finishes")).expect("decodes");
        assert_eq!(decoded, value);
    }

    #[test]
    fn frames_batch_and_unbatch_through_the_public_api() {
        let one = to_frame(0x21, 1, &sample()).expect("encodes");
        let two = to_frame(0x22, 2, &sample()).expect("encodes");
        let batch = encode_batch(&[one.clone(), two.clone()]).expect("packs");
        let received = Frame::decode(batch.encode().expect("encodes")).expect("decodes");
        let unpacked = decode_batch(&received).expect("unpacks");
        assert_eq!(unpacked, vec![one, two]);
    }

    #[test]
    fn a_truncated_payload_never_panics() {
        // Cut the encoding at every possible point. Each one must return an
        // error; none may panic, because every one of these is a frame a hostile
        // peer can send.
        let encoded = to_bytes(&sample()).expect("encodes");
        for cut in 0..encoded.len() {
            let result = from_bytes::<Envelope>(encoded.slice(..cut));
            assert!(result.is_err(), "a {cut}-byte prefix decoded successfully");
        }
    }

    #[test]
    fn a_corrupted_byte_never_panics() {
        let encoded = to_bytes(&sample()).expect("encodes");
        for index in 0..encoded.len() {
            for bit in 0..8u32 {
                let mut mutated = encoded.to_vec();
                mutated[index] ^= 1 << bit;
                // Success or failure are both acceptable. Panicking is not.
                let _ = from_bytes::<Envelope>(Bytes::from(mutated));
            }
        }
    }
}
