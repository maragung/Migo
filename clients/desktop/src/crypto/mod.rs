//! End-to-end encryption for this client.
//!
//! The primitives are not here: identities, X3DH, the Double Ratchet and the AEAD all live in
//! `migo-crypto`, shared with the server's own test vectors and mirrored by `@migo/crypto` and the
//! Kotlin port. This module is the *policy* on top of them — which session a message belongs to,
//! when to run X3DH instead of reusing a ratchet, and the exact bytes of the opaque `envelope`
//! field. No cryptography is invented at this layer, deliberately: brief ADR-0003 permits audited
//! libraries only, and a client that rolled its own would break interoperability the first time it
//! disagreed with the other three implementations by a byte.
//!
//! - [`content`]: the inner plaintext, `content_type || MSE body || padding`.
//! - [`envelope`]: the outer bytes the server routes but cannot read.
//! - [`session`]: one ratchet per remote device, and the X3DH policy that starts them.

pub mod content;
pub mod envelope;
pub mod session;

/// What can go wrong between "the user pressed Enter" and "the envelope is on the wire".
///
/// Every variant is a static string or a wrapped `migo-crypto` error. None of them carry plaintext,
/// ciphertext, a tag or key material: these values are produced while processing bytes an attacker
/// chose, they end up in logs, and brief section 174 does not permit any of that in a log line. It
/// also removes the temptation to distinguish "wrong tag" from "wrong key", which is a padding
/// oracle by another name.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// The envelope did not parse, or would not serialise.
    #[error("envelope: {0}")]
    Envelope(&'static str),

    /// The inner plaintext did not parse, or would not serialise.
    #[error("content: {0}")]
    Content(#[from] migo_wire::WireError),

    /// A primitive refused: a bad signature, a small-order key, a failed open.
    #[error("crypto: {0}")]
    Primitive(#[from] migo_crypto::CryptoError),

    /// A first message named a signed prekey this device no longer holds the private half of, so
    /// the session cannot be answered. Rotating a prekey away is a normal thing to have done; the
    /// peer will start a new session on its next send.
    #[error("no prekey pair for the id the sender named")]
    UnknownPrekey,

    /// A message arrived for a device we have no session with, and it carried no X3DH preamble to
    /// start one from.
    #[error("no session with that device, and no preamble to start one")]
    NoSession,

    /// A send needs the peer's prekey bundle and the caller did not supply one.
    #[error("no prekey bundle for that device")]
    NoBundle,
}
