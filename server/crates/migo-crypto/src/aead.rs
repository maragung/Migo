//! Authenticated encryption.
//!
//! XChaCha20-Poly1305, with a 24-byte nonce. The extended nonce is the reason for
//! the choice: at 24 bytes a randomly generated nonce has no meaningful collision
//! risk, so nonces can be random per message and no component has to maintain a
//! counter that must never repeat. AES-GCM's 12-byte nonce would force exactly
//! that bookkeeping, and nonce reuse under GCM is catastrophic — it leaks the
//! authentication key, not just one message.
//!
//! ChaCha20 is also the right shape for the target device: it is a software
//! stream cipher, fast and constant-time without hardware AES, which many of the
//! cheap Android phones Migo targets do not have.
//!
//! Associated data is always supplied and always authenticated. For a ratchet
//! message it is the header; for a stored blob it is the record identity. This is
//! what stops a valid ciphertext from being replayed into a different context.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use migo_core::Random;
use zeroize::Zeroize;

use crate::error::{CryptoError, Result};

/// Symmetric key length in bytes.
pub const KEY_LEN: usize = 32;
/// Nonce length in bytes.
pub const NONCE_LEN: usize = 24;
/// Poly1305 authentication tag length in bytes.
pub const TAG_LEN: usize = 16;

/// A 256-bit symmetric key that clears itself when dropped.
///
/// There is no `Display` and no plain `Debug` output: a key that can be printed
/// eventually is printed, into a log that is retained for ninety days.
#[derive(Clone, PartialEq, Eq, Zeroize)]
#[zeroize(drop)]
pub struct SymmetricKey([u8; KEY_LEN]);

impl SymmetricKey {
    /// Wraps existing key bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Generates a fresh key.
    ///
    /// Takes the source explicitly so a caller cannot accidentally reach for a
    /// seeded generator: `SeededRandom` exists for deterministic simulation and
    /// must never produce a real key.
    pub fn generate(random: &mut dyn Random) -> Self {
        let mut bytes = [0u8; KEY_LEN];
        random.fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// Parses a key from a slice.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let array: [u8; KEY_LEN] = bytes.try_into().map_err(|_| CryptoError::BadLength {
            what: "symmetric key",
            expected: KEY_LEN,
            actual: bytes.len(),
        })?;
        Ok(Self(array))
    }

    /// Borrows the raw bytes. The greppable audit point for key material leaving
    /// this type.
    #[must_use]
    pub fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl core::fmt::Debug for SymmetricKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SymmetricKey(***)")
    }
}

/// Encrypts `plaintext`, returning `nonce || ciphertext || tag`.
///
/// The nonce is prefixed rather than tracked separately because every caller
/// needs it and a caller that has to remember to store it separately eventually
/// will not.
pub fn seal(
    key: &SymmetricKey,
    associated_data: &[u8],
    plaintext: &[u8],
    random: &mut dyn Random,
) -> Result<Vec<u8>> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    random.fill_bytes(&mut nonce_bytes);
    seal_with_nonce(key, &nonce_bytes, associated_data, plaintext)
}

/// Encrypts with a caller-supplied nonce.
///
/// Exists for test vectors and for the ratchet, which derives its nonce from the
/// message key so that both sides agree without transmitting it. Application code
/// should call [`seal`] and let the nonce be random.
pub fn seal_with_nonce(
    key: &SymmetricKey,
    nonce: &[u8; NONCE_LEN],
    associated_data: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.expose().into());
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad: associated_data,
            },
        )
        .map_err(|_| CryptoError::DecryptionFailed)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypts `nonce || ciphertext || tag`.
pub fn open(key: &SymmetricKey, associated_data: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return Err(CryptoError::BadLength {
            what: "sealed message",
            expected: NONCE_LEN + TAG_LEN,
            actual: sealed.len(),
        });
    }
    let (nonce, body) = sealed.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(key.expose().into());
    cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: body,
                aad: associated_data,
            },
        )
        .map_err(|_| CryptoError::DecryptionFailed)
}

/// Decrypts with an explicit nonce, for ciphertext stored without its nonce.
pub fn open_with_nonce(
    key: &SymmetricKey,
    nonce: &[u8; NONCE_LEN],
    associated_data: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.expose().into());
    cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: associated_data,
            },
        )
        .map_err(|_| CryptoError::DecryptionFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::SeededRandom;

    fn key() -> SymmetricKey {
        SymmetricKey::from_bytes([7u8; KEY_LEN])
    }

    #[test]
    fn round_trips() {
        let mut random = SeededRandom::new(1234);
        let sealed = seal(&key(), b"context", b"pesan rahasia", &mut random).expect("seals");
        let opened = open(&key(), b"context", &sealed).expect("opens");
        assert_eq!(opened, b"pesan rahasia");
    }

    #[test]
    fn ciphertext_expands_by_exactly_the_nonce_and_tag() {
        let mut random = SeededRandom::new(1);
        let sealed = seal(&key(), b"", b"0123456789", &mut random).expect("seals");
        assert_eq!(sealed.len(), 10 + NONCE_LEN + TAG_LEN);
    }

    #[test]
    fn a_wrong_key_fails_to_open() {
        let mut random = SeededRandom::new(2);
        let sealed = seal(&key(), b"ad", b"plaintext", &mut random).expect("seals");
        let other = SymmetricKey::from_bytes([8u8; KEY_LEN]);
        assert_eq!(
            open(&other, b"ad", &sealed),
            Err(CryptoError::DecryptionFailed)
        );
    }

    #[test]
    fn wrong_associated_data_fails_to_open() {
        // This is the property that stops a ciphertext being replayed into a
        // different conversation or a different record.
        let mut random = SeededRandom::new(3);
        let sealed = seal(&key(), b"conversation-a", b"plaintext", &mut random).expect("seals");
        assert_eq!(
            open(&key(), b"conversation-b", &sealed),
            Err(CryptoError::DecryptionFailed)
        );
    }

    #[test]
    fn any_single_bit_flip_is_detected() {
        let mut random = SeededRandom::new(4);
        let sealed = seal(&key(), b"ad", b"plaintext here", &mut random).expect("seals");
        for index in 0..sealed.len() {
            for bit in 0..8u32 {
                let mut tampered = sealed.clone();
                tampered[index] ^= 1 << bit;
                assert!(
                    open(&key(), b"ad", &tampered).is_err(),
                    "a flip at byte {index} bit {bit} went undetected"
                );
            }
        }
    }

    #[test]
    fn a_truncated_message_is_rejected_by_length_not_by_panic() {
        let mut random = SeededRandom::new(5);
        let sealed = seal(&key(), b"", b"x", &mut random).expect("seals");
        for cut in 0..sealed.len() {
            assert!(open(&key(), b"", &sealed[..cut]).is_err());
        }
    }

    #[test]
    fn nonces_differ_between_messages() {
        let mut random = SeededRandom::new(6);
        let first = seal(&key(), b"", b"same plaintext", &mut random).expect("seals");
        let second = seal(&key(), b"", b"same plaintext", &mut random).expect("seals");
        assert_ne!(first[..NONCE_LEN], second[..NONCE_LEN]);
        assert_ne!(
            first, second,
            "identical plaintexts must not produce identical ciphertexts"
        );
    }

    #[test]
    fn an_explicit_nonce_round_trips() {
        let nonce = [3u8; NONCE_LEN];
        let sealed = seal_with_nonce(&key(), &nonce, b"ad", b"body").expect("seals");
        assert_eq!(&sealed[..NONCE_LEN], &nonce);
        let body = &sealed[NONCE_LEN..];
        assert_eq!(
            open_with_nonce(&key(), &nonce, b"ad", body).expect("opens"),
            b"body"
        );
    }

    #[test]
    fn a_key_does_not_print_itself() {
        assert_eq!(format!("{:?}", key()), "SymmetricKey(***)");
    }

    #[test]
    fn a_short_key_is_rejected() {
        assert_eq!(
            SymmetricKey::parse(&[0u8; 16]),
            Err(CryptoError::BadLength {
                what: "symmetric key",
                expected: 32,
                actual: 16
            })
        );
    }
}
