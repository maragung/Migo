//! The Migo root secret and its domains.
//!
//! The root is 32 bytes from the operating system's CSPRNG, generated on the
//! device, and it is the only secret a user who loses every device actually
//! needs to have backed up — everything else in the account is a function of
//! it (except per-device credentials, which are deliberately random so that a
//! leaked root alone cannot impersonate a registered device; see
//! [`crate::identity::DeviceCredential`]).
//!
//! # Domain separation, mechanically
//!
//! Each domain is one HKDF-SHA-256 expansion of the root under its own label.
//! The labels are constants here — not strings at call sites — so the full set
//! is greppable in one place and a new domain is a code review, not a typo.
//! The labels are versioned (`/V1`) because a derivation that ever needs to
//! change must become `V2` beside the old one, never a silent change under the
//! same name: the day a label changes meaning is the day every existing
//! account's derived keys change too.
//!
//! The type is `Zeroize`-on-drop and has no `Debug` or `Display`, for the same
//! reason `SymmetricKey` in `migo-crypto` has neither: a secret that can be
//! printed eventually is printed.

use migo_core::Random;
use migo_crypto::kdf;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{AccountError, Result};

/// Root secret length in bytes.
pub const ROOT_LEN: usize = 32;

/// The identity domain: login and account authentication (ML-DSA-65).
pub const DOMAIN_IDENTITY: &[u8] = b"MIGO/IDENTITY/V1";
/// The EVM wallet domain: BIP-32 master seed, BIP-44 coin type 60.
pub const DOMAIN_EVM: &[u8] = b"MIGO/EVM/V1";
/// The E2EE domain: the founding device's X3DH identity seeds.
pub const DOMAIN_E2EE: &[u8] = b"MIGO/E2EE/V1";
/// The backup domain: the .migo container's key schedule.
pub const DOMAIN_BACKUP: &[u8] = b"MIGO/BACKUP/V1";
/// The device domain label, documented for completeness: device credentials
/// are NOT derived from the root (ADR-0013) — this label exists so the
/// conformance vectors can pin the fact that deriving it is never required,
/// and so a future per-device derivation, if one is ever justified, has an
/// already-reserved name that does not collide with the four live domains.
pub const DOMAIN_DEVICE: &[u8] = b"MIGO/DEVICE/V1";

/// Sub-label under `MIGO/E2EE/V1` for the founding device's Ed25519 signing
/// seed. The E2EE domain seed is not used raw: the existing identity format is
/// two independent seeds (signing and exchange), so the domain seed is expanded
/// once more per key, and the E2EE stack above it is untouched.
pub const LABEL_E2EE_SIGNING: &[u8] = b"migo-e2ee-signing-v1";
/// Sub-label under `MIGO/E2EE/V1` for the founding device's X25519 exchange
/// seed.
pub const LABEL_E2EE_EXCHANGE: &[u8] = b"migo-e2ee-exchange-v1";

/// The account root secret.
///
/// Generated with [`MigoRoot::generate`], restored from a container (via
/// [`crate::container`]) or from raw bytes with [`MigoRoot::from_bytes`]. No
/// path in this crate serializes it to anything but a sealed container or a
/// zeroizing byte slice.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct MigoRoot([u8; ROOT_LEN]);

impl MigoRoot {
    /// Generates a fresh root from the operating system's CSPRNG.
    #[must_use]
    pub fn generate(random: &mut dyn Random) -> Self {
        let mut bytes = [0u8; ROOT_LEN];
        random.fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// Wraps existing root bytes, e.g. after opening a container.
    ///
    /// # Errors
    ///
    /// [`AccountError::BadLength`] if the slice is not exactly 32 bytes — a
    /// length that is wrong here is a container or a port that is wrong, not
    /// an input to round.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let array: [u8; ROOT_LEN] = bytes.try_into().map_err(|_| AccountError::BadLength {
            what: "root secret",
            expected: ROOT_LEN,
            actual: bytes.len(),
        })?;
        Ok(Self(array))
    }

    /// The root bytes, for sealing into a container. The borrow is the point:
    /// there is no `to_vec`, so the caller cannot accidentally end up with an
    /// unzeroized copy that outlives the root.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Derives the 32-byte seed of one domain.
    #[must_use]
    pub fn domain_seed(&self, label: &[u8]) -> [u8; 32] {
        kdf::derive::<32>(self.as_bytes(), None, label)
    }
}

impl std::fmt::Debug for MigoRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A root that renders as its own bytes is a root that ends up in a
        // crash report. The debug output names the type and nothing else.
        f.write_str("MigoRoot(<32 bytes>)")
    }
}

/// The founding device's E2EE identity seeds, derived from the E2EE domain.
///
/// Returns `(signing_seed, exchange_seed)`: the two 32-byte seeds the existing
/// X3DH identity format is built from (Ed25519 signing, X25519 exchange). The
/// E2EE protocol above them — X3DH, the Double Ratchet, the 64-byte wire form
/// — is unchanged by the account root; only the *origin* of the founding
/// device's seeds is, which is what makes the account's E2EE history
/// recoverable from a container while additional devices keep generating fresh
/// keys and therefore never inherit historical plaintext.
#[must_use]
pub fn founding_device_e2ee_seeds(root: &MigoRoot) -> ([u8; 32], [u8; 32]) {
    let domain = root.domain_seed(DOMAIN_E2EE);
    (
        kdf::derive::<32>(&domain, None, LABEL_E2EE_SIGNING),
        kdf::derive::<32>(&domain, None, LABEL_E2EE_EXCHANGE),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::SeededRandom;

    fn root(bytes: u8) -> MigoRoot {
        MigoRoot::from_bytes(&[bytes; ROOT_LEN]).expect("a fixed-length root parses")
    }

    #[test]
    fn domains_do_not_share_output() {
        let root = root(7);
        let labels = [
            DOMAIN_IDENTITY,
            DOMAIN_EVM,
            DOMAIN_E2EE,
            DOMAIN_BACKUP,
            DOMAIN_DEVICE,
        ];
        let seeds: Vec<[u8; 32]> = labels.iter().map(|l| root.domain_seed(l)).collect();
        for (i, a) in seeds.iter().enumerate() {
            for (j, b) in seeds.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "domains {i} and {j} share a seed");
                }
            }
        }
    }

    #[test]
    fn derivation_is_deterministic_and_root_sensitive() {
        let a = root(1);
        let b = root(1);
        let c = root(2);
        assert_eq!(
            a.domain_seed(DOMAIN_IDENTITY),
            b.domain_seed(DOMAIN_IDENTITY),
            "the same root must give the same seed — this is what restore relies on"
        );
        assert_ne!(
            a.domain_seed(DOMAIN_IDENTITY),
            c.domain_seed(DOMAIN_IDENTITY)
        );
    }

    #[test]
    fn e2ee_sub_seeds_are_distinct_from_the_domain_seed() {
        let root = root(3);
        let domain = root.domain_seed(DOMAIN_E2EE);
        let (signing, exchange) = founding_device_e2ee_seeds(&root);
        assert_ne!(signing, domain);
        assert_ne!(exchange, domain);
        assert_ne!(signing, exchange, "one seed must not serve two algorithms");
    }

    #[test]
    fn generate_uses_the_source_of_randomness() {
        let mut a = SeededRandom::new(1);
        let mut b = SeededRandom::new(1);
        let mut c = SeededRandom::new(2);
        assert_eq!(
            MigoRoot::generate(&mut a),
            MigoRoot::generate(&mut b),
            "same seed, same root"
        );
        assert_ne!(MigoRoot::generate(&mut a), MigoRoot::generate(&mut c));
    }

    #[test]
    fn debug_does_not_leak_the_root() {
        let root = root(0xAB);
        let rendered = format!("{root:?}");
        assert!(!rendered.contains("ab"), "the root bytes must not render");
        assert!(rendered.contains("MigoRoot"));
    }

    #[test]
    fn wrong_length_root_is_rejected() {
        assert!(MigoRoot::from_bytes(&[0u8; 31]).is_err());
        assert!(MigoRoot::from_bytes(&[0u8; 33]).is_err());
        assert!(MigoRoot::from_bytes(&[0u8; ROOT_LEN]).is_ok());
    }
}
