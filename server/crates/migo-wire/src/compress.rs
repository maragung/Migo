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

use std::io::{Read, Write};

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
pub fn inflate_raw(compressed: &[u8], max: usize) -> Result<Bytes> {
    let limit = max.min(MAX_FRAME_BYTES);
    let mut out = Vec::with_capacity(compressed.len().saturating_mul(4).min(limit));
    // Read one byte past the limit so an oversized payload is detected rather
    // than silently truncated.
    let mut decoder = flate2::read::DeflateDecoder::new(compressed).take(limit as u64 + 1);
    decoder
        .read_to_end(&mut out)
        .map_err(|_| WireError::DecompressFailed)?;
    if out.len() > limit {
        return Err(WireError::DecompressedTooLarge { max: limit });
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
