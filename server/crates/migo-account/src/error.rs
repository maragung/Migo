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

    /// An RPC-observed chain id does not match the configured network. The
    /// transaction was never built: a chain-id mismatch is the
    /// replay/confusion case, and the honest response is to close the
    /// session, not to pick one of the two ids.
    #[error("chain id mismatch: configured {configured}, RPC reported {observed}")]
    ChainMismatch {
        /// The chain id the client is configured for.
        configured: u64,
        /// The chain id the RPC reported.
        observed: u64,
    },

    /// Bytes handed to the transaction parser are not an EIP-1559 envelope
    /// at all — wrong type byte, wrong field count, or structurally not the
    /// list shape a type-2 transaction is.
    #[error("not an EIP-1559 transaction")]
    NotATransaction,

    /// A raw transaction or RLP item is structurally broken or non-canonical.
    /// The parser is deliberately strict — trailing bytes, non-minimal
    /// integers, and redundant length prefixes are all refused, because it
    /// parses bytes that arrived over a network.
    #[error("malformed RLP: {what}")]
    MalformedRlp {
        /// What was wrong. A static string, never input-derived text.
        what: &'static str,
    },

    /// A recipient string is not an address: wrong length or not hex.
    #[error("not a valid address")]
    BadAddress,

    /// A mixed-case address string's EIP-55 checksum does not match its
    /// contents. Reported distinctly from [`AccountError::BadAddress`]
    /// because the user's remedy is "fix the typo", not "the app is broken".
    #[error("address checksum failed")]
    AddressChecksumFailed,
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, AccountError>;
