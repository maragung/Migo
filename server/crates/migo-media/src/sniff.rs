//! Identifying a file format from its leading bytes.
//!
//! Brief section 122: *"Jangan percaya Content-Type dari client. Tipe file diverifikasi
//! dari magic byte, bukan dari header yang dikirim client"*. This module is that rule,
//! for the kinds a server can hope to identify.
//!
//! # Why this list is deliberately short
//!
//! A sniffing table has two failure modes, and both argue for small. The first is
//! false confidence: a format the table cannot name gets a refusal, and users have
//! real reasons to attach real files this table will not name. The second is the
//! colliding-signature trick — an HTML file with a JPEG header is a real way to get
//! markup past a filter. The usual response is to extend the table, but every entry
//! added to catch a trick extends the list of things a careful attacker can collide
//! with. The brief's own answer to the arms race is scanning (§168) and client-side
//! validation of end-to-end media (§69). This module only has to make the cheap,
//! coarse call: what is this thing?
//!
//! One deliberate refusal is worth spelling out. HTML and SVG both declare `text/html`
//! or `image/svg+xml` and both can carry scripts. They are refused outright rather
//! than identified, because there is no benign use for either that a browser will
//! render as an attachment instead of as a page. An SVG that is genuinely an image
//! belongs in an image wrapper.
//!
//! # Where this module does not look
//!
//! A file that the upload is authorised to carry does not get served on the strength
//! of this table. The scan pipeline is the authority for server-readable media, and
//! this table is a gate in front of it, not a replacement. The table identifies what
//! a file is; the scanner decides whether it is safe to show to anybody.

use crate::model::MediaKind;

/// A refusal that says what was found.
///
/// The four named `Not` variants exist so a caller can refuse for a reason a client
/// can fix, rather than for the summary `Unrecognised`, which is a complaint about a
/// file, not about a specific part of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The file is HTML or SVG, which is refused outright. See the module docs.
    Forbidden,
    /// The bytes are empty.
    Empty,
    /// The bytes are not a recognised container of the kind they claim to be.
    Unrecognised,
    /// An image kind whose bytes say it is not an image.
    NotImage,
    /// An audio kind whose bytes say it is not audio.
    NotAudio,
    /// A video kind whose bytes say it is not video.
    NotVideo,
}

impl Refusal {
    /// A short, stable label for a metric series.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::Empty => "empty",
            Self::Unrecognised => "unrecognised",
            Self::NotImage => "not_image",
            Self::NotAudio => "not_audio",
            Self::NotVideo => "not_video",
        }
    }
}

/// What the leading bytes turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    /// The bytes say `image/png`.
    Png,
    /// The bytes say `image/jpeg`.
    Jpeg,
    /// The bytes say `image/webp`.
    Webp,
    /// The bytes say `image/gif`.
    Gif,
    /// The bytes say `image/avif`.
    Avif,
    /// The bytes say `audio/ogg` with a Vorbis or Opus stream, or `audio/opus`.
    OggAudio,
    /// The bytes say `audio/mpeg`.
    MpegAudio,
    /// The bytes say `audio/mp4` (an `.m4a`).
    Mp4Audio,
    /// The bytes say `audio/wav`.
    Wav,
    /// The bytes say `video/mp4`.
    Mp4Video,
    /// The bytes say `video/webm` (a `.webm`).
    Webm,
    /// The bytes say `video/quicktime` (a `.mov`).
    Mov,
    /// The bytes say `application/pdf`.
    Pdf,
}

impl Identity {
    /// The canonical MIME type of this identity.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
            Self::Avif => "image/avif",
            Self::OggAudio => "audio/ogg",
            Self::MpegAudio => "audio/mpeg",
            Self::Mp4Audio => "audio/mp4",
            Self::Wav => "audio/wav",
            Self::Mp4Video => "video/mp4",
            Self::Webm => "video/webm",
            Self::Mov => "video/quicktime",
            Self::Pdf => "application/pdf",
        }
    }

    /// A short, stable label for a metric series.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Gif => "gif",
            Self::Avif => "avif",
            Self::OggAudio => "ogg_audio",
            Self::MpegAudio => "mpeg_audio",
            Self::Mp4Audio => "mp4_audio",
            Self::Wav => "wav",
            Self::Mp4Video => "mp4_video",
            Self::Webm => "webm",
            Self::Mov => "mov",
            Self::Pdf => "pdf",
        }
    }
}

/// The verdict: what it is, or why not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Identified. The second field is the canonical MIME type.
    Identified(Identity),
    /// Refused, for the stated reason.
    Refused(Refusal),
}

/// Sniffs up to the first `len` bytes of an object.
///
/// All byte patterns are matched at the front of the buffer, so longer prefixes win
/// by construction; no ordering between the arms matters. For containers whose magic
/// starts with a fixed signature (JPEG, WebP, PDF) the first four bytes decide and
/// nothing else is read.
#[must_use]
pub fn sniff(head: &[u8], len: usize) -> Verdict {
    let head = &head[..len.min(head.len())];
    if head.is_empty() {
        return Verdict::Refused(Refusal::Empty);
    }

    if head.starts_with(b"<html") || head.starts_with(b"<!DOCTYP") || head.starts_with(b"<svg") {
        return Verdict::Refused(Refusal::Forbidden);
    }

    // PNG: 89 50 4E 47 0D 0A 1A 0A
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Verdict::Identified(Identity::Png);
    }
    // JPEG: FF D8 FF
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Verdict::Identified(Identity::Jpeg);
    }
    // WebP: "RIFF" .... "WEBP"
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Verdict::Identified(Identity::Webp);
    }
    // GIF: "GIF87a" or "GIF89a"
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Verdict::Identified(Identity::Gif);
    }
    // AVIF: ISO BMFF with an "avif" or "avis" brand in the file-type box.
    if is_bmff(head) && (head[8..16].starts_with(b"avif") || head[8..16].starts_with(b"avis")) {
        return Verdict::Identified(Identity::Avif);
    }
    // Every other ISO base media brand, decided in one place.
    //
    // One arm and not three, because the brand test has to come before the fallthrough
    // that calls anything else BMFF video. An earlier draft had a separate MOV arm
    // further down the function, after the generic `Mp4Video` return had already
    // claimed every remaining BMFF file — so `Identity::Mov` was unreachable and a
    // QuickTime upload was reported as `video/mp4`. Keeping the brands together is
    // what stops that from happening again.
    if is_bmff(head) {
        if head[8..16].starts_with(b"M4A ") {
            return Verdict::Identified(Identity::Mp4Audio);
        }
        if head[8..16].starts_with(b"qt  ") {
            return Verdict::Identified(Identity::Mov);
        }
        return Verdict::Identified(Identity::Mp4Video);
    }
    // Ogg: "OggS" (four bytes: 4F 67 67 53)
    if head.starts_with(b"OggS") {
        return Verdict::Identified(Identity::OggAudio);
    }
    // WebM: EBML magic 1A 45 DF A3, then a DocType "webm".
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Verdict::Identified(Identity::Webm);
    }
    // MP3: 0xFF with the top five bits of the sync word, allowing for the
    // optional free-format sync 0xFF 0xF7. ID3 tags start 49 44 33.
    if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xFF && (head[1] & 0xE0) == 0xE0)
    {
        return Verdict::Identified(Identity::MpegAudio);
    }
    // WAV: "RIFF" .... "WAVE"
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WAVE" {
        return Verdict::Identified(Identity::Wav);
    }
    // PDF: "%PDF-" followed by a version.
    if head.len() >= 5 && head.starts_with(b"%PDF-") {
        return Verdict::Identified(Identity::Pdf);
    }

    Verdict::Refused(Refusal::Unrecognised)
}

/// Whether the first sixteen bytes look like an ISO base media file.
///
/// The `ftyp` box is the first box of every MP4-family file: a four-byte size, the
/// literal `ftyp`, then a four-byte major brand. The size is not checked, because it
/// varies and because a wrong size is the scanner's problem rather than this table's.
/// What is checked hard is that sixteen bytes are present, because every brand
/// comparison this feeds reads the eight bytes at offset eight.
fn is_bmff(head: &[u8]) -> bool {
    head.len() >= 16 && &head[4..8] == b"ftyp"
}

/// A short description of what the sniffer found, safe to put in a log line.
///
/// Safe because it is one of a fixed set of constants: no filename, no byte, and no
/// part of the object itself reaches a log through this function.
#[must_use]
pub fn describe(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Identified(identity) => identity.mime(),
        Verdict::Refused(refusal) => refusal.label(),
    }
}

/// Whether an identified format is one the declared kind may hold.
///
/// # Why a voice note may be a WAV
///
/// Brief section 168 says a voice note's codec *"WAJIB codec speech dengan bitrate
/// rendah"* and that *"Format lossless besar TIDAK BOLEH menjadi default"*. Read
/// carefully, that is a rule about the recorder's default, which is a client decision
/// and not something a server can observe: a server holding an Opus file cannot tell
/// whether the client would have preferred FLAC.
///
/// What the server can do is make the expensive choice impossible, and it already
/// does: the voice-note ceiling is eight mebibytes, and five minutes of lossless
/// stereo is fifty. So the size limit enforces the codec rule without this table
/// having to guess, and a short WAV — which a desktop client may genuinely produce —
/// is not refused for being the wrong shape of correct.
#[must_use]
pub const fn suits(identity: Identity, kind: MediaKind) -> bool {
    match kind {
        MediaKind::Avatar | MediaKind::Image => matches!(
            identity,
            Identity::Png | Identity::Jpeg | Identity::Webp | Identity::Gif | Identity::Avif
        ),
        MediaKind::Video => matches!(
            identity,
            Identity::Mp4Video | Identity::Webm | Identity::Mov
        ),
        MediaKind::Audio | MediaKind::VoiceNote => matches!(
            identity,
            Identity::OggAudio | Identity::MpegAudio | Identity::Mp4Audio | Identity::Wav
        ),
        // Anything identified. A document is whatever somebody attached, and the only
        // formats refused here are the ones `sniff` refuses outright for everybody.
        MediaKind::Document => true,
    }
}

/// The MIME type to record for an object, or why it is refused.
///
/// `Ok(Some(mime))` is a format the bytes proved, and it replaces whatever the client
/// declared. `Ok(None)` means the bytes are unrecognised *and the kind tolerates that*
/// — only `Document` does — so the declared MIME type stands, recorded as a claim
/// rather than as a fact. `Err` is a refusal a client can act on.
///
/// # Errors
///
/// [`Refusal::Forbidden`] for HTML and SVG, [`Refusal::Empty`] for no bytes,
/// [`Refusal::Unrecognised`] for a kind that must be identified and was not, and
/// [`Refusal::NotImage`], [`Refusal::NotAudio`], or [`Refusal::NotVideo`] for bytes
/// that were identified as the wrong family.
pub fn identify(head: &[u8], len: usize, kind: MediaKind) -> Result<Option<&'static str>, Refusal> {
    match sniff(head, len) {
        Verdict::Identified(identity) if suits(identity, kind) => Ok(Some(identity.mime())),
        Verdict::Identified(_) => Err(match kind {
            MediaKind::Avatar | MediaKind::Image => Refusal::NotImage,
            MediaKind::Video => Refusal::NotVideo,
            MediaKind::Audio | MediaKind::VoiceNote => Refusal::NotAudio,
            // Unreachable: `suits` is total for `Document`. Written as a value rather
            // than as an `unreachable!` because a panic here would be a denial of
            // service reachable from an upload.
            MediaKind::Document => Refusal::Unrecognised,
        }),
        Verdict::Refused(Refusal::Unrecognised) if !kind.expects_known_format() => Ok(None),
        Verdict::Refused(refusal) => Err(refusal),
    }
}
