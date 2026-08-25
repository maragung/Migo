//! The bot token: thirty-two random bytes, handed over once, stored only as a keyed tag.
//!
//! # Why not a signed, self-describing token
//!
//! A session token ([`migo_auth`]) is signed and carries claims, because the gateway must
//! learn who is speaking without a database round-trip on every frame. A bot token has no
//! such pressure: a bot authenticates once when it connects, and one store read at connect
//! time is nothing. So a bot token carries no structure at all — it is a lookup key and
//! nothing else. Structure in a lookup key is structure an attacker can study; there is none
//! here to study.
//!
//! # What is stored is not the token
//!
//! The token is thirty-two bytes of randomness, shown to the owner once in base64url. What
//! the `bot` row keeps is a keyed HMAC-SHA-256 tag of those bytes ([`Minter::tag_of`]), not
//! the token. A database dump therefore yields no working credential, and because the tag is
//! *keyed* — derived from the deployment secret under [`LABEL_BOT_TOKEN`] — an attacker with
//! the dump but not the key cannot confirm a guess offline the way a bare SHA-256 would let
//! them. This is exactly `migo-auth`'s refresh-token construction, for the same reasons.
//!
//! # The token never reaches a log
//!
//! Brief sections 77 and 145: a bot token, raw or hashed, must never be written to a log.
//! [`Minter::mint`] returns the raw token as a [`Secret`], whose `Debug` and serialization
//! both redact, and the stored form is a tag this module never formats. There is no code
//! path in this crate that prints either.

use base64::Engine as _;
use migo_core::{Random, Secret};
use migo_crypto::{mac::LABEL_BOT_TOKEN, MacKey};

/// How many random bytes a token is before encoding.
///
/// Thirty-two — the same width as a refresh token, and for the same reason: it carries no
/// claims, so there is nothing to encode and no reason for structure, and 256 bits of
/// randomness is past any brute-force worth attempting.
pub const BOT_TOKEN_BYTES: usize = 32;

/// Mints and verifies bot tokens against one deployment key.
///
/// Holds the [`MacKey`] derived from the deployment secret under [`LABEL_BOT_TOKEN`]. The
/// label is what keeps a bot token's tag from colliding with a session token's or a webhook
/// signature's even though all three descend from the same root secret.
#[derive(Debug)]
pub struct Minter {
    key: MacKey,
}

impl Minter {
    /// Derives a minter from the deployment's root key material.
    ///
    /// `root` is the same secret the other MAC keys descend from; the label separates this
    /// use from every other, so learning a bot tag tells an attacker nothing about a session
    /// tag drawn from the same root.
    #[must_use]
    pub fn new(root: &[u8]) -> Self {
        Self {
            key: MacKey::derive(root, LABEL_BOT_TOKEN),
        }
    }

    /// Generates a token and the tag to store for it.
    ///
    /// Returns `(token, stored_tag)`. Only the tag is persisted, so a database dump yields no
    /// working credential; the token is handed to the owner once and cannot be recovered
    /// afterwards, only rotated.
    pub fn mint(&self, random: &mut dyn Random) -> (Secret, [u8; 32]) {
        let mut bytes = [0u8; BOT_TOKEN_BYTES];
        random.fill_bytes(&mut bytes);
        let tag = self.key.tag(&bytes);
        (Secret::new(encode(&bytes)), tag)
    }

    /// Recomputes the stored tag for a token a client presented.
    ///
    /// Any input is accepted and produces a tag; a garbage token simply fails to match a row.
    /// Refusing early on a length check would let a caller distinguish "wrong shape" from
    /// "wrong value", and that distinction is worth nothing to a legitimate bot — its token
    /// is either the one it was given or it is not.
    #[must_use]
    pub fn tag_of(&self, token: &str) -> [u8; 32] {
        match decode(token) {
            Some(bytes) => self.key.tag(&bytes),
            // Tag the text as given, domain-separated so it cannot collide with a real tag —
            // a real one is computed over exactly `BOT_TOKEN_BYTES` of decoded input.
            None => self.key.tag_parts(&[b"raw:", token.as_bytes()]),
        }
    }
}

/// Base64url without padding — the token ends up in headers and JSON, where `=` is escaped
/// three different ways, so it carries none.
fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes a token to exactly [`BOT_TOKEN_BYTES`], or `None` if it is not one.
fn decode(token: &str) -> Option<[u8; BOT_TOKEN_BYTES]> {
    let mut out = [0u8; BOT_TOKEN_BYTES];
    let written = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode_slice(token.as_bytes(), &mut out)
        .ok()?;
    (written == BOT_TOKEN_BYTES).then_some(out)
}
