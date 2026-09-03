//! Passphrase hashing.
//!
//! Argon2id, the memory-hard winner of the Passphrase Hashing Competition and the
//! algorithm [OWASP recommends] first. The "id" variant runs a data-independent
//! pass followed by a data-dependent one, which resists side-channel attacks and
//! GPU cracking at the same time — the two threats that matter for a credential
//! database that might one day be stolen.
//!
//! What Migo does *not* do: SHA-256 of a passphrase, with or without a salt, with or
//! without a few thousand iterations of a loop somebody wrote. A GPU computes
//! billions of SHA-256 hashes per second. Argon2id at these parameters costs 19
//! MiB of memory per attempt, which is what makes parallel cracking expensive
//! rather than merely tedious.
//!
//! # Parameters
//!
//! 19 MiB, 2 passes, 1 lane — the OWASP baseline, and a deliberate choice for the
//! deployment target. Higher memory is better against an attacker and worse
//! against a login spike: a login is one hash, but ten thousand simultaneous
//! logins at 64 MiB is 640 MiB of transient allocation on a node that also has to
//! serve messages. 19 MiB with a real rate limit in front is the better trade
//! than 64 MiB without one.
//!
//! Parameters live in the encoded hash string, so they can be raised later and
//! existing hashes keep verifying. A verify that succeeds against outdated
//! parameters reports [`Verification::NeedsRehash`], and the caller re-hashes
//! with the current cost while it still has the plaintext passphrase in hand.
//!
//! [OWASP recommends]: https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use migo_core::{Random, Secret};

use crate::error::{CryptoError, Result};

/// Memory cost in kibibytes.
pub const MEMORY_KIB: u32 = 19 * 1024;
/// Number of passes over memory.
pub const TIME_COST: u32 = 2;
/// Degree of parallelism.
pub const LANES: u32 = 1;
/// Output length in bytes.
pub const OUTPUT_LEN: usize = 32;

/// Longest passphrase accepted.
///
/// Not a security limit — long passphrases are good. It is a denial-of-service
/// limit: Argon2 hashes its input, so a megabyte-long "passphrase" costs the hash of
/// a megabyte on every attempt, and an attacker will happily send one.
pub const MAX_PASSPHRASE_BYTES: usize = 1024;

/// Shortest passphrase accepted.
///
/// Length is the only requirement. Composition rules — one uppercase, one digit,
/// one symbol — measurably push people toward `Passphrase1!` and are not enforced
/// here; the client shows a strength meter and checks against a breached-passphrase
/// list instead.
pub const MIN_PASSPHRASE_BYTES: usize = 8;

/// Outcome of a successful verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The passphrase matched and the stored hash uses current parameters.
    Ok,
    /// The passphrase matched, but the hash was made with weaker parameters and
    /// should be replaced now, while the plaintext is available.
    NeedsRehash,
}

/// The configured hasher.
fn hasher() -> Result<Argon2<'static>> {
    let params = Params::new(MEMORY_KIB, TIME_COST, LANES, Some(OUTPUT_LEN))
        .map_err(|_| CryptoError::PassphraseHash)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Hashes a passphrase, returning a PHC string that embeds the salt and parameters.
///
/// The salt is per-passphrase and random. A shared or absent salt would let one
/// precomputed table crack every account at once, and would make identical
/// passphrases visible as identical hashes.
pub fn hash(passphrase: &str, random: &mut dyn Random) -> Result<Secret> {
    check_length(passphrase)?;
    let mut salt_bytes = [0u8; 16];
    random.fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| CryptoError::PassphraseHash)?;
    let encoded = hasher()?
        .hash_password(passphrase.as_bytes(), &salt)
        .map_err(|_| CryptoError::PassphraseHash)?
        .to_string();
    Ok(Secret::new(encoded))
}

/// Verifies a passphrase against a stored hash.
///
/// Returns `Ok(None)` when the passphrase does not match — a wrong passphrase is a
/// normal outcome, not an error, and conflating the two leads to callers that
/// treat a malformed stored hash as a failed login and lock nobody out.
pub fn verify(passphrase: &str, stored: &Secret) -> Result<Option<Verification>> {
    if passphrase.len() > MAX_PASSPHRASE_BYTES {
        // Refuse before hashing: this is the cheap path an attacker would abuse.
        return Ok(None);
    }
    let parsed = PasswordHash::new(stored.expose()).map_err(|_| CryptoError::PassphraseHash)?;
    match hasher()?.verify_password(passphrase.as_bytes(), &parsed) {
        Ok(()) => Ok(Some(if is_current(&parsed) {
            Verification::Ok
        } else {
            Verification::NeedsRehash
        })),
        Err(_) => Ok(None),
    }
}

/// True when a stored hash was produced with at least the current cost.
fn is_current(parsed: &PasswordHash<'_>) -> bool {
    let Ok(params) = Params::try_from(parsed) else {
        return false;
    };
    parsed.algorithm.as_str() == "argon2id"
        && params.m_cost() >= MEMORY_KIB
        && params.t_cost() >= TIME_COST
}

/// Rejects passphrases that are too short or long enough to be an attack.
fn check_length(passphrase: &str) -> Result<()> {
    if passphrase.len() < MIN_PASSPHRASE_BYTES {
        return Err(CryptoError::BadLength {
            what: "passphrase",
            expected: MIN_PASSPHRASE_BYTES,
            actual: passphrase.len(),
        });
    }
    if passphrase.len() > MAX_PASSPHRASE_BYTES {
        return Err(CryptoError::BadLength {
            what: "passphrase",
            expected: MAX_PASSPHRASE_BYTES,
            actual: passphrase.len(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::SeededRandom;

    // Argon2id at production parameters takes tens of milliseconds per call, so
    // these tests use a small number of hashes rather than a loop over many.

    #[test]
    fn a_correct_passphrase_verifies() {
        let mut random = SeededRandom::new(1);
        let stored = hash("kata sandi yang panjang", &mut random).expect("hashes");
        assert_eq!(
            verify("kata sandi yang panjang", &stored).expect("checks"),
            Some(Verification::Ok)
        );
    }

    #[test]
    fn a_wrong_passphrase_does_not_verify_and_is_not_an_error() {
        let mut random = SeededRandom::new(2);
        let stored = hash("correct horse battery", &mut random).expect("hashes");
        assert_eq!(
            verify("wrong horse battery", &stored).expect("checks"),
            None
        );
    }

    #[test]
    fn the_same_passphrase_hashes_differently_every_time() {
        let mut random = SeededRandom::new(3);
        let first = hash("same passphrase here", &mut random).expect("hashes");
        let second = hash("same passphrase here", &mut random).expect("hashes");
        assert_ne!(first.expose(), second.expose(), "salts must differ");
        assert_eq!(
            verify("same passphrase here", &first).expect("checks"),
            Some(Verification::Ok)
        );
        assert_eq!(
            verify("same passphrase here", &second).expect("checks"),
            Some(Verification::Ok)
        );
    }

    #[test]
    fn the_encoded_hash_names_argon2id_and_its_parameters() {
        let mut random = SeededRandom::new(4);
        let stored = hash("a valid passphrase", &mut random).expect("hashes");
        let encoded = stored.expose();
        assert!(encoded.starts_with("$argon2id$"), "{encoded}");
        assert!(encoded.contains(&format!("m={MEMORY_KIB}")), "{encoded}");
        assert!(encoded.contains(&format!("t={TIME_COST}")), "{encoded}");
    }

    #[test]
    fn a_short_passphrase_is_refused() {
        let mut random = SeededRandom::new(5);
        assert!(matches!(
            hash("short", &mut random),
            Err(CryptoError::BadLength { .. })
        ));
    }

    #[test]
    fn an_absurdly_long_passphrase_is_refused_before_hashing() {
        let mut random = SeededRandom::new(6);
        let long = "x".repeat(MAX_PASSPHRASE_BYTES + 1);
        assert!(matches!(
            hash(&long, &mut random),
            Err(CryptoError::BadLength { .. })
        ));

        let stored = hash("a valid passphrase", &mut random).expect("hashes");
        assert_eq!(verify(&long, &stored).expect("checks"), None);
    }

    #[test]
    fn a_hash_with_weaker_parameters_asks_to_be_rehashed() {
        // Simulates a hash written before the cost was raised.
        let weak_params = Params::new(8 * 1024, 1, 1, Some(OUTPUT_LEN)).expect("valid params");
        let weak = Argon2::new(Algorithm::Argon2id, Version::V0x13, weak_params);
        let salt = SaltString::encode_b64(&[9u8; 16]).expect("encodes");
        let encoded = weak
            .hash_password(b"legacy passphrase", &salt)
            .expect("hashes")
            .to_string();
        let stored = Secret::new(encoded);
        assert_eq!(
            verify("legacy passphrase", &stored).expect("checks"),
            Some(Verification::NeedsRehash)
        );
    }

    #[test]
    fn a_malformed_stored_hash_is_an_error_not_a_failed_login() {
        // The distinction matters: a corrupt row should page an operator, not
        // silently tell a user their passphrase is wrong.
        let stored = Secret::new("not a PHC string".to_string());
        assert_eq!(
            verify("any passphrase", &stored),
            Err(CryptoError::PassphraseHash)
        );
    }

    #[test]
    fn a_stored_hash_does_not_print_itself() {
        let mut random = SeededRandom::new(7);
        let stored = hash("a valid passphrase", &mut random).expect("hashes");
        let rendered = format!("{stored:?}");
        assert!(!rendered.contains("argon2"), "{rendered}");
    }

    #[test]
    fn unicode_passphrases_work() {
        let mut random = SeededRandom::new(8);
        let passphrase = "kata sandi émoji 🔐 panjang";
        let stored = hash(passphrase, &mut random).expect("hashes");
        assert_eq!(
            verify(passphrase, &stored).expect("checks"),
            Some(Verification::Ok)
        );
    }
}
