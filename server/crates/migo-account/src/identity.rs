//! The ML-DSA-65 account identity and the per-device credential.
//!
//! # How a key is born (and how it is not)
//!
//! FIPS 204 defines key generation from a 32-byte seed (Algorithm 6). The
//! identity domain seed goes *into that algorithm* via `SigningKey::from_seed`
//! — it is never hashed into a "private key", because ML-DSA has no such
//! format and inventing one is exactly what the brief forbids (§182, spec #3).
//! The practical consequence: the public key is a pure function of the seed,
//! so restoring the root on any device reproduces the same identity, and the
//! ports (Rust, TypeScript, Kotlin) agree by construction rather than by
//! convention.
//!
//! # Context strings
//!
//! Every signature carries an ML-DSA context, which is mixed into the message
//! digest: a signature made over a login challenge can never be replayed as a
//! rotation approval, because the two purposes sign under different context
//! strings. Login signs under [`CONTEXT_LOGIN`], rotation under
//! [`CONTEXT_ROTATE`]. Contexts are constants — 255 bytes is the FIPS 204
//! ceiling, and a caller-supplied context is a caller that can pick the
//! empty one.
//!
//! # Device credentials are random, not derived
//!
//! [`DeviceCredential`] holds a seed from the OS CSPRNG, not from the root
//! (ADR-0013). The login challenge requires both the account identity
//! signature *and* the device credential signature, so a root secret that
//! leaks from a backup alone cannot log in as a device that is still
//! registered — the thief has the account half of the ceremony and none of
//! the device half.

use migo_core::Random;
use ml_dsa::{MlDsa65, Seed, Signature, SigningKey, VerifyingKey};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{AccountError, Result};
use crate::root::{MigoRoot, DOMAIN_IDENTITY};

/// The algorithm name recorded beside every identity public key. A string, not
/// an enum, because algorithm agility (spec #55) means the *next* algorithm is
/// data this schema already holds, not a migration.
pub const IDENTITY_ALGORITHM: &str = "ML-DSA-65";
/// The key format version this build generates. A future format is version 2
/// beside version 1, never a silent change to version 1.
pub const KEY_VERSION_ONE: u16 = 1;
/// ML-DSA-65 public key length in bytes.
pub const PUBLIC_KEY_LEN: usize = 1952;
/// ML-DSA-65 signature length in bytes.
pub const SIGNATURE_LEN: usize = 3309;
/// Seed length for every ML-DSA parameter set.
pub const SEED_LEN: usize = 32;

/// The ML-DSA context for login challenge signatures.
pub const CONTEXT_LOGIN: &[u8] = b"migo-auth-login-v1";
/// The ML-DSA context for identity rotation approvals.
pub const CONTEXT_ROTATE: &[u8] = b"migo-auth-rotate-v1";
/// The ML-DSA context for device-credential signatures in the login ceremony.
pub const CONTEXT_LOGIN_DEVICE: &[u8] = b"migo-auth-device-v1";

/// The account identity signing key: the ML-DSA-65 key the
/// `MIGO/IDENTITY/V1` domain seed becomes.
///
/// Holds the seed and only the seed. The expanded signing key is reconstructed
/// on demand — `SigningKey::from_seed` *is* FIPS 204 key generation, which is
/// what a seed is for — so the one zeroizable secret this type owns is the
/// whole secret: drop the [`IdentityKey`] and the identity's key material is
/// zeroized with it, instead of lingering in an expanded-key struct the
/// crate's `Zeroize` cannot reach. The cost is one key expansion per sign, and
/// this type signs at login and rotation: a handful of times per device per
/// day, not per message.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct IdentityKey {
    seed: [u8; SEED_LEN],
}

impl IdentityKey {
    /// Derives the identity key from a root secret.
    #[must_use]
    pub fn from_root(root: &MigoRoot) -> Self {
        Self {
            seed: root.domain_seed(DOMAIN_IDENTITY),
        }
    }

    /// Reconstructs the identity key from its 32-byte seed.
    ///
    /// # Errors
    ///
    /// [`AccountError::BadLength`] if the seed is not exactly 32 bytes.
    pub fn from_seed(seed: &[u8]) -> Result<Self> {
        Ok(Self {
            seed: seed.try_into().map_err(|_| AccountError::BadLength {
                what: "identity seed",
                expected: SEED_LEN,
                actual: seed.len(),
            })?,
        })
    }

    /// The seed, for sealing into a container.
    #[must_use]
    pub fn seed(&self) -> &[u8] {
        &self.seed
    }

    /// The encoded public key (1952 bytes), the only form the server ever
    /// stores.
    #[must_use]
    pub fn public_key(&self) -> [u8; PUBLIC_KEY_LEN] {
        let signing = self.signing();
        signing.expanded_key().verifying_key().encode().into()
    }

    /// Signs a challenge payload under the login context.
    ///
    /// The payload is the server's canonical challenge bytes, signed exactly
    /// as received — the client never re-encodes a challenge, so two
    /// implementations cannot disagree about what was signed.
    ///
    /// # Errors
    ///
    /// [`AccountError::BadSignature`] if signing fails, which for
    /// deterministic signing under a constant context is unreachable.
    pub fn sign_login(&self, payload: &[u8]) -> Result<[u8; SIGNATURE_LEN]> {
        self.sign(payload, CONTEXT_LOGIN)
    }

    /// Signs under the rotation context.
    ///
    /// # Errors
    ///
    /// As [`IdentityKey::sign_login`].
    pub fn sign_rotate(&self, payload: &[u8]) -> Result<[u8; SIGNATURE_LEN]> {
        self.sign(payload, CONTEXT_ROTATE)
    }

    fn signing(&self) -> SigningKey<MlDsa65> {
        let seed: Seed = self.seed.into();
        SigningKey::from_seed(&seed)
    }

    fn sign(&self, payload: &[u8], context: &[u8]) -> Result<[u8; SIGNATURE_LEN]> {
        // Deterministic signing (FIPS 204 permits both variants): no RNG
        // dependency, and the same signature bytes in every port, which is
        // what lets the conformance vectors pin them.
        let signature = self
            .signing()
            .expanded_key()
            .sign_deterministic(payload, context)
            .map_err(|_| AccountError::BadSignature)?;
        Ok(signature.encode().into())
    }
}

impl std::fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IdentityKey(<ML-DSA-65>)")
    }
}

/// Verifies an identity signature against a public key.
///
/// The public key is the server's stored form (the encoded
/// [`VerifyingKey`]); the context must be the one the signature was made
/// under, which is why callers pass a constant rather than reaching for the
/// empty context.
///
/// # Errors
///
/// [`AccountError::BadLength`] if the public key or signature is not the
/// encoded ML-DSA-65 length — a wrong length is a client that is wrong, not
/// an input to trim. [`AccountError::BadSignature`] if the key or signature
/// does not decode, or the signature does not verify: the three cases are one
/// refusal, so a caller cannot use the difference as an oracle.
pub fn verify_identity(
    public_key: &[u8],
    payload: &[u8],
    context: &[u8],
    signature: &[u8],
) -> Result<()> {
    let key: [u8; PUBLIC_KEY_LEN] = public_key.try_into().map_err(|_| AccountError::BadLength {
        what: "identity public key",
        expected: PUBLIC_KEY_LEN,
        actual: public_key.len(),
    })?;
    let sig: [u8; SIGNATURE_LEN] = signature.try_into().map_err(|_| AccountError::BadLength {
        what: "identity signature",
        expected: SIGNATURE_LEN,
        actual: signature.len(),
    })?;
    // `VerifyingKey::decode` is infallible (every 1952-byte string is a
    // syntactically valid encoding); a garbage key simply fails verification
    // below, which lands in the same refusal.
    let verifying = VerifyingKey::<MlDsa65>::decode(&key.into());
    let Some(decoded) = Signature::<MlDsa65>::decode(&sig.into()) else {
        return Err(AccountError::BadSignature);
    };
    if verifying.verify_with_context(payload, context, &decoded) {
        Ok(())
    } else {
        Err(AccountError::BadSignature)
    }
}

/// A per-device signing credential, generated from a random seed on the
/// device it belongs to.
///
/// Same algorithm and wire forms as the identity key; what differs is the
/// origin of the seed, which is the whole point — see the module docs.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DeviceCredential {
    seed: [u8; SEED_LEN],
}

impl DeviceCredential {
    /// Generates a fresh credential from the OS CSPRNG.
    #[must_use]
    pub fn generate(random: &mut dyn Random) -> Self {
        let mut seed = [0u8; SEED_LEN];
        random.fill_bytes(&mut seed);
        Self { seed }
    }

    /// Reconstructs a credential from its stored seed.
    ///
    /// # Errors
    ///
    /// [`AccountError::BadLength`] if the seed is not exactly 32 bytes.
    pub fn from_seed(seed: &[u8]) -> Result<Self> {
        Ok(Self {
            seed: seed.try_into().map_err(|_| AccountError::BadLength {
                what: "device credential seed",
                expected: SEED_LEN,
                actual: seed.len(),
            })?,
        })
    }

    /// The seed, for the device vault.
    #[must_use]
    pub fn seed(&self) -> &[u8] {
        &self.seed
    }

    /// The encoded public key (1952 bytes) registered on the device row.
    #[must_use]
    pub fn public_key(&self) -> [u8; PUBLIC_KEY_LEN] {
        let seed: Seed = self.seed.into();
        SigningKey::<MlDsa65>::from_seed(&seed)
            .expanded_key()
            .verifying_key()
            .encode()
            .into()
    }

    /// Signs a login challenge under the device context. Login challenges are
    /// signed by both keys (account and device), each under its own context,
    /// so one signature can never be stood in for the other.
    ///
    /// # Errors
    ///
    /// [`AccountError::BadSignature`] on the (unreachable) signing failure.
    pub fn sign_login(&self, payload: &[u8]) -> Result<[u8; SIGNATURE_LEN]> {
        let seed: Seed = self.seed.into();
        let signature = SigningKey::<MlDsa65>::from_seed(&seed)
            .expanded_key()
            .sign_deterministic(payload, CONTEXT_LOGIN_DEVICE)
            .map_err(|_| AccountError::BadSignature)?;
        Ok(signature.encode().into())
    }
}

impl std::fmt::Debug for DeviceCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceCredential(<ML-DSA-65>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::root::MigoRoot;
    use migo_core::SeededRandom;

    fn payload() -> Vec<u8> {
        (0..64u8).collect()
    }

    #[test]
    fn the_identity_key_is_a_pure_function_of_the_root() {
        let root = MigoRoot::from_bytes(&[5u8; 32]).expect("root");
        let a = IdentityKey::from_root(&root);
        let b = IdentityKey::from_root(&root);
        assert_eq!(a.public_key(), b.public_key());
        assert_eq!(a.seed(), b.seed());
    }

    #[test]
    fn different_roots_give_different_identities() {
        let a = IdentityKey::from_root(&MigoRoot::from_bytes(&[1u8; 32]).expect("root"));
        let b = IdentityKey::from_root(&MigoRoot::from_bytes(&[2u8; 32]).expect("root"));
        assert_ne!(a.public_key(), b.public_key());
    }

    #[test]
    fn a_signature_verifies_and_a_forged_one_does_not() {
        let key = IdentityKey::from_root(&MigoRoot::from_bytes(&[9u8; 32]).expect("root"));
        let sig = key.sign_login(&payload()).expect("signing works");
        verify_identity(&key.public_key(), &payload(), CONTEXT_LOGIN, &sig)
            .expect("a real signature verifies");

        let mut forged = sig;
        forged[0] ^= 1;
        assert_eq!(
            verify_identity(&key.public_key(), &payload(), CONTEXT_LOGIN, &forged),
            Err(AccountError::BadSignature)
        );
    }

    #[test]
    fn a_signature_under_the_wrong_context_does_not_verify() {
        let key = IdentityKey::from_root(&MigoRoot::from_bytes(&[9u8; 32]).expect("root"));
        let sig = key.sign_login(&payload()).expect("signing works");
        // The same key, the same bytes, a different purpose: the context is
        // mixed into the digest, so this is the replay-between-purposes
        // defence being exercised, not an exotic corner.
        assert_eq!(
            verify_identity(&key.public_key(), &payload(), CONTEXT_ROTATE, &sig),
            Err(AccountError::BadSignature)
        );
    }

    #[test]
    fn signing_is_deterministic() {
        let key = IdentityKey::from_root(&MigoRoot::from_bytes(&[4u8; 32]).expect("root"));
        assert_eq!(
            key.sign_login(&payload()).expect("sign"),
            key.sign_login(&payload()).expect("sign again"),
            "deterministic signing is what makes the cross-port vectors pin byte-for-byte"
        );
    }

    #[test]
    fn a_device_credential_is_independent_of_the_root() {
        let mut rng = SeededRandom::new(11);
        let credential = DeviceCredential::generate(&mut rng);
        let root = MigoRoot::from_bytes(&[8u8; 32]).expect("root");
        let identity = IdentityKey::from_root(&root);
        assert_ne!(
            credential.public_key().to_vec(),
            identity.public_key().to_vec(),
            "the device credential must not be a function of the root"
        );
        // And both halves of the login ceremony sign the same payload under
        // different contexts.
        let payload = payload();
        let identity_sig = identity.sign_login(&payload).expect("sign");
        let device_sig = credential.sign_login(&payload).expect("sign");
        assert_ne!(identity_sig.to_vec(), device_sig.to_vec());
        verify_identity(
            &credential.public_key(),
            &payload,
            CONTEXT_LOGIN_DEVICE,
            &device_sig,
        )
        .expect("the device signature verifies under the device context");
    }

    #[test]
    fn wrong_lengths_are_rejected_not_trimmed() {
        let key = IdentityKey::from_root(&MigoRoot::from_bytes(&[3u8; 32]).expect("root"));
        let sig = key.sign_login(&payload()).expect("signing works");
        assert_eq!(
            verify_identity(&key.public_key()[..100], &payload(), CONTEXT_LOGIN, &sig),
            Err(AccountError::BadLength {
                what: "identity public key",
                expected: PUBLIC_KEY_LEN,
                actual: 100,
            })
        );
        assert_eq!(
            verify_identity(&key.public_key(), &payload(), CONTEXT_LOGIN, &sig[..10]),
            Err(AccountError::BadLength {
                what: "identity signature",
                expected: SIGNATURE_LEN,
                actual: 10,
            })
        );
    }

    #[test]
    fn a_public_key_that_does_not_decode_is_a_plain_refusal() {
        let key = IdentityKey::from_root(&MigoRoot::from_bytes(&[3u8; 32]).expect("root"));
        let sig = key.sign_login(&payload()).expect("signing works");
        // Right length, not a decodable ML-DSA-65 key: same error as a bad
        // signature, and no hint about which half failed.
        let mut garbage = [0xFFu8; PUBLIC_KEY_LEN];
        garbage[0] = 0x42;
        assert_eq!(
            verify_identity(&garbage, &payload(), CONTEXT_LOGIN, &sig),
            Err(AccountError::BadSignature)
        );
    }

    #[test]
    fn debug_names_the_type_and_nothing_else() {
        let key = IdentityKey::from_root(&MigoRoot::from_bytes(&[6u8; 32]).expect("root"));
        assert_eq!(format!("{key:?}"), "IdentityKey(<ML-DSA-65>)");
    }
}
