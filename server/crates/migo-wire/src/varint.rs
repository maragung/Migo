//! LEB128 varints.
//!
//! Seven bits of payload per byte, high bit as continuation. Small numbers cost
//! one byte, which is the whole reason the protocol uses them: sequence numbers,
//! opcodes, correlation ids, and field lengths are almost always small, and a
//! fixed 4-byte field would waste three bytes on nearly every one.
//!
//! Two strictness rules, both deliberate:
//!
//! * At most [`MAX_VARINT_BYTES`] bytes. A hostile peer must not be able to make
//!   the decoder spin over a long run of `0x80`.
//! * **Minimal encodings only.** `0x80 0x00` also decodes to zero, but it is
//!   rejected. The codec has to be canonical because mesh frames are signed:
//!   two byte sequences for one value would mean two valid signatures for one
//!   message, and dedup by hash would stop working.

use bytes::BufMut;

use crate::error::{Result, WireError};
use crate::limits::MAX_VARINT_BYTES;

/// Number of bytes [`encode_u64`] will write for `value`.
#[must_use]
pub fn encoded_len(value: u64) -> usize {
    // 64 bits at 7 bits per byte, with a floor of one byte for zero.
    let bits = 64 - value.leading_zeros() as usize;
    if bits == 0 {
        1
    } else {
        bits.div_ceil(7)
    }
}

/// Appends `value` to `out`.
///
/// Generic over [`BufMut`] so the same encoder serves the struct writer, which
/// builds into a `Vec<u8>`, and the frame header, which builds into a
/// `BytesMut`. One implementation means one place for the encoding to be right.
pub fn encode_u64<B: BufMut>(value: u64, out: &mut B) {
    let mut remaining = value;
    while remaining >= 0x80 {
        out.put_u8((remaining as u8) | 0x80);
        remaining >>= 7;
    }
    out.put_u8(remaining as u8);
}

/// Reads a varint from `input` at `offset`, returning the value and the number
/// of bytes consumed.
pub fn decode_u64(input: &[u8], offset: usize) -> Result<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    let mut index = 0usize;

    loop {
        if index >= MAX_VARINT_BYTES {
            return Err(WireError::VarintTooLong {
                offset,
                max: MAX_VARINT_BYTES,
            });
        }
        let byte = *input.get(offset + index).ok_or(WireError::UnexpectedEnd {
            offset: offset + index,
            needed: 1,
        })?;
        index += 1;

        let payload = u64::from(byte & 0x7F);
        // The tenth byte may only contribute the single remaining bit of a u64.
        if shift == 63 && payload > 1 {
            return Err(WireError::VarintTooLong {
                offset,
                max: MAX_VARINT_BYTES,
            });
        }
        value |= payload << shift;

        if byte & 0x80 == 0 {
            // A multi-byte encoding whose final group is zero is padded, and
            // therefore not the canonical encoding of this value.
            if index > 1 && byte == 0 {
                return Err(WireError::NonMinimalVarint { offset });
            }
            return Ok((value, index));
        }
        shift += 7;
    }
}

/// Maps a signed value onto an unsigned one so that small magnitudes of either
/// sign stay short: `0, -1, 1, -2, 2 -> 0, 1, 2, 3, 4`.
#[must_use]
pub const fn zigzag_encode(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

/// Inverse of [`zigzag_encode`].
#[must_use]
pub const fn zigzag_decode(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(value: u64) {
        let mut buf = Vec::new();
        encode_u64(value, &mut buf);
        assert_eq!(
            buf.len(),
            encoded_len(value),
            "length prediction for {value}"
        );
        let (decoded, used) = decode_u64(&buf, 0).expect("decodes");
        assert_eq!(decoded, value);
        assert_eq!(used, buf.len());
    }

    #[test]
    fn round_trips_boundary_values() {
        for value in [
            0,
            1,
            127,
            128,
            255,
            256,
            16_383,
            16_384,
            u32::MAX as u64,
            u64::MAX,
        ] {
            round_trip(value);
        }
    }

    #[test]
    fn small_values_cost_one_byte() {
        assert_eq!(encoded_len(0), 1);
        assert_eq!(encoded_len(127), 1);
        assert_eq!(encoded_len(128), 2);
        assert_eq!(encoded_len(u64::MAX), 10);
    }

    #[test]
    fn known_encodings_are_stable() {
        // These bytes are part of the protocol contract and appear in the
        // cross-language test vectors.
        let cases: &[(u64, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (127, &[0x7F]),
            (128, &[0x80, 0x01]),
            (300, &[0xAC, 0x02]),
            (16_384, &[0x80, 0x80, 0x01]),
        ];
        for (value, expected) in cases {
            let mut buf = Vec::new();
            encode_u64(*value, &mut buf);
            assert_eq!(&buf, expected, "encoding of {value}");
        }
    }

    #[test]
    fn rejects_non_minimal_encodings() {
        assert_eq!(
            decode_u64(&[0x80, 0x00], 0),
            Err(WireError::NonMinimalVarint { offset: 0 })
        );
        assert_eq!(
            decode_u64(&[0x81, 0x80, 0x00], 0),
            Err(WireError::NonMinimalVarint { offset: 0 })
        );
    }

    #[test]
    fn rejects_overlong_encodings() {
        let bomb = [0x80u8; 16];
        assert_eq!(
            decode_u64(&bomb, 0),
            Err(WireError::VarintTooLong {
                offset: 0,
                max: MAX_VARINT_BYTES
            })
        );
    }

    #[test]
    fn rejects_values_that_overflow_u64() {
        // Ten bytes whose final group carries more than the one remaining bit.
        let overflow = [0xFFu8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F];
        assert!(decode_u64(&overflow, 0).is_err());
    }

    #[test]
    fn rejects_truncated_input() {
        assert_eq!(
            decode_u64(&[0x80], 0),
            Err(WireError::UnexpectedEnd {
                offset: 1,
                needed: 1
            })
        );
        assert!(decode_u64(&[], 0).is_err());
    }

    #[test]
    fn decodes_at_an_offset() {
        let buf = [0xFF, 0xFF, 0xAC, 0x02];
        let (value, used) = decode_u64(&buf, 2).expect("decodes");
        assert_eq!((value, used), (300, 2));
    }

    #[test]
    fn zigzag_keeps_small_magnitudes_small() {
        for (signed, unsigned) in [(0i64, 0u64), (-1, 1), (1, 2), (-2, 3), (2, 4)] {
            assert_eq!(zigzag_encode(signed), unsigned);
            assert_eq!(zigzag_decode(unsigned), signed);
        }
        assert_eq!(zigzag_decode(zigzag_encode(i64::MIN)), i64::MIN);
        assert_eq!(zigzag_decode(zigzag_encode(i64::MAX)), i64::MAX);
    }
}
