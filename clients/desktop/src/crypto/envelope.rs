//! The bytes that go in the opaque `envelope` field of a `MESSAGE_SEND` (brief section 11).
//!
//! The server never reads this. It sees a byte string of some length, routes it, and stores it. Both
//! ends encode it identically, which is the whole point: a message sealed by this client opens on
//! the web client and on Android, and vice versa. It mirrors `packages/sdk/src/session-crypto.ts`.
//!
//! ```text
//! u8      envelope_version           always ENVELOPE_VERSION
//! u8      scheme                     decides which fields follow
//! varint  sender_key_id              0 for 1:1; the field exists for the group layout
//! -- X3DH preamble, present only for SCHEME_DOUBLE_RATCHET_PREKEY --
//! 64      initiator_identity         IdentityPublic::to_bytes(); lets the responder run X3DH
//! 32      ephemeral_key              the initiator's X3DH ephemeral public key
//! varint  signed_prekey_id           which of the responder's signed prekeys was used
//! u8      has_one_time_prekey        1 if a one-time prekey was used, else 0
//! varint  one_time_prekey_id         present only when has_one_time_prekey is 1
//! -- Double Ratchet header + body --
//! 32      ratchet_public_key         the sender's current ratchet public key
//! varint  message_counter            index within the sender's current chain
//! varint  previous_chain_length      messages the sender sent in its previous chain
//! bytes   ciphertext                 to the end; the trailing 16 bytes are the AEAD tag
//! ```
//!
//! # No field names, and no JSON
//!
//! Section 11 forbids JSON inside the envelope. Field names would cost bytes on every message and
//! leak structure through length, and the layout is fixed on both ends anyway, so there is nothing
//! for a name to disambiguate. Everything is positional; the `scheme` byte is what varies the shape.
//!
//! # A separate scheme rather than a flag for the first message
//!
//! `SCHEME_DOUBLE_RATCHET_PREKEY` changes which fields are *present*, not just how one is
//! interpreted. That is what a scheme is for; a boolean flag whose value silently adds a hundred
//! bytes to the layout is how parsers end up disagreeing about where the ciphertext starts.

use bytes::BufMut;
use migo_crypto::identity::{IDENTITY_PUBLIC_LEN, PUBLIC_KEY_LEN};
use migo_crypto::{IdentityPublic, RatchetHeader};
use migo_wire::varint;

use super::CryptoError;

/// The only envelope version this build writes, and the only one it reads.
pub const ENVELOPE_VERSION: u8 = 1;

/// An established 1:1 Double Ratchet message — no X3DH preamble.
pub const SCHEME_DOUBLE_RATCHET: u8 = 1;
/// A 1:1 first message: the same ratchet body, preceded by the X3DH material the peer needs.
pub const SCHEME_DOUBLE_RATCHET_PREKEY: u8 = 2;
/// A group (sender-key) message. Belongs to the group layer, not this one.
pub const SCHEME_SENDER_KEY: u8 = 3;

/// The X3DH material a first message carries so the responder can derive the same secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preamble {
    /// The initiator's long-term identity.
    pub identity: IdentityPublic,
    /// The initiator's ephemeral public key for this session.
    pub ephemeral_key: [u8; PUBLIC_KEY_LEN],
    /// Which of the responder's signed prekeys was used.
    pub signed_prekey_id: u32,
    /// Which one-time prekey was used, if any.
    pub one_time_prekey_id: Option<u32>,
}

/// A parsed or about-to-be-written envelope.
#[derive(Debug, Clone)]
pub struct Envelope {
    /// Which of the `SCHEME_*` constants this is.
    pub scheme: u8,
    /// `0` for 1:1. Named in the layout because the group layout needs it in the same position.
    pub sender_key_id: u32,
    /// Present exactly when `scheme` is [`SCHEME_DOUBLE_RATCHET_PREKEY`].
    pub preamble: Option<Preamble>,
    /// The ratchet header: sender's ratchet key, counter, previous chain length.
    pub header: RatchetHeader,
    /// The AEAD output, tag included.
    pub ciphertext: Vec<u8>,
}

impl Envelope {
    /// An envelope for a message in an established session.
    #[must_use]
    pub fn established(header: RatchetHeader, ciphertext: Vec<u8>) -> Self {
        Self {
            scheme: SCHEME_DOUBLE_RATCHET,
            sender_key_id: 0,
            preamble: None,
            header,
            ciphertext,
        }
    }

    /// An envelope for the first message of a session, carrying the X3DH preamble.
    #[must_use]
    pub fn initial(preamble: Preamble, header: RatchetHeader, ciphertext: Vec<u8>) -> Self {
        Self {
            scheme: SCHEME_DOUBLE_RATCHET_PREKEY,
            sender_key_id: 0,
            preamble: Some(preamble),
            header,
            ciphertext,
        }
    }

    /// Serialises the envelope.
    pub fn encode(&self) -> Result<Vec<u8>, CryptoError> {
        if self.scheme == SCHEME_DOUBLE_RATCHET_PREKEY && self.preamble.is_none() {
            return Err(CryptoError::Envelope(
                "prekey scheme without an X3DH preamble",
            ));
        }
        if self.scheme == SCHEME_DOUBLE_RATCHET && self.preamble.is_some() {
            return Err(CryptoError::Envelope(
                "established scheme with an X3DH preamble",
            ));
        }

        let mut out = Vec::with_capacity(
            2 + 5
                + IDENTITY_PUBLIC_LEN
                + PUBLIC_KEY_LEN
                + 16
                + RatchetHeader::ENCODED_LEN
                + self.ciphertext.len(),
        );
        out.put_u8(ENVELOPE_VERSION);
        out.put_u8(self.scheme);
        varint::encode_u64(u64::from(self.sender_key_id), &mut out);

        if let Some(preamble) = &self.preamble {
            out.extend_from_slice(&preamble.identity.to_bytes());
            out.extend_from_slice(&preamble.ephemeral_key);
            varint::encode_u64(u64::from(preamble.signed_prekey_id), &mut out);
            match preamble.one_time_prekey_id {
                Some(id) => {
                    out.put_u8(1);
                    varint::encode_u64(u64::from(id), &mut out);
                }
                None => out.put_u8(0),
            }
        }

        out.extend_from_slice(&self.header.ratchet_key);
        varint::encode_u64(u64::from(self.header.message_number), &mut out);
        varint::encode_u64(u64::from(self.header.previous_chain_length), &mut out);
        out.extend_from_slice(&self.ciphertext);
        Ok(out)
    }

    /// Parses an envelope.
    ///
    /// Every failure is one of a handful of static reasons and none of them carry bytes. These are
    /// attacker-supplied inputs, they end up in logs, and a log line is not the place for a
    /// half-parsed ciphertext (brief section 174).
    pub fn decode(bytes: &[u8]) -> Result<Self, CryptoError> {
        let mut cursor = Cursor::new(bytes);

        let version = cursor.u8()?;
        if version != ENVELOPE_VERSION {
            return Err(CryptoError::Envelope("unsupported envelope version"));
        }
        let scheme = cursor.u8()?;
        let sender_key_id = cursor.varint_u32()?;

        let preamble = match scheme {
            SCHEME_DOUBLE_RATCHET => None,
            SCHEME_DOUBLE_RATCHET_PREKEY => {
                let identity = IdentityPublic::parse(cursor.take(IDENTITY_PUBLIC_LEN)?)
                    .map_err(|_| CryptoError::Envelope("initiator identity is not usable"))?;
                let mut ephemeral_key = [0u8; PUBLIC_KEY_LEN];
                ephemeral_key.copy_from_slice(cursor.take(PUBLIC_KEY_LEN)?);
                let signed_prekey_id = cursor.varint_u32()?;
                let one_time_prekey_id = match cursor.u8()? {
                    0 => None,
                    1 => Some(cursor.varint_u32()?),
                    // Canonical or rejected: `2` is not "true with spare bits", it is a sender this
                    // parser does not agree with, and guessing is how a parsing bug becomes a
                    // security bug.
                    _ => return Err(CryptoError::Envelope("one-time-prekey flag is not 0 or 1")),
                };
                Some(Preamble {
                    identity,
                    ephemeral_key,
                    signed_prekey_id,
                    one_time_prekey_id,
                })
            }
            SCHEME_SENDER_KEY => {
                return Err(CryptoError::Envelope("sender-key envelope on the 1:1 path"))
            }
            _ => return Err(CryptoError::Envelope("unknown envelope scheme")),
        };

        let mut ratchet_key = [0u8; PUBLIC_KEY_LEN];
        ratchet_key.copy_from_slice(cursor.take(PUBLIC_KEY_LEN)?);
        let message_number = cursor.varint_u32()?;
        let previous_chain_length = cursor.varint_u32()?;
        let ciphertext = cursor.rest().to_vec();
        if ciphertext.len() < migo_crypto::TAG_LEN {
            return Err(CryptoError::Envelope(
                "ciphertext is shorter than an AEAD tag",
            ));
        }

        Ok(Self {
            scheme,
            sender_key_id,
            preamble,
            header: RatchetHeader {
                ratchet_key,
                previous_chain_length,
                message_number,
            },
            ciphertext,
        })
    }
}

/// A forward-only reader over the envelope bytes.
///
/// Its own small type rather than [`migo_wire::Reader`] because the envelope is not MSE: it is a
/// fixed byte layout with raw varints and one run of bytes that continues to the end. Borrowing the
/// struct reader here would mean pretending the envelope has a struct header it does not have.
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn u8(&mut self) -> Result<u8, CryptoError> {
        let byte = *self
            .bytes
            .get(self.offset)
            .ok_or(CryptoError::Envelope("envelope ended mid-field"))?;
        self.offset += 1;
        Ok(byte)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CryptoError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(CryptoError::Envelope("envelope length overflow"))?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(CryptoError::Envelope("envelope ended mid-field"))?;
        self.offset = end;
        Ok(slice)
    }

    fn varint_u32(&mut self) -> Result<u32, CryptoError> {
        let (value, consumed) = varint::decode_u64(self.bytes, self.offset)
            .map_err(|_| CryptoError::Envelope("malformed varint"))?;
        self.offset += consumed;
        u32::try_from(value).map_err(|_| CryptoError::Envelope("varint does not fit its field"))
    }

    fn rest(&mut self) -> &'a [u8] {
        let slice = &self.bytes[self.offset.min(self.bytes.len())..];
        self.offset = self.bytes.len();
        slice
    }
}
