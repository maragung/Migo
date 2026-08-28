//! HMAC-SHA256 for things the server authenticates to itself.
//!
//! This is a different job from the AEAD in [`crate::aead`]. That one keeps a
//! message unreadable. This one keeps a value *unforgeable* while leaving it
//! perfectly readable: a session token, a resume cursor, a signed media URL, a
//! pagination key. The server hands the value to a client, the client hands it
//! back, and the server needs to know it was not edited on the way.
//!
//! # Why not a JWT
//!
//! Migo issues opaque tokens with a MAC over a compact payload instead of JWTs,
//! for three reasons that all come from having watched JWTs go wrong:
//!
//! 1. **`alg` is attacker-controlled input.** A JWT tells the verifier which
//!    algorithm to use, which is how `alg: none` and RS256-verified-as-HS256
//!    happened. Here the algorithm is a compile-time fact.
//! 2. **A JWT is a bag of claims that everyone extends.** Once the format is
//!    self-describing, unrelated data ends up inside it and rides along on
//!    every request. A fixed payload stays the size it was designed to be —
//!    which matters when the target is a phone on a metered 3G plan.
//! 3. **Revocation was always going to be needed anyway.** The stateless-token
//!    argument dissolves the first time a user taps "log out my other devices",
//!    so Migo checks a session record it has to keep regardless. The MAC's job
//!    is only to make the lookup key untamperable.
//!
//! # Domain separation
//!
//! Every purpose gets its own label, and the label is mixed into the key rather
//! than into the message. Mixing it into the message would still be sound, but
//! deriving a per-purpose subkey means a bug that leaks one subkey does not
//! hand over the others, and it makes rotating a single purpose possible.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

use crate::error::{CryptoError, Result};
use crate::kdf;

/// Length of a full-width tag.
pub const TAG_LEN: usize = 32;

/// Minimum accepted truncated tag length, in bytes.
///
/// 16 bytes is the same 128-bit forgery margin the AEAD tag carries. Anything
/// shorter starts to be brute-forceable by an attacker who can retry, and Migo
/// has no purpose that needs to save those bytes badly enough.
pub const MIN_TAG_LEN: usize = 16;

/// Session tokens handed to clients.
pub const LABEL_SESSION_TOKEN: &[u8] = b"migo-session-token-v1";
/// Refresh tokens, which are stored as a tag rather than as themselves.
///
/// A separate label from [`LABEL_SESSION_TOKEN`] because the two keys protect
/// different things: one authenticates a value the client sends back, the other
/// turns a value into a database lookup key. Sharing a key would mean a stored
/// refresh tag is also a valid access-token signature over the same bytes, which
/// is not exploitable today only because the two formats differ — and "not
/// exploitable today" is not a property worth relying on.
pub const LABEL_REFRESH_TOKEN: &[u8] = b"migo-refresh-token-v1";
/// Resume cursors, which a client echoes back after a reconnect.
pub const LABEL_RESUME_CURSOR: &[u8] = b"migo-resume-cursor-v1";
/// Signed media URLs.
pub const LABEL_MEDIA_URL: &[u8] = b"migo-media-url-v1";
/// Pagination cursors on list endpoints.
pub const LABEL_PAGINATION: &[u8] = b"migo-pagination-v1";
/// Single-use email and phone verification codes.
pub const LABEL_VERIFICATION: &[u8] = b"migo-verification-v1";
/// Webhook bodies delivered to bot owners.
pub const LABEL_WEBHOOK: &[u8] = b"migo-webhook-v1";
/// Bot bearer tokens. A separate label from [`LABEL_REFRESH_TOKEN`] for the same
/// reason that one is separate from [`LABEL_SESSION_TOKEN`]: a stored bot-token tag
/// must not also be a valid tag under any other key, so that a database dump of one
/// kind of credential can never be replayed as another. Like a refresh token, a bot
/// token is a random value turned into a lookup key, never a signed claim.
pub const LABEL_BOT_TOKEN: &[u8] = b"migo-bot-token-v1";
/// Password-recovery tokens, the MAC tag stored alongside a recovery row. A
/// separate label from every other token kind for the same reason
/// [`LABEL_BOT_TOKEN`] is: a database dump that contains recovery tags must not
/// also be a valid tag for any other purpose, so an operator can revoke one
/// kind of credential without invalidating the others.
pub const LABEL_RECOVERY: &[u8] = b"migo-recovery-v1";

/// A key for one purpose, derived from the deployment's root secret.
///
/// Construct one per purpose at startup and keep it; the derivation is cheap but
/// it is not free, and doing it per request would show up in a flame graph long
/// before the HMAC itself did.
pub struct MacKey {
    key: [u8; 32],
}

impl Drop for MacKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl core::fmt::Debug for MacKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MacKey(***)")
    }
}

impl MacKey {
    /// Derives the key for one purpose from the deployment root secret.
    ///
    /// `root` is the raw bytes of the configured signing secret. `migod` refuses
    /// to start in production if that secret is empty or still the development
    /// default, so this function does not have to guess whether it is safe.
    #[must_use]
    pub fn derive(root: &[u8], label: &[u8]) -> Self {
        Self {
            key: kdf::derive::<32>(root, None, label),
        }
    }

    /// Wraps raw key bytes, for tests and for keys loaded from a KMS.
    #[must_use]
    pub fn from_bytes(key: [u8; 32]) -> Self {
        Self { key }
    }

    /// Full-width tag over `message`.
    #[must_use]
    pub fn tag(&self, message: &[u8]) -> [u8; TAG_LEN] {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.key)
            .expect("HMAC-SHA256 accepts a 32-byte key");
        mac.update(message);
        mac.finalize().into_bytes().into()
    }

    /// Tag over several parts, length-prefixed so the split is unambiguous.
    ///
    /// Concatenating parts directly would let `("ab", "c")` and `("a", "bc")`
    /// produce the same tag. That is not hypothetical: it is how a token for
    /// user `1` device `23` becomes a valid token for user `12` device `3`.
    #[must_use]
    pub fn tag_parts(&self, parts: &[&[u8]]) -> [u8; TAG_LEN] {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.key)
            .expect("HMAC-SHA256 accepts a 32-byte key");
        for part in parts {
            mac.update(&(part.len() as u64).to_be_bytes());
            mac.update(part);
        }
        mac.finalize().into_bytes().into()
    }

    /// Verifies a tag in constant time.
    ///
    /// Accepts a truncated tag as long as it is at least [`MIN_TAG_LEN`], so a
    /// caller who needs a shorter cursor can have one without reimplementing the
    /// comparison. A shorter tag than that is refused rather than quietly
    /// weakened.
    pub fn verify(&self, message: &[u8], tag: &[u8]) -> Result<()> {
        self.verify_expected(&self.tag(message), tag)
    }

    /// Verifies a tag produced by [`MacKey::tag_parts`].
    pub fn verify_parts(&self, parts: &[&[u8]], tag: &[u8]) -> Result<()> {
        self.verify_expected(&self.tag_parts(parts), tag)
    }

    fn verify_expected(&self, expected: &[u8; TAG_LEN], tag: &[u8]) -> Result<()> {
        if tag.len() < MIN_TAG_LEN || tag.len() > TAG_LEN {
            return Err(CryptoError::BadLength {
                what: "mac tag",
                expected: TAG_LEN,
                actual: tag.len(),
            });
        }
        // Compare only as many bytes as the caller sent, but compare all of them:
        // `ConstantTimeEq` does not exit early, which is the whole point. A
        // byte-by-byte `==` here leaks the length of the correct prefix and turns
        // forgery into 32 sequential guessing games of 256 tries each.
        if expected[..tag.len()].ct_eq(tag).into() {
            Ok(())
        } else {
            Err(CryptoError::BadSignature)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> MacKey {
        MacKey::derive(b"root secret for tests", LABEL_SESSION_TOKEN)
    }

    #[test]
    fn a_tag_verifies() {
        let k = key();
        let tag = k.tag(b"session-42");
        assert!(k.verify(b"session-42", &tag).is_ok());
    }

    #[test]
    fn known_vector_is_stable() {
        // Computed from RFC 2104 independently of this implementation, over the
        // subkey HKDF-SHA256(salt=none, ikm=b"root", info=LABEL_SESSION_TOKEN).
        // Note that "no salt" means a zero salt of hash length, not an absent
        // one; a re-derivation that gets that wrong will fail here rather than
        // silently producing a second, incompatible token format.
        let k = MacKey::derive(b"root", LABEL_SESSION_TOKEN);
        assert_eq!(
            k.tag(b"migo"),
            [
                0x14, 0x7a, 0x49, 0x74, 0x74, 0x28, 0x29, 0xd6, 0xe7, 0x56, 0x1c, 0x6f, 0x20, 0xe0,
                0xfb, 0xd6, 0x93, 0xf2, 0xf6, 0x45, 0xd1, 0x41, 0x51, 0x1f, 0xb1, 0x98, 0xfd, 0xff,
                0x88, 0x0b, 0x7c, 0xd0,
            ]
        );
    }

    #[test]
    fn an_edited_message_is_refused() {
        let k = key();
        let tag = k.tag(b"session-42");
        assert_eq!(
            k.verify(b"session-43", &tag),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn an_edited_tag_is_refused() {
        let k = key();
        let mut tag = k.tag(b"session-42");
        for bit in 0..8 {
            let mut broken = tag;
            broken[0] ^= 1 << bit;
            assert_eq!(
                k.verify(b"session-42", &broken),
                Err(CryptoError::BadSignature)
            );
        }
        tag[TAG_LEN - 1] ^= 1;
        assert_eq!(
            k.verify(b"session-42", &tag),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_different_purpose_is_a_different_key() {
        let session = MacKey::derive(b"root", LABEL_SESSION_TOKEN);
        let media = MacKey::derive(b"root", LABEL_MEDIA_URL);
        let tag = session.tag(b"same message");
        assert_eq!(
            media.verify(b"same message", &tag),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_different_root_is_a_different_key() {
        let a = MacKey::derive(b"root a", LABEL_SESSION_TOKEN);
        let b = MacKey::derive(b"root b", LABEL_SESSION_TOKEN);
        assert_eq!(b.verify(b"m", &a.tag(b"m")), Err(CryptoError::BadSignature));
    }

    #[test]
    fn parts_cannot_be_reshuffled() {
        // The attack this length prefix exists to stop.
        let k = key();
        let tag = k.tag_parts(&[b"1", b"23"]);
        assert_eq!(
            k.verify_parts(&[b"12", b"3"], &tag),
            Err(CryptoError::BadSignature)
        );
        assert!(k.verify_parts(&[b"1", b"23"], &tag).is_ok());
    }

    #[test]
    fn an_empty_part_is_still_a_part() {
        let k = key();
        let tag = k.tag_parts(&[b"a", b"", b"b"]);
        assert_eq!(
            k.verify_parts(&[b"a", b"b"], &tag),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_truncated_tag_verifies_down_to_the_floor() {
        let k = key();
        let tag = k.tag(b"cursor");
        assert!(k.verify(b"cursor", &tag[..MIN_TAG_LEN]).is_ok());
        assert!(k.verify(b"cursor", &tag[..24]).is_ok());
    }

    #[test]
    fn a_tag_shorter_than_the_floor_is_refused() {
        let k = key();
        let tag = k.tag(b"cursor");
        // Not `BadSignature`: the bytes may well be a correct prefix. The tag is
        // refused because 15 bytes is not enough security margin, and saying so
        // is what stops someone from "fixing" this by shortening it further.
        assert!(matches!(
            k.verify(b"cursor", &tag[..MIN_TAG_LEN - 1]),
            Err(CryptoError::BadLength { .. })
        ));
        assert!(matches!(
            k.verify(b"cursor", &[]),
            Err(CryptoError::BadLength { .. })
        ));
    }

    #[test]
    fn a_key_does_not_print_its_secret() {
        assert_eq!(format!("{:?}", key()), "MacKey(***)");
    }
}
