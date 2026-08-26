//! The upload ticket: an authenticated, self-describing token with no server state.
//!
//! # Why the ticket is not a row and not a cache entry
//!
//! Brief section 168 describes `MEDIA_UPLOAD_BEGIN` as issuing *"upload ticket berisi
//! signed URL, upload_id, ukuran chunk, dan masa berlaku"*, and says that *"Media yang
//! tidak di-commit dalam batas waktu tertentu dibersihkan oleh job terjadwal"*. It does
//! not say where the ticket lives, and the obvious two answers are both worse than the
//! one taken here.
//!
//! A database row per begun upload means a row for every upload a user starts and
//! abandons — every cancelled picture, every backgrounded app, every dead network — and
//! a sweeper whose job is to delete rows that never mattered. A cache entry means the
//! upload fails when Redis restarts, and brief section 173 is explicit that losing the
//! cache must not lose anything that matters.
//!
//! So the ticket carries its own contents and a MAC over them. Nothing is written until
//! commit, which means an abandoned upload leaves no row at all — only bytes in the
//! bucket, which is exactly what an object-storage lifecycle rule is for, and which is
//! the *"job terjadwal"* the brief asks for. The server holds no state between begin
//! and commit and therefore cannot get that state wrong.
//!
//! # What the MAC buys
//!
//! Everything the server would otherwise have to trust the client for at commit time:
//! which object this is, who owns it, which device may finish it, where it is going,
//! how large it was allowed to be, whether the destination is end-to-end, and when the
//! permission runs out. All seven were decided at begin, when the server had the
//! conversation row and the quota in front of it. Re-deciding them at commit would mean
//! re-reading all of it, and *not* re-deciding them without a MAC would mean a client
//! could commit a two-gigabyte object against a two-mebibyte avatar ticket.
//!
//! Binding the device satisfies brief section 69's *"Private attachment token yang
//! terikat pada account dan device"*.
//!
//! # Format
//!
//! Fixed-width big-endian, hand-rolled rather than serialized with anything, because
//! the layout is ninety-seven bytes and a dependency that can produce a different
//! encoding next minor version is a dependency that can invalidate every ticket in
//! flight during a rolling deploy.
//!
//! ```text
//! offset  size  field
//!      0     1  VERSION
//!      1    16  media_id
//!     17    16  owner account_id
//!     33    16  device_id
//!     49    16  conversation_id, nil for profile media
//!     65     2  kind
//!     67     8  byte_size
//!     75     8  expires_at, milliseconds
//!     83     1  flags
//!     84     4  width in pixels, zero when the client supplied none
//!     88     4  height in pixels, zero when the client supplied none
//!     92     4  duration in milliseconds, zero when the client supplied none
//!     96     1  mime length
//!     97     n  mime, UTF-8
//!   97+n    32  tag over bytes 0..97+n
//! ```
//!
//! # Why the dimensions are in here
//!
//! They are the client's own description of its object and the server never verifies
//! them, so carrying them looks like carrying nothing. What they buy is that the three
//! numbers `begin` accepted are the three numbers `commit` writes. The alternative is
//! to take them again at commit, which means a voice note whose duration was checked
//! against this deployment's ceiling at begin can be committed with a different one, and
//! the check becomes decorative. Zero means absent: no real object has a zero width, a
//! zero height, or a zero duration, and a presence bit per field would be three flags
//! and three branches to save nothing.

use migo_core::{Error, Id, Timestamp};
use migo_crypto::mac::{MacKey, TAG_LEN};
use migo_protocol::fault;

use crate::model::{Destination, MediaKind, MAX_MIME_LEN};

/// Layout version. Bumping it invalidates every ticket in flight, by design.
///
/// Version two added the three dimension fields. A ticket minted by a version-one build
/// is refused rather than parsed, which during a rolling deploy costs an upload its
/// `begin` call and nothing else, because no ticket is worth more than its thirty
/// minutes and nothing is written until commit.
const VERSION: u8 = 2;

/// Everything before the MIME string.
const HEADER_LEN: usize = 97;

/// Set when the destination conversation is end-to-end encrypted.
const FLAG_END_TO_END: u8 = 1 << 0;

/// Domain separator inside the MAC'd message.
///
/// The ticket key is derived with `LABEL_MEDIA_URL`, which is also the label a
/// filesystem storage backend uses to sign download URLs. Two different message shapes
/// under one key is exactly the setup that produces a cross-protocol forgery, so every
/// message this module authenticates starts with a constant that no URL starts with.
/// Cheap, and it removes a whole class of question from a security review.
const DOMAIN: &[u8] = b"migo-upload-ticket-v1";

/// What a ticket asserts.
///
/// Every field was decided by the server at begin, with the conversation row and the
/// account's quota in hand. None of it is re-derived at commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The object this ticket is for.
    pub media_id: Id,
    /// The account that may commit it.
    pub owner_id: Id,
    /// The device that may commit it.
    pub device_id: Id,
    /// Where the object is going.
    pub destination: Destination,
    /// What kind of object it is.
    pub kind: MediaKind,
    /// The largest size that may be committed.
    pub byte_size: u64,
    /// When this ticket stops being accepted.
    pub expires_at: Timestamp,
    /// Whether the destination conversation is end-to-end encrypted.
    ///
    /// Decides two things at commit: whether the bytes can be identified by their
    /// leading signature, and whether the object is cleared to serve immediately. Both
    /// were decided from the conversation row at begin; carrying the answer means
    /// commit does not read the conversation again, which also means a conversation
    /// that switched modes mid-upload does not change the rules the upload started
    /// under.
    pub end_to_end: bool,
    /// The MIME type the client declared.
    pub mime: String,
    /// Pixel width the client declared, if any.
    pub width: Option<u32>,
    /// Pixel height the client declared, if any.
    pub height: Option<u32>,
    /// Duration the client declared, if any.
    ///
    /// For a voice note this is the value checked against the deployment's ceiling at
    /// begin, which is why it travels inside the MAC rather than being taken again.
    pub duration_ms: Option<u32>,
}

impl Claim {
    /// Whether the server may look at these bytes.
    #[must_use]
    pub const fn server_readable(&self) -> bool {
        !self.end_to_end
    }
}

/// Authenticates a claim into a token.
///
/// # Panics
///
/// Never, for a claim this crate built: the MIME length is checked before a ticket is
/// minted. A caller constructing a `Claim` by hand with an over-long MIME gets a
/// truncated one rather than a panic, and commit will then disagree with it.
#[must_use]
pub fn seal(key: &MacKey, claim: &Claim) -> Vec<u8> {
    let mime = claim.mime.as_bytes();
    let mime = &mime[..mime.len().min(MAX_MIME_LEN)];

    let mut body = Vec::with_capacity(HEADER_LEN + mime.len() + TAG_LEN);
    body.push(VERSION);
    body.extend_from_slice(claim.media_id.as_bytes());
    body.extend_from_slice(claim.owner_id.as_bytes());
    body.extend_from_slice(claim.device_id.as_bytes());
    body.extend_from_slice(
        claim
            .destination
            .conversation_id()
            .unwrap_or(Id::NIL)
            .as_bytes(),
    );
    body.extend_from_slice(&claim.kind.to_i16().to_be_bytes());
    body.extend_from_slice(&claim.byte_size.to_be_bytes());
    body.extend_from_slice(&claim.expires_at.as_millis().to_be_bytes());
    body.push(if claim.end_to_end { FLAG_END_TO_END } else { 0 });
    body.extend_from_slice(&claim.width.unwrap_or(0).to_be_bytes());
    body.extend_from_slice(&claim.height.unwrap_or(0).to_be_bytes());
    body.extend_from_slice(&claim.duration_ms.unwrap_or(0).to_be_bytes());
    body.push(u8::try_from(mime.len()).unwrap_or(0));
    body.extend_from_slice(mime);

    debug_assert_eq!(body.len(), HEADER_LEN + mime.len());
    let tag = key.tag_parts(&[DOMAIN, &body]);
    body.extend_from_slice(&tag);
    body
}

/// Why a token was not accepted.
///
/// # Why this is an enum and not two `Error` values
///
/// Both variants become the same `VALIDATION_FAILED` on the wire — see
/// [`Rejection::into_error`] — so the caller cannot tell them apart from the `Error`
/// alone. It needs to, because they mean opposite things operationally: an expired
/// ticket is a slow network and a forged one is somebody probing the MAC, and those
/// belong on different metric series. Returning a typed reason is how the caller gets
/// the distinction without matching on a string, which is a thing that works right up
/// until somebody rewords a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The ticket's expiry has passed.
    Expired,
    /// Anything else: a bad length, a bad version, a tag that did not verify, a MIME
    /// string that is not UTF-8.
    Unusable,
}

impl Rejection {
    /// The refusal a client sees.
    ///
    /// Expiry says so, because an honest client needs to know to call begin again.
    /// Everything else says only `unusable`: telling a forger which of six checks it
    /// failed is a free oracle, and no legitimate client can act on the answer.
    #[must_use]
    pub fn into_error(self) -> Error {
        match self {
            Self::Expired => fault::validation("upload_ticket", "expired"),
            Self::Unusable => fault::validation("upload_ticket", "unusable"),
        }
    }
}

impl From<Rejection> for Error {
    fn from(rejection: Rejection) -> Self {
        rejection.into_error()
    }
}

/// Verifies a token and reads what it asserts.
///
/// # Order of operations
///
/// Length, then MAC, then everything else. Nothing but the length is believed before the
/// tag verifies, and the tag is checked with a constant-time comparison inside
/// `MacKey::verify_parts`. The version byte is read after authentication even though it
/// selects the layout, because there is exactly one layout: a token claiming version two
/// is a token this build refuses, not a token this build parses differently.
///
/// # Errors
///
/// [`Rejection::Expired`] for a ticket whose time has passed, [`Rejection::Unusable`]
/// for everything else.
pub fn open(key: &MacKey, token: &[u8], now: Timestamp) -> Result<Claim, Rejection> {
    if token.len() < HEADER_LEN + TAG_LEN || token.len() > HEADER_LEN + MAX_MIME_LEN + TAG_LEN {
        return Err(Rejection::Unusable);
    }
    let split = token.len() - TAG_LEN;
    let (body, tag) = token.split_at(split);
    key.verify_parts(&[DOMAIN, body], tag)
        .map_err(|_| Rejection::Unusable)?;

    if body[0] != VERSION {
        return Err(Rejection::Unusable);
    }
    let mime_len = usize::from(body[96]);
    if body.len() != HEADER_LEN + mime_len {
        return Err(Rejection::Unusable);
    }
    let mime = core::str::from_utf8(&body[HEADER_LEN..]).map_err(|_| Rejection::Unusable)?;

    let expires_at = Timestamp::from_millis(i64::from_be_bytes(read8(body, 75)));
    if now.as_millis() >= expires_at.as_millis() {
        return Err(Rejection::Expired);
    }

    let conversation = Id::from_bytes(read16(body, 49));
    Ok(Claim {
        media_id: Id::from_bytes(read16(body, 1)),
        owner_id: Id::from_bytes(read16(body, 17)),
        device_id: Id::from_bytes(read16(body, 33)),
        destination: if conversation.is_nil() {
            Destination::Profile
        } else {
            Destination::Conversation(conversation)
        },
        kind: MediaKind::of_i16(i16::from_be_bytes([body[65], body[66]])),
        byte_size: u64::from_be_bytes(read8(body, 67)),
        expires_at,
        end_to_end: body[83] & FLAG_END_TO_END != 0,
        mime: mime.to_string(),
        width: absent_if_zero(u32::from_be_bytes(read4(body, 84))),
        height: absent_if_zero(u32::from_be_bytes(read4(body, 88))),
        duration_ms: absent_if_zero(u32::from_be_bytes(read4(body, 92))),
    })
}

/// Turns the wire's zero back into the absence it stands for.
const fn absent_if_zero(value: u32) -> Option<u32> {
    if value == 0 {
        None
    } else {
        Some(value)
    }
}

/// Reads sixteen bytes at `offset`. The caller has already checked the length.
fn read16(body: &[u8], offset: usize) -> [u8; 16] {
    let mut out = [0u8; 16];
    out.copy_from_slice(&body[offset..offset + 16]);
    out
}

/// Reads eight bytes at `offset`. The caller has already checked the length.
fn read8(body: &[u8], offset: usize) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(&body[offset..offset + 8]);
    out
}

/// Reads four bytes at `offset`. The caller has already checked the length.
fn read4(body: &[u8], offset: usize) -> [u8; 4] {
    let mut out = [0u8; 4];
    out.copy_from_slice(&body[offset..offset + 4]);
    out
}
