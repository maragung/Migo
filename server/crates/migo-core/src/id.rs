//! 128-bit identifiers.
//!
//! Every entity in Migo is identified by a 16-byte [`Id`] laid out as a ULID:
//! a 48-bit big-endian millisecond timestamp followed by 80 bits of
//! randomness. That gives us three properties we want at the same time:
//!
//! * **Lexicographically sortable** — the text form sorts in creation order,
//!   so `ORDER BY id` is a time order and B-tree inserts stay at the right
//!   edge of the index instead of scattering like UUIDv4.
//! * **Client-generatable** — a client can mint a message id offline, which is
//!   what makes send idempotent (see `docs/02-protocol.md`).
//! * **Compact on the wire** — 16 raw bytes, never the 36-byte text form.
//!
//! The text form is Crockford base32 (26 characters, no `I`, `L`, `O`, `U`) so
//! ids survive being read aloud, typed by hand, and pasted into a bug report.
//!
//! An `Id` is *not* a secret and *not* a capability. It leaks its creation
//! time by design. Anything that must be unguessable uses a token from
//! `migo-crypto`, never an id.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::random::Random;
use crate::time::Timestamp;

/// Crockford base32 alphabet: digits plus uppercase letters minus `I L O U`.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Length of the canonical text form.
pub const ID_TEXT_LEN: usize = 26;

/// Length of the binary form.
pub const ID_BYTE_LEN: usize = 16;

/// A 128-bit ULID-shaped identifier.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Id([u8; ID_BYTE_LEN]);

impl Id {
    /// The all-zero id. Used as an explicit "absent" marker in wire structs
    /// where an optional field would cost more bytes than it saves.
    pub const NIL: Id = Id([0u8; ID_BYTE_LEN]);

    /// Builds an id from raw bytes without interpreting them.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; ID_BYTE_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrows the raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; ID_BYTE_LEN] {
        &self.0
    }

    /// Consumes the id and returns the raw bytes.
    #[must_use]
    pub const fn into_bytes(self) -> [u8; ID_BYTE_LEN] {
        self.0
    }

    /// True when this is [`Id::NIL`].
    #[must_use]
    pub fn is_nil(&self) -> bool {
        self.0 == [0u8; ID_BYTE_LEN]
    }

    /// Mints a new id from an explicit clock reading and randomness source.
    ///
    /// Both inputs are injected rather than read from the ambient environment
    /// so that deterministic simulation tests can replay an exact id sequence
    /// (ADR-0009).
    #[must_use]
    pub fn generate(unix_ms: u64, random: &mut dyn Random) -> Self {
        let mut bytes = [0u8; ID_BYTE_LEN];
        let ms = unix_ms & 0x0000_FFFF_FFFF_FFFF;
        bytes[0] = (ms >> 40) as u8;
        bytes[1] = (ms >> 32) as u8;
        bytes[2] = (ms >> 24) as u8;
        bytes[3] = (ms >> 16) as u8;
        bytes[4] = (ms >> 8) as u8;
        bytes[5] = ms as u8;
        random.fill_bytes(&mut bytes[6..]);
        Self(bytes)
    }

    /// Mints a new id from a Migo-epoch timestamp.
    #[must_use]
    pub fn generate_at(at: Timestamp, random: &mut dyn Random) -> Self {
        Self::generate(at.as_unix_ms().max(0) as u64, random)
    }

    /// The embedded creation time, in milliseconds since the Unix epoch.
    #[must_use]
    pub fn unix_ms(&self) -> u64 {
        u64::from(self.0[0]) << 40
            | u64::from(self.0[1]) << 32
            | u64::from(self.0[2]) << 24
            | u64::from(self.0[3]) << 16
            | u64::from(self.0[4]) << 8
            | u64::from(self.0[5])
    }

    /// The embedded creation time as a Migo-epoch timestamp.
    #[must_use]
    pub fn timestamp(&self) -> Timestamp {
        Timestamp::from_unix_ms(self.unix_ms() as i64)
    }

    /// Renders the canonical 26-character text form.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = [0u8; ID_TEXT_LEN];
        let n = u128::from_be_bytes(self.0);
        for (i, slot) in out.iter_mut().enumerate() {
            // 26 characters carry 130 bits; the leading character holds the
            // top 2 bits and is therefore never above '7'.
            let shift = 125 - (i * 5);
            let index = ((n >> shift) & 0x1F) as usize;
            *slot = ALPHABET[index];
        }
        // Every byte written above comes from ALPHABET, which is ASCII.
        String::from_utf8(out.to_vec()).unwrap_or_default()
    }

    /// Parses the canonical text form, tolerating lowercase input and the
    /// human confusions Crockford base32 is designed to absorb (`I`/`l` read
    /// as `1`, `O`/`o` read as `0`).
    pub fn parse(text: &str) -> Result<Self, IdParseError> {
        let bytes = text.as_bytes();
        if bytes.len() != ID_TEXT_LEN {
            return Err(IdParseError::Length(bytes.len()));
        }
        let mut n: u128 = 0;
        for (position, raw) in bytes.iter().enumerate() {
            let value = decode_char(*raw).ok_or(IdParseError::Character { position })?;
            if position == 0 && value > 7 {
                // Would overflow 128 bits.
                return Err(IdParseError::Overflow);
            }
            n = (n << 5) | u128::from(value);
        }
        Ok(Self(n.to_be_bytes()))
    }

    /// Derives the short, human-facing alias shown in profiles and rooms.
    ///
    /// The alias is a *display* projection, not an identity: it is short enough
    /// to read over voice chat, which means collisions are possible. The
    /// database owns uniqueness via a unique index on the stored alias column;
    /// on collision the caller re-rolls the id. See `docs/04-data-model.md`.
    #[must_use]
    pub fn public_id(&self, kind: PublicId) -> String {
        let mixed = mix64(u64::from_be_bytes([
            self.0[6], self.0[7], self.0[8], self.0[9], self.0[10], self.0[11], self.0[12],
            self.0[13],
        ]));
        match kind {
            PublicId::User => format!("MGO-{:08X}", (mixed >> 32) as u32),
            PublicId::Room => format!("MGO-ROOM-{:06X}", (mixed & 0x00FF_FFFF) as u32),
            PublicId::Group => format!("MGO-GRP-{:06X}", (mixed & 0x00FF_FFFF) as u32),
            PublicId::Bot => format!("MGO-BOT-{:06X}", (mixed & 0x00FF_FFFF) as u32),
        }
    }
}

/// Which alias format [`Id::public_id`] should render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicId {
    /// `MGO-7F82A91C`
    User,
    /// `MGO-ROOM-82F91A`
    Room,
    /// `MGO-GRP-1C40BE`
    Group,
    /// `MGO-BOT-9A2D07`
    Bot,
}

/// Finalizer from SplitMix64. Spreads the random half of the id across all
/// output bits so short aliases do not share a visible prefix.
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn decode_char(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'H' => Some(c - b'A' + 10),
        b'J' | b'K' => Some(c - b'J' + 18),
        b'M' | b'N' => Some(c - b'M' + 20),
        b'P'..=b'T' => Some(c - b'P' + 22),
        b'V'..=b'Z' => Some(c - b'V' + 27),
        b'a'..=b'h' => Some(c - b'a' + 10),
        b'j' | b'k' => Some(c - b'j' + 18),
        b'm' | b'n' => Some(c - b'm' + 20),
        b'p'..=b't' => Some(c - b'p' + 22),
        b'v'..=b'z' => Some(c - b'v' + 27),
        // Crockford's documented confusions.
        b'I' | b'i' | b'L' | b'l' => Some(1),
        b'O' | b'o' => Some(0),
        _ => None,
    }
}

/// Why a text id failed to parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    /// Wrong number of characters.
    #[error("identifier must be {ID_TEXT_LEN} characters, got {0}")]
    Length(usize),
    /// A character is not in the Crockford base32 alphabet.
    #[error("invalid character at position {position}")]
    Character {
        /// Zero-based index of the offending character.
        position: usize,
    },
    /// The leading character encodes bits above 128.
    #[error("identifier overflows 128 bits")]
    Overflow,
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Same as Display: a hex dump of an id has never helped anyone read a log.
        write!(f, "Id({})", self.to_text())
    }
}

impl FromStr for Id {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl From<[u8; ID_BYTE_LEN]> for Id {
    fn from(bytes: [u8; ID_BYTE_LEN]) -> Self {
        Self(bytes)
    }
}

impl From<Id> for [u8; ID_BYTE_LEN] {
    fn from(id: Id) -> Self {
        id.0
    }
}

impl From<u128> for Id {
    fn from(value: u128) -> Self {
        Self(value.to_be_bytes())
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // JSON carries the text form; the binary protocol never uses serde.
        serializer.serialize_str(&self.to_text())
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdVisitor;

        impl Visitor<'_> for IdVisitor {
            type Value = Id;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a 26-character Crockford base32 identifier")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Id, E> {
                Id::parse(value).map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_str(IdVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random::SeededRandom;

    #[test]
    fn text_round_trip() {
        let mut rng = SeededRandom::new(7);
        for i in 0..1000u64 {
            let id = Id::generate(1_700_000_000_000 + i, &mut rng);
            let text = id.to_text();
            assert_eq!(text.len(), ID_TEXT_LEN);
            assert_eq!(Id::parse(&text).expect("parses"), id);
        }
    }

    #[test]
    fn text_form_sorts_like_bytes() {
        let mut rng = SeededRandom::new(11);
        let mut ids: Vec<Id> = (0..200u64)
            .map(|i| Id::generate(1_700_000_000_000 + i * 3, &mut rng))
            .collect();
        let mut texts: Vec<String> = ids.iter().map(Id::to_text).collect();
        ids.sort();
        texts.sort();
        let sorted_from_ids: Vec<String> = ids.iter().map(Id::to_text).collect();
        assert_eq!(texts, sorted_from_ids);
    }

    #[test]
    fn embedded_timestamp_survives_round_trip() {
        let mut rng = SeededRandom::new(3);
        let id = Id::generate(1_712_345_678_901, &mut rng);
        assert_eq!(id.unix_ms(), 1_712_345_678_901);
    }

    #[test]
    fn nil_renders_as_all_zeros() {
        assert_eq!(Id::NIL.to_text(), "0".repeat(ID_TEXT_LEN));
        assert!(Id::NIL.is_nil());
    }

    /// Pins the text form against a constant computed outside this crate.
    ///
    /// Every other test here is symmetric: `parse` of `to_text` returns what went in, and
    /// would keep doing so with a rotated alphabet or an off-by-one shift. The identifier
    /// text appears in URLs, log lines and support tickets, and the TypeScript codec
    /// renders it independently, so the mapping is a compatibility surface and needs an
    /// external anchor. This is the canonical example from the ULID specification, and
    /// `packages/wire/test/values.test.ts` pins the same three constants.
    #[test]
    fn canonical_ulid_matches_the_specification() {
        const TEXT: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        const BYTES: [u8; ID_BYTE_LEN] = [
            0x01, 0x56, 0x3e, 0x3a, 0xb5, 0xd3, 0xd6, 0x76, 0x4c, 0x61, 0xef, 0xb9, 0x93, 0x02,
            0xbd, 0x5b,
        ];

        assert_eq!(Id::from_bytes(BYTES).to_text(), TEXT);
        assert_eq!(Id::parse(TEXT).expect("parses").into_bytes(), BYTES);
        assert_eq!(
            Id::parse(TEXT).expect("parses").unix_ms(),
            1_469_922_850_259
        );

        // Both ends of the range. Twenty-six characters carry 130 bits and an id holds
        // 128, so the highest representable id starts at `7`, not `Z`.
        assert_eq!(
            Id::from_bytes([0x00; ID_BYTE_LEN]).to_text(),
            "0".repeat(ID_TEXT_LEN)
        );
        assert_eq!(
            Id::from_bytes([0xff; ID_BYTE_LEN]).to_text(),
            "7ZZZZZZZZZZZZZZZZZZZZZZZZZ"
        );

        // Every symbol in order, through the low five bits of the last byte. Pins the
        // alphabet and the shift together: either one wrong fails here.
        let alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        for (value, expected) in alphabet.chars().enumerate() {
            let mut bytes = [0u8; ID_BYTE_LEN];
            bytes[ID_BYTE_LEN - 1] = value as u8;
            let text = Id::from_bytes(bytes).to_text();
            assert_eq!(text.chars().next_back(), Some(expected), "symbol {value}");
        }
        for excluded in ['I', 'L', 'O', 'U'] {
            assert!(
                !alphabet.contains(excluded),
                "{excluded} must not be in the alphabet"
            );
        }
    }

    #[test]
    fn lenient_parsing_absorbs_human_confusions() {
        let id = Id::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("parses");
        let typed_by_hand = Id::parse("oiarz3ndektsv4rrffq69g5fav").expect("parses");
        assert_eq!(id, typed_by_hand);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(Id::parse("short"), Err(IdParseError::Length(5))));
        assert!(matches!(
            Id::parse("01ARZ3NDEKTSV4RRFFQ69G5FA!"),
            Err(IdParseError::Character { position: 25 })
        ));
        assert!(matches!(
            Id::parse(&"Z".repeat(26)),
            Err(IdParseError::Overflow)
        ));
    }

    #[test]
    fn public_ids_have_the_documented_shape() {
        let mut rng = SeededRandom::new(99);
        let id = Id::generate(1_700_000_000_000, &mut rng);
        let user = id.public_id(PublicId::User);
        let room = id.public_id(PublicId::Room);
        assert!(user.starts_with("MGO-"), "{user}");
        assert_eq!(user.len(), 12);
        assert!(room.starts_with("MGO-ROOM-"), "{room}");
        assert_eq!(room.len(), 15);
    }

    #[test]
    fn serde_uses_the_text_form() {
        let id = Id::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("parses");
        let json = serde_json::to_string(&id).expect("serializes");
        assert_eq!(json, "\"01ARZ3NDEKTSV4RRFFQ69G5FAV\"");
        let back: Id = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, id);
    }
}
