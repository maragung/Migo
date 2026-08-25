//! Crypto errors.
//!
//! Every variant here is deliberately vague about *why* something failed. That
//! is not laziness — it is the point. A decryption routine that distinguishes
//! "wrong key" from "bad padding" from "unknown ratchet step" hands an attacker
//! an oracle, and padding oracles have broken real protocols. Callers get "this
//! did not authenticate"; the details stay inside.

use thiserror::Error;

/// A cryptographic operation failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CryptoError {
    /// A key, nonce, or signature had the wrong length.
    #[error("{what} must be {expected} bytes, got {actual}")]
    BadLength {
        /// What was being parsed. A static string, never peer-supplied text.
        what: &'static str,
        /// Required length.
        expected: usize,
        /// Length supplied.
        actual: usize,
    },

    /// A public key was not a valid point, or was a low-order point.
    #[error("invalid public key")]
    InvalidPublicKey,

    /// A signature did not verify.
    #[error("signature verification failed")]
    BadSignature,

    /// AEAD authentication failed: wrong key, wrong nonce, or tampered
    /// ciphertext. Which one is not disclosed.
    #[error("decryption failed")]
    DecryptionFailed,

    /// The message referenced a ratchet step this session cannot reach.
    #[error("no session state for this message")]
    NoSession,

    /// A message arrived so far ahead of the chain that catching up would mean
    /// deriving more keys than the bound allows.
    ///
    /// Without this bound, a single message claiming counter 4 000 000 000 would
    /// make the receiver derive four billion keys. The limit is what turns a
    /// remote CPU exhaustion into a rejected message.
    #[error("message is too far ahead of the ratchet chain")]
    ChainGapTooLarge,

    /// A message key was already used, and Migo does not allow reuse.
    ///
    /// Reuse would let a replayed frame decrypt a second time and re-deliver an
    /// old message as if it were new.
    #[error("message key has already been used")]
    KeyAlreadyUsed,

    /// The header of a ratchet message did not parse.
    #[error("malformed ratchet header")]
    MalformedHeader,

    /// Password hashing or verification failed for a reason internal to the
    /// hasher, such as an unparseable stored hash.
    #[error("password hash operation failed")]
    PasswordHash,

    /// A prekey bundle failed validation, so no session was established.
    ///
    /// Almost always a signed prekey whose signature does not match the identity
    /// key — which is exactly the case where continuing would mean talking to
    /// whoever the server chose rather than to the intended person.
    #[error("prekey bundle failed validation")]
    InvalidPrekeyBundle,
}

/// Crypto result alias.
pub type Result<T, E = CryptoError> = core::result::Result<T, E>;
