//! The Migo account: one root secret, five isolated domains.
//!
//! # One account, one root, one backup
//!
//! A Migo user has one account, and behind it one 32-byte root secret from
//! which every credential the account needs is derived. The user never sees
//! the root and never manages the keys it produces; the complexity belongs
//! inside the security architecture, not in the user's hands.
//!
//! What the root is *not* is a master key. It never signs, never decrypts, and
//! never leaves the device it was generated on. It is only ever input to
//! HKDF-SHA-256 under one of five immutable domain labels, and each domain
//! consumes its own seed through the standard construction of that domain —
//! never through a hash that pretends to be a key:
//!
//! | Domain | Seed becomes | Consumed by |
//! |---|---|---|
//! | `MIGO/IDENTITY/V1` | ML-DSA-65 seed | FIPS 204 key generation (Algorithm 6) |
//! | `MIGO/EVM/V1` | BIP-32 master seed | BIP-32 / BIP-44 derivation, secp256k1 |
//! | `MIGO/E2EE/V1` | the founding device's E2EE seeds | the existing X3DH / Double Ratchet stack |
//! | `MIGO/BACKUP/V1` | the .migo container's key schedule | Argon2id, then XChaCha20-Poly1305 |
//! | `MIGO/DEVICE/V1` | nothing — device credentials are random per device | see [`identity::DeviceCredential`] |
//!
//! The separation is the point (ADR-0013). One raw key fed to two algorithms is
//! how a key that protects one thing ends up protecting another, and ML-DSA has
//! no BIP-32, so "derive subkeys from the identity key" is not an option the
//! architecture can take even if it wanted to.
//!
//! # What the server never holds
//!
//! This crate extends the promise of `migo-crypto` from messages to the account
//! itself. The server verifies ML-DSA signatures against public keys and stores
//! addresses; the root secret, every domain seed, the EVM private keys, and the
//! recovery credential that unlocks a `.migo` container exist only on devices.
//! A database breach yields public keys and ciphertext, and a leaked backup
//! without its recovery credential yields Argon2id work, not an account.
//!
//! # Ports
//!
//! This crate is the reference implementation. It is consumed by the server
//! (verification only) and by the desktop client (in full), and ported to
//! TypeScript (`packages/crypto`) and Kotlin (`clients/android/core`). The
//! three ports must agree byte for byte, which is what the conformance vectors
//! in `shared/protocol/vectors/crypto/` pin: the domain derivations against an
//! independent Python implementation, and the ML-DSA and container formats
//! against this crate as the reference (a provenance each vector file records
//! honestly, because a lattice signature cannot be re-derived from a RFC by a
//! script the way HKDF can).
//!
//! # No custom primitives
//!
//! Like `migo-crypto`, this crate composes audited implementations and writes
//! none of its own: [`ml_dsa`] for FIPS 204, [`secp256k1`] for the curve,
//! [`tiny_keccak`] for Keccak-256, Argon2id from the `argon2` crate, and the
//! HKDF, XChaCha20-Poly1305, and zeroization habits of `migo-crypto` reused
//! rather than forked. What is written here is the part that is Migo's to get
//! right: which secret feeds which domain, under which label, and what a
//! container header promises before any of it is trusted.

pub mod container;
pub mod error;
pub mod evm;
pub mod identity;
pub mod root;

pub use container::{open_container, seal_container, AccountFile, ContainerParams, HEADER_LEN};
pub use error::{AccountError, Result};
pub use evm::{EvmWallet, EIP155_COIN_TYPE, EVM_BIP44_PATH};
pub use identity::{
    DeviceCredential, IdentityKey, IDENTITY_ALGORITHM, KEY_VERSION_ONE, PUBLIC_KEY_LEN,
    SIGNATURE_LEN,
};
pub use root::{MigoRoot, DOMAIN_BACKUP, DOMAIN_DEVICE, DOMAIN_E2EE, DOMAIN_EVM, DOMAIN_IDENTITY};
