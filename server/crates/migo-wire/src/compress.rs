//! Payload compression.
//!
//! Raw DEFLATE (RFC 1951), and the choice of algorithm was made for the *client*
//! rather than the server. Browsers ship `CompressionStream('deflate-raw')`
//! natively, so the web client gets compression for zero bundle bytes. Brotli
//! would compress a little better and cost every user a WASM download; zstd is
//! not in browsers at all. See ADR-0002.
//!
//! WebSocket `permessage-deflate` is deliberately disabled in favour of this.
//! Per-message compression at the protocol layer lets us apply a *policy* —
//! compress only when it pays — and keeps a shared compression window from
//! leaking information between messages.
//!
//! Two guards, both mandatory:
//!
//! * **Never expand.** Compression that does not save at least
//!   [`COMPRESS_MIN_GAIN_PERCENT`] is discarded, and payloads under
//!   [`COMPRESS_MIN_BYTES`] are never attempted. A 40-byte typing indicator
//!   grows under DEFLATE.
//! * **Bounded inflation.** Decompression stops at [`MAX_FRAME_BYTES`]. A few
//!   hundred bytes of crafted DEFLATE can otherwise expand to gigabytes, which
//!   makes an unbounded decompressor a remote kill switch.

use std::io::Write;

use bytes::Bytes;
use flate2::write::DeflateEncoder;
use flate2::Compression;

use crate::error::{Result, WireError};
use crate::limits::{COMPRESS_MIN_BYTES, COMPRESS_MIN_GAIN_PERCENT, MAX_FRAME_BYTES};

/// Compression level. Level 6 is the usual quality/CPU knee; on a chat payload
/// the difference to level 9 is under one percent for roughly twice the CPU, and
/// this runs on the fanout path.
const LEVEL: Compression = Compression::new(6);

/// Compresses `payload` with raw DEFLATE.
#[must_use]
pub fn deflate_raw(payload: &[u8]) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::with_capacity(payload.len() / 2 + 32), LEVEL);
    // Writing to a Vec cannot fail, and neither can finishing it.
    let _ = encoder.write_all(payload);
    encoder.finish().unwrap_or_else(|_| payload.to_vec())
}

/// Applies the compression policy.
///
/// Returns `Some(compressed)` only when compression is worth the CPU on both
/// sides; otherwise `None`, and the caller sends the payload uncompressed with
/// the `COMPRESSED` flag clear.
#[must_use]
pub fn maybe_deflate(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() < COMPRESS_MIN_BYTES {
        return None;
    }
    let compressed = deflate_raw(payload);
    if compressed.len() >= payload.len() {
        return None;
    }
    let saved = payload.len() - compressed.len();
    let gain_percent = (saved * 100) / payload.len();
    if gain_percent < COMPRESS_MIN_GAIN_PERCENT as usize {
        return None;
    }
    Some(compressed)
}

/// Decompresses raw DEFLATE, refusing to produce more than `max` bytes.
///
/// Two properties that a `read_to_end`-shaped decoder cannot give, both found by
/// the `compress.json` conformance vectors:
///
/// * **Truncation is refusal, not short output.** `read_to_end` returns `Ok`
///   when the *reader* is exhausted, and a DEFLATE reader cut mid-block reports
///   `Ok(0)` — indistinguishable from a clean end — so a truncated stream came
///   back as the partial bytes and the caller never learned the message was cut.
///   The web and Android ports already refuse that input (the platform's
///   `DecompressionStream` errors on it; the Kotlin loop exits only on
///   `finished()`), which made this crate the odd one out in a 3-vs-1. The fix
///   is to demand proof the stream ended: [`flate2::Status::StreamEnd`].
/// * **A bomb costs one chunk, not its full expansion.** Output is produced into
///   an 8 KiB scratch buffer and the limit is checked after every chunk, so the
///   `Vec` never holds more than `limit + CHUNK` bytes and a stream that expands
///   past the limit is detected on its first overflowing chunk.
///
/// When a stream is both over the limit and truncated, the size error wins: it
/// is the more specific fault, and it is checked first so the decision is
/// visible in the code rather than implied by ordering elsewhere.
pub fn inflate_raw(compressed: &[u8], max: usize) -> Result<Bytes> {
    let limit = max.min(MAX_FRAME_BYTES);
    const CHUNK: usize = 8 * 1024;
    let mut scratch = vec![0u8; CHUNK];
    let mut out = Vec::with_capacity(compressed.len().saturating_mul(4).min(limit));
    // Raw DEFLATE: no zlib header, no Adler-32 trailer.
    let mut inflater = flate2::Decompress::new(true);

    loop {
        let before_in = inflater.total_in() as usize;
        let before_out = inflater.total_out() as usize;
        let status = inflater
            .decompress(
                &compressed[before_in..],
                &mut scratch,
                flate2::FlushDecompress::Finish,
            )
            .map_err(|_| WireError::DecompressFailed)?;
        let consumed = inflater.total_in() as usize - before_in;
        let produced = inflater.total_out() as usize - before_out;
        out.extend_from_slice(&scratch[..produced]);
        if out.len() > limit {
            return Err(WireError::DecompressedTooLarge { max: limit });
        }
        match status {
            flate2::Status::StreamEnd => {
                // Bytes after the final block are ignored rather than refused.
                // That is a deliberate three-way agreement, not an oversight:
                // the web port decompresses through `DecompressionStream`,
                // whose API reports no consumed-count, so it cannot see them,
                // and one port accepting what another refuses is the divergence
                // this crate's own vectors exist to catch.
                break;
            }
            _ if consumed == 0 && produced == 0 => {
                // No progress in either direction with the input gone means the
                // stream was cut before its final block. The reader running out
                // is not the stream ending, and returning the partial bytes
                // would pass a truncated message off as a whole one.
                return Err(WireError::DecompressFailed);
            }
            _ => {}
        }
    }
    Ok(Bytes::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let payload = "halo dunia ".repeat(200).into_bytes();
        let compressed = deflate_raw(&payload);
        assert!(compressed.len() < payload.len());
        let restored = inflate_raw(&compressed, MAX_FRAME_BYTES).expect("inflates");
        assert_eq!(restored, payload);
    }

    #[test]
    fn small_payloads_are_not_compressed() {
        let payload = vec![0u8; COMPRESS_MIN_BYTES - 1];
        assert!(maybe_deflate(&payload).is_none());
    }

    #[test]
    fn incompressible_payloads_are_not_compressed() {
        // A pseudo-random payload has no redundancy for DEFLATE to remove, so the
        // policy must decline rather than send something larger.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let payload: Vec<u8> = (0..COMPRESS_MIN_BYTES * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect();
        assert!(
            maybe_deflate(&payload).is_none(),
            "must refuse to expand a payload"
        );
    }

    #[test]
    fn compressible_payloads_above_the_floor_are_compressed() {
        let payload = vec![b'a'; COMPRESS_MIN_BYTES * 2];
        let compressed = maybe_deflate(&payload).expect("worth compressing");
        assert!(
            compressed.len() * 10 < payload.len(),
            "highly redundant input"
        );
    }

    #[test]
    fn a_marginal_gain_is_declined() {
        // Mostly random with a small repeated tail: compresses, but not by 10%.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut payload: Vec<u8> = (0..COMPRESS_MIN_BYTES * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect();
        payload.extend(std::iter::repeat_n(b'z', 64));
        if let Some(compressed) = maybe_deflate(&payload) {
            let gain = ((payload.len() - compressed.len()) * 100) / payload.len();
            assert!(
                gain >= COMPRESS_MIN_GAIN_PERCENT as usize,
                "accepted a {gain}% gain"
            );
        }
    }

    #[test]
    fn a_truncated_stream_is_refused() {
        // A stream cut before its final block must not come back as the partial
        // bytes: the reader running out is not the stream ending. The web and
        // Android ports already refuse this input, and the `truncated_fixed_block`
        // case in compress.json pins all three to the same answer.
        let payload = "ab".repeat(24).into_bytes();
        let whole = deflate_raw(&payload);
        assert_eq!(
            inflate_raw(&whole, MAX_FRAME_BYTES).expect("whole inflates"),
            payload
        );
        let cut = &whole[..whole.len() - 1];
        assert_eq!(
            inflate_raw(cut, MAX_FRAME_BYTES),
            Err(WireError::DecompressFailed)
        );
    }

    #[test]
    fn a_decompression_bomb_is_refused() {
        // 8 MiB of zeros compresses to a few kilobytes; inflating it must stop at
        // the frame limit rather than allocate 8 MiB.
        let bomb = deflate_raw(&vec![0u8; 8 * 1024 * 1024]);
        assert!(bomb.len() < 16 * 1024, "bomb is {} bytes", bomb.len());
        assert_eq!(
            inflate_raw(&bomb, MAX_FRAME_BYTES),
            Err(WireError::DecompressedTooLarge {
                max: MAX_FRAME_BYTES
            })
        );
    }

    #[test]
    fn a_payload_exactly_at_the_limit_is_accepted() {
        let payload = vec![7u8; MAX_FRAME_BYTES];
        let compressed = deflate_raw(&payload);
        let restored = inflate_raw(&compressed, MAX_FRAME_BYTES).expect("inflates");
        assert_eq!(restored.len(), MAX_FRAME_BYTES);
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(
            inflate_raw(&[0xFF, 0xFF, 0xFF, 0xFF], MAX_FRAME_BYTES),
            Err(WireError::DecompressFailed)
        );
    }
}
