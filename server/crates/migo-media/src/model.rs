//! Types the media service takes and returns.
//!
//! # Why none of these are protocol structs
//!
//! Brief section 145 reserves opcodes 128 to 133 for the media frames —
//! `MEDIA_UPLOAD_BEGIN`, `MEDIA_UPLOAD_STATUS`, `MEDIA_UPLOAD_COMMIT`,
//! `MEDIA_UPLOAD_ABORT`, `MEDIA_FETCH_URL`, `MEDIA_STATE_EVENT` — and section 168
//! marks the block `STATUS: SPEC untuk opcode media. Kebijakan sudah final`. None of
//! the six is in the generated packet registry, so there is no `MediaUploadBegin` wire
//! struct to accept and no `MediaStateEvent` to publish.
//!
//! These types exist instead, and the API layer maps them. The policy they encode is
//! not provisional even though the frames are: the brief says the policy is final, and
//! a domain crate does not get to extend the wire format on the way past.
//!
//! # Why a URL is wrapped in `Secret`
//!
//! Brief section 69 is unusually direct about this: *"Signed URL TIDAK BOLEH ditulis
//! ke log, ke analytics, atau ke crash report, karena URL itu sendiri adalah
//! kredensial"*. A signed URL is a bearer token that happens to be shaped like an
//! address, and every logging habit in the industry — log the request, log the
//! response, attach the response to the crash report — treats addresses as safe to
//! print.
//!
//! So the URL never exists in this crate as a bare `String`. A [`Grant`] holds it in a
//! [`Secret`], whose `Debug` renders `Secret(*** n bytes)`, which means the ordinary
//! ways a value leaks into a log line — `tracing`'s `?field` sigil, a `dbg!` left
//! behind, `#[derive(Debug)]` on some enclosing request struct — cannot leak this one.
//! Reaching the characters requires calling [`Grant::expose`], which is one grep away
//! from any reviewer asking where the URLs go.

use migo_core::{Id, Secret, Timestamp};
use migo_protocol::EncryptionMode;
use migo_ratelimit::TrustTier;
use migo_store::model::media_scan;

/// Largest object any deployment will accept, whatever it configures.
///
/// One gibibyte. Not a policy — a sanity bound, so a typo in a configuration file
/// cannot turn one upload into a disk-space incident. Real ceilings are per kind and
/// live in [`Policy`].
pub const HARD_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// Chunk size offered to clients.
///
/// Eight mebibytes, which is the smallest part size S3 multipart accepts for anything
/// but the final part. Choosing the floor is deliberate: the brief requires a failure
/// at eighty percent to resume near eighty percent, and resume granularity is the
/// chunk size. A larger chunk means a cheaper upload and a more expensive retry, and
/// on the mobile networks this has to work on, retries are the common case.
pub const CHUNK_BYTES: u32 = 8 * 1024 * 1024;

/// How long an upload ticket stays valid.
///
/// Thirty minutes. Long enough for a large video on a slow connection, short enough
/// that a ticket recovered from a device backup months later is worthless.
pub const TICKET_TTL_MS: i64 = 30 * 60 * 1000;

/// Leading bytes read back from storage to identify a format.
///
/// Thirty-two. Enough for every signature in [`crate::sniff()`] — the longest useful one
/// is an ISO base media `ftyp` brand ending at offset twelve — and small enough that
/// reading it is not the byte proxying brief section 168 forbids.
pub const SNIFF_BYTES: usize = 32;

/// Longest MIME type accepted from a client.
///
/// Two hundred and fifty-five, because the ticket encodes the length in one byte. Real
/// MIME types are under fifty characters; anything approaching this bound is somebody
/// probing the parser.
pub const MAX_MIME_LEN: usize = 255;

/// Longest content hash accepted from a client.
///
/// Sixty-four bytes, which covers SHA-512. The column is `bytea` with no length
/// constraint, and an unbounded field a client controls is a row-size attack.
pub const MAX_CHECKSUM_LEN: usize = 64;

/// Default duration ceiling for a voice note, in milliseconds.
///
/// Five minutes, from brief section 122: *"Voice note, dengan batas durasi default 5
/// menit dan dapat dikonfigurasi"*.
pub const VOICE_NOTE_MAX_MS: u32 = 5 * 60 * 1000;

/// What kind of thing was uploaded.
///
/// The six kinds brief section 122 tells the server to set separate limits for. Stored
/// in `media_object.kind`, which is a `smallint` with no check constraint, so this enum
/// is the only place the mapping is written down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MediaKind {
    /// A profile picture. Readable by anybody who can see the profile.
    #[default]
    Avatar = 0,
    /// A still image.
    Image = 1,
    /// A video.
    Video = 2,
    /// Music or a recording that is not a voice note.
    Audio = 3,
    /// A push-to-talk recording. See brief section 167.
    VoiceNote = 4,
    /// Anything else a user attaches.
    Document = 5,
}

impl MediaKind {
    /// Every kind, for registering metric series and for iterating limits.
    pub const ALL: [Self; 6] = [
        Self::Avatar,
        Self::Image,
        Self::Video,
        Self::Audio,
        Self::VoiceNote,
        Self::Document,
    ];

    /// The stored discriminant.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Reads a stored discriminant.
    ///
    /// An unrecognised value becomes `Document`, the kind with the loosest content
    /// rules and no content-based validation. A row written by a newer build must not
    /// make an older build panic, and it must not accidentally acquire the *weaker*
    /// checks of a kind it is not — resolving to `Document` is the conservative
    /// direction, because `Document` is the one kind this crate never claims to have
    /// identified by its bytes.
    #[must_use]
    pub const fn of_i16(value: i16) -> Self {
        match value {
            0 => Self::Avatar,
            1 => Self::Image,
            2 => Self::Video,
            3 => Self::Audio,
            4 => Self::VoiceNote,
            _ => Self::Document,
        }
    }

    /// Stable label for a metric series.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Avatar => "avatar",
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::VoiceNote => "voice_note",
            Self::Document => "document",
        }
    }

    /// Index into a per-kind array.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Whether the server should recognise the leading bytes of this kind.
    ///
    /// Everything but `Document`. An image, a video, an audio track, a voice note, and
    /// an avatar all live in container formats with well-known signatures, so a server
    /// that cannot name the format is looking at something that is not what the client
    /// said it was. Documents are the open set — a text file, a CSV, a font — and
    /// demanding a recognised signature there would reject legitimate uploads to make a
    /// check that a determined uploader defeats by prepending eight bytes.
    #[must_use]
    pub const fn expects_known_format(self) -> bool {
        !matches!(self, Self::Document)
    }
}

/// Where an upload is going.
///
/// The distinction that makes authorization possible at download time. A conversation
/// destination is checked against membership; a profile destination is checked against
/// nothing, because an avatar is rendered by everybody who can see the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    /// A direct conversation, a group, or a room's conversation.
    ///
    /// One variant for all three, because `migo_store` gives a room's conversation
    /// exactly the same `conversation_member` rows a group has — joining a room
    /// inserts one. So a single membership question authorises every case, and the
    /// alternative, a `Room(Id)` variant with its own lookup, would be a second copy
    /// of a check that is already correct.
    Conversation(Id),
    /// Profile media, readable by any authenticated account.
    Profile,
}

impl Destination {
    /// The value stored in `media_object.conversation_id`.
    #[must_use]
    pub const fn conversation_id(self) -> Option<Id> {
        match self {
            Self::Conversation(id) => Some(id),
            Self::Profile => None,
        }
    }

    /// Reads a stored column.
    #[must_use]
    pub const fn of_column(value: Option<Id>) -> Self {
        match value {
            Some(id) => Self::Conversation(id),
            None => Self::Profile,
        }
    }
}

/// Whether an object is cleared to be served to somebody other than its owner.
///
/// Brief section 168: *"Scan status media yang dapat dibaca server memiliki tiga nilai:
/// pending, clean, rejected. Media pending TIDAK BOLEH disajikan ke pengguna lain"*.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scan {
    /// Uploaded, not yet cleared. Servable only to the uploader.
    #[default]
    Pending,
    /// Cleared to serve.
    ///
    /// Either a scan passed, or the object is end-to-end encrypted and therefore
    /// unscannable by design — see [`Policy::clearance_at_commit`].
    Clean,
    /// Refused. The bytes are gone; the row stays so the same checksum is not rescanned.
    Rejected,
}

impl Scan {
    /// Every value, for registering metric series.
    pub const ALL: [Self; 3] = [Self::Pending, Self::Clean, Self::Rejected];

    /// The stored discriminant.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        match self {
            Self::Pending => media_scan::PENDING,
            Self::Clean => media_scan::CLEAN,
            Self::Rejected => media_scan::REJECTED,
        }
    }

    /// Reads a stored discriminant.
    ///
    /// Anything unrecognised is `Pending`, which is the value that withholds the
    /// object. A row from a newer build with a scan state this build does not
    /// understand must not be served on the strength of not being understood.
    #[must_use]
    pub const fn of_i16(value: i16) -> Self {
        match value {
            media_scan::CLEAN => Self::Clean,
            media_scan::REJECTED => Self::Rejected,
            _ => Self::Pending,
        }
    }

    /// Stable label for a metric series.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Clean => "clean",
            Self::Rejected => "rejected",
        }
    }

    /// Index into a per-status array.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Pending => 0,
            Self::Clean => 1,
            Self::Rejected => 2,
        }
    }
}

/// What a scanner decided.
///
/// Two values, not three: `Pending` is the absence of a decision, and a scanner that
/// could report it would be reporting that it did not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing found. The object may be served.
    Clean,
    /// Refused. The caller is expected to have removed the bytes already.
    Rejected,
}

impl Verdict {
    /// The scan status this verdict writes.
    #[must_use]
    pub const fn status(self) -> Scan {
        match self {
            Self::Clean => Scan::Clean,
            Self::Rejected => Scan::Rejected,
        }
    }
}

/// Who is asking.
///
/// No `reauthenticated` flag: nothing here is step-up protected. Uploading is not a
/// privileged action, and deleting one's own upload is less destructive than the
/// account deletion that does require a step-up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection the request arrived on.
    ///
    /// Recorded because brief section 69 asks for a *"private attachment token yang
    /// terikat pada account dan device"*: the upload ticket is bound to both, so a
    /// ticket lifted from one device cannot be committed from another.
    pub device_id: Id,
    /// Standing, for the rate limiter.
    pub tier: TrustTier,
    /// Server time for this request.
    pub now: Timestamp,
    /// Correlation id, for joining a trace to a log line.
    pub request_id: Option<String>,
}

impl Caller {
    /// A caller at `now`.
    #[must_use]
    pub fn new(account_id: Id, device_id: Id, tier: TrustTier, now: Timestamp) -> Self {
        Self {
            account_id,
            device_id,
            tier,
            now,
            request_id: None,
        }
    }

    /// Sets the correlation id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}

/// What a client asks for when it wants to upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRequest {
    /// What kind of object it is.
    pub kind: MediaKind,
    /// The MIME type the client believes it is sending.
    ///
    /// Recorded in the ticket and then *checked against the bytes* at commit. Brief
    /// section 122: *"Jangan percaya Content-Type dari client"*. This field is a hint
    /// used to reject early; it is not what ends up in the row unless the bytes agree.
    pub mime: String,
    /// How many bytes the client intends to upload.
    ///
    /// Declared up front so quota and size limits are enforced before a single byte
    /// reaches storage, and verified against the object's real size at commit.
    pub byte_size: u64,
    /// Where it is going.
    pub destination: Destination,
    /// Pixel width, for an image or a video.
    pub width: Option<u32>,
    /// Pixel height, for an image or a video.
    pub height: Option<u32>,
    /// Duration, for audio, a voice note, or a video.
    pub duration_ms: Option<u32>,
}

/// What a client sends to finish an upload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Commit {
    /// How many bytes the client says it uploaded, when the wire it came over carries a
    /// size at all.
    ///
    /// Checked against what storage reports, because brief section 168: *"server
    /// memverifikasi ukuran serta content hash lalu membuat record media"*. The MWP
    /// commit carries only a digest — the size it agrees on is storage's own count
    /// against the size the ticket was issued for — so `None` is the wire's honest "I
    /// did not say", and not a claim of zero.
    pub byte_size: Option<u64>,
    /// A hash of the plaintext, computed by the client.
    ///
    /// The server records it and never recomputes it: for end-to-end media it cannot,
    /// because it holds ciphertext, and a hash the server computes over ciphertext
    /// would answer a question nobody asked. Its value is client-side deduplication
    /// and integrity checking after download.
    pub checksum: Option<Vec<u8>>,
}

/// A signed URL and the moment it stops working.
///
/// See the module docs for why the URL is a [`Secret`].
#[derive(Debug)]
pub struct Grant {
    url: Secret,
    /// When the signature expires.
    pub expires_at: Timestamp,
}

impl Grant {
    /// Wraps a URL a storage backend just signed.
    #[must_use]
    pub fn new(url: impl Into<String>, expires_at: Timestamp) -> Self {
        Self {
            url: Secret::new(url),
            expires_at,
        }
    }

    /// The URL, for putting on the wire and nowhere else.
    ///
    /// Every call site is a place a reviewer has to check. There should be exactly two
    /// in the whole server: the frame encoder, and the storage backend's own tests.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.url.expose()
    }

    /// How long the grant has left, in milliseconds, at `now`.
    #[must_use]
    pub const fn remaining_ms(&self, now: Timestamp) -> i64 {
        self.expires_at.as_millis().saturating_sub(now.as_millis())
    }
}

/// Everything a client needs to start pushing bytes.
#[derive(Debug)]
pub struct Ticket {
    /// The id the object will have once committed.
    ///
    /// Handed out before the row exists so the client can reference the attachment in
    /// a message it is composing, and so a retry of `begin` that the client never saw
    /// the answer to does not leave two objects behind — the second `begin` mints a
    /// new id, and the abandoned one never becomes a row at all.
    pub media_id: Id,
    /// The opaque token to send back with status, commit, and abort.
    ///
    /// Authenticated, self-describing, and not stored anywhere on the server. See
    /// [`crate::ticket`].
    pub token: Vec<u8>,
    /// Where to put the bytes.
    pub upload: Grant,
    /// How large each chunk should be.
    pub chunk_bytes: u32,
    /// When the ticket stops being accepted.
    pub expires_at: Timestamp,
}

/// How far an unfinished upload got.
///
/// Answers brief section 168's resume requirement: *"Kegagalan pada 80 persen
/// dilanjutkan dari sekitar 80 persen, bukan dari nol"*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// The object being uploaded.
    pub media_id: Id,
    /// How many bytes storage already holds.
    pub uploaded_bytes: u64,
    /// How many bytes the ticket was issued for.
    pub byte_size: u64,
    /// When the ticket stops being accepted.
    pub expires_at: Timestamp,
}

impl Progress {
    /// Whether storage holds every byte the ticket promised.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.uploaded_bytes >= self.byte_size
    }
}

/// An object, as a client sees it.
///
/// # What is deliberately missing
///
/// No `storage_key` and no `conversation_id`. The key is the server's private naming
/// of a private bucket and telling a client about it invites the belief that it can be
/// fetched directly, which is the thing signed URLs exist to prevent. The destination
/// is an authorization input, and echoing an authorization input back to the party
/// being authorized is how a check becomes a suggestion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    /// The object.
    pub media_id: Id,
    /// Who uploaded it.
    pub owner_id: Id,
    /// What kind of object it is.
    pub kind: MediaKind,
    /// The MIME type, as identified from the bytes where that was possible.
    pub mime: String,
    /// Size in bytes, as storage reported it.
    pub byte_size: u64,
    /// Pixel width, if the client supplied one.
    pub width: Option<u32>,
    /// Pixel height, if the client supplied one.
    pub height: Option<u32>,
    /// Duration, if the client supplied one.
    pub duration_ms: Option<u32>,
    /// The conversation the object was uploaded into, when it was uploaded into one.
    ///
    /// An avatar has none. A conversation attachment has one, and the committer's next
    /// act is to tell that conversation the object exists — which is exactly why the
    /// projection carries it: the reply's recipient is the uploader, and the uploader
    /// is the one party that does not need telling.
    pub conversation_id: Option<Id>,
    /// Whether it may be served to somebody other than its owner.
    pub scan: Scan,
    /// The content hash the client supplied.
    pub checksum: Option<Vec<u8>>,
    /// When it was committed.
    pub created_at: Timestamp,
}

/// Per-kind ceilings and the two numbers deployment sets.
///
/// # Why the limits live here and not in `MediaConfig`
///
/// `migo_core::config::MediaConfig` has one `max_upload_bytes`, because that is the
/// number an operator wants to set: the largest thing this deployment will hold.
/// Brief section 122 asks for six, one per kind, and those are *product* decisions —
/// an avatar has no business being sixteen mebibytes whatever the operator allows.
///
/// So the six defaults are written here and every one of them is clamped to the
/// operator's ceiling by [`Policy::from_config`]. An operator lowering
/// `max_upload_bytes` lowers all six; an operator raising it never raises an avatar
/// past two mebibytes, because that limit was never about disk space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Largest object of each kind, indexed by [`MediaKind::index`].
    pub max_bytes: [u64; 6],
    /// Longest voice note.
    pub voice_note_max_ms: u32,
    /// How long a signed download URL lasts.
    pub download_ttl_ms: i64,
    /// How long an upload ticket lasts.
    pub ticket_ttl_ms: i64,
    /// Chunk size offered to clients.
    pub chunk_bytes: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_bytes: [
                2 * 1024 * 1024,   // Avatar: a face, at a sane resolution.
                16 * 1024 * 1024,  // Image: a phone camera's full-quality JPEG.
                128 * 1024 * 1024, // Video: a minute or two at phone bitrates.
                32 * 1024 * 1024,  // Audio: a full track.
                8 * 1024 * 1024,   // VoiceNote: five minutes of low-bitrate speech.
                32 * 1024 * 1024,  // Document: a slide deck with pictures in it.
            ],
            voice_note_max_ms: VOICE_NOTE_MAX_MS,
            download_ttl_ms: 300_000,
            ticket_ttl_ms: TICKET_TTL_MS,
            chunk_bytes: CHUNK_BYTES,
        }
    }
}

impl Policy {
    /// Applies a deployment's configuration to the product defaults.
    ///
    /// Every per-kind ceiling is clamped to `max_upload_bytes`, and that value is
    /// itself clamped to [`HARD_MAX_BYTES`]. Clamping rather than replacing is the
    /// whole point: configuration sets the roof, not the room sizes.
    #[must_use]
    pub fn from_config(config: &migo_core::config::MediaConfig) -> Self {
        let ceiling = config.max_upload_bytes.clamp(1, HARD_MAX_BYTES);
        let defaults = Self::default();
        let mut max_bytes = defaults.max_bytes;
        for limit in &mut max_bytes {
            *limit = (*limit).min(ceiling);
        }
        Self {
            max_bytes,
            download_ttl_ms: i64::try_from(config.signed_url_ttl_seconds.saturating_mul(1000))
                .unwrap_or(i64::MAX),
            ..defaults
        }
    }

    /// Largest object of one kind.
    #[must_use]
    pub const fn max_bytes(&self, kind: MediaKind) -> u64 {
        self.max_bytes[kind.index()]
    }

    /// What scan status an object gets the moment it is committed.
    ///
    /// # The one place the brief needs a reading rather than a quotation
    ///
    /// Two rules in brief section 168 have to hold at once. *"Media pending TIDAK BOLEH
    /// disajikan ke pengguna lain"*, and *"Media E2E tidak dapat discan server"*. An
    /// end-to-end object left `Pending` because nothing can ever scan it would be
    /// permanently unservable, which would delete the feature rather than protect it.
    ///
    /// The resolution is in the sentence that introduces the three values: *"Scan
    /// status media **yang dapat dibaca server** memiliki tiga nilai"*. The axis
    /// belongs to server-readable media. For end-to-end media there is no scan to wait
    /// for, so the object is cleared when it is committed, and section 69 says plainly
    /// where its protection lives instead: *"Untuk media E2E, perlindungan berada di
    /// sisi client, yaitu batas ukuran, validasi tipe setelah dekripsi, dan pelaporan
    /// oleh user"*.
    ///
    /// The value of writing it this way is that the *serving* rule needs no branch:
    /// a non-owner is served an object when it is `Clean`, always, and there is no
    /// second path through the authorization code that a later change could widen by
    /// accident.
    #[must_use]
    pub const fn clearance_at_commit(encryption: EncryptionMode) -> Scan {
        match encryption {
            EncryptionMode::EndToEnd => Scan::Clean,
            // Unknown counts as scannable, and therefore as pending. A conversation
            // whose mode this build does not recognise is one this build must not
            // clear on its own authority.
            _ => Scan::Pending,
        }
    }
}
