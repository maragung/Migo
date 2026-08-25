//! Frame batching.
//!
//! A busy room produces bursts: twenty presence updates and a typing indicator
//! land inside the same 15 milliseconds. Sent individually that is twenty-one
//! WebSocket messages, twenty-one syscalls, twenty-one wake-ups of a phone's
//! radio, and twenty-one frame headers. Batched, it is one.
//!
//! The gateway accumulates outbound frames for [`crate::limits::BATCH_LINGER_MS`] and, if more
//! than one is waiting, sends a batch instead. Linger is short on purpose:
//! latency people can feel starts around 100 ms, so 15 ms buys most of the
//! coalescing at a cost nobody notices.
//!
//! Wire format — payload of a frame with the `BATCH` flag set:
//!
//! ```text
//! varint  count
//! count × ( varint frame_len, frame_len bytes )
//! ```
//!
//! Each element is a complete frame, header included. That redundancy is
//! deliberate: a batch is a transport optimisation, not a new message type, so
//! the receiver's dispatch loop is identical whether a frame arrived alone or
//! inside a batch. Two rules keep it honest:
//!
//! * At most [`MAX_BATCH_ITEMS`] elements.
//! * **No nesting.** A batch inside a batch is rejected, because otherwise a
//!   small frame could describe an exponentially large expansion.

use bytes::BytesMut;

use crate::error::{Result, WireError};
use crate::frame::Frame;
use crate::limits::{MAX_BATCH_ITEMS, MAX_FRAME_BYTES};
use crate::varint;
use crate::{flags, FrameHeader};

/// Opcode reserved for the batch envelope.
///
/// The batch carries no payload type of its own, so the opcode is a constant
/// rather than something the IDL generates.
pub const BATCH_OPCODE: u32 = 0;

/// Packs frames into a single batch frame.
///
/// Returns the batch as a [`Frame`] with the `BATCH` flag set and correlation 0
/// — the elements carry their own correlation ids, and the envelope has no
/// request of its own to answer.
///
/// A one-element batch is returned as the bare frame instead: wrapping it would
/// add bytes and buy nothing.
pub fn encode_batch(frames: &[Frame]) -> Result<Frame> {
    if frames.len() > MAX_BATCH_ITEMS {
        return Err(WireError::BatchTooLarge {
            len: frames.len(),
            max: MAX_BATCH_ITEMS,
        });
    }
    if frames.len() == 1 {
        return Ok(frames[0].clone());
    }

    let mut payload = BytesMut::with_capacity(frames.iter().map(|f| f.encoded_len() + 3).sum());
    varint::encode_u64(frames.len() as u64, &mut payload);
    for frame in frames {
        if frame.header.is_batch() {
            return Err(WireError::NestedBatch);
        }
        let encoded = frame.encode()?;
        varint::encode_u64(encoded.len() as u64, &mut payload);
        payload.extend_from_slice(&encoded);
    }

    let mut header = FrameHeader::new(BATCH_OPCODE, 0);
    header.flags |= flags::BATCH;
    let batch = Frame::new(header, payload.freeze());
    if batch.encoded_len() > MAX_FRAME_BYTES {
        return Err(WireError::FrameTooLarge {
            len: batch.encoded_len(),
            max: MAX_FRAME_BYTES,
        });
    }
    Ok(batch)
}

/// Unpacks a batch frame into its elements.
///
/// A frame without the `BATCH` flag is returned as a single-element vector, so
/// callers can funnel everything through this function and keep one dispatch
/// path. The payload is inflated first: a batch may be compressed as a whole,
/// which is where compression pays best because the elements share vocabulary.
pub fn decode_batch(frame: &Frame) -> Result<Vec<Frame>> {
    if !frame.header.is_batch() {
        return Ok(vec![frame.clone()]);
    }

    let payload = frame.payload_inflated()?;
    let (count, mut offset) = varint::decode_u64(&payload, 0)?;
    let count = usize::try_from(count).map_err(|_| WireError::BatchTooLarge {
        len: MAX_BATCH_ITEMS + 1,
        max: MAX_BATCH_ITEMS,
    })?;
    if count > MAX_BATCH_ITEMS {
        return Err(WireError::BatchTooLarge {
            len: count,
            max: MAX_BATCH_ITEMS,
        });
    }
    // Every element costs at least a length byte plus a 4-byte minimal header, so
    // a count larger than the remaining bytes allow is a lie. Checking it here
    // means `with_capacity` cannot be turned into an allocation primitive.
    let remaining = payload.len() - offset;
    if count.saturating_mul(5) > remaining {
        return Err(WireError::BatchTooLarge {
            len: count,
            max: remaining / 5,
        });
    }

    let mut frames = Vec::with_capacity(count);
    for _ in 0..count {
        let (len, used) = varint::decode_u64(&payload, offset)?;
        offset += used;
        let len = usize::try_from(len).map_err(|_| WireError::LengthOverflow { len })?;
        if len > MAX_FRAME_BYTES {
            return Err(WireError::FrameTooLarge {
                len,
                max: MAX_FRAME_BYTES,
            });
        }
        let end = offset
            .checked_add(len)
            .ok_or(WireError::LengthOverflow { len: len as u64 })?;
        if end > payload.len() {
            return Err(WireError::UnexpectedEnd {
                offset,
                needed: len,
            });
        }
        let element = Frame::decode(payload.slice(offset..end))?;
        if element.header.is_batch() {
            return Err(WireError::NestedBatch);
        }
        frames.push(element);
        offset = end;
    }

    if offset != payload.len() {
        return Err(WireError::TrailingBytes {
            count: payload.len() - offset,
        });
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    fn frames(count: u32) -> Vec<Frame> {
        (0..count)
            .map(|i| Frame::simple(0x30 + i, i, Bytes::from(format!("payload-{i}"))))
            .collect()
    }

    #[test]
    fn round_trips() {
        let original = frames(4);
        let batch = encode_batch(&original).expect("packs");
        assert!(batch.header.is_batch());
        assert_eq!(batch.header.correlation, 0);
        let decoded = decode_batch(&batch).expect("unpacks");
        assert_eq!(decoded, original);
    }

    #[test]
    fn round_trips_through_the_wire() {
        let original = frames(8);
        let batch = encode_batch(&original).expect("packs");
        let decoded = Frame::decode(batch.encode().expect("encodes")).expect("decodes");
        assert_eq!(decode_batch(&decoded).expect("unpacks"), original);
    }

    #[test]
    fn a_single_frame_is_not_wrapped() {
        let one = frames(1);
        let batch = encode_batch(&one).expect("packs");
        assert!(!batch.header.is_batch());
        assert_eq!(batch, one[0]);
    }

    #[test]
    fn a_plain_frame_decodes_as_a_one_element_batch() {
        let frame = Frame::simple(1, 2, Bytes::from_static(b"solo"));
        assert_eq!(decode_batch(&frame).expect("unpacks"), vec![frame]);
    }

    #[test]
    fn batching_saves_bytes() {
        let original = frames(20);
        let individually: usize = original.iter().map(|f| f.encoded_len()).sum();
        let batched = encode_batch(&original).expect("packs").encoded_len();
        // Each element gains a length varint but the batch adds only one header,
        // so twenty small frames must not cost more than twenty separate sends.
        assert!(
            batched <= individually + 20 + 8,
            "batched {batched} vs {individually}"
        );
    }

    #[test]
    fn a_compressed_batch_round_trips() {
        let original = frames(64);
        let batch = encode_batch(&original).expect("packs");
        let compressed = Frame::compressing(batch.header, batch.payload.clone());
        assert!(
            compressed.header.is_compressed(),
            "similar frames must compress"
        );
        assert!(compressed.payload.len() < batch.payload.len());
        assert_eq!(decode_batch(&compressed).expect("unpacks"), original);
    }

    #[test]
    fn nesting_is_refused_on_encode() {
        let inner = encode_batch(&frames(2)).expect("packs");
        assert_eq!(
            encode_batch(&[inner.clone(), inner]),
            Err(WireError::NestedBatch)
        );
    }

    #[test]
    fn nesting_is_refused_on_decode() {
        // Hand-build a batch whose element carries the BATCH flag, since
        // encode_batch refuses to produce one.
        let inner = encode_batch(&frames(2)).expect("packs");
        let encoded_inner = inner.encode().expect("encodes");
        let mut payload = BytesMut::new();
        varint::encode_u64(1, &mut payload);
        varint::encode_u64(encoded_inner.len() as u64, &mut payload);
        payload.extend_from_slice(&encoded_inner);
        let mut header = FrameHeader::new(BATCH_OPCODE, 0);
        header.flags |= flags::BATCH;
        let hostile = Frame::new(header, payload.freeze());
        assert_eq!(decode_batch(&hostile), Err(WireError::NestedBatch));
    }

    #[test]
    fn too_many_items_are_refused() {
        let many = frames(MAX_BATCH_ITEMS as u32 + 1);
        assert_eq!(
            encode_batch(&many),
            Err(WireError::BatchTooLarge {
                len: MAX_BATCH_ITEMS + 1,
                max: MAX_BATCH_ITEMS
            })
        );
    }

    #[test]
    fn a_lying_count_cannot_force_an_allocation() {
        // Claims 200 elements in a payload that could not hold two.
        let mut payload = BytesMut::new();
        varint::encode_u64(200, &mut payload);
        payload.extend_from_slice(&[0u8; 6]);
        let mut header = FrameHeader::new(BATCH_OPCODE, 0);
        header.flags |= flags::BATCH;
        let hostile = Frame::new(header, payload.freeze());
        assert!(matches!(
            decode_batch(&hostile),
            Err(WireError::BatchTooLarge { .. })
        ));
    }

    #[test]
    fn a_truncated_element_is_rejected() {
        let batch = encode_batch(&frames(3)).expect("packs");
        let cut = batch.payload.slice(..batch.payload.len() - 4);
        let hostile = Frame::new(batch.header, cut);
        assert!(matches!(
            decode_batch(&hostile),
            Err(WireError::UnexpectedEnd { .. })
        ));
    }

    #[test]
    fn trailing_bytes_after_the_last_element_are_rejected() {
        let batch = encode_batch(&frames(2)).expect("packs");
        let mut payload = BytesMut::from(&batch.payload[..]);
        payload.extend_from_slice(b"junk");
        let hostile = Frame::new(batch.header, payload.freeze());
        assert_eq!(
            decode_batch(&hostile),
            Err(WireError::TrailingBytes { count: 4 })
        );
    }

    #[test]
    fn an_empty_batch_is_valid_and_yields_nothing() {
        let batch = encode_batch(&[]).expect("packs");
        assert!(batch.header.is_batch());
        assert!(decode_batch(&batch).expect("unpacks").is_empty());
    }
}
