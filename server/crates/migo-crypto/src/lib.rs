//! Cryptographic primitives for Migo.
//!
//! # What the server never holds
//!
//! This crate is the reason the server can be honest about privacy. Three
//! things never exist on any Migo server, in any environment, for any role
//! including administrators:
//!
//! - **A private key.** Identity keys, prekey secrets, and ratchet state are
//!   generated on the device and stay there. The server stores public halves
//!   and signatures, which are useless for reading anything.
//! - **Plaintext of a private message.** The server routes `envelope` bytes it
//!   cannot open. A database dump is a pile of ciphertext.
//! - **An escrow or recovery copy.** There is no "break glass" key, because a
//!   key that can be produced under pressure is a key that will be.
//!
//! Public and Managed rooms are a deliberate exception: they are protected in
//! transit and at rest but readable by the server, because moderation of a
//! public space requires reading it. The product rule that follows is that the
//! UI must say exactly which of the two a conversation is — never imply
//! end-to-end protection that is not there.
//!
//! # No custom primitives
//!
//! Per ADR-0003 this crate composes audited implementations and writes none of
//! its own: [`ed25519_dalek`] for signatures, [`x25519_dalek`] for Diffie-
//! Hellman, [`chacha20poly1305`] for AEAD, [`hkdf`] over [`sha2`] for key
//! derivation, and [`argon2`] for passphrase hashing. What *is* written here is
//! the part that is Migo's to get right: which secret feeds which KDF, under
//! which label, in which order, and what happens when a peer lies.
//!
//! One design departure worth naming: identity is **two keys**, Ed25519 for
//! signing and X25519 for exchange, published as 64 bytes of `signing ||
//! exchange`. Signal derives both from one key with XEdDSA's birational map.
//! That is sound and saves 32 bytes, and it would also mean implementing a
//! birational map in Rust, TypeScript, and Kotlin and being right three times.
//! Boring is the right default in cryptographic code.
//!
//! # Module map
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`error`] | One error type, deliberately vague, so failures are not oracles |
//! | [`kdf`] | HKDF-SHA256 with a distinct label per purpose |
//! | [`aead`] | XChaCha20-Poly1305 sealing and opening |
//! | [`identity`] | Long-term identity, ephemeral key pairs, signed prekeys |
//! | [`x3dh`] | Asynchronous session establishment against a published bundle |
//! | [`ratchet`] | Double Ratchet for 1:1 conversations |
//! | [`sender_key`] | Sender-key ratchet for groups: encrypt once, fan out |
//! | [`passphrase`] | Argon2id hashing and verification |
//! | [`mac`] | HMAC-SHA256 for session tokens, cursors, and signed URLs |
//! | [`node`] | Server node identity and the mesh handshake |
//!
//! # Rules this crate follows
//!
//! 1. **Verify before you derive.** A signature or MAC is checked before any
//!    key material is computed from the data it covers. A forged frame must
//!    cost the receiver a signature check, not a session.
//! 2. **Mutate state only after success.** Ratchet advance, prekey
//!    consumption, and skipped-key storage all happen after decryption
//!    succeeds, so an injected frame cannot destroy a working session.
//! 3. **Bound everything a peer controls.** Chain gaps, retained skipped keys,
//!    messages per chain, and passphrase length all have hard ceilings. Each
//!    bound in this crate is documented as the attack it prevents, not as a
//!    tuning knob.
//! 4. **Errors say little.** No error distinguishes "wrong key" from "damaged
//!    ciphertext", and no error quotes the bytes that failed.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod aead;
pub mod error;
pub mod identity;
pub mod kdf;
pub mod mac;
pub mod node;
pub mod passphrase;
pub mod ratchet;
pub mod sender_key;
pub mod x3dh;

pub use crate::aead::{open, seal, SymmetricKey, KEY_LEN, NONCE_LEN, TAG_LEN};
pub use crate::error::{CryptoError, Result};
pub use crate::identity::{
    IdentityPublic, IdentitySecret, KeyPair, SignedPrekey, IDENTITY_PUBLIC_LEN, PUBLIC_KEY_LEN,
    SIGNATURE_LEN,
};
pub use crate::mac::{MacKey, LABEL_RECOVERY};
pub use crate::node::{NodeHello, NodeProof, NodePublic, NodeSecret};
pub use crate::passphrase::Verification;
pub use crate::ratchet::{RatchetHeader, RatchetSession};
pub use crate::sender_key::{
    ReceiverKeyState, SenderKeyDistribution, SenderKeyHeader, SenderKeyMessage, SenderKeyState,
};
pub use crate::x3dh::{InitialMessage, PrekeyBundle, SessionSeed};
