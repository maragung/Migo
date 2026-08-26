//! The inner plaintext: what a message *is*, before it is sealed.
//!
//! Brief section 11 fixes these bytes — a `content_type` byte, an MSE body chosen by that byte, and
//! optional fixed-bucket padding — and forbids JSON inside the ciphertext. This module is the Rust
//! side of that contract, and it is a client-to-client contract rather than a client-to-server one:
//! the server never sees any of it. It mirrors `packages/sdk/src/content.ts` field for field, so a
//! message composed here opens on the web client and on Android and the reverse holds.
//!
//! # Why the body needs no length prefix
//!
//! The layout is `content_type || body || padding` with nothing marking where the body ends, because
//! nothing needs to: every MSE field is self-delimiting, so decoding the body consumes exactly its
//! own bytes and leaves the padding untouched. The decoder therefore never calls
//! [`Reader::finish`](migo_wire::Reader::finish) — trailing bytes here are padding, not a disagreement.
//!
//! # Why padding at all
//!
//! Ciphertext length leaks through the envelope even when its content does not: "sealed 12 bytes"
//! and "sealed 4000 bytes" are different observations to anyone counting bytes on a wire. Rounding
//! up to one of a few buckets collapses many lengths into one, so every message up to the bucket
//! size is indistinguishable by length.

use migo_core::Id;
use migo_wire::{Reader, Result as WireResult, WireError, Writer};

/// The kind of a decrypted body.
///
/// A distinct byte space from the protocol's `MessageKind`: that one travels in cleartext so the
/// server can route and count by coarse kind, this one lives inside the ciphertext and names the
/// exact struct that follows. They version separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ContentType {
    Text = 1,
    MediaRef = 2,
    VoiceNoteRef = 3,
    Reaction = 4,
    ControlEvent = 5,
}

impl ContentType {
    /// `None` for a type byte this build does not know — a message from a newer peer, which the
    /// interface renders as "unsupported" rather than treating as corruption.
    #[must_use]
    pub const fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Text,
            2 => Self::MediaRef,
            3 => Self::VoiceNoteRef,
            4 => Self::Reaction,
            5 => Self::ControlEvent,
            _ => return None,
        })
    }
}

/// A decrypted message body.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// Written text, with the users referenced inline for client-side highlighting.
    Text { text: String, mentions: Vec<Id> },
    /// A pointer to an encrypted blob in object storage. The server holds the ciphertext and cannot
    /// read it: `key` and `nonce` travel only here, inside this message's own ciphertext.
    MediaRef {
        media_id: Id,
        mime_type: String,
        size_bytes: u64,
        key: Vec<u8>,
        nonce: Vec<u8>,
        width: Option<u32>,
        height: Option<u32>,
        blurhash: Option<String>,
        caption: Option<String>,
    },
    /// A pointer to an encrypted voice note. `waveform` is a coarse amplitude preview for the UI.
    VoiceNoteRef {
        media_id: Id,
        mime_type: String,
        size_bytes: u64,
        duration_ms: u32,
        key: Vec<u8>,
        nonce: Vec<u8>,
        waveform: Option<Vec<u8>>,
    },
    /// An emoji reaction. `remove` retracts one the sender placed earlier.
    Reaction {
        target_message_id: Id,
        emoji: String,
        remove: bool,
    },
    /// An out-of-band signal that is not a chat message: an edit, a sender-key distribution, a
    /// revocation. `data` is opaque to this layer.
    ControlEvent {
        event: String,
        data: Option<Vec<u8>>,
    },
    /// A type byte this build does not know. Kept rather than dropped so the interface can say so.
    Unsupported { content_type: u8 },
}

impl Content {
    /// Plain text with no mentions — what the composer produces.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            mentions: Vec::new(),
        }
    }

    /// The type byte this body is written under.
    #[must_use]
    pub fn content_type(&self) -> u8 {
        match self {
            Self::Text { .. } => ContentType::Text as u8,
            Self::MediaRef { .. } => ContentType::MediaRef as u8,
            Self::VoiceNoteRef { .. } => ContentType::VoiceNoteRef as u8,
            Self::Reaction { .. } => ContentType::Reaction as u8,
            Self::ControlEvent { .. } => ContentType::ControlEvent as u8,
            Self::Unsupported { content_type } => *content_type,
        }
    }
}

/// The buckets an unpadded plaintext is rounded up to.
///
/// Fine at the low end so a one-word reply and a sentence look identical, coarse at the high end so
/// a large body is not fingerprinted to the byte. Past the largest bucket, lengths round up to the
/// next multiple of it.
const BUCKETS: [usize; 5] = [64, 256, 1024, 4096, 16384];

/// The padded length for an unpadded plaintext of `length` bytes.
fn bucket_for(length: usize) -> usize {
    for bucket in BUCKETS {
        if length <= bucket {
            return bucket;
        }
    }
    let largest = BUCKETS[BUCKETS.len() - 1];
    length.div_ceil(largest) * largest
}

/// Encodes a body to the section 11 inner plaintext, padded up to the next bucket unless `pad` is
/// false.
///
/// The padding bytes are zero. They are never read back — the decoder stops at the end of the MSE
/// struct — so their value is immaterial, and zero keeps the sealed ciphertext free of extra entropy
/// that might otherwise hint at where the real body ended.
pub fn encode(content: &Content, pad: bool) -> WireResult<Vec<u8>> {
    let mut w = Writer::new();
    encode_body(&mut w, content)?;
    let body = w.finish_vec()?;

    let unpadded = 1 + body.len();
    let total = if pad { bucket_for(unpadded) } else { unpadded };
    let mut out = vec![0u8; total];
    out[0] = content.content_type();
    out[1..1 + body.len()].copy_from_slice(&body);
    Ok(out)
}

/// Decodes the section 11 inner plaintext, ignoring any trailing padding.
pub fn decode(plaintext: &[u8]) -> WireResult<Content> {
    let Some((tag, body)) = plaintext.split_first() else {
        return Err(WireError::UnexpectedEnd {
            offset: 0,
            needed: 1,
        });
    };
    let Some(content_type) = ContentType::from_byte(*tag) else {
        return Ok(Content::Unsupported { content_type: *tag });
    };
    let mut r = Reader::from_slice(body);
    decode_body(content_type, &mut r)
}

/// Writes the MSE body for a content struct.
fn encode_body(w: &mut Writer, content: &Content) -> WireResult<()> {
    match content {
        Content::Text { text, mentions } => {
            w.enter()?;
            w.write_str(text)?;
            w.write_u32(u32::from(!mentions.is_empty()));
            if !mentions.is_empty() {
                w.optional(1, |sub| {
                    sub.list_len(mentions.len())?;
                    for id in mentions {
                        sub.write_id(id);
                    }
                    Ok(())
                })?;
            }
            w.leave();
        }
        Content::MediaRef {
            media_id,
            mime_type,
            size_bytes,
            key,
            nonce,
            width,
            height,
            blurhash,
            caption,
        } => {
            w.enter()?;
            w.write_id(media_id);
            w.write_str(mime_type)?;
            w.write_u64(*size_bytes);
            w.write_bytes(key)?;
            w.write_bytes(nonce)?;
            let present = usize::from(width.is_some())
                + usize::from(height.is_some())
                + usize::from(blurhash.is_some())
                + usize::from(caption.is_some());
            w.write_u32(present as u32);
            if let Some(v) = width {
                w.optional(1, |sub| {
                    sub.write_u32(*v);
                    Ok(())
                })?;
            }
            if let Some(v) = height {
                w.optional(2, |sub| {
                    sub.write_u32(*v);
                    Ok(())
                })?;
            }
            if let Some(v) = blurhash {
                w.optional(3, |sub| sub.write_str(v))?;
            }
            if let Some(v) = caption {
                w.optional(4, |sub| sub.write_str(v))?;
            }
            w.leave();
        }
        Content::VoiceNoteRef {
            media_id,
            mime_type,
            size_bytes,
            duration_ms,
            key,
            nonce,
            waveform,
        } => {
            w.enter()?;
            w.write_id(media_id);
            w.write_str(mime_type)?;
            w.write_u64(*size_bytes);
            w.write_u32(*duration_ms);
            w.write_bytes(key)?;
            w.write_bytes(nonce)?;
            w.write_u32(u32::from(waveform.is_some()));
            if let Some(v) = waveform {
                w.optional(1, |sub| sub.write_bytes(v))?;
            }
            w.leave();
        }
        Content::Reaction {
            target_message_id,
            emoji,
            remove,
        } => {
            w.enter()?;
            w.write_id(target_message_id);
            w.write_str(emoji)?;
            w.write_bool(*remove);
            w.write_u32(0);
            w.leave();
        }
        Content::ControlEvent { event, data } => {
            w.enter()?;
            w.write_str(event)?;
            w.write_u32(u32::from(data.is_some()));
            if let Some(v) = data {
                w.optional(1, |sub| sub.write_bytes(v))?;
            }
            w.leave();
        }
        // Never written. This variant only ever comes *out* of `decode`, where a type byte this
        // build does not know is parked so the interface can render it honestly. Round-tripping it
        // would mean re-sending a body we never parsed, so it is refused: `FieldOverflow` is the
        // codec's "this value does not belong in that field", and `content_type` is the field.
        Content::Unsupported { .. } => {
            return Err(WireError::FieldOverflow {
                field: "content_type",
            })
        }
    }
    Ok(())
}

/// Reads the MSE body for a content struct of the given type.
fn decode_body(content_type: ContentType, r: &mut Reader) -> WireResult<Content> {
    match content_type {
        ContentType::Text => {
            r.enter()?;
            let text = r.read_string()?;
            let mut mentions = Vec::new();
            for _ in 0..r.read_u32()? {
                let (field_id, mut sub) = r.read_optional()?;
                if field_id == 1 {
                    let count = sub.read_list_len()?;
                    mentions.reserve(count);
                    for _ in 0..count {
                        mentions.push(sub.read_id()?);
                    }
                }
            }
            r.leave();
            Ok(Content::Text { text, mentions })
        }
        ContentType::MediaRef => {
            r.enter()?;
            let media_id = r.read_id()?;
            let mime_type = r.read_string()?;
            let size_bytes = r.read_u64()?;
            let key = r.read_bytes()?;
            let nonce = r.read_bytes()?;
            let mut width = None;
            let mut height = None;
            let mut blurhash = None;
            let mut caption = None;
            for _ in 0..r.read_u32()? {
                let (field_id, mut sub) = r.read_optional()?;
                match field_id {
                    1 => width = Some(sub.read_u32()?),
                    2 => height = Some(sub.read_u32()?),
                    3 => blurhash = Some(sub.read_string()?),
                    4 => caption = Some(sub.read_string()?),
                    _ => {}
                }
            }
            r.leave();
            Ok(Content::MediaRef {
                media_id,
                mime_type,
                size_bytes,
                key,
                nonce,
                width,
                height,
                blurhash,
                caption,
            })
        }
        ContentType::VoiceNoteRef => {
            r.enter()?;
            let media_id = r.read_id()?;
            let mime_type = r.read_string()?;
            let size_bytes = r.read_u64()?;
            let duration_ms = r.read_u32()?;
            let key = r.read_bytes()?;
            let nonce = r.read_bytes()?;
            let mut waveform = None;
            for _ in 0..r.read_u32()? {
                let (field_id, mut sub) = r.read_optional()?;
                if field_id == 1 {
                    waveform = Some(sub.read_bytes()?);
                }
            }
            r.leave();
            Ok(Content::VoiceNoteRef {
                media_id,
                mime_type,
                size_bytes,
                duration_ms,
                key,
                nonce,
                waveform,
            })
        }
        ContentType::Reaction => {
            r.enter()?;
            let target_message_id = r.read_id()?;
            let emoji = r.read_string()?;
            let remove = r.read_bool()?;
            for _ in 0..r.read_u32()? {
                let _ = r.read_optional()?;
            }
            r.leave();
            Ok(Content::Reaction {
                target_message_id,
                emoji,
                remove,
            })
        }
        ContentType::ControlEvent => {
            r.enter()?;
            let event = r.read_string()?;
            let mut data = None;
            for _ in 0..r.read_u32()? {
                let (field_id, mut sub) = r.read_optional()?;
                if field_id == 1 {
                    data = Some(sub.read_bytes()?);
                }
            }
            r.leave();
            Ok(Content::ControlEvent { event, data })
        }
    }
}
