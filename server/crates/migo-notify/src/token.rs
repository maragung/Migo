//! Sealing and hashing a push token.
//!
//! # Why a push token is both sealed and hashed
//!
//! Brief section 77: *"Push token disimpan dalam bentuk hash dan TIDAK BOLEH ditulis ke
//! log."* Section 174 lists a raw push token among the things that must never reach a
//! log, without exception.
//!
//! Taken literally, "stored as a hash" would end the discussion and also end push
//! notifications: a one-way hash of a push token is a token nobody can ever send to.
//! So the credential is split in two, and each half does one of the two jobs the rule
//! is actually asking for.
//!
//! * The **hash** is the identity. Every lookup uses it, every deduplication uses it,
//!   and every log line and metric that has to refer to a registration refers to this.
//!   That is what makes "never log the token" a rule an engineer can follow while still
//!   being able to debug delivery.
//! * The **sealed** form is the credential. XChaCha20-Poly1305 under a key derived from
//!   the deployment secret, which lives in the process and not in the database. A dump
//!   of the `device` table is therefore not a set of push credentials — which is the
//!   property section 77 is protecting, and the reason it says "hash" in the first
//!   place.
//!
//! # Why the device id is associated data
//!
//! Because a sealed token is otherwise portable. Move the ciphertext to another
//! `device` row and it still opens, and now an attacker with write access to one column
//! can redirect somebody else's wake-ups to a handset they control. With the device id
//! bound in, the same move produces a decryption failure, the registration is retired,
//! and the phone re-registers on its next foreground.
//!
//! # No custom cryptography
//!
//! ADR-0003, and brief section 3's *"Jangan membuat custom crypto"*. Everything here is
//! `migo-crypto` calling audited primitives: HKDF for the two keys, HMAC-SHA256 for the
//! hash, XChaCha20-Poly1305 for the seal. This module contributes labels and an order of
//! operations, and nothing else.

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use migo_core::{Id, Random, Result};
use migo_crypto::{kdf, MacKey, SymmetricKey};
use migo_protocol::fault;
use migo_store::model::PushRegistration;

use crate::model::{RawToken, MAX_TOKEN_LEN};

/// HKDF label for the sealing key.
const SEAL_LABEL: &[u8] = b"migo/push-token/seal/v1";
/// HKDF label for the lookup key.
const HASH_LABEL: &[u8] = b"migo/push-token/hash/v1";

/// The two keys a push token needs, derived from the deployment secret.
///
/// Not `Clone` and not `Debug`: a copy of this is a copy of two keys, and a `Debug`
/// impl is a key in a log line waiting for somebody to add `?keeper` to a span.
pub struct TokenKeeper {
    seal: SymmetricKey,
    mac: MacKey,
}

impl TokenKeeper {
    /// Derives both keys from the deployment root secret.
    ///
    /// Two separate labels, so the sealing key and the lookup key are independent. One
    /// key used for both would mean a hash that leaks something about the ciphertext's
    /// key, which is the kind of "it is probably fine" that ADR-0003 exists to refuse.
    ///
    /// `migod` refuses to start in production when the root secret is empty or still the
    /// development default, so this does not have to guess whether it was given
    /// anything worth deriving from.
    #[must_use]
    pub fn derive(root: &[u8]) -> Self {
        Self {
            seal: SymmetricKey::from_bytes(kdf::derive::<32>(root, None, SEAL_LABEL)),
            mac: MacKey::derive(root, HASH_LABEL),
        }
    }

    /// Wraps keys directly, for tests and for a key loaded from a KMS.
    #[must_use]
    pub fn from_keys(seal: SymmetricKey, mac: MacKey) -> Self {
        Self { seal, mac }
    }

    /// The lookup handle for a raw token.
    ///
    /// Lower-case hex over the full 32-byte tag. Hex rather than base64 because this
    /// value ends up in log lines, in a `text` column, and occasionally in a support
    /// ticket, and hex survives all three without an encoding argument.
    #[must_use]
    pub fn hash(&self, token: &str) -> String {
        let tag = self.mac.tag(token.as_bytes());
        let mut out = String::with_capacity(tag.len() * 2);
        for byte in tag {
            out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
        }
        out
    }

    /// Seals a token for one device.
    ///
    /// Validates before sealing, because a token that is empty or absurdly long is a
    /// client bug and there is nothing to be gained from encrypting it first. The
    /// refusal names the field and not the value.
    ///
    /// # Errors
    ///
    /// `VALIDATION_FAILED` naming `push_token` when the token is blank or longer than
    /// [`MAX_TOKEN_LEN`], and `INTERNAL` if sealing itself fails. Neither error carries
    /// the token, and the second one does not say why — see [`TokenKeeper::open`].
    pub fn seal(
        &self,
        device_id: Id,
        token: &RawToken,
        random: &mut dyn Random,
    ) -> Result<PushRegistration> {
        if token.is_empty() {
            return Err(fault::validation("push_token", "must not be empty"));
        }
        if token.len() > MAX_TOKEN_LEN {
            return Err(fault::validation("push_token", "is too long"));
        }
        let raw = token.expose();
        let sealed = migo_crypto::seal(&self.seal, device_id.as_bytes(), raw.as_bytes(), random)
            .map_err(|_| fault::internal("push token could not be sealed"))?;
        Ok(PushRegistration {
            sealed: STANDARD_NO_PAD.encode(sealed),
            hash: self.hash(raw),
            provider: token.provider(),
        })
    }

    /// Opens a sealed token for one device.
    ///
    /// Every failure returns the same error, and none of them says which step failed.
    /// The caller's only sensible response to any of them is identical — retire the
    /// registration and let the device re-register — and an error that distinguished
    /// "not valid base64" from "authentication failed" would be an oracle for anybody
    /// who can write to that column.
    ///
    /// # Errors
    ///
    /// One error, `INTERNAL`, for every way this can fail: malformed base64, a wrong or
    /// rotated key, a ciphertext moved from another device's row, and a hash that does
    /// not match the ciphertext beside it. The caller's response to all four is the
    /// same — retire the registration and let the device register again.
    pub fn open(&self, device_id: Id, registration: &PushRegistration) -> Result<String> {
        let sealed = STANDARD_NO_PAD
            .decode(registration.sealed.as_bytes())
            .map_err(|_| unsealable())?;
        let plain = migo_crypto::open(&self.seal, device_id.as_bytes(), &sealed)
            .map_err(|_| unsealable())?;
        let token = String::from_utf8(plain).map_err(|_| unsealable())?;
        // The hash is checked against the ciphertext's contents rather than trusted from
        // the column. They can only disagree if somebody edited one of the two, and a
        // registration whose halves disagree is one whose lookups and sends are about
        // to point at different phones.
        if self.hash(&token) != registration.hash {
            return Err(unsealable());
        }
        Ok(token)
    }
}

/// The single error every failure to open a token returns.
fn unsealable() -> migo_core::Error {
    fault::internal("push registration could not be opened")
}

impl core::fmt::Debug for TokenKeeper {
    /// Prints nothing about either key.
    ///
    /// Not derived, and it could not be. A derived `Debug` on a type whose two fields
    /// are a sealing key and a MAC key puts both of them one `tracing::debug!` away from
    /// a log file — and unlike a push token, which at least expires, a leaked deployment
    /// key opens every registration ever written.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("TokenKeeper(<sealed>)")
    }
}
