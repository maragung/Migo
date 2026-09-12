//! The call's video half: the remote picture, depacketized, decoded, and handed to the
//! overlay as pixels — the receiving side of the stage the web client's overlay renders.
//!
//! # What is here, and what deliberately is not
//!
//! This build *answers* a video call with a working video stage and *places* none: the
//! desktop has no camera stack (a cross-platform capture dependency is a build risk this
//! client does not carry), so a video invite from the web client is answered with a
//! RecvOnly video m-line and the incoming track is rendered full-stage, while the chat
//! header's call button keeps placing voice calls. The web client's answer path makes the
//! symmetric trade the other way (`answerMediaWithFallback`): a video invite answered where
//! only a microphone exists is still a call worth taking.
//!
//! # The pipeline
//!
//! WebRTC hands the remote track's RTP packets to [`video_pump`], which:
//!
//! 1. strips the VP8 RTP payload descriptor with the `rtp` crate's depacketizer — the same
//!    descriptor code the sender's packetizer wrote;
//! 2. reassembles the fragments of one frame by RTP timestamp (a VP8 frame spans as many
//!    packets as the encoder's bitrate at the connection's MTU demands, and the descriptor's
//!    start-of-partition bit marks the first fragment of a frame's first partition);
//! 3. decodes the assembled elementary frame with `oxideav-vp8`'s stateful decoder, which
//!    carries the reference frames and entropy state the inter-frames predict against;
//! 4. converts the I420 planes to RGBA;
//! 5. parks the latest decoded frame in a shared slot the overlay reads each repaint.
//!
//! The slot is latest-frame-wins on purpose: a call is live media, not a mailbox, and a
//! frame the overlay never drew is a frame whose moment has passed. The decoder runs on the
//! track's own pump task, off the UI thread; the slot is a mutex, held for the duration of
//! one pointer swap and never across an await.
//!
//! # The keyframe request
//!
//! A decoder that joins a stream mid-GOP cannot show anything until the next key frame, so
//! the pump asks for one: a PLI over the peer connection's RTCP path, repeated at a
//! civilized interval until a frame decodes. The same request covers a corrupted reference
//! chain after packet loss — the decoder refuses the frame, the pump asks again, the sender
//! obliges; that is what the feedback exists for.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use webrtc::rtp::codecs::vp8::Vp8Packet;
use webrtc::rtp::packetizer::Depacketizer;
use webrtc::track::track_remote::TrackRemote;

use oxideav_vp8::Vp8DecoderState;

/// One decoded frame of the remote video, ready for a texture.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// RGBA, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Where the overlay reads the picture from: the latest decoded frame, or none yet.
///
/// `Arc<Mutex<..>>` because the pump task writes it and the UI thread reads it; the lock is
/// held only for the swap, never across an await.
pub type VideoSlot = Arc<Mutex<Option<VideoFrame>>>;

/// How long the pump waits before repeating a keyframe request. Generous on purpose: the
/// sender decides when to honour a PLI, and a request per received packet would be the pump
/// nagging, not negotiating.
const PLI_INTERVAL: Duration = Duration::from_millis(500);

/// The frame-size cap the decoder enforces on the remote's declared dimensions. The default
/// (32k×32k) is a parse-time DoS cap, not a video opinion; a call in this client has no
/// business decoding past 4K, and a smaller cap bounds the RGBA conversion too.
const MAX_PIXELS_PER_FRAME: u64 = 4096 * 4096;

/// The I420-to-RGBA conversion: BT.601 limited range, the standard matrix every soft path
/// uses, so a frame decodes to the colours the sender's encoder thought it was sending.
fn i420_to_rgba(frame: &oxideav_vp8::Vp8DecodedFrame) -> VideoFrame {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let chroma_width = width.div_ceil(2);
    let y_plane = frame.y.as_slice();
    let u_plane = frame.u.as_slice();
    let v_plane = frame.v.as_slice();
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let chroma_offset = (y / 2) * chroma_width + x / 2;
            let chroma_u = f32::from(u_plane[chroma_offset]) - 128.0;
            let chroma_v = f32::from(v_plane[chroma_offset]) - 128.0;
            // BT.601 inverse: the luma's 16..235 window stretched back to 0..255, the
            // chroma's offsets applied with the standard coefficients, each channel
            // clamped before the round. f32 throughout — the frame is at most 4K at call
            // framerates, and exactness beats speed here.
            let luma = 1.164_383 * (f32::from(y_plane[y * width + x]) - 16.0);
            let r = luma + 1.596_027 * chroma_v;
            let g = luma - 0.391_762 * chroma_u - 0.812_968 * chroma_v;
            let b = luma + 2.017_232 * chroma_u;
            rgba.extend_from_slice(&[
                r.clamp(0.0, 255.0).round() as u8,
                g.clamp(0.0, 255.0).round() as u8,
                b.clamp(0.0, 255.0).round() as u8,
                255,
            ]);
        }
    }
    VideoFrame {
        width: frame.width,
        height: frame.height,
        rgba,
    }
}

/// Feeds the remote video track into the shared slot: depacketize, reassemble, decode, park.
///
/// `pli` asks the peer for a key frame (the sender's encoder obliges); it is fired until the
/// first frame decodes and again whenever a decode fails after loss. A read that fails is
/// the track closing — the call ended or the transport died — and the pump returns; the slot
/// keeps the last frame, harmless: the overlay stops drawing video the moment the phase
/// leaves Connected, so a parked frame is a frame nobody renders.
pub async fn video_pump(
    track: Arc<TrackRemote>,
    slot: VideoSlot,
    pli: Arc<dyn Fn() + Send + Sync>,
) {
    let mut depacketizer = Vp8Packet::default();
    let mut decoder = Vp8DecoderState::new().with_max_pixels_per_frame(MAX_PIXELS_PER_FRAME);
    // The frame being reassembled: its RTP timestamp, and the bytes gathered so far. A frame
    // spans however many packets the MTU forced; the descriptor's start bit begins the first
    // fragment of the frame's first partition, and the marker bit ends the frame.
    let mut assembly: Option<(u32, Vec<u8>)> = None;
    // Set so the first mid-frame join fires a PLI immediately rather than after the interval.
    let mut last_request = tokio::time::Instant::now()
        .checked_sub(PLI_INTERVAL)
        .unwrap_or_else(tokio::time::Instant::now);
    loop {
        let Ok((packet, _)) = track.read_rtp().await else {
            return;
        };
        // The payload descriptor off, the fragment bytes in hand.
        let fragment = match depacketizer.depacketize(&packet.payload) {
            Ok(fragment) => fragment,
            // A descriptor this build cannot parse is one fragment dropped of one frame;
            // the partial assembly is poison now, so drop it too and let the keyframe path
            // recover rather than feeding the decoder a frame with a hole in it.
            Err(_) => {
                assembly = None;
                continue;
            }
        };
        let timestamp = packet.header.timestamp;
        let start = depacketizer.s != 0;
        match &mut assembly {
            None if !start => {
                // A continuation with nothing to continue: the pump joined mid-frame, or the
                // frame's first fragments were lost. Only a key frame can start from here.
                request_keyframe(&pli, &mut last_request);
                continue;
            }
            None => assembly = Some((timestamp, fragment.to_vec())),
            Some((assembled_at, bytes)) if *assembled_at == timestamp => {
                // The same frame, continuing.
                bytes.extend_from_slice(&fragment);
            }
            Some(_) => {
                // A new frame began under the old one's feet: the old frame's marker was
                // lost, so its assembly is complete by default. Decode it, then start the
                // new one with this fragment if this fragment starts one.
                let complete = assembly.take().map(|(_, bytes)| bytes).unwrap_or_default();
                decode_into(&mut decoder, &complete, &slot, &pli, &mut last_request);
                assembly = if start {
                    Some((timestamp, fragment.to_vec()))
                } else {
                    None
                };
            }
        }
        // The marker bit set means this packet ended the frame: decode it now rather than
        // waiting for the next timestamp to observe the boundary.
        if packet.header.marker {
            let complete = assembly.take().map(|(_, bytes)| bytes);
            if let Some(bytes) = complete {
                decode_into(&mut decoder, &bytes, &slot, &pli, &mut last_request);
            }
        }
    }
}

/// Decodes one assembled frame, parking it in the slot when it is for display, and asking
/// for a key frame when it cannot be decoded.
fn decode_into(
    decoder: &mut Vp8DecoderState,
    bytes: &[u8],
    slot: &VideoSlot,
    pli: &Arc<dyn Fn() + Send + Sync>,
    last_request: &mut tokio::time::Instant,
) {
    match decoder.decode_frame(bytes) {
        Ok(decoded) => {
            if decoder.last_frame_shown() != Some(false) {
                let frame = i420_to_rgba(&decoded);
                if let Ok(mut guard) = slot.lock() {
                    *guard = Some(frame);
                }
            }
            // An invisible altref update: reference state moved, nothing to show — the
            // next visible frame will say what this one set up.
        }
        Err(_) => {
            // The stream predates this decoder's state (joined mid-GOP), or loss broke the
            // reference chain: a key frame is the recovery, rate-limited so a bad patch of
            // stream is not a PLI storm.
            request_keyframe(pli, last_request);
        }
    }
}

/// Fires the keyframe request if the interval since the last one has elapsed.
fn request_keyframe(pli: &Arc<dyn Fn() + Send + Sync>, last_request: &mut tokio::time::Instant) {
    if last_request.elapsed() >= PLI_INTERVAL {
        *last_request = tokio::time::Instant::now();
        pli();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame's conversion: the shape is preserved and every pixel comes out opaque. The
    /// exact RGB is not pinned — the matrix is the standard's, not ours to restate in a
    /// test — but the geometry and full alpha are this module's contract with the texture.
    #[test]
    fn an_i420_frame_becomes_opaque_rgba_of_the_same_shape() {
        let frame = oxideav_vp8::Vp8DecodedFrame {
            width: 4,
            height: 4,
            y: vec![128u8; 16],
            u: vec![128u8; 4],
            v: vec![128u8; 4],
        };
        let converted = i420_to_rgba(&frame);
        assert_eq!(converted.width, 4);
        assert_eq!(converted.height, 4);
        assert_eq!(converted.rgba.len(), 4 * 4 * 4);
        assert!(
            converted
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| px[3] == 255),
            "every pixel is opaque"
        );
    }

    /// The neutral chroma (128, 128) must land on the grey its luma says: the matrix's
    /// centre is the one value whose channel balance a test can state without restating
    /// the matrix itself.
    #[test]
    fn neutral_chroma_keeps_the_luma_grey() {
        let frame = oxideav_vp8::Vp8DecodedFrame {
            width: 2,
            height: 2,
            y: vec![128u8; 4],
            u: vec![128u8; 1],
            v: vec![128u8; 1],
        };
        let converted = i420_to_rgba(&frame);
        let px = &converted.rgba[..4];
        assert!(
            (i16::from(px[0]) - i16::from(px[1])).abs() <= 2
                && (i16::from(px[1]) - i16::from(px[2])).abs() <= 2,
            "neutral chroma is a grey, not a tint: {px:?}"
        );
    }

    /// Luma 16 with neutral chroma is studio black: all three channels clamp to 0.
    #[test]
    fn studio_black_clamps_to_zero() {
        let frame = oxideav_vp8::Vp8DecodedFrame {
            width: 1,
            height: 1,
            y: vec![16u8],
            u: vec![128u8],
            v: vec![128u8],
        };
        let converted = i420_to_rgba(&frame);
        assert_eq!(&converted.rgba[..3], &[0, 0, 0]);
    }

    /// Luma 235 with neutral chroma is studio white: all three channels clamp to 255.
    #[test]
    fn studio_white_clamps_to_full() {
        let frame = oxideav_vp8::Vp8DecodedFrame {
            width: 1,
            height: 1,
            y: vec![235u8],
            u: vec![128u8],
            v: vec![128u8],
        };
        let converted = i420_to_rgba(&frame);
        assert_eq!(&converted.rgba[..3], &[255, 255, 255]);
    }
}
