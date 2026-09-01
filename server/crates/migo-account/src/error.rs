//! Account errors.
//!
//! Same policy as `migo-crypto::error`: deliberately vague about *why* a
//! credential or container failed, because a container reader that
//! distinguishes "wrong recovery credential" from "file was tampered with"
//! hands an attacker a free oracle for how far their guess got. The brief is
//! explicit that both must fail with the same message (§182).

use thiserror::Error;

/// An account-root operation failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AccountError {
    /// A key, seed, signature, or container had the wrong length.
    #[error("{what} must be {expected} bytes, got {actual}")]
    BadLength {
        /// What was being parsed. A static string, never user-supplied text.
        what: &'static str,
        /// Required length.
        expected: usize,
        /// Length supplied.
        actual: usize,
    },

    /// A .migo container is not one: wrong magic, wrong version, or a
    /// construction this build does not read.
    #[error("not a Migo account container")]
    NotAContainer,

    /// The container's format or crypto version is newer than this build
    /// understands. Named precisely because the honest remedy is "update the
    /// app", not "try another password".
    #[error("container version {found} is not supported (this build reads {supported})")]
    UnsupportedVersion {
        /// The version the file carries.
        found: u16,
        /// The highest version this build reads.
        supported: u16,
    },

    /// The container's key-derivation function is not one this build
    /// implements. Argon2id is id 1; a future id means a container this build
    /// must refuse rather than guess at.
    #[error("unknown key derivation id {found}")]
    UnknownKdf {
        /// The KDF id the header names.
        found: u8,
    },

    /// Decryption failed: wrong recovery credential, tampered file, or both —
    /// and the caller is told neither which nor how far it got.
    #[error("container could not be opened")]
    OpenFailed,

    /// An ML-DSA signature did not verify, or a key did not decode.
    #[error("identity signature verification failed")]
    BadSignature,

    /// A BIP-32 derivation step produced an invalid scalar (zero or ≥ n).
    /// BIP-32 assigns this probability ~2^-127, so in practice it means the
    /// input was not a seed this construction should be applied to.
    #[error("invalid derivation step")]
    InvalidDerivation,

    /// The Argon2id parameters in a header are outside the range this build
    /// will spend memory on. A hostile container naming 4 GiB of Argon2 memory
    /// must be refused before the allocation, not after.
    #[error("container KDF parameters are out of range")]
    KdfOutOfRange,
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, AccountError>;
