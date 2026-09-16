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
//! This client records Opus in an Ogg — mono, 24 kHz input, 24 kbps — the same posture the
//! Android recorder's own settings keep, so a note from this client is the same container,
//! codec, and size a note from an Android phone is, and Opus is the speech codec the web
//! client's recorder produces as well (Chrome and Firefox record Opus in a WebM). The pages
//! are written as the recording runs, so the draft file is a playable Ogg from its first
//! second. What it *plays* is whatever the sender recorded: this client's own Ogg Opus, the
//! WAV this client recorded before the switch, a browser's WebM/Opus or MP4/AAC — so the
//! decoder is a real demuxer-and-codec stack that picks its path off the container's own
//! magic, never off a claimed type or a file extension.

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

/// The server's cap on a voice note's byte size. A five-minute note at this client's 24 kbps
/// is roughly 900 kB, comfortably inside; the cap is stated anyway because the recorder
/// refuses against it, not against a derived guess.
pub(crate) const VOICE_NOTE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The server's cap on a voice note's playing time, in milliseconds.
pub(crate) const VOICE_NOTE_MAX_MS: u64 = 300_000;

/// What a voice note is recorded at on this client: mono Opus, 24 kHz input. Twenty-four
/// kilohertz is the rate the Android recorder configures — the same rate on both clients
/// means the same bandwidth and roughly the same size of note — and it is Opus's
/// superwideband, a speech note worth hearing clearly rather than the call wire's 8 kHz
/// narrowband. Opus itself always runs at 48 kHz internally; the rate stated here is the
/// encoder's input rate and the bandwidth it may use.
pub(crate) const VOICE_NOTE_SAMPLE_RATE: u32 = 24_000;

/// The rate the previous release of this client recorded raw PCM at — the one rate that
/// client ever wrote. Restated because a draft it left on disk is raw PCM at this rate, and
/// re-encoding such a draft must happen at the rate it was taken at, not the rate notes are
/// taken at now.
pub(crate) const LEGACY_VOICE_NOTE_SAMPLE_RATE: u32 = 8_000;

/// The bitrate a voice note is encoded at, the same figure the Android recorder configures:
/// a speech bitrate, not a music one — section 179's "Normal" quality, where a five-minute
/// note stays a small upload.
pub(crate) const VOICE_NOTE_OPUS_BITRATE: i32 = 24_000;

/// How many bars a recorded waveform folds into, and what a bubble renders at most — the
/// web client's `WAVEFORM_BARS` and the Android client's, the same number, so a note
/// recorded on any client carries the same fifty-byte preview section 167 asks for.
pub(crate) const WAVEFORM_BARS: usize = 50;

/// How many samples of the note's own rate one live waveform bar covers: a tenth of a
/// second, the cadence the Android recorder samples at and the web's analyser graph
/// approximates, so the bar a speaker watches land is the bar the fold will keep. The
/// widening cast rather than `u64::from` because a const is not allowed to call it on this
/// toolchain, and the cast is const and infallible both.
pub(crate) const WAVEFORM_WINDOW_SAMPLES: u64 = VOICE_NOTE_SAMPLE_RATE as u64 / 10;

/// One sampled amplitude as a 0–255 bar: the scale the Android client's `amplitudeToBar`
/// states, so a whisper recorded on either client draws the same height on the third.
pub(crate) fn amplitude_to_bar(amplitude: i16) -> u8 {
    let magnitude = u32::from(amplitude.unsigned_abs());
    ((magnitude * 255) / 32_767).min(255) as u8
}

/// Folds a stream of sampled bars into the fixed-width waveform a message carries and a
/// bubble renders.
///
/// Each output bar is the *maximum* bar in its slice of the input, because a peak — not an
/// average — is what a waveform bar is drawn from: a syllable landing inside a bucket must
/// show, and averaging would flatten it into the silence around it. The output is always
/// exactly [`WAVEFORM_BARS`] bytes: an input shorter than the bar count pads with silence at
/// the tail (a very short recording simply runs out of samples) and an empty input is all
/// silence. The fold only ever runs on this client's own samples; a waveform that arrives
/// from a peer is drawn as it stands, and the strip that draws it clips to its own width
/// rather than trusting the sender's to be sane.
pub(crate) fn downsample_waveform(bars: &[u8]) -> Vec<u8> {
    let mut folded = vec![0u8; WAVEFORM_BARS];
    if bars.is_empty() {
        return folded;
    }
    let bucket = bars.len().div_ceil(WAVEFORM_BARS);
    for (index, bar) in bars.iter().enumerate() {
        let slot = (index / bucket).min(WAVEFORM_BARS - 1);
        if *bar > folded[slot] {
            folded[slot] = *bar;
        }
    }
    folded
}

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
    /// The folded waveform, for a voice note — the sender's own samples, computed before the
    /// seal because the sealed bytes are the one place the server can never compute it from.
    pub waveform: Option<Vec<u8>>,
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
                waveform: self.waveform.clone(),
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
                waveform: self.waveform.clone(),
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

/// One Opus frame's playing time, in milliseconds: the unit the encoder is fed in, the
/// smallest packet the stream holds, and the granularity a recording's tail is padded to.
const OPUS_FRAME_MS: u64 = 20;

/// One frame's worth of samples at Opus's own internal rate: the count the Ogg granule
/// position advances by per frame, whatever rate the input was taken at — RFC 7845 states
/// granule positions in 48 kHz samples because that is the rate the codec itself runs at.
const OPUS_FRAME_SAMPLES_AT_48K: u64 = 48_000 * OPUS_FRAME_MS / 1_000;

/// The largest legal Opus packet, the capacity one frame is encoded into.
const OPUS_MAX_PACKET: usize = 1_275;

/// The encoder complexity: below libopus's default, on purpose. This encoder runs on the
/// capture thread, in pure Rust, keeping pace with a live microphone — complexity 5 is
/// libopus's own mid-point of quality against effort, and at a 24 kbps speech bitrate the
/// difference from higher settings is inaudible while the headroom is what keeps a slow
/// machine from falling behind realtime.
const OPUS_COMPLEXITY: i32 = 5;

/// The encoder's lookahead, in 48 kHz samples: the six and a half milliseconds libopus
/// buffers before its first output sample. Stated in the identification header as the
/// pre-skip, the field every Ogg Opus writer fills with this figure, so a player that
/// honours it drops exactly the samples the encoder held back.
const OPUS_PRE_SKIP: u16 = 312;

/// How many lacing values a page under construction may hold before it is written: the
/// Ogg page header caps a page at 255 segments, and flushing a little early keeps every
/// page well inside the cap whatever sizes VBR packets take.
const OGG_PAGE_LACING_FLUSH: usize = 200;

/// The body size a page under construction aims for, the four kilobytes Ogg writers from
/// `opusenc` on down batch to: small enough that a death mid-recording costs the note's
/// last moment rather than its last several seconds, large enough that page headers stay a
/// rounding error in the byte budget.
const OGG_PAGE_TARGET_BYTES: usize = 4_096;

/// Ogg's own checksum: CRC-32 with the MPEG-2 polynomial, MSB first, no initial value and
/// no final XOR, taken over the page with the checksum's own four bytes zeroed. The table
/// is built at compile time — the bit loop runs once per entry of a constant, not once per
/// byte of a recording.
const fn ogg_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut entry = 0;
    while entry < 256 {
        let mut value = (entry as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 0x8000_0000 != 0 {
                (value << 1) ^ 0x04C1_1DB7
            } else {
                value << 1
            };
            bit += 1;
        }
        table[entry] = value;
        entry += 1;
    }
    table
}

static OGG_CRC_TABLE: [u32; 256] = ogg_crc_table();

/// The CRC of one page's bytes, checksum field included — the caller zeroes the field
/// first, then writes this value into it.
fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for byte in bytes {
        let index = (((crc >> 24) as u8) ^ byte) as usize;
        crc = (crc << 8) ^ OGG_CRC_TABLE[index];
    }
    crc
}

/// Writes one Ogg page: RFC 3533's 27-byte header, the lacing table, and the packet body,
/// with the header-type flag, granule position, serial number, and sequence number the
/// caller states. The lacing table must hold at most 255 values — the page header's one
/// byte of segment count — which the writer's flush threshold guarantees.
fn write_ogg_page<W: std::io::Write>(
    out: &mut W,
    header_type: u8,
    granule: u64,
    serial: u32,
    sequence: u32,
    lacing: &[u8],
    body: &[u8],
) -> std::io::Result<()> {
    debug_assert!(lacing.len() <= 255);
    let mut page = Vec::with_capacity(27 + lacing.len() + body.len());
    page.extend_from_slice(b"OggS");
    page.push(0); // stream structure version
    page.push(header_type);
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&sequence.to_le_bytes());
    page.extend_from_slice(&[0; 4]); // the checksum, written once it can be computed
    page.push(lacing.len() as u8);
    page.extend_from_slice(lacing);
    page.extend_from_slice(body);
    let crc = ogg_crc(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    out.write_all(&page)
}

/// Appends one packet's lacing values: a packet is a run of 255s ending in the remainder,
/// and a packet whose length is a multiple of 255 ends in a zero — the terminating value
/// that tells a reader the packet ended there.
fn push_ogg_lacing(lacing: &mut Vec<u8>, packet: &[u8]) {
    let mut remaining = packet.len();
    while remaining >= 255 {
        lacing.push(255);
        remaining -= 255;
    }
    lacing.push(remaining as u8);
}

/// The identification header RFC 7845 puts in the first page's first packet: the codec's
/// name, the mapping version, the channel count, the pre-skip, the input sample rate, the
/// output gain, and the channel mapping family. Nineteen bytes, one fixed shape.
fn opus_head_packet(input_rate: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(19);
    out.extend_from_slice(b"OpusHead");
    out.push(1); // mapping version
    out.push(1); // channels: mono
    out.extend_from_slice(&OPUS_PRE_SKIP.to_le_bytes());
    out.extend_from_slice(&input_rate.to_le_bytes());
    out.extend_from_slice(&0i16.to_le_bytes()); // output gain: none
    out.push(0); // channel mapping family: RTP
    out
}

/// The comment header, the second page's whole content: the codec's name, a vendor string,
/// and a count of zero comments. Nothing here is a judgement about the audio; the page
/// exists because the mapping says it must.
fn opus_tags_packet() -> Vec<u8> {
    let vendor = b"migo-desktop";
    let mut out = Vec::with_capacity(8 + 4 + vendor.len() + 4);
    out.extend_from_slice(b"OpusTags");
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor);
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// The incremental Ogg Opus writer a recording is sealed into as it runs: PCM in, pages
/// out, never holding more than the current partial frame and the current partial page.
///
/// Written by hand rather than through a container crate because a voice note's stream is
/// one fixed shape — a single mono Opus logical stream, two header packets, 20 ms frames —
/// the same reasoning that had this module writing its own WAV header for years. The Opus
/// packets themselves come from `oporus`, the same pure-Rust codec the decoder below
/// already leans on: no C toolchain enters the build to carry one.
///
/// The granule position advances by 960 per frame — one 20 ms frame at the codec's own
/// 48 kHz, whatever rate the input was taken at — starting from the pre-skip, so the
/// position a page states is the count of 48 kHz samples a decoder would have produced by
/// the end of that page, which is exactly what RFC 7845 asks the field to mean.
pub(crate) struct OggOpusEncoder<W: std::io::Write> {
    /// The stream's destination — the recording's draft file, buffered.
    out: W,
    /// The codec. VoIP tuning, because a note is a voice and not a concert.
    encoder: oporus::Encoder,
    /// The logical stream's serial number, drawn fresh per recording so two notes never
    /// share a stream identity by accident.
    serial: u32,
    /// The next page's sequence number, one per page from zero.
    sequence: u32,
    /// One frame's worth of input samples at the note's own rate.
    frame_len: usize,
    /// The samples of the frame being filled — fewer than one frame's worth, always.
    pending: Vec<i16>,
    /// 48 kHz samples encoded so far, pre-skip included: the granule the next page states.
    granule: u64,
    /// The lacing values of the packets batched into the page being built.
    page_lacing: Vec<u8>,
    /// The bytes of the packets batched into the page being built.
    page_body: Vec<u8>,
    /// Whether the two header pages have been written — deferred to the first push so a
    /// writer that never receives a sample still produces a well-formed, empty stream.
    started: bool,
}

impl<W: std::io::Write> OggOpusEncoder<W> {
    /// Builds the encoder for one recording at `rate` — one of Opus's five rates — under
    /// the speech bitrate this client's notes all use.
    pub(crate) fn new(out: W, rate: u32, serial: u32) -> Result<Self, &'static str> {
        let encoder =
            oporus::Encoder::builder(rate, oporus::Channels::Mono, oporus::Application::Voip)
                .bitrate(oporus::Bitrate::Bits(VOICE_NOTE_OPUS_BITRATE))
                .complexity(OPUS_COMPLEXITY)
                .build()
                .map_err(|_| "could not start the opus encoder")?;
        let frame_len = rate as usize * OPUS_FRAME_MS as usize / 1_000;
        Ok(Self {
            out,
            encoder,
            serial,
            sequence: 0,
            frame_len,
            pending: Vec::with_capacity(frame_len),
            granule: u64::from(OPUS_PRE_SKIP),
            page_lacing: Vec::new(),
            page_body: Vec::new(),
            started: false,
        })
    }

    /// Writes the two header pages, once: the identification page a reader detects the
    /// stream by, and the comment page behind it.
    fn start(&mut self) -> std::io::Result<()> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        let head = opus_head_packet(self.encoder.sample_rate());
        write_ogg_page(
            &mut self.out,
            0x02, // beginning of stream
            0,
            self.serial,
            self.sequence,
            &[head.len() as u8],
            &head,
        )?;
        self.sequence += 1;
        let tags = opus_tags_packet();
        write_ogg_page(
            &mut self.out,
            0x00,
            0,
            self.serial,
            self.sequence,
            &[tags.len() as u8],
            &tags,
        )?;
        self.sequence += 1;
        Ok(())
    }

    /// Appends PCM at the note's own rate. A frame is encoded each time enough samples
    /// have arrived, so the recording exists on disk — inside whatever page is batching —
    /// from the microphone's first moment, and the bytes are never held whole in memory.
    pub(crate) fn push(&mut self, pcm: &[i16]) -> std::io::Result<()> {
        self.start()?;
        self.pending.extend_from_slice(pcm);
        let frame_len = self.frame_len;
        while self.pending.len() >= frame_len {
            let frame: Vec<i16> = self.pending.drain(..frame_len).collect();
            self.encode_frame(&frame)?;
        }
        Ok(())
    }

    /// Encodes one complete frame into one packet and batches it into the current page.
    fn encode_frame(&mut self, frame: &[i16]) -> std::io::Result<()> {
        let packet = self
            .encoder
            .encode_vec(frame, OPUS_MAX_PACKET)
            .map_err(|_| std::io::Error::other("the opus encoder refused a frame"))?;
        self.granule += OPUS_FRAME_SAMPLES_AT_48K;
        push_ogg_lacing(&mut self.page_lacing, &packet);
        self.page_body.extend_from_slice(&packet);
        if self.page_lacing.len() >= OGG_PAGE_LACING_FLUSH
            || self.page_body.len() >= OGG_PAGE_TARGET_BYTES
        {
            self.flush_page(false)?;
        }
        Ok(())
    }

    /// Writes the page under construction. `eos` sets the end-of-stream flag — the last
    /// page of the stream, whatever it holds.
    fn flush_page(&mut self, eos: bool) -> std::io::Result<()> {
        let header_type = if eos { 0x04 } else { 0x00 };
        write_ogg_page(
            &mut self.out,
            header_type,
            self.granule,
            self.serial,
            self.sequence,
            &self.page_lacing,
            &self.page_body,
        )?;
        self.sequence += 1;
        self.page_lacing.clear();
        self.page_body.clear();
        Ok(())
    }

    /// Ends the stream and hands the writer back. A partial frame at the end is padded
    /// with silence when `pad_tail` — a speaker cut off mid-word still gets their last
    /// twenty milliseconds — and dropped when it is the cap's own overrun, so a note the
    /// cap stopped does not claim time it was refused.
    pub(crate) fn finish(mut self, pad_tail: bool) -> std::io::Result<W> {
        self.start()?;
        if pad_tail && !self.pending.is_empty() {
            let mut frame = std::mem::take(&mut self.pending);
            frame.resize(self.frame_len, 0);
            self.encode_frame(&frame)?;
        }
        if self.page_lacing.is_empty() {
            // Nothing is batched — either every packet landed on a page boundary or the
            // recording never made a frame. The stream still ends with an end-of-stream
            // page, carrying a zero-length packet's lacing so the page is well-formed.
            self.page_lacing.push(0);
        }
        self.flush_page(true)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Encodes a whole buffer's PCM into Ogg Opus at `rate` — the one-shot shape of the
/// incremental writer, for a draft re-encoded in place rather than recorded live.
pub(crate) fn encode_ogg_opus(
    samples: &[i16],
    rate: u32,
    serial: u32,
) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    let mut encoder = OggOpusEncoder::new(&mut out, rate, serial)?;
    encoder
        .push(samples)
        .map_err(|_| "could not encode the recording")?;
    encoder
        .finish(true)
        .map_err(|_| "could not encode the recording")?;
    Ok(out)
}

/// States a note's playing time off the pages themselves: the last granule position any
/// complete page carries, less the pre-skip, in milliseconds. The encoded stream's own
/// statement of its length — independent of any descriptor — and one that stays honest
/// for a draft whose tail died with the process, because a truncated page simply is not
/// counted. `None` when the bytes hold no complete page that states any audio.
pub(crate) fn ogg_opus_playtime_ms(bytes: &[u8]) -> Option<u64> {
    let mut last = 0u64;
    let mut offset = 0usize;
    while offset + 27 <= bytes.len() {
        if &bytes[offset..offset + 4] != b"OggS" {
            return None;
        }
        let segments = bytes[offset + 26] as usize;
        let header_len = 27 + segments;
        if offset + header_len > bytes.len() {
            break; // the header itself was cut short: the page never finished
        }
        let body: usize = bytes[offset + 27..offset + header_len]
            .iter()
            .map(|value| usize::from(*value))
            .sum();
        if offset + header_len + body > bytes.len() {
            break; // the body was cut short: the page never finished
        }
        let granule = u64::from_le_bytes(bytes[offset + 6..offset + 14].try_into().ok()?);
        if granule > 0 {
            last = granule;
        }
        offset += header_len + body;
    }
    if last < u64::from(OPUS_PRE_SKIP) {
        return None;
    }
    Some((last - u64::from(OPUS_PRE_SKIP)) * 1_000 / 48_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::{OsRandom, SeededRandom};

    /// A 440 Hz sine at `rate`, `len` samples long, at the given amplitude — a stand-in for
    /// speech that carries real energy through the codec.
    fn sine(len: usize, rate: u32, amplitude: f64) -> Vec<i16> {
        (0..len)
            .map(|index| {
                let phase = (index as f64) * 440.0 * std::f64::consts::TAU / f64::from(rate);
                (phase.sin() * amplitude) as i16
            })
            .collect()
    }

    /// A canonical 44-byte WAV header plus the samples: PCM, 16-bit, mono, at `rate` — the
    /// container this client recorded before the Opus switch, and still the shape the WAV
    /// passthrough test needs. Written by hand for the same reason it always was: a fixed
    /// shape, and the test pins its layout.
    fn wav_bytes(samples: &[i16], rate: u32) -> Vec<u8> {
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

    /// Walks an Ogg stream the way any Ogg reader walks it — lacing table first, body
    /// length from it — and states each page's header-type flags, granule position,
    /// sequence number, and body. Every page's checksum is verified against its own bytes
    /// on the way past, so a test that walks a stream has already proven the checksums.
    fn ogg_pages(bytes: &[u8]) -> Vec<(u8, u64, u32, Vec<u8>)> {
        let mut pages = Vec::new();
        let mut offset = 0usize;
        while offset + 27 <= bytes.len() {
            assert_eq!(
                &bytes[offset..offset + 4],
                b"OggS",
                "every page opens with the capture pattern"
            );
            let flags = bytes[offset + 5];
            let granule = u64::from_le_bytes(bytes[offset + 6..offset + 14].try_into().unwrap());
            let sequence = u32::from_le_bytes(bytes[offset + 18..offset + 22].try_into().unwrap());
            let carried = u32::from_le_bytes(bytes[offset + 22..offset + 26].try_into().unwrap());
            let segments = bytes[offset + 26] as usize;
            let header_len = 27 + segments;
            let body_len: usize = bytes[offset + 27..offset + header_len]
                .iter()
                .map(|value| usize::from(*value))
                .sum();
            let body = bytes[offset + header_len..offset + header_len + body_len].to_vec();
            let mut zeroed = bytes[offset..offset + header_len + body_len].to_vec();
            zeroed[22..26].fill(0);
            assert_eq!(
                carried,
                ogg_crc(&zeroed),
                "the checksum a page carries is the one its bytes state"
            );
            pages.push((flags, granule, sequence, body));
            offset += header_len + body_len;
        }
        assert_eq!(offset, bytes.len(), "the pages account for every byte");
        pages
    }

    /// The bar one window of PCM folds to — the recorder's own live-sampling judgement,
    /// restated for tests that must take it over a buffer rather than a stream.
    fn window_bar(window: &[i16]) -> u8 {
        let peak = window.iter().copied().fold(0i16, |peak, sample| {
            if sample.unsigned_abs() > peak.unsigned_abs() {
                sample
            } else {
                peak
            }
        });
        amplitude_to_bar(peak)
    }

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

    /// The WAV shape this client used to send is still the shape the decode path must
    /// accept, and the helper that crafts one lays out the canonical header a sniffer — and
    /// every other player — reads.
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

    /// An encoded note is an Ogg Opus stream, stated by its own bytes: the capture pattern,
    /// a beginning-of-stream page whose one packet is the identification header, the
    /// comment page behind it, numbered pages in order, and a final page flagged
    /// end-of-stream whose granule position states the whole note at the codec's own rate.
    #[test]
    fn an_encoded_note_is_ogg_opus() {
        let samples = sine(
            VOICE_NOTE_SAMPLE_RATE as usize,
            VOICE_NOTE_SAMPLE_RATE,
            12_000.0,
        );
        let ogg = encode_ogg_opus(&samples, VOICE_NOTE_SAMPLE_RATE, 0x1234_5678)
            .expect("one second encodes");

        assert_eq!(&ogg[..4], b"OggS", "the stream opens with Ogg's magic");
        let pages = ogg_pages(&ogg);
        assert!(pages.len() >= 3, "identification, comment, and audio pages");

        let (flags, granule, sequence, head) = &pages[0];
        assert_eq!(*flags & 0x02, 0x02, "the first page begins the stream");
        assert_eq!(*granule, 0, "the identification page states no time");
        assert_eq!(*sequence, 0);
        assert_eq!(&head[..8], b"OpusHead");
        assert_eq!(
            head.len(),
            19,
            "the identification packet is the fixed 19 bytes"
        );
        assert_eq!(head[8], 1, "mapping version 1");
        assert_eq!(head[9], 1, "mono");
        assert_eq!(
            u16::from_le_bytes(head[10..12].try_into().unwrap()),
            OPUS_PRE_SKIP
        );
        assert_eq!(
            u32::from_le_bytes(head[12..16].try_into().unwrap()),
            VOICE_NOTE_SAMPLE_RATE
        );
        assert_eq!(head[18], 0, "the RTP channel mapping family");

        let (flags, granule, sequence, tags) = &pages[1];
        assert_eq!(*flags, 0x00, "the comment page is a middle page");
        assert_eq!(*granule, 0);
        assert_eq!(*sequence, 1);
        assert_eq!(&tags[..8], b"OpusTags");

        for (index, (_, _, sequence, _)) in pages.iter().enumerate() {
            assert_eq!(
                *sequence, index as u32,
                "the pages number themselves in order"
            );
        }

        let (flags, granule, _, _) = pages.last().expect("the last page exists");
        assert_eq!(*flags & 0x04, 0x04, "the last page ends the stream");
        assert_eq!(
            *granule,
            u64::from(OPUS_PRE_SKIP) + 48_000,
            "one second of audio, stated at the codec's own 48 kHz, pre-skip included"
        );
    }

    /// A crafted Opus note decodes to PCM: the same pass that plays a web or Android note
    /// plays this client's own, at the 48 kHz the Ogg mapping states Opus at, one frame's
    /// worth of samples per packet and the same playing time as the PCM that went in.
    #[test]
    fn a_small_opus_note_decodes_to_pcm() {
        let samples = sine(
            VOICE_NOTE_SAMPLE_RATE as usize,
            VOICE_NOTE_SAMPLE_RATE,
            12_000.0,
        );
        let ogg = encode_ogg_opus(&samples, VOICE_NOTE_SAMPLE_RATE, 0x0B0B_0B0B)
            .expect("one second encodes");
        let decoded = decode_audio(&ogg).expect("the note decodes");
        assert_eq!(decoded.rate, 48_000);
        // 50 frames of 20 ms, 960 samples at 48 kHz each: exactly the second that went in.
        assert_eq!(decoded.samples.len(), 48_000);
        let loudest = decoded
            .samples
            .iter()
            .map(|sample| sample.unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(
            u32::from(loudest) > 8_000,
            "the decoded samples carry the encoded signal, not silence"
        );
    }

    /// The WAV this client sent before the switch still decodes — history must keep
    /// playing — and decodes to exactly the samples it was crafted from.
    #[test]
    fn a_wav_note_still_decodes() {
        let samples: Vec<i16> = (-100..100).map(|index| (index * 300) as i16).collect();
        let wav = wav_bytes(&samples, VOICE_NOTE_SAMPLE_RATE);
        let decoded = decode_audio(&wav).expect("a wav note decodes");
        assert_eq!(decoded.rate, VOICE_NOTE_SAMPLE_RATE);
        assert_eq!(decoded.samples, samples);
    }

    /// The waveform over an Opus note is the waveform of its decoded PCM: the bars the
    /// recorder sampled live, from the PCM that fed the encoder, and the bars taken over
    /// the decoded note agree on which tenths of a second were speech and which were
    /// silence — a lossy codec may move a peak a little, never a syllable's place.
    #[test]
    fn the_waveform_of_an_opus_note_matches_its_decoded_pcm() {
        let rate = VOICE_NOTE_SAMPLE_RATE as usize;
        let mut samples = vec![0i16; rate / 10 * 3]; // three tenths of silence
        samples.extend(sine(rate / 10 * 4, VOICE_NOTE_SAMPLE_RATE, 14_000.0)); // four of speech
        samples.extend(vec![0i16; rate / 10 * 3]); // three of silence

        // The bars the recorder samples while recording, one per tenth of a second.
        let live: Vec<u8> = samples.chunks(rate / 10).map(window_bar).collect();
        assert_eq!(live.len(), 10);

        let ogg = encode_ogg_opus(&samples, VOICE_NOTE_SAMPLE_RATE, 7).expect("the note encodes");
        let decoded = decode_audio(&ogg).expect("the note decodes");
        let decoded_bars: Vec<u8> = decoded
            .samples
            .chunks(decoded.rate as usize / 10)
            .map(window_bar)
            .collect();
        assert_eq!(
            decoded_bars.len(),
            10,
            "the decoded note is the same length"
        );

        for (index, bar) in decoded_bars.iter().enumerate() {
            let live_bar = live[index];
            match index {
                3..=6 => {
                    assert!(
                        live_bar >= 100,
                        "the live bars hear the speech (bar {index})"
                    );
                    assert!(
                        *bar >= 60,
                        "the decoded bars hear the same speech (bar {index}, live {live_bar})"
                    );
                }
                _ => {
                    assert_eq!(live_bar, 0, "the live bars hear the silence (bar {index})");
                    assert!(
                        *bar <= 16,
                        "the decoded bars hear the same silence (bar {index})"
                    );
                }
            }
        }
    }

    /// The incremental writer writes the same stream the whole-buffer writer does: a
    /// recording fed in whatever chunk sizes a microphone hands over becomes, byte for
    /// byte, the note the same PCM encodes in one push — the buffering is invisible in the
    /// stream, which is the whole point of recording into it incrementally.
    #[test]
    fn an_incrementally_encoded_note_is_the_note_encoded_whole() {
        let samples = sine(
            VOICE_NOTE_SAMPLE_RATE as usize * 137 / 100,
            VOICE_NOTE_SAMPLE_RATE,
            9_000.0,
        );
        let whole = encode_ogg_opus(&samples, VOICE_NOTE_SAMPLE_RATE, 0x00C0_FFEE)
            .expect("the whole-buffer encode succeeds");

        let mut buffer = Vec::new();
        {
            let mut encoder = OggOpusEncoder::new(&mut buffer, VOICE_NOTE_SAMPLE_RATE, 0x00C0_FFEE)
                .expect("the incremental encoder starts");
            let sizes = [7usize, 133, 1, 999, 61, 480];
            let mut offset = 0usize;
            let mut which = 0usize;
            while offset < samples.len() {
                let end = (offset + sizes[which % sizes.len()]).min(samples.len());
                encoder.push(&samples[offset..end]).expect("a chunk pushes");
                offset = end;
                which += 1;
            }
            encoder.finish(true).expect("the stream finishes");
        }
        assert_eq!(buffer, whole);
    }

    /// The playtime comes from the pages' own granule positions: a note with a tail shorter
    /// than one frame states the frames it actually holds, no page at all states nothing,
    /// and a stream whose last page was cut off by a death states the time its complete
    /// pages hold rather than a figure nobody can play.
    #[test]
    fn the_playtime_comes_from_the_pages_granule_positions() {
        // 1.23 s: 61 frames and a half — the tail pads to 62 frames, 1 240 ms.
        let samples = sine(
            VOICE_NOTE_SAMPLE_RATE as usize * 123 / 100,
            VOICE_NOTE_SAMPLE_RATE,
            9_000.0,
        );
        let ogg = encode_ogg_opus(&samples, VOICE_NOTE_SAMPLE_RATE, 5).expect("the note encodes");
        assert_eq!(ogg_opus_playtime_ms(&ogg), Some(1_240));

        assert_eq!(ogg_opus_playtime_ms(&[]), None, "no bytes state no time");

        let mut truncated = ogg.clone();
        truncated.truncate(truncated.len() - 1);
        assert_eq!(
            ogg_opus_playtime_ms(&truncated),
            None,
            "a stream whose only audio page was cut short states no time"
        );
    }

    /// A five-minute note at the speech bitrate fits the byte cap: two seconds encoded,
    /// scaled to the hundred and fifty two-second shares the cap holds, stays inside — the
    /// recorder refuses against the cap itself, and this is the arithmetic that keeps the
    /// refusal a formality.
    #[test]
    fn a_capped_recording_fits_the_byte_cap() {
        let samples = sine(
            VOICE_NOTE_SAMPLE_RATE as usize * 2,
            VOICE_NOTE_SAMPLE_RATE,
            10_000.0,
        );
        let ogg = encode_ogg_opus(&samples, VOICE_NOTE_SAMPLE_RATE, 1).expect("two seconds encode");
        assert!(
            ogg.len() as u64 * 150 <= VOICE_NOTE_MAX_BYTES,
            "two seconds is {}, so the five-minute cap needs {} bytes of the {} allowed",
            ogg.len(),
            ogg.len() * 150,
            VOICE_NOTE_MAX_BYTES
        );
    }

    /// A draft recorded at the previous release's rate encodes into the same container:
    /// the interchange is the Ogg, whatever rate the PCM inside it was taken at, and the
    /// decoded note is the same length of time the PCM was.
    #[test]
    fn a_legacy_pcm_rate_encodes_to_the_same_container() {
        let samples = sine(
            LEGACY_VOICE_NOTE_SAMPLE_RATE as usize,
            LEGACY_VOICE_NOTE_SAMPLE_RATE,
            12_000.0,
        );
        let ogg = encode_ogg_opus(&samples, LEGACY_VOICE_NOTE_SAMPLE_RATE, 9)
            .expect("one second at the legacy rate encodes");
        let decoded = decode_audio(&ogg).expect("the note decodes");
        assert_eq!(decoded.rate, 48_000);
        assert_eq!(decoded.samples.len(), 48_000, "one second, at 48 kHz");
    }

    /// The amplitude scale is the Android client's: silence is 0, full scale is 255, and the
    /// slope between them is the plain ratio, so the same sample draws the same bar on either
    /// client.
    #[test]
    fn an_amplitude_scales_to_its_bar() {
        assert_eq!(amplitude_to_bar(0), 0);
        assert_eq!(amplitude_to_bar(i16::MIN), 255);
        assert_eq!(amplitude_to_bar(i16::MAX), 255);
        // A quarter of full scale rounds to its own share, not down to nothing.
        assert_eq!(amplitude_to_bar(8_192), 63);
    }

    /// The fold keeps peaks, not averages: a syllable landing inside a bucket shows at its own
    /// height however quiet the silence around it, and the output is always exactly the fixed
    /// width — a short input pads with silence and an empty input is all silence.
    #[test]
    fn the_waveform_fold_keeps_peaks_at_a_fixed_width() {
        assert_eq!(downsample_waveform(&[]), vec![0u8; WAVEFORM_BARS]);
        // A note too short to fill the bars: the samples it did take keep their order at the
        // head and the tail stays silent.
        let short = downsample_waveform(&[10, 200, 30]);
        assert_eq!(short.len(), WAVEFORM_BARS);
        assert_eq!(&short[..3], &[10, 200, 30]);
        assert!(short[3..].iter().all(|bar| *bar == 0));
        // A sample count the width itself: one bar per bucket, every sample keeps its own
        // place, and the fold changes nothing.
        let bars: Vec<u8> = (0..WAVEFORM_BARS)
            .map(|bucket| if bucket == 7 { 240 } else { (bucket % 3) as u8 })
            .collect();
        let folded = downsample_waveform(&bars);
        assert_eq!(folded, bars);
        // More samples than buckets: the fold packs them three to a bucket (150 samples,
        // fifty bars), and each bucket keeps the peak that landed in it — the loud samples
        // sit at 5, 55, and 105, which the fold files under bars 1, 18, and 35.
        let long: Vec<u8> = (0..WAVEFORM_BARS * 3)
            .map(|index| if index % WAVEFORM_BARS == 5 { 128 } else { 1 })
            .collect();
        let folded = downsample_waveform(&long);
        assert_eq!(folded.len(), WAVEFORM_BARS);
        assert!(folded.iter().enumerate().all(|(slot, bar)| match slot {
            1 | 18 | 35 => *bar == 128,
            _ => *bar == 1,
        }));
    }
}
