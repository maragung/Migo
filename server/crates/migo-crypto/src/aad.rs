//! What an envelope's tag authenticates besides its contents.
//!
//! The AEAD in `aead.rs` authenticates a ciphertext against an *associated data*
//! string. That string is where a protocol binds the metadata it is not willing
//! to encrypt but is not willing to leave forgeable either — and until envelope
//! version 2, Migo's bound less than the specification asks for.
//!
//! # What version 1 binds, and the hole that leaves
//!
//! A v1 associated data is `x3dh_associated_data || ratchet_header`: the two
//! device identities, through X3DH's `IK_initiator || IK_responder`, and the
//! ratchet public key with its two counters. The identity binding is the
//! anti-unknown-key-share protection section 11 cares about most, and it holds.
//! What v1 does not bind is *context*: which conversation the envelope belongs
//! to, which device sent it, which message it is, and which version and scheme
//! the envelope claims.
//!
//! Section 11 names the consequence: without it, "E2E melindungi isi tetapi tidak
//! melindungi konteks". A server that can read the `conversation_id` field of a
//! `MessageSend` frame can rewrite it, and the tag will still verify — the
//! receiver computes the same associated data either way. For an ordinary
//! message that is a denial of service at worst. For a *sender-key
//! distribution*, which is the pairwise envelope this layer exists to carry, it
//! is worse: the distribution installs a chain key, and moving one into a
//! conversation it was not meant for installs a chain there that the sender
//! never authorised.
//!
//! # What version 2 binds, and why the layout is what it is
//!
//! The v2 associated data is the v1 one followed by a context naming the five
//! fields of section 11:
//!
//! ```text
//! "migo-envelope-aad"   17 bytes   purpose label, so these bytes are not any other structure's
//! envelope_version      u8         must equal the envelope's own version byte
//! scheme                u8         must equal the envelope's own scheme byte
//! sender_device         16 bytes   the device the frame claims to be from
//! conversation_id       16 bytes   the conversation the frame claims to belong to
//! flags                 u8         bit 0: a message id follows
//! message_id            16 bytes   present only when flags bit 0 is set
//! ```
//!
//! Every field but the message id is fixed width, so no length prefix is needed
//! and no two different field assignments can encode to the same bytes. The
//! message id gets a flag byte rather than a length prefix because it is the
//! only optional field: one byte states everything the receiver needs, and a
//! length that could disagree with the flag is a length that will.
//!
//! The context is built from the metadata the *frame claims*, not from anything
//! the receiver independently knows. That is the whole mechanism: the receiver
//! recomputes the context from what it was told, the tag only verifies if the
//! sender bound exactly those values, and a server that edits any of them turns
//! a readable message into a failed authentication instead of a silent
//! relocation.
//!
//! # The version-1 rule
//!
//! [`context`] returns empty bytes for [`EnvelopeVersion::V1`], whatever
//! metadata the caller passes. This is not a convenience: reading the version
//! byte before the tag has been verified is only safe while a v1 envelope's
//! associated data does not depend on the metadata, and a receiver must be able
//! to compute the right associated data for both versions from the byte it sees.
//! If a v1 context ever became non-empty, every stored v1 message would fail to
//! open, and the failure would look like corruption rather than a version
//! mistake.
//!
//! The cross-language vectors in `shared/protocol/vectors/crypto/aad-context.json`
//! pin both the layout and that rule, and the four implementations are checked
//! against them.

use migo_core::id::ID_BYTE_LEN;
use migo_core::Id;

use crate::error::CryptoError;

/// The purpose label every v2 context starts with.
pub const DOMAIN: &[u8] = b"migo-envelope-aad";

/// Bit 0 of the flags byte: a message id follows.
pub const FLAG_MESSAGE_ID: u8 = 0x01;

/// Flags no build writes. A receiver refuses a context carrying one, because a
/// bit whose meaning is undefined is a bit an attacker chooses the meaning of.
pub const KNOWN_FLAGS: u8 = FLAG_MESSAGE_ID;

/// Double Ratchet, no prekey material. The 1:1 path.
pub const SCHEME_DOUBLE_RATCHET: u8 = 1;
/// Double Ratchet with X3DH material attached: the first message of a session.
pub const SCHEME_DOUBLE_RATCHET_PREKEY: u8 = 2;

/// An envelope version, as a value that cannot be constructed from a number the
/// protocol does not define.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvelopeVersion {
    /// Binds identities and the ratchet header, and nothing else.
    V1,
    /// Binds the five context fields of section 11 as well.
    V2,
}

impl EnvelopeVersion {
    /// Every version a reader must accept, oldest first.
    pub const ACCEPTED: [Self; 2] = [Self::V1, Self::V2];

    /// The version byte as it travels.
    #[must_use]
    pub const fn wire(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }

    /// Reads a version byte, or `None` for a value no build writes.
    ///
    /// A reader refuses anything outside [`Self::ACCEPTED`] rather than
    /// defaulting, because a default would mean computing an associated data for
    /// a layout the sender did not choose.
    #[must_use]
    pub const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::V1),
            2 => Some(Self::V2),
            _ => None,
        }
    }

    /// Whether this version's associated data carries a context.
    #[must_use]
    pub const fn binds_context(self) -> bool {
        matches!(self, Self::V2)
    }
}

/// Builds the context an envelope of `version` binds.
///
/// Returns empty bytes for [`EnvelopeVersion::V1`] — see the module docs; that is
/// the compatibility rule, not an optimisation.
///
/// `message_id` is `None` for an envelope that is not a message: a sender-key
/// distribution rides inside the message that carries it, so at seal time it has
/// no id of its own to bind, and pretending otherwise by binding the carrier's id
/// would bind a value the receiver does not have.
#[must_use]
pub fn context(
    version: EnvelopeVersion,
    scheme: u8,
    sender_device: &Id,
    conversation_id: &Id,
    message_id: Option<&Id>,
) -> Vec<u8> {
    if !version.binds_context() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(DOMAIN.len() + 3 + ID_BYTE_LEN * 3);
    out.extend_from_slice(DOMAIN);
    out.push(version.wire());
    out.push(scheme);
    out.extend_from_slice(sender_device.as_bytes());
    out.extend_from_slice(conversation_id.as_bytes());
    out.push(if message_id.is_some() {
        FLAG_MESSAGE_ID
    } else {
        0
    });
    if let Some(id) = message_id {
        out.extend_from_slice(id.as_bytes());
    }
    out
}

/// Assembles the associated data a tag is computed over: `v1 prefix || context`.
///
/// The one place the two halves are joined, so a reader and a writer of the same
/// version cannot disagree about the order.
#[must_use]
pub fn assemble(associated_data: &[u8], header: &[u8], context: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(associated_data.len() + header.len() + context.len());
    out.extend_from_slice(associated_data);
    out.extend_from_slice(header);
    out.extend_from_slice(context);
    out
}

/// Reads the fields a context claims back out of its bytes.
///
/// A receiver does not need this — it builds the context from the metadata it was
/// handed and lets the tag decide. It exists for the conformance vectors and for
/// diagnostics, so it validates rather than assumes: a buffer that is not a
/// well-formed context, or that carries a flag no build writes, is refused.
///
/// # Errors
///
/// [`CryptoError::BadLength`] if the buffer is truncated, and
/// [`CryptoError::MalformedHeader`] for an unknown flag byte.
pub fn parse_context(bytes: &[u8]) -> crate::Result<ParsedContext> {
    let fixed = DOMAIN.len() + 3 + ID_BYTE_LEN * 2;
    if bytes.len() < fixed {
        return Err(CryptoError::BadLength {
            what: "envelope aad context",
            expected: fixed,
            actual: bytes.len(),
        });
    }
    if &bytes[..DOMAIN.len()] != DOMAIN {
        return Err(CryptoError::MalformedHeader);
    }
    let mut at = DOMAIN.len();
    let version_value = bytes[at];
    let version = EnvelopeVersion::from_wire(version_value).ok_or(CryptoError::MalformedHeader)?;
    at += 1;
    let scheme = bytes[at];
    at += 1;
    let mut device = [0u8; ID_BYTE_LEN];
    device.copy_from_slice(&bytes[at..at + ID_BYTE_LEN]);
    at += ID_BYTE_LEN;
    let mut conversation = [0u8; ID_BYTE_LEN];
    conversation.copy_from_slice(&bytes[at..at + ID_BYTE_LEN]);
    at += ID_BYTE_LEN;
    let flags = bytes[at];
    at += 1;
    if flags & !KNOWN_FLAGS != 0 {
        return Err(CryptoError::MalformedHeader);
    }
    let message_id = if flags & FLAG_MESSAGE_ID != 0 {
        if bytes.len() < at + ID_BYTE_LEN {
            return Err(CryptoError::BadLength {
                what: "envelope aad context message id",
                expected: at + ID_BYTE_LEN,
                actual: bytes.len(),
            });
        }
        let mut id = [0u8; ID_BYTE_LEN];
        id.copy_from_slice(&bytes[at..at + ID_BYTE_LEN]);
        at += ID_BYTE_LEN;
        Some(Id::from_bytes(id))
    } else {
        None
    };
    if at != bytes.len() {
        return Err(CryptoError::BadLength {
            what: "envelope aad context",
            expected: at,
            actual: bytes.len(),
        });
    }
    Ok(ParsedContext {
        version,
        scheme,
        sender_device: Id::from_bytes(device),
        conversation_id: Id::from_bytes(conversation),
        message_id,
    })
}

/// The fields a context carries, as [`parse_context`] recovered them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedContext {
    /// The envelope version the context binds.
    pub version: EnvelopeVersion,
    /// The scheme byte the context binds.
    pub scheme: u8,
    /// The sending device the context binds.
    pub sender_device: Id,
    /// The conversation the context binds.
    pub conversation_id: Id,
    /// The message id the context binds, when it carries one.
    pub message_id: Option<Id>,
}
