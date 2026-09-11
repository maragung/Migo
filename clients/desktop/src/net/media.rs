//! Media attachments: the byte-level contracts every Migo client speaks, and the codecs
//! this one needs to honour them.
//!
//! The module is pure by design — bytes in, bytes out, no sockets, no worker state — so
//! every rule that has to match the other clients byte for byte lives here where a test can
//! pin it. The web client's `lib/migo/media.ts` and `lib/migo/voice.ts` are the reference;
//! where this module states a number or a layout, it states what those files state.
//!
//! # The seal
//!
//! An attachment in an end-to-end conversation is sealed under a fresh XChaCha20-Poly1305
//! key before any bytes cross the wire. The sealed blob is `nonce ‖ ciphertext ‖ tag` —
//! no version byte, the same shape `migo_crypto::aead::seal` produces and the web's
//! `sealing.seal` produces. The associated data is the raw bytes of the domain label:
//! `b"migo-media"` for images and documents, `b"migo-voice"` for voice notes, so a blob
//! swapped between the two never opens.
//!
//! # The two paths
//!
//! A public or managed room's conversation is server-readable, and its content policy is
//! the server's: the room path uploads the *plaintext* bytes claiming their real MIME
//! type, with the key and nonce slots zero-filled so a receiver can tell the two paths
//! apart. The one exception is documents, which are sealed even into rooms — the same
//! exception the web client makes. Voice notes and images keep the room exception.
//!
//! # Voice notes
//!
//! This client records plain WAV — PCM 16-bit, mono, 8 kHz — which the server's sniffer
//! identifies from the `RIFF`…`WAVE` magic alone. What it *plays* is whatever the sender
//! recorded: Chrome and Firefox produce `audio/webm` (Opus in an EBML container), Safari
//! `audio/mp4`, so the decoder is a real demuxer-and-codec stack rather than a WAV parser.

use std::path::Path;
use std::sync::Arc;

use migo_core::Random;
use migo_crypto::aead::{self, SymmetricKey};

use crate::crypto::content::Content;
use crate::model::Body;

/// The associated-data label for image and document seals. The web client's
/// `MEDIA_SEAL_DOMAIN`, byte for byte.
pub(crate) const MEDIA_DOMAIN: &[u8] = b"migo-media";

/// The associated-data label for voice-note seals. The web client's `VOICE_SEAL_DOMAIN`.
pub(crate) const VOICE_DOMAIN: &[u8] = b"migo-voice";

/// The server's cap on a still image (`Policy::default`). Checked before any bytes cross
/// the wire so a refusal costs nothing.
pub(crate) const IMAGE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// The server's cap on a document.
pub(crate) const DOCUMENT_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// The server's cap on a voice note's byte size. A five-minute 8 kHz mono WAV is 4.8 MB,
/// comfortably inside; the cap is stated anyway because the recorder refuses against it,
/// not against a derived guess.
pub(crate) const VOICE_NOTE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The server's cap on a voice note's playing time, in milliseconds.
pub(crate) const VOICE_NOTE_MAX_MS: u64 = 300_000;

/// What a voice note is recorded at on this client: mono, 16-bit PCM. Eight kilohertz is
/// the wire's own call rate — the one rate every Migo client already resamples to — and at
/// the five-minute cap it produces 4.8 MB, less than the byte cap the policy states.
pub(crate) const VOICE_NOTE_SAMPLE_RATE: u32 = 8_000;

/// The key and nonce slots a room's plaintext upload fills: all zeroes, so a receiver can
/// tell "sealed" from "was never sealed" without another wire field. The web client's
/// `LEGACY_PLAINTEXT_SLOTS`, byte for byte.
pub(crate) const LEGACY_PLAINTEXT_KEY: [u8; aead::KEY_LEN] = [0u8; aead::KEY_LEN];
pub(crate) const LEGACY_PLAINTEXT_NONCE: [u8; aead::NONCE_LEN] = [0u8; aead::NONCE_LEN];

/// The wire's `MediaKind` numbers, as `MediaBegin.kind` carries them.
pub(crate) const KIND_IMAGE: u32 = 1;
pub(crate) const KIND_VOICE_NOTE: u32 = 4;
pub(crate) const KIND_DOCUMENT: u32 = 5;

/// Whether a message's key slots are the all-zero legacy pair, meaning the bytes on the
/// server are plaintext and must be passed through unopened.
pub(crate) fn is_legacy_plaintext(key: &[u8]) -> bool {
    key.len() == aead::KEY_LEN && key.iter().all(|byte| *byte == 0)
}

/// A fresh seal over one attachment's plaintext: the sealed blob for the wire, and the key
/// material that will ride inside the message to its recipients.
pub(crate) struct SealedMedia {
    /// The key that opened nothing yet and will open the blob on every receiver.
    pub key: Vec<u8>,
    /// The nonce the seal drew — the sealed blob's first 24 bytes, restated for the
    /// message's nonce slot.
    pub nonce: Vec<u8>,
    /// `nonce ‖ ciphertext ‖ tag`, exactly the bytes the PUT carries.
    pub sealed: Vec<u8>,
}

/// Seals `plaintext` under a fresh key and the given domain, the same composition the
/// web client's `sealing.seal` performs.
pub(crate) fn seal_media(
    plaintext: &[u8],
    domain: &[u8],
    random: &mut dyn Random,
) -> Result<SealedMedia, &'static str> {
    let key = SymmetricKey::generate(random);
    let sealed =
        aead::seal(&key, domain, plaintext, random).map_err(|_| "could not seal the attachment")?;
    let nonce = sealed[..aead::NONCE_LEN].to_vec();
    Ok(SealedMedia {
        key: key.expose().to_vec(),
        nonce,
        sealed,
    })
}

/// Opens a downloaded attachment.
///
/// The key must be 32 bytes and the nonce 24, and the sealed blob's first 24 bytes must
/// equal the nonce the message carried — the splice guard, so a blob swapped between
/// messages never opens under the wrong reference. Every failure collapses into the one
/// sentence: an opener that distinguished "bad key" from "bad bytes" would be a
/// padding-oracle oracle, and there is nothing a caller could do with the difference
/// anyway.
pub(crate) fn open_media(
    key: &[u8],
    nonce: &[u8],
    domain: &[u8],
    sealed: &[u8],
) -> Result<Vec<u8>, &'static str> {
    if key.len() != aead::KEY_LEN || nonce.len() != aead::NONCE_LEN {
        return Err("the attachment's key material has the wrong shape");
    }
    if sealed.len() < aead::NONCE_LEN + aead::TAG_LEN {
        return Err("the attachment could not be opened");
    }
    if &sealed[..aead::NONCE_LEN] != nonce {
        return Err("the attachment could not be opened");
    }
    let key = SymmetricKey::parse(key).map_err(|_| "the attachment could not be opened")?;
    let nonce: [u8; aead::NONCE_LEN] = nonce
        .try_into()
        .map_err(|_| "the attachment could not be opened")?;
    aead::open_with_nonce(&key, &nonce, domain, &sealed[aead::NONCE_LEN..])
        .map_err(|_| "the attachment could not be opened")
}

/// One attachment this device is sending: what the message will claim, and the key
/// material that opens the object. The media id is the upload ticket's, filled in when
/// the upload commits — it is the one fact the server mints.
pub(crate) struct OutgoingMedia {
    /// The wire's `MediaKind` number, which selects the server's size and scan policy.
    pub kind: u32,
    /// The MIME type the *message* claims — the real one, whether the bytes crossed
    /// sealed or plain.
    pub mime_type: String,
    /// The plaintext size the message claims.
    pub size_bytes: u64,
    /// The key that opens the object: fresh for a sealed upload, the all-zero legacy
    /// slots for a room's plaintext path.
    pub key: Vec<u8>,
    /// The nonce paired with the key.
    pub nonce: Vec<u8>,
    /// The pixel width, when the sender knows it, so receivers can lay out before
    /// downloading.
    pub width: Option<u32>,
    /// The pixel height.
    pub height: Option<u32>,
    /// The playing time, for a voice note.
    pub duration_ms: Option<u64>,
    /// A sender-typed caption, for an image.
    pub caption: Option<String>,
    /// The disappearing lifetime the send was armed with, sealed into the content beside the
    /// object it references — the same ride a text message's lifetime takes, because the
    /// promise "this vanishes" is about the send, not the medium.
    pub expires_in_ms: Option<u32>,
}

impl OutgoingMedia {
    /// The legacy plaintext slots, for a room's image or voice note.
    pub fn plaintext_slots() -> (Vec<u8>, Vec<u8>) {
        (
            LEGACY_PLAINTEXT_KEY.to_vec(),
            LEGACY_PLAINTEXT_NONCE.to_vec(),
        )
    }

    /// The message content that references the uploaded object, built once the ticket has
    /// named the media id. The key and nonce land in the slots verbatim: they are the
    /// only copies that ever reach a receiver.
    pub fn content(&self, media_id: migo_core::Id) -> Content {
        match self.kind {
            KIND_VOICE_NOTE => Content::VoiceNoteRef {
                media_id,
                mime_type: self.mime_type.clone(),
                size_bytes: self.size_bytes,
                duration_ms: u32::try_from(self.duration_ms.unwrap_or(0)).unwrap_or(0),
                key: self.key.clone(),
                nonce: self.nonce.clone(),
                waveform: None,
                expires_in_ms: self.expires_in_ms,
            },
            _ => Content::MediaRef {
                media_id,
                mime_type: self.mime_type.clone(),
                size_bytes: self.size_bytes,
                key: self.key.clone(),
                nonce: self.nonce.clone(),
                width: self.width,
                height: self.height,
                blurhash: None,
                caption: self.caption.clone(),
                expires_in_ms: self.expires_in_ms,
            },
        }
    }

    /// The UI body for the optimistic row, the same facts the content carries.
    pub fn body(&self, media_id: migo_core::Id) -> Body {
        match self.kind {
            KIND_VOICE_NOTE => Body::VoiceNote {
                media_id,
                duration_ms: u32::try_from(self.duration_ms.unwrap_or(0)).unwrap_or(0),
            },
            _ => Body::Media {
                media_id,
                mime_type: self.mime_type.clone(),
                size_bytes: self.size_bytes,
                width: self.width,
                height: self.height,
                caption: self.caption.clone(),
            },
        }
    }
}

/// One downloaded attachment as the worker holds it between asks.
///
/// Images keep their *original* opened bytes — a save must write the sender's file, not a
/// re-encode of decoded pixels — and decode for a texture at ask time. Voice notes keep
/// decoded samples, because playing is the only thing asked of them. Documents keep the
/// bytes. Cloning is cheap where it can be: the samples sit behind an `Arc` so starting a
/// playback never copies a note's whole audio.
#[derive(Clone)]
pub(crate) enum CachedMedia {
    /// An opened image: the sender's original bytes, so a save writes the file that was
    /// sent and the pixels are decoded per serve. The claimed MIME type is not kept — the
    /// row already shows it from the message, and a save's extension is the typed path's.
    Image { bytes: Vec<u8> },
    /// A decoded voice note: mono samples at the rate the container stated.
    Audio { samples: Arc<Vec<i16>>, rate: u32 },
    /// Anything else: the bytes, for a save.
    Document { bytes: Vec<u8> },
}

impl CachedMedia {
    /// The opened bytes, for a save. A voice note has none to give: it is kept decoded,
    /// and writing a WAV re-encode of another client's note back to disk is a save of
    /// something the sender never sent.
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Image { bytes, .. } | Self::Document { bytes, .. } => Some(bytes),
            Self::Audio { .. } => None,
        }
    }

    /// How much of the cache budget this entry costs.
    pub fn cost(&self) -> usize {
        match self {
            Self::Image { bytes, .. } | Self::Document { bytes, .. } => bytes.len(),
            Self::Audio { samples, .. } => samples.len() * 2,
        }
    }
}

/// Decoded audio, ready for the speaker path: mono `i16` samples at `rate`.
pub(crate) struct DecodedAudio {
    pub samples: Vec<i16>,
    pub rate: u32,
}

/// Decodes a voice note's container into mono samples.
///
/// Whatever the sender's client recorded — this one's WAV, a browser's WebM/Opus or
/// MP4/AAC, another player's Ogg or MP3 — the same probe-and-decode pass reads it, and
/// anything this build's codec set cannot read is the one honest failure. Multi-channel
/// audio is downmixed by averaging: a voice note is a voice, and a voice in the middle of
/// a stereo field is still that voice at half amplitude.
pub(crate) fn decode_audio(bytes: &[u8]) -> Result<DecodedAudio, String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL, CODEC_TYPE_OPUS};
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let source = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes.to_vec())),
        Default::default(),
    );
    // No hint: the container's own magic says what it is, and a lying extension on a
    // saved file must not stop a valid container from opening.
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|error| format!("not audio this build can play ({error})"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "the recording has no audio in it".to_owned())?;
    let track_id = track.id;

    // Opus is the one codec symphonia's demuxers recognise but its codec registry cannot
    // decode — and it is the web client's dominant recording (audio/webm). Those packets
    // go to the pure-Rust oporus decoder instead; everything else goes to the registry.
    if track.codec_params.codec == CODEC_TYPE_OPUS {
        // Copied out before the mutable borrow: the parameters live inside the reader,
        // and the reader is what the decode loop takes by &mut.
        let channels = track.codec_params.channels;
        let sample_rate = track.codec_params.sample_rate;
        return decode_opus(format.as_mut(), track_id, channels, sample_rate);
    }

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|error| format!("not audio this build can play ({error})"))?;

    let mut samples: Vec<i16> = Vec::new();
    let mut rate = track
        .codec_params
        .sample_rate
        .unwrap_or(VOICE_NOTE_SAMPLE_RATE);
    // `while let` stops at the container's end — an Err is the normal ending, signalled
    // the same way an I/O error is — keeping whatever decoded so far as the whole note.
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A damaged packet is skipped, not fatal: the note plays with a gap rather
            // than not at all.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(_) => break,
        };
        if decoded.frames() == 0 {
            continue;
        }
        let spec = *decoded.spec();
        rate = spec.rate;
        let mut sampler = SampleBuffer::<i16>::new(decoded.frames() as u64, spec);
        sampler.copy_interleaved_ref(decoded);
        let interleaved = sampler.samples();
        match spec.channels.count() {
            0 | 1 => samples.extend_from_slice(interleaved),
            channels => {
                for frame in interleaved.chunks(channels) {
                    let sum: i32 = frame.iter().map(|sample| i32::from(*sample)).sum();
                    samples.push((sum / channels as i32) as i16);
                }
            }
        }
    }
    if samples.is_empty() {
        return Err("the recording has no audio in it".to_owned());
    }
    Ok(DecodedAudio { samples, rate })
}

/// The Opus half of [`decode_audio`]: symphonia pulls the packets out of the container
/// (WebM or Ogg — both demuxers recognise the track), and oporus — a pure-Rust port of
/// libopus — turns each packet into samples.
///
/// The decode call is capacity-style, libopus's own shape: the buffer is one largest-legal
/// frame (120 ms) and the return value is the count of frames actually decoded, so a note
/// of any packet duration decodes without knowing its frame sizes in advance. A damaged
/// packet is skipped rather than fatal, the same mercy the symphonia path shows.
fn decode_opus(
    format: &mut dyn symphonia::core::formats::FormatReader,
    track_id: u32,
    channels: Option<symphonia::core::audio::Channels>,
    sample_rate: Option<u32>,
) -> Result<DecodedAudio, String> {
    // The container names its channels as a mask; Opus itself is mono or stereo, so the
    // mask is only asked how many it names. A track claiming more than two is malformed.
    let channels = match channels.map_or(1, |mask| mask.count()) {
        1 => oporus::Channels::Mono,
        2 => oporus::Channels::Stereo,
        _ => return Err("the recording has too many channels to play".to_owned()),
    };
    // Opus runs at one of libopus's five rates and 48 kHz is the canonical one. A
    // container that claims anything else is mislabelled, not a different codec, so the
    // honest answer is the canonical rate rather than a decoder that refuses to init.
    let rate = match sample_rate {
        Some(rate) if matches!(rate, 8_000 | 12_000 | 16_000 | 24_000 | 48_000) => rate,
        _ => 48_000,
    };
    let mut decoder = oporus::Decoder::new(rate, channels)
        .map_err(|_| "could not start the opus decoder".to_owned())?;
    let channel_count = channels.count();
    let mut pcm = vec![0i16; 5_760 * channel_count];
    let mut samples: Vec<i16> = Vec::new();
    // `while let` stops at the container's end — an Err is the normal ending — keeping
    // whatever decoded so far as the whole note.
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        // `buf()` is the accessor for `Packet`'s public `data: Box<[u8]>` field — the source of
        // symphonia-core 0.5.5 says so, and the compiler's two earlier hints pointed at it from
        // both sides (`&packet.buf` reached for the field through a method's name; `data()`
        // reached for the field through a method that does not exist).
        let frames = match decoder.decode(packet.buf(), &mut pcm, false) {
            Ok(frames) => frames,
            Err(_) => continue,
        };
        let decoded = &pcm[..frames * channel_count];
        match channel_count {
            1 => samples.extend_from_slice(decoded),
            _ => {
                for frame in decoded.chunks(channel_count) {
                    let sum: i32 = frame.iter().map(|sample| i32::from(*sample)).sum();
                    samples.push((sum / channel_count as i32) as i16);
                }
            }
        }
    }
    if samples.is_empty() {
        return Err("the recording has no audio in it".to_owned());
    }
    Ok(DecodedAudio { samples, rate })
}

/// Decodes an image into RGBA pixels and its dimensions.
pub(crate) fn decode_image(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let decoded = image::load_from_memory(bytes)
        .map_err(|error| format!("not an image this build can show ({error})"))?
        .to_rgba8();
    let (width, height) = decoded.dimensions();
    Ok((width, height, decoded.into_raw()))
}

/// The MIME type to claim for an image, from the bytes when they state it and from the
/// file's extension otherwise. Only the formats the server's sniffer recognises as images
/// are claimed as such; anything else is the neutral claim the server re-judges anyway.
pub(crate) fn image_mime_of_bytes(bytes: &[u8], path: &Path) -> String {
    let sniffed = image::guess_format(bytes)
        .ok()
        .and_then(|format| match format {
            image::ImageFormat::Png => Some("image/png"),
            image::ImageFormat::Jpeg => Some("image/jpeg"),
            image::ImageFormat::WebP => Some("image/webp"),
            image::ImageFormat::Gif => Some("image/gif"),
            _ => None,
        });
    match sniffed {
        Some(mime) => mime.to_owned(),
        None => image_mime_of_extension(path).to_owned(),
    }
}

/// The extension-based fallback the avatar flow already uses, restated here: the same
/// judgement, from the filename a user typed, for the bytes magic cannot identify.
fn image_mime_of_extension(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "application/octet-stream",
    }
}

/// The MIME type to claim for a document, from its extension. A document is whatever
/// somebody attached, so the claim is honest but loose: the neutral claim is the default
/// and the server records whatever it can identify from the bytes.
pub(crate) fn document_mime_of(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("zip") => "application/zip",
        Some("gz") => "application/gzip",
        Some("tar") => "application/x-tar",
        Some("doc") | Some("docx") => "application/msword",
        Some("xls") | Some("xlsx") => "application/vnd.ms-excel",
        Some("ppt") | Some("pptx") => "application/vnd.ms-powerpoint",
        _ => "application/octet-stream",
    }
}

/// A canonical 44-byte WAV header plus the samples: PCM, 16-bit, mono, at `rate`.
///
/// Written by hand rather than through a codec crate because a voice note's container is
/// one fixed shape on this client, and the server's sniffer needs nothing more than the
/// `RIFF`…`WAVE` magic the header carries.
pub(crate) fn wav_bytes(samples: &[i16], rate: u32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&u32::to_le_bytes((36 + data_len) as u32));
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&u32::to_le_bytes(16)); // the fmt chunk's own size
    out.extend_from_slice(&u16::to_le_bytes(1)); // PCM
    out.extend_from_slice(&u16::to_le_bytes(1)); // mono
    out.extend_from_slice(&u32::to_le_bytes(rate));
    out.extend_from_slice(&u32::to_le_bytes(rate * 2)); // byte rate: mono × 16-bit
    out.extend_from_slice(&u16::to_le_bytes(2)); // block align
    out.extend_from_slice(&u16::to_le_bytes(16)); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&u32::to_le_bytes(data_len as u32));
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::{OsRandom, SeededRandom};

    fn keying() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        // A sealed blob under the media domain, opened the way a receiver would.
        let mut random = OsRandom;
        let sealed = seal_media(b"attachment", MEDIA_DOMAIN, &mut random).expect("seals");
        (sealed.key, sealed.nonce, sealed.sealed)
    }

    /// The seal is the web client's seal: `nonce ‖ ciphertext ‖ tag`, 24 + plaintext + 16,
    /// and the nonce in the message slot matches the blob's own first bytes.
    #[test]
    fn a_seal_round_trips_under_its_domain() {
        let (key, nonce, sealed) = keying();
        assert_eq!(
            sealed.len(),
            aead::NONCE_LEN + b"attachment".len() + aead::TAG_LEN
        );
        assert_eq!(&sealed[..aead::NONCE_LEN], nonce.as_slice());
        let opened = open_media(&key, &nonce, MEDIA_DOMAIN, &sealed).expect("opens");
        assert_eq!(opened, b"attachment");
    }

    /// The domain is binding: a voice note's seal does not open as an image's.
    #[test]
    fn a_seal_from_the_wrong_domain_refuses() {
        let mut random = OsRandom;
        let sealed = seal_media(b"note", VOICE_DOMAIN, &mut random).expect("seals");
        let opened = open_media(&sealed.key, &sealed.nonce, MEDIA_DOMAIN, &sealed.sealed);
        assert!(opened.is_err());
    }

    /// The splice guard: a blob whose first bytes are not the message's nonce — a blob
    /// swapped between messages, or a truncated one — never opens, and says only the one
    /// sentence.
    #[test]
    fn a_spliced_blob_refuses() {
        let (key, nonce, sealed) = keying();
        let mut spliced = sealed.clone();
        spliced[0] ^= 1;
        assert!(open_media(&key, &nonce, MEDIA_DOMAIN, &spliced).is_err());
        // A short blob is refused the same way, for the same reason.
        assert!(open_media(&key, &nonce, MEDIA_DOMAIN, &sealed[..10]).is_err());
    }

    /// The legacy slots are all zeroes and read as plaintext; a real key never does.
    #[test]
    fn the_legacy_slots_are_recognisable() {
        assert!(is_legacy_plaintext(&LEGACY_PLAINTEXT_KEY));
        assert!(is_legacy_plaintext(&[0u8; aead::KEY_LEN]));
        let (key, _, _) = keying();
        assert!(!is_legacy_plaintext(&key));
        // A wrong-length key is not "legacy" — it is nothing at all, and the caller says so.
        assert!(!is_legacy_plaintext(&[0u8; 31]));
    }

    /// Sealing is random: two seals of the same plaintext differ, and both open.
    #[test]
    fn each_seal_draws_a_fresh_nonce() {
        let mut random = SeededRandom::new(9);
        let first = seal_media(b"same", MEDIA_DOMAIN, &mut random).expect("seals");
        let second = seal_media(b"same", MEDIA_DOMAIN, &mut random).expect("seals");
        assert_ne!(first.sealed, second.sealed);
        assert_ne!(first.key, second.key);
    }

    /// The WAV writer produces the canonical header a sniffer — and every other player —
    /// reads: RIFF/WAVE magic, PCM mono 16-bit at the stated rate, and little-endian
    /// samples in a data chunk whose length matches.
    #[test]
    fn the_wav_writer_lays_out_the_canonical_header() {
        let samples: Vec<i16> = vec![-1, 0, 258, i16::MAX];
        let wav = wav_bytes(&samples, VOICE_NOTE_SAMPLE_RATE);
        assert_eq!(wav.len(), 44 + samples.len() * 2);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(wav[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1); // PCM
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1); // mono
        assert_eq!(
            u32::from_le_bytes(wav[24..28].try_into().unwrap()),
            VOICE_NOTE_SAMPLE_RATE
        );
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16); // bits
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(
            u32::from_le_bytes(wav[40..44].try_into().unwrap()),
            (samples.len() * 2) as u32
        );
        // The samples themselves, little-endian, in order.
        assert_eq!(wav[44..46], (-1i16).to_le_bytes());
        assert_eq!(wav[48..50], 258i16.to_le_bytes());
    }

    /// A five-minute recording at the stated rate stays inside the byte cap, so the two
    /// caps the policy states can never disagree on this client.
    #[test]
    fn a_capped_recording_fits_the_byte_cap() {
        let max_samples = VOICE_NOTE_MAX_MS * u64::from(VOICE_NOTE_SAMPLE_RATE) / 1_000;
        let wav = wav_bytes(&vec![0i16; max_samples as usize], VOICE_NOTE_SAMPLE_RATE);
        assert!(wav.len() as u64 <= VOICE_NOTE_MAX_BYTES);
    }
}
