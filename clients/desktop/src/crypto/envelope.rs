//! The bytes that go in the opaque `envelope` field of a `MESSAGE_SEND` (brief section 11).
//!
//! The server never reads this. It sees a byte string of some length, routes it, and stores it. Both
//! ends encode it identically, which is the whole point: a message sealed by this client opens on
//! the web client and on Android, and vice versa. It mirrors `packages/sdk/src/session-crypto.ts`.
//!
//! ```text
//! u8      envelope_version           the version this build writes; 1 and 2 are both read
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
//!
//! # The version byte is read, not assumed
//!
//! Version 2 is version 1 with section 11's context appended to the associated data, and the change
//! is additive in exactly one direction: a version-2 envelope's tag covers `associated_data ||
//! header || context`, and a version-1 envelope is the same bytes with an empty tail, so a reader
//! that knows the context opens everything written before the flip. What it is *not* is backwards
//! compatible the other way, which is why the version had to wait for all four implementations to
//! land together, and why the parsed version travels on [`Envelope`] rather than being read as a
//! constant wherever the associated data is built: a reader has to reproduce the *sender's* bytes.

use bytes::BufMut;
use migo_crypto::identity::{IDENTITY_PUBLIC_LEN, PUBLIC_KEY_LEN};
use migo_crypto::{EnvelopeVersion, IdentityPublic, RatchetHeader};
use migo_wire::varint;

use super::CryptoError;

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
    /// The version the bytes declare, which is what decides the associated data the tag covers.
    ///
    /// It defaults to the version this build writes and is set by the parser to the byte it read,
    /// because the two are not always the same: a message written before the flip is version 1 and
    /// binds no context, and a reader that rebuilt the context anyway would refuse every message in
    /// stored history.
    pub version: EnvelopeVersion,
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
            version: EnvelopeVersion::WRITTEN,
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
            version: EnvelopeVersion::WRITTEN,
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
        out.put_u8(self.version.wire());
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

        // The accepted set is `migo-crypto`'s, not a second list kept here: a version this parser
        // agreed to read but the ratchet refuses to build a context for would be a version that
        // decodes and then cannot be opened.
        let version = EnvelopeVersion::from_wire(cursor.u8()?)
            .ok_or(CryptoError::Envelope("unsupported envelope version"))?;
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
            version,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ratchet_header() -> RatchetHeader {
        RatchetHeader {
            ratchet_key: [0x42; PUBLIC_KEY_LEN],
            previous_chain_length: 3,
            message_number: 5,
        }
    }

    /// 64 bytes of "ciphertext": comfortably more than the 16-byte AEAD tag the decoder demands,
    /// and opaque to it either way — the envelope layer never opens what it carries.
    fn established() -> Envelope {
        Envelope::established(ratchet_header(), vec![0xAB; 64])
    }

    #[test]
    fn an_established_envelope_round_trips() {
        let envelope = established();
        let bytes = envelope.encode().expect("a well-formed envelope encodes");
        let parsed = Envelope::decode(&bytes).expect("its own encoding decodes");

        // `Envelope` carries no `PartialEq`, and the wire bytes are the contract anyway: the
        // fields are asserted individually, then the parsed envelope is re-encoded and must
        // reproduce the exact bytes, which is the property a web or Android peer depends on.
        assert_eq!(parsed.scheme, SCHEME_DOUBLE_RATCHET);
        assert_eq!(parsed.sender_key_id, 0);
        assert!(parsed.preamble.is_none());
        assert_eq!(parsed.header.ratchet_key, [0x42; PUBLIC_KEY_LEN]);
        assert_eq!(parsed.header.message_number, 5);
        assert_eq!(parsed.header.previous_chain_length, 3);
        assert_eq!(parsed.ciphertext, vec![0xAB; 64]);
        assert_eq!(
            parsed.encode().expect("the parsed envelope re-encodes"),
            bytes
        );
    }

    #[test]
    fn a_version_no_build_writes_is_refused_clearly_and_deterministically() {
        // Brief section 176: a change to the envelope format needs a new `envelope_version`, and a
        // client must keep reading the old one because stored history cannot be re-encoded — which
        // is exactly what the version-2 flip did to this parser, and why the accepted set is
        // `migo-crypto`'s rather than a single current value. Every value the byte can hold is
        // swept, so the two the protocol defines are the only ones accepted, and every refusal is
        // the same named reason rather than a panic.
        //
        // What this test does *not* claim is that the accepted versions are interchangeable: the
        // version selects the associated data, and a version-1 label on a version-2 envelope is
        // refused by the AEAD rather than by the parser. `session.rs` pins that half, where the
        // ratchet is.
        let bytes = established().encode().expect("encodes");
        for byte in 0u8..=255 {
            let mut candidate = bytes.clone();
            candidate[0] = byte;
            match Envelope::decode(&candidate) {
                Ok(parsed) => assert!(
                    EnvelopeVersion::ACCEPTED.contains(&parsed.version),
                    "version {byte} decoded but is not one a reader must accept"
                ),
                Err(error) => {
                    assert!(
                        EnvelopeVersion::from_wire(byte).is_none(),
                        "version {byte} is one this build reads and was refused anyway"
                    );
                    assert!(
                        matches!(error, CryptoError::Envelope("unsupported envelope version")),
                        "version {byte} was refused as {error:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_prefix_of_an_envelope_is_answered_never_with_a_panic() {
        // The envelope is self-delimiting only by running to the end, so a truncated download with
        // a ciphertext tail of sixteen bytes or more parses as a *shorter valid envelope* — that
        // is the fixed layout's design, not a bug. What must hold for every prefix is weaker and
        // more important: the parser always answers (Ok or Err, never a panic or an index out of
        // bounds — history sync hands it bytes shaped by whatever wrote them), and whatever it
        // accepts is canonical, meaning re-encoding the parsed envelope reproduces the exact
        // prefix, so a truncated envelope can never decode into something claiming more bytes
        // than it has.
        let bytes = established().encode().expect("encodes");
        for len in 0..=bytes.len() {
            match Envelope::decode(&bytes[..len]) {
                Err(_) => {}
                Ok(parsed) => assert_eq!(
                    parsed.encode().expect("an accepted envelope re-encodes"),
                    &bytes[..len],
                    "a prefix of {len} bytes decoded non-canonically"
                ),
            }
        }
    }
}
